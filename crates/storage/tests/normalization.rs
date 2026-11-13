use briefly_searcher_storage::Storage;
use sqlx::PgPool;

#[sqlx::test]
async fn reads_only_blocks_below_completed_boundary(pool: PgPool) {
    sqlx::query("INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES (1, '{}', 'test'), (2, '{}', 'test')")
        .execute(&pool).await.unwrap();
    let storage = Storage::from_pool(pool.clone());
    let input = storage.normalization_input(None).await.unwrap();
    assert!(input.is_empty());
    let input = storage.normalization_input(Some(1)).await.unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0].id, 1);
}

#[sqlx::test]
async fn replacement_and_boundary_roll_back_together(pool: PgPool) {
    use briefly_searcher_normalizer::{Article, ArticleStatus, Normalized, Post};
    let at = chrono::Utc::now();
    sqlx::query("INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES (1, '{}', 'test'), (2, '{}', 'test')")
        .execute(&pool).await.unwrap();
    let storage = Storage::from_pool(pool.clone());
    let mut result = Normalized {
        articles: vec![Article {
            id: 1,
            title: None,
            search_text: "Первый".into(),
            status: ArticleStatus::Single,
            declared_parts: None,
            published_at: at,
        }],
        posts: vec![Post {
            message_id: 1,
            article_id: 1,
            part_no: None,
            text: "Первый".into(),
            published_at: at,
            forwarded_from: Some("Канал".into()),
        }],
    };
    storage
        .replace_normalized(&result, Some(1), at, 10)
        .await
        .unwrap();
    result.articles[0].id = 2;
    result.posts[0].message_id = 2;
    result.posts[0].article_id = 999; // FK: вставка постов падает после вставки статей.
    assert!(
        storage
            .replace_normalized(&result, Some(2), at, 20)
            .await
            .is_err()
    );
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM articles")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, [1]);
    let ids: Vec<i64> = sqlx::query_scalar("SELECT message_id FROM posts")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, [1]);
    assert_eq!(
        storage.normalizer_state().await.unwrap().normalized_up_to,
        Some(1)
    );
    result.posts[0].article_id = 2;
    result.posts[0].part_no = Some(1);
    result.articles[0].declared_parts = Some(2);
    storage
        .replace_normalized(&result, Some(2), at, 20)
        .await
        .unwrap();
    let row: (i64, Option<i32>, Option<String>) =
        sqlx::query_as("SELECT message_id, part_no, forwarded_from FROM posts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (2, Some(1), Some("Канал".into())));
    assert_eq!(
        storage.normalizer_state().await.unwrap().normalized_up_to,
        Some(2)
    );
    storage
        .replace_normalized(
            &Normalized {
                articles: vec![],
                posts: vec![],
            },
            None,
            at,
            0,
        )
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM articles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        storage.normalizer_state().await.unwrap().normalized_up_to,
        None
    );
}

use briefly_searcher_storage::{RawMessage, Storage};
use chrono::Utc;
use sqlx::PgPool;

#[sqlx::test]
async fn summary_limits_incomplete_articles_and_orders_their_parts(pool: PgPool) {
    sqlx::query("INSERT INTO raw_posts (message_id, payload, payload_schema) SELECT id, '{}', 'test' FROM generate_series(1, 100) id").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO articles (id, title, search_text, status, published_at) SELECT id, 'Материал', 'Текст', 'incomplete', '2026-11-14T00:00:00Z'::timestamptz FROM generate_series(1, 21) id").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO posts (message_id, article_id, part_no, text, published_at) VALUES (21, 21, 3, 'Третья', now()), (22, 21, 1, 'Первая', now())").execute(&pool).await.unwrap();
    let summary = Storage::from_pool(pool).admin_summary().await.unwrap();
    assert_eq!(summary.incomplete_articles_count, 21);
    assert_eq!(summary.incomplete_articles.0.len(), 20);
    assert_eq!(summary.incomplete_articles.0[0].id, 21);
    assert_eq!(summary.incomplete_articles.0[19].id, 2);
    assert_eq!(summary.incomplete_articles.0[0].part_ids, [22, 21]);
    assert_eq!(summary.posts_count, 2);
}

#[sqlx::test]
async fn summary_counts_only_blocks_in_published_normalization(pool: PgPool) {
    let storage = Storage::from_pool(pool);
    let payload = serde_json::json!({});
    let messages: Vec<_> = (1..=5)
        .map(|message_id| RawMessage {
            message_id,
            payload: &payload,
            payload_schema: "test",
        })
        .collect();
    storage
        .finish_pass(&messages[..4], Utc::now())
        .await
        .unwrap();
    storage.save_page(&messages[4..], Utc::now()).await.unwrap();
    let at = "2026-11-14T01:00:00Z".parse().unwrap();
    let result = briefly_searcher_normalizer::Normalized {
        posts: vec![],
        articles: vec![],
    };
    storage
        .replace_normalized(&result, Some(3), at, 42)
        .await
        .unwrap();
    let summary = storage.admin_summary().await.unwrap();
    assert_eq!(summary.normalized_up_to, Some(3));
    assert_eq!(summary.posts_count, 0);
    assert_eq!(summary.excluded_blocks_count, 3);
    assert_eq!(summary.single_articles_count, 0);
    assert_eq!(summary.complete_articles_count, 0);
    assert_eq!(summary.incomplete_articles_count, 0);
    assert_eq!(summary.normalizer_recalculation_started_at, Some(at));
    assert!(summary.normalizer_recalculation_duration_ms.unwrap() >= 42);
    assert!(summary.incomplete_articles.0.is_empty());
}

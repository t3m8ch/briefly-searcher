//! Настоящий загрузчик с источником на веб-ленте против локального сервера с
//! записанными страницами `t.me/s/brieflyru` и тестовой PostgreSQL.

#[path = "../../telegram/tests/support/mod.rs"]
mod support;

use briefly_searcher::clock::SystemClock;
use briefly_searcher::loader::{Loader, Settings};
use briefly_searcher_storage::Storage;
use chrono::TimeDelta;
use sqlx::PgPool;

use support::{FeedServer, Reply, fixtures};

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn first_pass_walks_recorded_start_of_channel_down_to_empty_page(pool: PgPool) {
    // Канал, самая свежая страница которого — записанная `before=30`.
    let server = FeedServer::start([
        (None, Reply::page(fixtures::BEFORE_30)),
        (Some(10), Reply::page(fixtures::BEFORE_10)),
        (Some(1), Reply::page(fixtures::BEFORE_1)),
    ])
    .await;
    let settings = Settings {
        page_size: 100,
        request_delay: TimeDelta::zero(),
        poll_interval: TimeDelta::minutes(10),
    };
    let mut loader = Loader::new(
        Storage::from_pool(pool.clone()),
        server.source(),
        SystemClock,
        settings,
    );

    loader.run_pass().await.unwrap();

    // 29…10 | 9 8 1 | пустая лента с пометкой начала канала.
    assert_eq!(
        server.queries(),
        [
            None,
            Some("before=10".to_owned()),
            Some("before=1".to_owned())
        ]
    );
    let rows = sqlx::query!(
        "SELECT message_id, payload, payload_schema FROM raw_posts ORDER BY message_id"
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let ids: Vec<i64> = rows.iter().map(|row| row.message_id).collect();
    assert_eq!(
        ids,
        [1, 8, 9].into_iter().chain(10..=29).collect::<Vec<_>>()
    );
    for row in &rows {
        assert_eq!(row.payload_schema, "t.me/s html v1");
        let html = row.payload["html"].as_str().unwrap();
        assert!(html.contains(&format!("data-post=\"brieflyru/{}\"", row.message_id)));
    }
    assert!(
        rows[0].payload["html"]
            .as_str()
            .unwrap()
            .contains("Channel created")
    );

    let newest_fetched_id = sqlx::query_scalar!("SELECT newest_fetched_id FROM ingestion_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(newest_fetched_id, Some(29));
}

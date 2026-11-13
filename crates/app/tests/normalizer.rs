use briefly_searcher::{clock::SystemClock, normalizer::Normalizer};
use briefly_searcher_storage::Storage;
use chrono::TimeDelta;
use sqlx::PgPool;

async fn add_block(pool: &PgPool, id: i64) {
    let html = format!(
        r#"<div class="tgme_widget_message" data-post="brieflyru/{id}"><div class="tgme_widget_message_text">Текст {id}</div><a class="tgme_widget_message_date"><time datetime="2026-11-14T00:00:00Z"></time></a></div>"#
    );
    sqlx::query("INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES ($1, $2, 't.me/s html v1')")
        .bind(id).bind(serde_json::json!({"html": html})).execute(pool).await.unwrap();
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn startup_rebuilds_then_only_boundary_growth_triggers_rebuild(pool: PgPool) {
    add_block(&pool, 1).await;
    sqlx::query("UPDATE ingestion_state SET newest_fetched_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let storage = Storage::from_pool(pool.clone());
    let normalizer = Normalizer::new(storage.clone(), SystemClock, TimeDelta::seconds(30));
    assert!(normalizer.poll(true).await.unwrap());
    let first = storage.normalizer_state().await.unwrap();
    assert!(!normalizer.poll(false).await.unwrap());
    assert_eq!(
        storage
            .normalizer_state()
            .await
            .unwrap()
            .last_recalculation_started_at,
        first.last_recalculation_started_at
    );
    add_block(&pool, 2).await;
    assert!(!normalizer.poll(false).await.unwrap());
    sqlx::query("UPDATE ingestion_state SET newest_fetched_id = 2")
        .execute(&pool)
        .await
        .unwrap();
    assert!(normalizer.poll(false).await.unwrap());
    assert_eq!(
        storage.normalizer_state().await.unwrap().normalized_up_to,
        Some(2)
    );
    // Новый процесс принудительно применяет правила даже при прежней границе.
    assert!(normalizer.poll(true).await.unwrap());
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn malformed_block_keeps_published_result_and_records_error(pool: PgPool) {
    add_block(&pool, 1).await;
    sqlx::query("UPDATE ingestion_state SET newest_fetched_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let storage = Storage::from_pool(pool.clone());
    let normalizer = Normalizer::new(storage.clone(), SystemClock, TimeDelta::seconds(30));
    normalizer.poll(true).await.unwrap();
    sqlx::query("INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES (2, '{}', 't.me/s html v1')").execute(&pool).await.unwrap();
    sqlx::query("UPDATE ingestion_state SET newest_fetched_id = 2")
        .execute(&pool)
        .await
        .unwrap();
    assert!(normalizer.poll(false).await.is_err());
    let state = storage.normalizer_state().await.unwrap();
    assert_eq!(state.normalized_up_to, Some(1));
    assert!(state.last_error.unwrap().contains("блок 2"));
    assert!(state.last_error_at.is_some());
    let ids: Vec<i64> = sqlx::query_scalar("SELECT message_id FROM posts")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, [1]);
    sqlx::query("DELETE FROM raw_posts WHERE message_id = 2")
        .execute(&pool)
        .await
        .unwrap();
    add_block(&pool, 2).await;
    assert!(normalizer.poll(false).await.unwrap());
    assert_eq!(
        storage.normalizer_state().await.unwrap().normalized_up_to,
        Some(2)
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn lock_is_independent_of_loader_and_released_on_shutdown(pool: PgPool) {
    use briefly_searcher::normalizer::{Error, lock};
    let storage = Storage::from_pool(pool);
    let loader_lock = storage.try_lock_loader().await.unwrap().unwrap();
    let first = lock(&storage).await.unwrap();
    assert!(matches!(lock(&storage).await, Err(Error::AlreadyRunning)));
    Normalizer::new(storage.clone(), SystemClock, TimeDelta::seconds(30))
        .run(first, std::future::ready(()))
        .await
        .unwrap();
    lock(&storage).await.unwrap().release().await.unwrap();
    loader_lock.release().await.unwrap();
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn null_boundary_publishes_empty_result(pool: PgPool) {
    add_block(&pool, 1).await;
    let storage = Storage::from_pool(pool.clone());
    Normalizer::new(storage.clone(), SystemClock, TimeDelta::seconds(30))
        .poll(true)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM posts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        storage
            .normalizer_state()
            .await
            .unwrap()
            .last_recalculation_started_at
            .is_some()
    );
}

/// Часы отдают управление тесту на каждом ожидании, без реальных пауз.
#[derive(Clone)]
struct ControlledClock {
    now: std::sync::Arc<std::sync::Mutex<chrono::DateTime<chrono::Utc>>>,
    waiting: std::sync::Arc<tokio::sync::Notify>,
    advance: std::sync::Arc<tokio::sync::Notify>,
}

impl briefly_searcher::clock::Clock for ControlledClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        *self.now.lock().unwrap()
    }
    async fn sleep_until(&self, deadline: chrono::DateTime<chrono::Utc>) {
        self.waiting.notify_one();
        self.advance.notified().await;
        *self.now.lock().unwrap() = deadline;
    }
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn process_retries_startup_error_on_next_poll_and_stops_while_waiting(pool: PgPool) {
    use briefly_searcher::normalizer::lock;
    let clock = ControlledClock {
        now: std::sync::Arc::new(std::sync::Mutex::new(chrono::Utc::now())),
        waiting: Default::default(),
        advance: Default::default(),
    };
    sqlx::query("INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES (1, '{}', 't.me/s html v1')").execute(&pool).await.unwrap();
    sqlx::query("UPDATE ingestion_state SET newest_fetched_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let storage = Storage::from_pool(pool.clone());
    let guard = lock(&storage).await.unwrap();
    let normalizer = Normalizer::new(storage.clone(), clock.clone(), TimeDelta::seconds(15));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let process = tokio::spawn(async move {
        normalizer
            .run(guard, async {
                stopped.await.unwrap();
            })
            .await
    });
    clock.waiting.notified().await;
    assert!(
        storage
            .normalizer_state()
            .await
            .unwrap()
            .last_error
            .is_some()
    );
    sqlx::query("DELETE FROM raw_posts")
        .execute(&pool)
        .await
        .unwrap();
    add_block(&pool, 1).await;
    clock.advance.notify_one();
    clock.waiting.notified().await;
    assert_eq!(
        storage.normalizer_state().await.unwrap().normalized_up_to,
        Some(1)
    );
    let first = storage
        .normalizer_state()
        .await
        .unwrap()
        .last_recalculation_started_at;
    clock.advance.notify_one();
    clock.waiting.notified().await;
    assert_eq!(
        storage
            .normalizer_state()
            .await
            .unwrap()
            .last_recalculation_started_at,
        first
    );
    stop.send(()).unwrap();
    process.await.unwrap().unwrap();
    lock(&storage).await.unwrap().release().await.unwrap();
}

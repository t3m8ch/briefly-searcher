use briefly_searcher_storage::Storage;
use sqlx::PgPool;

/// Столбцы таблиц части 1: (таблица, столбец, тип, допускает NULL).
const EXPECTED_COLUMNS: &[(&str, &str, &str, bool)] = &[
    ("ingestion_state", "singleton", "boolean", false),
    ("ingestion_state", "newest_fetched_id", "bigint", true),
    (
        "ingestion_state",
        "flood_wait_until",
        "timestamp with time zone",
        true,
    ),
    (
        "ingestion_state",
        "last_successful_request_at",
        "timestamp with time zone",
        true,
    ),
    ("raw_posts", "message_id", "bigint", false),
    ("raw_posts", "payload", "jsonb", false),
    ("raw_posts", "payload_schema", "text", false),
    ("raw_posts", "fetched_at", "timestamp with time zone", false),
    ("telegram_session", "singleton", "boolean", false),
    ("telegram_session", "session", "bytea", false),
    (
        "telegram_session",
        "updated_at",
        "timestamp with time zone",
        false,
    ),
    ("worker_state", "singleton", "boolean", false),
    (
        "worker_state",
        "last_heartbeat_at",
        "timestamp with time zone",
        true,
    ),
    ("worker_state", "last_error", "text", true),
    (
        "worker_state",
        "last_error_at",
        "timestamp with time zone",
        true,
    ),
    (
        "worker_state",
        "next_attempt_at",
        "timestamp with time zone",
        true,
    ),
];

#[sqlx::test(migrations = false)]
async fn migrate_creates_loader_schema_on_empty_database(pool: PgPool) {
    Storage::from_pool(pool.clone()).migrate().await.unwrap();

    let columns = sqlx::query!(
        r#"
        SELECT
            table_name::text AS "table_name!",
            column_name::text AS "column_name!",
            data_type::text AS "data_type!",
            is_nullable = 'YES' AS "is_nullable!"
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'
        ORDER BY table_name, ordinal_position
        "#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let columns: Vec<_> = columns
        .iter()
        .map(|c| {
            (
                c.table_name.as_str(),
                c.column_name.as_str(),
                c.data_type.as_str(),
                c.is_nullable,
            )
        })
        .collect();
    assert_eq!(columns, EXPECTED_COLUMNS);

    let primary_keys = sqlx::query!(
        r#"
        SELECT
            tc.table_name::text AS "table_name!",
            kcu.column_name::text AS "column_name!"
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
            USING (constraint_schema, constraint_name)
        WHERE tc.table_schema = 'public'
            AND tc.constraint_type = 'PRIMARY KEY'
            AND tc.table_name <> '_sqlx_migrations'
        ORDER BY tc.table_name
        "#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let primary_keys: Vec<_> = primary_keys
        .iter()
        .map(|k| (k.table_name.as_str(), k.column_name.as_str()))
        .collect();
    assert_eq!(
        primary_keys,
        [
            ("ingestion_state", "singleton"),
            ("raw_posts", "message_id"),
            ("telegram_session", "singleton"),
            ("worker_state", "singleton"),
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn migrate_is_idempotent(pool: PgPool) {
    let storage = Storage::from_pool(pool);
    storage.migrate().await.unwrap();
    storage.migrate().await.unwrap();
}

#[sqlx::test]
async fn state_tables_start_with_their_single_row(pool: PgPool) {
    let ingestion = sqlx::query!(
        "SELECT newest_fetched_id, flood_wait_until, last_successful_request_at FROM ingestion_state"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ingestion.newest_fetched_id, None);
    assert_eq!(ingestion.flood_wait_until, None);
    assert_eq!(ingestion.last_successful_request_at, None);

    let worker = sqlx::query!("SELECT last_heartbeat_at, last_error FROM worker_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(worker.last_heartbeat_at, None);
    assert_eq!(worker.last_error, None);

    // До команды login сессии нет.
    let sessions = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM telegram_session"#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);

    // Второй записи в таблицах-одиночках быть не может.
    let second = sqlx::query!("INSERT INTO worker_state (singleton) VALUES (FALSE)")
        .execute(&pool)
        .await;
    assert!(second.is_err());
}

#[sqlx::test]
async fn raw_posts_keeps_first_payload_for_message_id(pool: PgPool) {
    for text in ["первый", "второй"] {
        sqlx::query!(
            "INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES ($1, $2, $3)
             ON CONFLICT (message_id) DO NOTHING",
            42_i64,
            serde_json::json!({ "message": text }),
            "test",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let payload = sqlx::query_scalar!("SELECT payload FROM raw_posts WHERE message_id = 42")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload, serde_json::json!({ "message": "первый" }));
}

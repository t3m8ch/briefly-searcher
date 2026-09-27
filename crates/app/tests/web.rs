//! Веб-админка: HTTP-запросы к роутеру над тестовой БД с подготовленным состоянием.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use briefly_searcher::web;
use briefly_searcher_storage::Storage;
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

/// Ответ роутера на `GET uri` с `Host` loopback-адреса по умолчанию.
async fn get(pool: &PgPool, uri: &str) -> (StatusCode, String) {
    request(
        pool,
        Request::get(uri).header(header::HOST, "127.0.0.1:3000"),
    )
    .await
}

async fn request(pool: &PgPool, request: axum::http::request::Builder) -> (StatusCode, String) {
    let router = web::router(Storage::from_pool(pool.clone()));
    let response = router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_telegram_pause_deadline(pool: PgPool) {
    sqlx::query!("UPDATE worker_state SET last_heartbeat_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query!("UPDATE ingestion_state SET flood_wait_until = '2099-01-02 03:04:05Z'")
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) = get(&pool, "/status").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("пауза Telegram"), "{body}");
    assert!(
        body.contains(r#"<time datetime="2099-01-02T03:04:05Z">2099-01-02 03:04:05 UTC</time>"#),
        "{body}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_error_text_time_and_next_attempt(pool: PgPool) {
    sqlx::query!(
        "UPDATE worker_state SET last_heartbeat_at = now(),
             last_error = 'Telegram: <b>timeout</b>',
             last_error_at = '2026-09-27 03:00:00Z',
             next_attempt_at = '2099-01-02 03:04:05Z'"
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query!("UPDATE ingestion_state SET last_successful_request_at = '2026-09-27 02:59:00Z'")
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) = get(&pool, "/status").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("ошибка, ждёт повтора"), "{body}");
    // Текст ошибки экранирован: HTML из него не исполняется.
    assert!(
        body.contains("Telegram: &#60;b&#62;timeout&#60;/b&#62;"),
        "{body}"
    );
    assert!(!body.contains("<b>timeout"), "{body}");
    assert!(
        body.contains(r#"<time datetime="2026-09-27T03:00:00Z">"#),
        "{body}"
    );
    assert!(body.contains("Следующая попытка запроса"), "{body}");
    assert!(
        body.contains(r#"<time datetime="2099-01-02T03:04:05Z">"#),
        "{body}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_past_error_as_resolved(pool: PgPool) {
    sqlx::query!(
        "UPDATE worker_state SET last_heartbeat_at = now(),
             last_error = 'connection reset by peer',
             last_error_at = now() - interval '7 hours',
             next_attempt_at = now() - interval '7 hours' + interval '1 minute'"
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query!("UPDATE ingestion_state SET last_successful_request_at = now()")
        .execute(&pool)
        .await
        .unwrap();

    let (_, body) = get(&pool, "/status").await;

    assert!(body.contains("работает"), "{body}");
    assert!(body.contains("после неё были успешные запросы"), "{body}");
    assert!(body.contains("connection reset by peer"), "{body}");
    assert!(!body.contains("Следующая попытка"), "{body}");
}

/// Сохраняет сообщения с данными ID, как это сделал бы загрузчик.
async fn insert_messages(pool: &PgPool, ids: &[i64]) {
    for &id in ids {
        sqlx::query!(
            "INSERT INTO raw_posts (message_id, payload, payload_schema) VALUES ($1, '{}', 'test')",
            id
        )
        .execute(pool)
        .await
        .unwrap();
    }
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_how_far_first_pass_reached(pool: PgPool) {
    insert_messages(&pool, &[43_870, 43_869, 21_384]).await;

    let (_, body) = get(&pool, "/status").await;

    assert!(body.contains("Первый проход:"), "{body}");
    assert!(body.contains("дошёл до ID 21\u{a0}384"), "{body}");
    assert!(body.contains("3 сообщения"), "{body}");
    assert!(
        body.contains("newest_fetched_id:</span><span class=\"t-dim\">NULL"),
        "{body}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_loaded_history_and_newest_fetched_id(pool: PgPool) {
    insert_messages(&pool, &[1, 2, 43_870]).await;
    sqlx::query!("UPDATE ingestion_state SET newest_fetched_id = 43870")
        .execute(&pool)
        .await
        .unwrap();

    let (_, body) = get(&pool, "/status").await;

    assert!(body.contains("загружена до ID 43\u{a0}870"), "{body}");
    assert!(body.contains("незавершённого прохода нет"), "{body}");
    assert!(
        body.contains("newest_fetched_id:</span><span>43\u{a0}870"),
        "{body}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_how_far_pass_for_new_messages_reached(pool: PgPool) {
    insert_messages(&pool, &[1, 2, 43_870, 43_905, 43_990]).await;
    sqlx::query!("UPDATE ingestion_state SET newest_fetched_id = 43870")
        .execute(&pool)
        .await
        .unwrap();

    let (_, body) = get(&pool, "/status").await;

    assert!(body.contains("загружена до ID 43\u{a0}870"), "{body}");
    // min(message_id) среди строк новее newest_fetched_id, а не среди всех.
    assert!(
        body.contains("проход за новыми дошёл до ID 43\u{a0}905"),
        "{body}"
    );
    assert!(body.contains("5 сообщений"), "{body}");
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_heartbeat_and_last_successful_request(pool: PgPool) {
    sqlx::query!("UPDATE worker_state SET last_heartbeat_at = now() - interval '4 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query!("UPDATE ingestion_state SET last_successful_request_at = '2026-09-27 03:00:00Z'")
        .execute(&pool)
        .await
        .unwrap();

    let (_, body) = get(&pool, "/status").await;

    assert!(body.contains("работает"), "{body}");
    assert!(body.contains("Heartbeat загрузчика"), "{body}");
    assert!(body.contains("4 с назад"), "{body}");
    assert!(body.contains("Успешный запрос к Telegram"), "{body}");
    assert!(
        body.contains(r#"<time datetime="2026-09-27T03:00:00Z">"#),
        "{body}"
    );
    // Полный объём истории неизвестен, «43 000+» знаменателем не служит.
    assert!(!body.contains("43 000"), "{body}");
    assert!(!body.contains('%'), "{body}");
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_shows_silent_loader_even_during_pause(pool: PgPool) {
    sqlx::query!("UPDATE worker_state SET last_heartbeat_at = now() - interval '6 minutes'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query!("UPDATE ingestion_state SET flood_wait_until = now() + interval '38 minutes'")
        .execute(&pool)
        .await
        .unwrap();

    let (_, body) = get(&pool, "/status").await;

    assert!(
        body.contains("loader — <span class=\"t-bad\">не отвечает"),
        "{body}"
    );
    assert!(body.contains("с тех пор загрузчик молчит"), "{body}");
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_on_fresh_database_says_loader_never_ran(pool: PgPool) {
    let (status, body) = get(&pool, "/status").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("ещё не запускался"), "{body}");
    assert!(body.contains("не начата"), "{body}");
    assert!(body.contains("0 сообщений"), "{body}");
    assert!(body.contains("событий ещё не было"), "{body}");
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn page_contains_summary_and_polls_status(pool: PgPool) {
    sqlx::query!("UPDATE ingestion_state SET flood_wait_until = '2099-01-02 03:04:05Z'")
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) = get(&pool, "/").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with("<!doctype html>"), "{body}");
    // Сводка уже на странице — тот же блок, что отдаёт /status.
    assert!(
        body.contains(r#"<time datetime="2099-01-02T03:04:05Z">"#),
        "{body}"
    );
    assert!(body.contains(r#"hx-get="/status""#), "{body}");
    assert!(body.contains(r#"hx-trigger="every 5s""#), "{body}");
    assert!(
        body.contains(r#"<script src="/htmx.min.js"></script>"#),
        "{body}"
    );
    // Никаких сторонних скриптов и стилей (CDN).
    assert!(
        !body.contains("http://") && !body.contains("https://"),
        "{body}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn server_serves_its_own_htmx(pool: PgPool) {
    let router = web::router(Storage::from_pool(pool));
    let response = router
        .oneshot(
            Request::get("/htmx.min.js")
                .header(header::HOST, "localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains(r#"version:"2.0.11""#));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn requests_to_loopback_hosts_are_served(pool: PgPool) {
    for host in [
        "localhost",
        "localhost:3000",
        "127.0.0.1:3000",
        "127.0.0.2",
        "[::1]:3000",
    ] {
        let (status, _) = request(&pool, Request::get("/status").header(header::HOST, host)).await;
        assert_eq!(status, StatusCode::OK, "Host: {host}");
    }
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn requests_to_foreign_hosts_are_rejected(pool: PgPool) {
    // Защита от DNS rebinding: чужое имя, указывающее на 127.0.0.1, не проходит.
    for host in [
        "evil.example",
        "evil.example:3000",
        "localhost.evil.example",
        "10.0.0.1:3000",
    ] {
        for uri in ["/", "/status", "/htmx.min.js"] {
            let (status, body) = request(&pool, Request::get(uri).header(header::HOST, host)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "Host: {host}, {uri}");
            assert!(!body.contains("briefly-searcher loader"), "{body}");
        }
    }

    let (status, _) = request(&pool, Request::get("/status")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "без Host");
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn telegram_session_never_appears_on_page(pool: PgPool) {
    sqlx::query!("INSERT INTO telegram_session (session) VALUES ('SECRET-SESSION-7f3a')")
        .execute(&pool)
        .await
        .unwrap();

    for uri in ["/", "/status"] {
        let (status, body) = get(&pool, uri).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("SECRET-SESSION-7f3a"), "{uri}: {body}");
        assert!(!body.contains("telegram_session"), "{uri}: {body}");
    }
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn admin_has_no_mutating_routes(pool: PgPool) {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for uri in ["/", "/status"] {
            let (status, _) = request(
                &pool,
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::HOST, "127.0.0.1:3000"),
            )
            .await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        }
    }
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn status_answers_5xx_when_storage_fails(pool: PgPool) {
    // htmx 2 не вставляет ответы 4xx/5xx: на экране останутся последние данные.
    pool.close().await;

    let (status, body) = get(&pool, "/status").await;

    assert!(status.is_server_error(), "{status}");
    assert!(!body.contains("briefly-searcher loader"), "{body}");
}

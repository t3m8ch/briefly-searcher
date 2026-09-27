//! Лог ответа `429` от веб-ленты. Отдельный тестовый бинарник: подписчик
//! `tracing`, установленный на поток теста, пропускает события, если тот же
//! callsite параллельно регистрирует тест в другом потоке без подписчика
//! (кэш интереса callsite в `tracing-core` глобальный).

mod support;

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use briefly_searcher_telegram::{HistorySource, WebFeed, WebFeedSettings};

use support::{CHANNEL, FeedServer, Reply};

/// Логи `tracing`, записанные в память.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for Logs {
    type Writer = Self;

    fn make_writer(&self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn too_many_requests_response_is_logged_as_warning_with_headers_and_body_start() {
    let body = format!("начало тела ответа {}КОНЕЦ", "x".repeat(3000));
    let server = FeedServer::start([(
        Some(100),
        Reply::status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "17")
            .header("x-feed-marker", "marker-value")
            .body(&body),
    )])
    .await;
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    WebFeed::new(WebFeedSettings {
        base_url: server.base_url(),
        channel: CHANNEL.to_owned(),
        flood_wait_secs: 60,
    })
    .unwrap()
    .fetch_page(100, 100)
    .await
    .unwrap_err();

    let logs = logs.text();
    let line = logs
        .lines()
        .find(|line| line.contains("WARN"))
        .expect(&logs);
    let url = format!("{}/s/brieflyru?before=100", server.base_url());
    assert!(line.contains(&url), "{line}");
    assert!(line.contains("429"), "{line}");
    for header in ["retry-after", "17", "x-feed-marker", "marker-value"] {
        assert!(line.contains(header), "{header}: {line}");
    }
    assert!(line.contains(&body.len().to_string()), "{line}");
    assert!(line.contains("начало тела ответа xxx"), "{line}");
    assert!(!line.contains("КОНЕЦ"), "{line}");
}

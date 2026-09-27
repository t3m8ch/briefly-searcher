//! Источник на веб-ленте против локального HTTP-сервера, который отдаёт
//! записанные страницы `t.me/s/brieflyru`. Тесты смотрят только на то, что
//! источник вернул и какие запросы отправил.

mod support;

use briefly_searcher_telegram::{HistoryError, HistorySource, Message, WebFeed, WebFeedSettings};

use axum::http::StatusCode;
use support::{CHANNEL, FeedServer, Reply, fixtures};

const FLOOD_WAIT_SECS: u32 = 60;

fn source(server: &FeedServer) -> WebFeed {
    WebFeed::new(WebFeedSettings {
        base_url: server.base_url(),
        channel: CHANNEL.to_owned(),
        flood_wait_secs: FLOOD_WAIT_SECS,
    })
    .unwrap()
}

fn ids(page: &[Message]) -> Vec<i64> {
    page.iter().map(|m| m.id).collect()
}

fn html(message: &Message) -> &str {
    message.payload["html"].as_str().unwrap()
}

#[tokio::test]
async fn page_returns_blocks_newest_first_with_block_html() {
    let server = FeedServer::start([(Some(47107), Reply::page(fixtures::BEFORE_47107))]).await;

    let page = source(&server).fetch_page(47107, 100).await.unwrap();

    assert_eq!(
        ids(&page),
        [
            47106, 47105, 47104, 47100, 47099, 47098, 47097, 47096, 47093, 47092, 47091, 47090,
            47089, 47086
        ]
    );
    for message in &page {
        assert_eq!(message.payload_schema, "t.me/s html v1");
        assert_eq!(message.payload.as_object().unwrap().len(), 1);
        let html = html(message);
        assert!(
            html.starts_with("<div class=\"tgme_widget_message "),
            "{html:.80}"
        );
        assert!(html.ends_with("</div>"));
        assert!(html.contains(&format!("data-post=\"brieflyru/{}\"", message.id)));
        // В HTML блока нет соседних блоков.
        assert_eq!(html.matches("data-post=").count(), 1);
    }
    assert_eq!(server.queries(), [Some("before=47107".to_owned())]);
}

#[tokio::test]
async fn album_is_one_record_with_first_photo_id_and_all_photo_ids_in_html() {
    let server = FeedServer::start([(Some(31170), Reply::page(fixtures::BEFORE_31170))]).await;

    let page = source(&server).fetch_page(31170, 100).await.unwrap();

    assert_eq!(ids(&page), [31162, 31160, 31157, 31148]);
    let album = &page[3];
    for photo in 31148..=31156 {
        assert!(html(album).contains(&format!("https://t.me/brieflyru/{photo}?single")));
    }
    assert!(!html(album).contains("brieflyru/31157?single"));
}

#[tokio::test]
async fn blocks_not_older_than_offset_id_are_dropped() {
    // Лента вернула на `before=47100` страницу, где есть блоки 47100–47106.
    let server = FeedServer::start([(Some(47100), Reply::page(fixtures::BEFORE_47107))]).await;

    let page = source(&server).fetch_page(47100, 100).await.unwrap();

    assert_eq!(
        ids(&page),
        [
            47099, 47098, 47097, 47096, 47093, 47092, 47091, 47090, 47089, 47086
        ]
    );
}

#[tokio::test]
async fn zero_offset_id_requests_feed_without_before() {
    let server = FeedServer::start([(None, Reply::page(fixtures::BEFORE_47107))]).await;

    let page = source(&server).fetch_page(0, 100).await.unwrap();

    assert_eq!(page.len(), 14);
    assert_eq!(page[0].id, 47106);
    assert_eq!(server.queries(), [None]);
}

#[tokio::test]
async fn limit_keeps_only_newest_blocks() {
    let server = FeedServer::start([(Some(30), Reply::page(fixtures::BEFORE_30))]).await;

    let page = source(&server).fetch_page(30, 3).await.unwrap();

    assert_eq!(ids(&page), [29, 28, 27]);
}

#[tokio::test]
async fn start_of_channel_pages_end_with_empty_page() {
    let server = FeedServer::start([
        (Some(10), Reply::page(fixtures::BEFORE_10)),
        (Some(5), Reply::page(fixtures::BEFORE_5)),
        (Some(1), Reply::page(fixtures::BEFORE_1)),
    ])
    .await;
    let source = source(&server);

    assert_eq!(ids(&source.fetch_page(10, 100).await.unwrap()), [9, 8, 1]);
    let first = source.fetch_page(5, 100).await.unwrap();
    assert_eq!(ids(&first), [1]);
    assert!(html(&first[0]).contains("Channel created"));
    assert_eq!(source.fetch_page(1, 100).await.unwrap(), []);
}

#[tokio::test]
async fn page_without_blocks_and_without_start_marker_is_an_error() {
    let feed_without_marker =
        fixtures::BEFORE_1.replace("tme_no_messages_found", "tme_something_else");
    let server = FeedServer::start([
        (Some(1), Reply::page(&feed_without_marker)),
        (Some(2), Reply::page("")),
    ])
    .await;
    let source = source(&server);

    for offset_id in [1, 2] {
        let error = source.fetch_page(offset_id, 100).await.unwrap_err();
        assert!(matches!(error, HistoryError::Other(_)), "{error:?}");
    }
}

#[tokio::test]
async fn block_of_another_channel_is_an_error() {
    let foreign =
        fixtures::BEFORE_5.replace("data-post=\"brieflyru/1\"", "data-post=\"otherchannel/1\"");
    let server = FeedServer::start([(Some(5), Reply::page(&foreign))]).await;

    let error = source(&server).fetch_page(5, 100).await.unwrap_err();

    assert!(matches!(error, HistoryError::Other(_)), "{error:?}");
}

#[tokio::test]
async fn too_many_requests_is_flood_wait_with_own_pause_whatever_retry_after_says() {
    let server = FeedServer::start([
        (Some(100), Reply::status(StatusCode::TOO_MANY_REQUESTS)),
        (
            Some(200),
            Reply::status(StatusCode::TOO_MANY_REQUESTS).header("retry-after", "5"),
        ),
    ])
    .await;
    let source = source(&server);

    for offset_id in [100, 200] {
        let error = source.fetch_page(offset_id, 100).await.unwrap_err();
        assert!(
            matches!(error, HistoryError::FloodWait(FLOOD_WAIT_SECS)),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn redirect_and_server_errors_are_other_errors() {
    let server = FeedServer::start([
        // Так лента отвечает на несуществующий канал; переход по Location
        // привёл бы к настоящей странице.
        (
            Some(100),
            Reply::status(StatusCode::FOUND).header("location", "/s/brieflyru?before=47107"),
        ),
        (Some(47107), Reply::page(fixtures::BEFORE_47107)),
        (Some(200), Reply::status(StatusCode::INTERNAL_SERVER_ERROR)),
        (Some(300), Reply::status(StatusCode::BAD_GATEWAY)),
    ])
    .await;
    let source = source(&server);

    for offset_id in [100, 200, 300] {
        let error = source.fetch_page(offset_id, 100).await.unwrap_err();
        assert!(matches!(error, HistoryError::Other(_)), "{error:?}");
    }
    assert_eq!(server.queries().len(), 3);
}

#[tokio::test]
async fn connection_failure_is_other_error() {
    // Порт, который только что был занят и освобождён: соединение отклоняется.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let source = WebFeed::new(WebFeedSettings {
        base_url: format!("http://{addr}"),
        channel: CHANNEL.to_owned(),
        flood_wait_secs: FLOOD_WAIT_SECS,
    })
    .unwrap();

    let error = source.fetch_page(0, 100).await.unwrap_err();

    assert!(matches!(error, HistoryError::Other(_)), "{error:?}");
}

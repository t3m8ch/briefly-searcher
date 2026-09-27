//! Локальный HTTP-сервер, который изображает веб-ленту `t.me/s/brieflyru`:
//! отдаёт записанные страницы и заданные сценарием ответы по параметру
//! `before` и ведёт журнал запросов.
//!
//! Модуль общий для тестов crate доступа к Telegram и приложения, поэтому
//! фикстуры подключаются через `include_str!` относительно этого файла.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// Канал, ленту которого изображает сервер.
pub const CHANNEL: &str = "brieflyru";

/// Страницы `t.me/s/brieflyru?before=<N>`, записанные 27.09.2026.
pub mod fixtures {
    /// Обычные посты 47089–47106 и альбом 47086 (фото 47086–47088).
    pub const BEFORE_47107: &str = include_str!("../fixtures/before-47107.html");
    /// Две составные статьи: 47058, 47060, 47061 («Часть i/3») и 47062,
    /// 47065 («Часть i/2»).
    pub const BEFORE_47066: &str = include_str!("../fixtures/before-47066.html");
    /// Четыре альбома, 23 фото: 31148, 31157, 31160, 31162.
    pub const BEFORE_31170: &str = include_str!("../fixtures/before-31170.html");
    /// Начало канала: посты 10–29.
    pub const BEFORE_30: &str = include_str!("../fixtures/before-30.html");
    /// Начало канала: посты 1, 8, 9.
    pub const BEFORE_10: &str = include_str!("../fixtures/before-10.html");
    /// Начало канала: только сообщение 1 («Channel created»).
    pub const BEFORE_5: &str = include_str!("../fixtures/before-5.html");
    /// Пустая лента с пометкой `tme_no_messages_found`.
    pub const BEFORE_1: &str = include_str!("../fixtures/before-1.html");
}

/// Ответ сервера на запрос ленты.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

impl Reply {
    /// Ответ `200` с записанной страницей.
    pub fn page(html: &str) -> Self {
        Self::status(StatusCode::OK).body(html)
    }

    pub fn status(status: StatusCode) -> Self {
        Self {
            status,
            headers: vec![("content-type", "text/html; charset=utf-8".to_owned())],
            body: String::new(),
        }
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }

    pub fn body(mut self, body: &str) -> Self {
        self.body = body.to_owned();
        self
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        let mut response = (self.status, self.body).into_response();
        for (name, value) in self.headers {
            response.headers_mut().insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(&value).unwrap(),
            );
        }
        response
    }
}

/// Сервер ленты. Запрос с `before`, которого нет в сценарии, получает `404`.
pub struct FeedServer {
    addr: SocketAddr,
    feed: Arc<Feed>,
}

struct Feed {
    /// Ответ по значению `before`; `None` — запрос без `before`.
    replies: HashMap<Option<i64>, Reply>,
    /// Строка запроса каждого обращения к ленте.
    queries: Mutex<Vec<Option<String>>>,
}

impl FeedServer {
    pub async fn start(replies: impl IntoIterator<Item = (Option<i64>, Reply)>) -> Self {
        let feed = Arc::new(Feed {
            replies: replies.into_iter().collect(),
            queries: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/s/{channel}", get(serve_feed))
            .with_state(feed.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { addr, feed }
    }

    /// Базовый адрес для источника, как `https://t.me` в работе.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Строки запроса всех обращений к ленте по порядку.
    pub fn queries(&self) -> Vec<Option<String>> {
        self.feed.queries.lock().unwrap().clone()
    }
}

async fn serve_feed(
    State(feed): State<Arc<Feed>>,
    Path(channel): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    feed.queries.lock().unwrap().push(query.clone());
    if channel != CHANNEL {
        return StatusCode::NOT_FOUND.into_response();
    }
    let before = match query.as_deref().map(|q| q.strip_prefix("before=")) {
        None => None,
        Some(Some(value)) => match value.parse() {
            Ok(before) => Some(before),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        Some(None) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match feed.replies.get(&before) {
        Some(reply) => reply.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

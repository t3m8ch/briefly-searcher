//! Источник истории на публичной веб-ленте `t.me/s/<канал>` (ADR-0003).
//!
//! Страница ленты — HTML для браузера без официальной документации; что в
//! нём известно, описано в `docs/research/telegram-web-preview.md`.

use std::sync::LazyLock;
use std::time::Duration;

use reqwest::StatusCode;
use scraper::{Html, Selector};

use crate::{HistoryError, HistorySource, Message};

/// `payload_schema` записей веб-ленты.
const PAYLOAD_SCHEMA: &str = "t.me/s html v1";

/// Сколько ждать ответа ленты, включая чтение тела.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Сколько байт тела ответа `429` писать в лог.
const LOGGED_BODY_LIMIT: usize = 2048;

/// Собственный `User-Agent`: название и версия приложения.
const USER_AGENT: &str = concat!("briefly-searcher/", env!("CARGO_PKG_VERSION"));

/// **Блок веб-ленты**: отдельное сообщение или целый альбом.
static BLOCK: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".tgme_widget_message").unwrap());

/// Пометка пустой ленты: так лента показывает, что раньше блоков нет.
static NO_MESSAGES_FOUND: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".tgme_channel_history .tme_no_messages_found").unwrap());

/// Параметры источника на веб-ленте.
#[derive(Clone, Debug)]
pub struct WebFeedSettings {
    /// Адрес Telegram без завершающего `/`: в работе `https://t.me`.
    pub base_url: String,
    /// Имя канала без `@`.
    pub channel: String,
    /// Пауза, которую источник требует при ответе `429`, в секундах.
    pub flood_wait_secs: u32,
}

/// Источник истории на веб-ленте канала. Каждый блок ленты — одно
/// [`Message`] с ID из `data-post` и `payload = {"html": <HTML блока>}`;
/// альбом — одна запись с ID первой фотографии.
pub struct WebFeed {
    client: reqwest::Client,
    /// `<base_url>/s/<канал>`.
    feed_url: String,
    channel: String,
    flood_wait_secs: u32,
}

impl WebFeed {
    /// Создаёт источник с HTTP-клиентом на rustls: таймаут запроса 30 с,
    /// без перехода по редиректам.
    ///
    /// # Errors
    ///
    /// Если не удалось создать HTTP-клиент.
    pub fn new(
        settings: WebFeedSettings,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            // `302` — ответ ленты на несуществующий канал, а не страница.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            feed_url: format!(
                "{}/s/{}",
                settings.base_url.trim_end_matches('/'),
                settings.channel
            ),
            channel: settings.channel,
            flood_wait_secs: settings.flood_wait_secs,
        })
    }
}

impl HistorySource for WebFeed {
    async fn fetch_page(&self, offset_id: i64, limit: u32) -> Result<Vec<Message>, HistoryError> {
        let url = if offset_id == 0 {
            self.feed_url.clone()
        } else {
            format!("{}?before={offset_id}", self.feed_url)
        };
        let response = self.client.get(&url).send().await.map_err(other)?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            // Что лента сообщает при ограничении, неизвестно, поэтому
            // `Retry-After` не читается: пауза — собственная, а ответ целиком
            // уходит в лог, чтобы потом по нему сделать ожидание умнее.
            log_too_many_requests(&url, response).await;
            return Err(HistoryError::FloodWait(self.flood_wait_secs));
        }
        if status != StatusCode::OK {
            return Err(other(format!("лента ответила {status} на {url}")));
        }
        let body = response.text().await.map_err(other)?;
        let mut blocks = parse_blocks(&body, &self.channel)?;
        // Контракт «ID строго меньше `offset_id`» держит сам источник, чтобы
        // загрузчик не зациклился, если лента вернёт блоки новее границы.
        if offset_id != 0 {
            blocks.retain(|block| block.id < offset_id);
        }
        // В разметке блоки идут по возрастанию ID, отдаём от свежих к ранним.
        blocks.sort_by_key(|block| std::cmp::Reverse(block.id));
        blocks.truncate(limit as usize);
        Ok(blocks)
    }
}

/// Пишет ответ `429` в лог: адрес, код, все заголовки, размер тела и его
/// начало. Секретов там нет: запрос анонимный, лента публичная.
async fn log_too_many_requests(url: &str, response: reqwest::Response) {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    match response.bytes().await {
        Ok(body) => {
            let text = String::from_utf8_lossy(&body);
            tracing::warn!(
                url,
                status,
                ?headers,
                body_len = body.len(),
                body_start = prefix(&text, LOGGED_BODY_LIMIT),
                "лента ограничила частоту запросов"
            );
        }
        Err(error) => tracing::warn!(
            url,
            status,
            ?headers,
            %error,
            "лента ограничила частоту запросов; тело ответа не прочитано"
        ),
    }
}

/// Начало строки не длиннее `max_len` байт, не разрезающее символ.
fn prefix(text: &str, max_len: usize) -> &str {
    let mut end = text.len().min(max_len);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Блоки страницы в порядке разметки. Страница без блоков — начало канала
/// только при пометке [`NO_MESSAGES_FOUND`], иначе ошибка: сбой или
/// незнакомую разметку нельзя принять за конец ленты.
fn parse_blocks(body: &str, channel: &str) -> Result<Vec<Message>, HistoryError> {
    let document = Html::parse_document(body);
    let blocks = document
        .select(&BLOCK)
        .map(|block| {
            let post = block
                .attr("data-post")
                .ok_or_else(|| other("блок ленты без data-post"))?;
            let id = block_id(post, channel)
                .ok_or_else(|| other(format!("блок с неожиданным data-post=\"{post}\"")))?;
            Ok(Message {
                id,
                payload: serde_json::json!({ "html": block.html() }),
                payload_schema: PAYLOAD_SCHEMA.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if blocks.is_empty() && document.select(&NO_MESSAGES_FOUND).next().is_none() {
        return Err(other(
            "в ответе ленты нет ни блоков, ни пометки начала канала",
        ));
    }
    Ok(blocks)
}

/// ID из `data-post="<канал>/<ID>"`; `None` для чужого канала или
/// неразбираемого ID.
fn block_id(post: &str, channel: &str) -> Option<i64> {
    let (post_channel, id) = post.split_once('/')?;
    if !post_channel.eq_ignore_ascii_case(channel) {
        return None;
    }
    id.parse().ok().filter(|&id| id > 0)
}

/// Прочая ошибка источника.
fn other(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> HistoryError {
    HistoryError::Other(error.into())
}

//! Доступ к Telegram: trait `HistorySource` и его реализация на публичной
//! веб-ленте `t.me/s/<канал>` — [`WebFeed`] (ADR-0003).
//!
//! Crate не обращается к БД. MTProto-реализация на `grammers` отложена, пока
//! нельзя получить `api_id`/`api_hash`.

use std::future::Future;

mod web_feed;

pub use web_feed::{WebFeed, WebFeedSettings};

/// Наибольший размер страницы: столько принимает `messages.getHistory`.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Сообщение канала в том виде, в каком его сохраняет загрузчик.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// ID сообщения в Telegram; больший ID означает более позднюю публикацию.
    pub id: i64,
    /// Исходник сообщения в JSON: у веб-ленты — `{"html": <HTML блока>}`
    /// (ADR-0003), у MTProto — декодированный объект сообщения (ADR-0001).
    pub payload: serde_json::Value,
    /// Чем записан `payload`, например `t.me/s html v1`.
    pub payload_schema: String,
}

/// Ошибка запроса страницы истории.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// Telegram ограничил частоту запросов (`FLOOD_WAIT_X` у MTProto, `429`
    /// у веб-ленты): повторять запрос можно не раньше чем через указанное
    /// число секунд.
    #[error("Telegram требует подождать {0} с (FLOOD_WAIT)")]
    FloodWait(u32),
    /// Прочая ошибка источника.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

/// Источник истории канала: одна операция «страница по `offset_id`», как
/// `messages.getHistory` или `t.me/s/<канал>?before=<offset_id>`.
///
/// Загрузчик зависит только от этого trait, поэтому тесты подставляют
/// поддельный источник.
pub trait HistorySource {
    /// Запрашивает **страницу**: до `limit` (не больше [`MAX_PAGE_SIZE`])
    /// сообщений с ID строго меньше `offset_id`, от большего ID к меньшему.
    /// `offset_id = 0` означает «без границы» — самые свежие сообщения канала.
    /// Пустая страница означает, что более ранних сообщений нет.
    fn fetch_page(
        &self,
        offset_id: i64,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Message>, HistoryError>> + Send;
}

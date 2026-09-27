//! Доступ к Telegram: MTProto-клиент `grammers` и trait `HistorySource`.
//!
//! Crate не обращается к БД: сессию Telegram он читает и сохраняет через
//! crate хранилища.

pub mod login;
pub mod session;

use std::future::Future;

pub use login::{ApiHash, LoginError, LoginPrompt, login};

/// Наибольший размер страницы, который принимает `messages.getHistory`.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Сообщение канала в том виде, в каком его сохраняет загрузчик.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// ID сообщения в Telegram; больший ID означает более позднюю публикацию.
    pub id: i64,
    /// Декодированный объект сообщения, сериализованный в JSON (ADR-0001).
    pub payload: serde_json::Value,
    /// Чем записан `payload`: версия crate и TL layer, например
    /// `grammers-tl-types 0.x / layer N`.
    pub payload_schema: String,
}

/// Ошибка запроса страницы истории.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// Telegram ответил `FLOOD_WAIT_X`: повторять запрос можно не раньше чем
    /// через указанное число секунд.
    #[error("Telegram требует подождать {0} с (FLOOD_WAIT)")]
    FloodWait(u32),
    /// Прочая ошибка источника.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

/// Источник истории канала: одна операция `messages.getHistory`.
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

//! Загрузчик: проходы по каналу через `HistorySource` с сохранением в `raw_posts`.
//!
//! **Проход** листает канал от самых свежих сообщений вниз и сохраняет все
//! сообщения новее `newest_fetched_id`. Состояние незавершённого прохода не
//! хранится отдельно, а выводится хранилищем из `raw_posts` и
//! `newest_fetched_id` (см. `docs/scraper-plan.md`, раздел 1).

use std::convert::Infallible;

use briefly_searcher_storage::{RawMessage, Storage};
use briefly_searcher_telegram::{HistoryError, HistorySource, MAX_PAGE_SIZE, Message};
use chrono::{DateTime, TimeDelta, Utc};

use crate::clock::Clock;

/// Настройки темпа загрузчика.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Сколько сообщений запрашивать за раз: от 1 до [`MAX_PAGE_SIZE`].
    pub page_size: u32,
    /// Пауза между ответом источника и следующим запросом.
    pub request_delay: TimeDelta,
    /// Пауза между концом прохода и началом следующего.
    pub poll_interval: TimeDelta,
}

/// Ошибка, прервавшая проход.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ошибка источника истории: {0}")]
    Source(#[from] HistoryError),
    #[error(transparent)]
    Storage(#[from] briefly_searcher_storage::Error),
}

/// Загрузчик одного канала. Запросы к источнику идут строго по одному.
pub struct Loader<S, C> {
    storage: Storage,
    source: S,
    clock: C,
    settings: Settings,
    /// Когда источник ответил на последний запрос этого экземпляра.
    last_request_at: Option<DateTime<Utc>>,
}

impl<S: HistorySource, C: Clock> Loader<S, C> {
    /// # Panics
    ///
    /// Если `settings.page_size` не в пределах от 1 до [`MAX_PAGE_SIZE`].
    pub fn new(storage: Storage, source: S, clock: C, settings: Settings) -> Self {
        assert!(
            (1..=MAX_PAGE_SIZE).contains(&settings.page_size),
            "размер страницы должен быть от 1 до {MAX_PAGE_SIZE}"
        );
        Self {
            storage,
            source,
            clock,
            settings,
            last_request_at: None,
        }
    }

    /// Повторяет проходы с интервалом опроса. Пока возвращается при первой
    /// же ошибке источника или хранилища.
    pub async fn run(&mut self) -> Result<Infallible, Error> {
        loop {
            self.run_pass().await?;
            let next_pass_at = self.clock.now() + self.settings.poll_interval;
            self.clock.sleep_until(next_pass_at).await;
        }
    }

    /// Выполняет один проход: продолжает незавершённый или начинает новый с
    /// самых свежих сообщений. Каждая страница сохраняется одной транзакцией;
    /// последняя — вместе со сдвигом `newest_fetched_id`. Если проход прерван
    /// ошибкой, следующий вызов продолжит его с первой несохранённой страницы.
    pub async fn run_pass(&mut self) -> Result<(), Error> {
        let state = self.storage.pass_state().await?;
        let mut before = state.resume_before;
        loop {
            let page = self.fetch_page(before).await?;
            let messages: Vec<_> = page.iter().map(raw_message).collect();
            let fetched_at = self.clock.now();
            // Проход завершается на пустой странице (начало канала) или на
            // странице, где встретилось уже сохранённое сообщение.
            match page.iter().map(|m| m.id).min() {
                Some(min_id) if state.newest_fetched_id.is_none_or(|newest| min_id > newest) => {
                    self.storage.save_page(&messages, fetched_at).await?;
                    before = Some(min_id);
                }
                _ => {
                    self.storage.finish_pass(&messages, fetched_at).await?;
                    return Ok(());
                }
            }
        }
    }

    /// Запрашивает страницу не раньше чем через `request_delay` после ответа
    /// на предыдущий запрос.
    async fn fetch_page(&mut self, before: Option<i64>) -> Result<Vec<Message>, HistoryError> {
        if let Some(last) = self.last_request_at {
            self.clock
                .sleep_until(last + self.settings.request_delay)
                .await;
        }
        let page = self
            .source
            .fetch_page(before, self.settings.page_size)
            .await;
        self.last_request_at = Some(self.clock.now());
        page
    }
}

fn raw_message(message: &Message) -> RawMessage<'_> {
    RawMessage {
        message_id: message.id,
        payload: &message.payload,
        payload_schema: &message.payload_schema,
    }
}

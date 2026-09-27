//! Загрузчик: проходы по каналу через `HistorySource` с сохранением в `raw_posts`.
//!
//! **Проход** листает канал от самых свежих сообщений вниз и сохраняет все
//! сообщения новее `newest_fetched_id`. Состояние незавершённого прохода не
//! хранится отдельно, а выводится хранилищем из `raw_posts` и
//! `newest_fetched_id` (см. `docs/scraper-plan.md`, раздел 1).

use std::future::{Future, pending};
use std::ops::ControlFlow;
use std::pin::{Pin, pin};

use briefly_searcher_storage::{LoaderLock, RawMessage, Storage};
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

/// Ошибка, прервавшая проход или не давшая загрузчику начать работу.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "загрузчик уже запущен: advisory-блокировку PostgreSQL держит другой экземпляр, \
         второй не нужен"
    )]
    AlreadyRunning,
    #[error("ошибка источника истории: {0}")]
    Source(#[from] HistoryError),
    #[error(transparent)]
    Storage(#[from] briefly_searcher_storage::Error),
}

/// Берёт блокировку единственного экземпляра загрузчика (ADR-0002) и
/// держит её, пока [`Loader::run`] не вернётся. Вызывается до создания
/// источника: если блокировку держит другой экземпляр, процесс завершается с
/// [`Error::AlreadyRunning`], не обращаясь к Telegram.
pub async fn lock(storage: &Storage) -> Result<LoaderLock, Error> {
    storage
        .try_lock_loader()
        .await?
        .ok_or(Error::AlreadyRunning)
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

    /// Повторяет проходы с интервалом опроса, пока не завершится `shutdown`,
    /// и держит `lock` до возврата. Пока возвращается и при первой же ошибке
    /// источника или хранилища.
    ///
    /// Остановка не прерывает сохранение: полученная от источника страница
    /// сохраняется своей транзакцией, и только потом `run` возвращается.
    /// Запрос, на который источник ещё не ответил, бросается — после
    /// перезапуска он повторится. Новых запросов после сигнала нет.
    pub async fn run(
        &mut self,
        lock: LoaderLock,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        let result = self.run_passes(shutdown).await;
        let released = lock.release().await;
        result?;
        Ok(released?)
    }

    async fn run_passes(&mut self, shutdown: impl Future<Output = ()>) -> Result<(), Error> {
        let mut shutdown = pin!(shutdown);
        loop {
            if self.pass(shutdown.as_mut()).await?.is_break() {
                return Ok(());
            }
            let next_pass_at = self.clock.now() + self.settings.poll_interval;
            let sleep = self.clock.sleep_until(next_pass_at);
            if unless_stopped(shutdown.as_mut(), sleep).await.is_break() {
                return Ok(());
            }
        }
    }

    /// Выполняет один проход: продолжает незавершённый или начинает новый с
    /// самых свежих сообщений. Каждая страница сохраняется одной транзакцией;
    /// последняя — вместе со сдвигом `newest_fetched_id`. Если проход прерван
    /// ошибкой, следующий вызов продолжит его с первой несохранённой страницы.
    pub async fn run_pass(&mut self) -> Result<(), Error> {
        let _: ControlFlow<()> = self.pass(pin!(pending())).await?;
        Ok(())
    }

    /// Проход, который прекращается без новых запросов к источнику, когда
    /// завершается `shutdown`; тогда возвращает `Break`.
    async fn pass(
        &mut self,
        mut shutdown: Pin<&mut impl Future<Output = ()>>,
    ) -> Result<ControlFlow<()>, Error> {
        let state = self.storage.pass_state().await?;
        let mut before = state.resume_before;
        loop {
            let fetch = self.fetch_page(before);
            let ControlFlow::Continue(page) = unless_stopped(shutdown.as_mut(), fetch).await else {
                return Ok(ControlFlow::Break(()));
            };
            let page = page?;
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
                    return Ok(ControlFlow::Continue(()));
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

/// Ждёт `future`, пока не завершился `shutdown`; `Break` — если остановка
/// пришла раньше. Сигнал проверяется до `future`, поэтому после него не
/// начинается новой работы, а результат, готовый одновременно с сигналом,
/// бросается.
async fn unless_stopped<T>(
    shutdown: Pin<&mut impl Future<Output = ()>>,
    future: impl Future<Output = T>,
) -> ControlFlow<(), T> {
    tokio::select! {
        biased;
        () = shutdown => ControlFlow::Break(()),
        output = future => ControlFlow::Continue(output),
    }
}

fn raw_message(message: &Message) -> RawMessage<'_> {
    RawMessage {
        message_id: message.id,
        payload: &message.payload,
        payload_schema: &message.payload_schema,
    }
}

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

/// Heartbeat пишется не реже этого в любом состоянии: ожидание — цикл тиков
/// такой длины, и каждый тик обновляет `last_heartbeat_at`.
pub const HEARTBEAT_INTERVAL: TimeDelta = TimeDelta::seconds(15);

/// Сколько ждать ответа источника: зависший запрос становится ошибкой, а не
/// тишиной в heartbeat.
pub const REQUEST_TIMEOUT: TimeDelta = TimeDelta::seconds(30);

/// Настройки темпа загрузчика.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Сколько сообщений запрашивать за раз: от 1 до [`MAX_PAGE_SIZE`].
    pub page_size: u32,
    /// Пауза между ответом источника и следующим запросом.
    pub request_delay: TimeDelta,
    /// Пауза между концом прохода и началом следующего.
    pub poll_interval: TimeDelta,
    /// Пауза между ошибкой и следующей попыткой.
    pub retry_delay: TimeDelta,
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
    #[error("источник истории не ответил за {} с", REQUEST_TIMEOUT.num_seconds())]
    Timeout,
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
    /// До какого момента Telegram запретил запросы (`FLOOD_WAIT`). Хранится и
    /// в БД; здесь — на случай, если записать срок в БД не удалось.
    flood_wait_until: Option<DateTime<Utc>>,
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
            flood_wait_until: None,
        }
    }

    /// Повторяет проходы с интервалом опроса, пока не завершится `shutdown`,
    /// и держит `lock` до возврата. Из-за ошибок источника или хранилища не
    /// завершается: записывает ошибку в `worker_state` и после паузы
    /// `retry_delay` продолжает проход. Ошибку возвращает, только если не
    /// удалось снять блокировку.
    ///
    /// Остановка не прерывает сохранение: полученная от источника страница
    /// сохраняется своей транзакцией, и только потом `run` возвращается.
    /// Запрос, на который источник ещё не ответил, бросается — после
    /// перезапуска он повторится. Новых запросов после сигнала нет; ожидание
    /// между проходами, перед повтором и во время паузы `FLOOD_WAIT`
    /// прерывается сигналом.
    ///
    /// `lock` передаётся по значению: вместе с ним `run` забирает
    /// ответственность за снятие блокировки. Только `run` знает, когда работа
    /// закончилась, поэтому снимает блокировку сам. Вызывающий код не может ни
    /// забыть её снять, ни уронить раньше времени, пока идут проходы:
    /// блокировка живёт ровно столько, сколько `run`. Получить `LoaderLock`
    /// можно только через [`lock`] — до создания источника.
    pub async fn run(
        &mut self,
        lock: LoaderLock,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        self.run_passes(shutdown).await;
        Ok(lock.release().await?)
    }

    async fn run_passes(&mut self, shutdown: impl Future<Output = ()>) {
        let mut shutdown = pin!(shutdown);
        loop {
            let next_attempt_at = match self.pass(shutdown.as_mut()).await {
                Ok(ControlFlow::Continue(saved)) => {
                    tracing::info!(saved, "проход завершён");
                    self.clock.now() + self.settings.poll_interval
                }
                Ok(ControlFlow::Break(saved)) => {
                    tracing::info!(saved, "проход остановлен по сигналу");
                    return;
                }
                Err(error) => self.record_error(&error).await,
            };
            let wait = self.wait_until(next_attempt_at);
            if unless_stopped(shutdown.as_mut(), wait).await.is_break() {
                return;
            }
        }
    }

    /// Ждёт до `deadline` тиками не длиннее [`HEARTBEAT_INTERVAL`], записывая
    /// heartbeat в начале каждого тика и в конце ожидания.
    ///
    /// Heartbeat пишет основной цикл, а не фоновая задача, чтобы зависший
    /// цикл не выглядел живым. Ошибка записи heartbeat ожидание не прерывает.
    async fn wait_until(&self, deadline: DateTime<Utc>) {
        loop {
            let now = self.clock.now();
            if let Err(error) = self.storage.record_heartbeat(now).await {
                tracing::warn!(%error, "не удалось записать heartbeat");
            }
            if now >= deadline {
                return;
            }
            self.clock
                .sleep_until(deadline.min(now + HEARTBEAT_INTERVAL))
                .await;
        }
    }

    /// Записывает ошибку и возвращает время следующей попытки.
    async fn record_error(&self, error: &Error) -> DateTime<Utc> {
        let at = self.clock.now();
        let next_attempt_at = at + self.settings.retry_delay;
        tracing::error!(%error, %next_attempt_at, "проход прерван ошибкой");
        let text = error.to_string();
        if let Err(error) = self.storage.record_error(&text, at, next_attempt_at).await {
            tracing::error!(%error, "не удалось записать ошибку в worker_state");
        }
        next_attempt_at
    }

    /// Выполняет один проход: продолжает незавершённый или начинает новый с
    /// самых свежих сообщений. Каждая страница сохраняется одной транзакцией;
    /// последняя — вместе со сдвигом `newest_fetched_id`. Если проход прерван
    /// ошибкой, следующий вызов продолжит его с первой несохранённой страницы.
    ///
    /// Возвращает, сколько сообщений этот вызов сохранил впервые.
    pub async fn run_pass(&mut self) -> Result<u64, Error> {
        let (ControlFlow::Continue(saved) | ControlFlow::Break(saved)) =
            self.pass(pin!(pending())).await?;
        Ok(saved)
    }

    /// Проход, который прекращается без новых запросов к источнику, когда
    /// завершается `shutdown`; тогда возвращает `Break`. В обоих случаях —
    /// сколько сообщений сохранено впервые.
    async fn pass(
        &mut self,
        mut shutdown: Pin<&mut impl Future<Output = ()>>,
    ) -> Result<ControlFlow<u64, u64>, Error> {
        // Срок паузы мог записать предыдущий экземпляр загрузчика.
        let stored_flood_wait = self.storage.flood_wait_until().await?;
        self.flood_wait_until = self.flood_wait_until.max(stored_flood_wait);
        let state = self.storage.pass_state().await?;
        let mut before = state.resume_before;
        let mut saved = 0;
        loop {
            let fetch = self.fetch_page(before);
            let ControlFlow::Continue(page) = unless_stopped(shutdown.as_mut(), fetch).await else {
                return Ok(ControlFlow::Break(saved));
            };
            let page = match page {
                // Срок паузы, как и страница, записывается и при остановке.
                Err(Error::Source(HistoryError::FloodWait(seconds))) => {
                    self.start_flood_wait(seconds).await?;
                    continue;
                }
                page => page?,
            };
            let messages: Vec<_> = page.iter().map(raw_message).collect();
            let fetched_at = self.clock.now();
            // Проход завершается на пустой странице (начало канала) или на
            // странице, где встретилось уже сохранённое сообщение.
            match page.iter().map(|m| m.id).min() {
                Some(min_id) if state.newest_fetched_id.is_none_or(|newest| min_id > newest) => {
                    saved += self.storage.save_page(&messages, fetched_at).await?;
                    before = Some(min_id);
                }
                _ => {
                    saved += self.storage.finish_pass(&messages, fetched_at).await?;
                    return Ok(ControlFlow::Continue(saved));
                }
            }
        }
    }

    /// Запрашивает страницу не раньше чем через `request_delay` после ответа
    /// на предыдущий запрос и не раньше конца паузы `FLOOD_WAIT`; источник
    /// ждёт не дольше [`REQUEST_TIMEOUT`].
    async fn fetch_page(&mut self, before: Option<i64>) -> Result<Vec<Message>, Error> {
        let not_before = self
            .last_request_at
            .map(|last| last + self.settings.request_delay)
            .max(self.flood_wait_until);
        self.wait_until(not_before.unwrap_or_else(|| self.clock.now()))
            .await;
        let timeout_at = self.clock.now() + REQUEST_TIMEOUT;
        let page = tokio::select! {
            // Первым проверяется ответ: поддельные часы в тестах
            // «дожидаются» таймаута мгновенно.
            biased;
            page = self.source.fetch_page(before, self.settings.page_size) => page,
            () = self.clock.sleep_until(timeout_at) => return Err(Error::Timeout),
        };
        self.last_request_at = Some(self.clock.now());
        Ok(page?)
    }

    /// Записывает срок паузы после `FloodWait(seconds)`: до него запросов к
    /// источнику не будет, в том числе после перезапуска.
    async fn start_flood_wait(&mut self, seconds: u32) -> Result<(), Error> {
        let until = self.clock.now() + TimeDelta::seconds(seconds.into());
        tracing::warn!(%until, "Telegram требует подождать {seconds} с (FLOOD_WAIT)");
        self.flood_wait_until = Some(until);
        self.storage.set_flood_wait_until(until).await?;
        Ok(())
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

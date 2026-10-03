//! Проход загрузчика на поддельном источнике и поддельных часах поверх
//! настоящей PostgreSQL. Тесты смотрят только на содержимое БД и журнал
//! запросов источника.

use std::collections::{BTreeMap, HashMap};
use std::future::{Future, pending};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};

use briefly_searcher::clock::Clock;
use briefly_searcher::loader::{self, Error, Loader, Settings};
use briefly_searcher_storage::Storage;
use briefly_searcher_telegram::{HistoryError, HistorySource, Message};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::{Notify, oneshot};

const PAYLOAD_SCHEMA: &str = "fake-source v1";
const PAGE_SIZE: u32 = 3;

/// Часы, которые не спят, а переводят время на момент окончания ожидания.
///
/// Остановленные часы больше не переводятся: любое ожидание длится вечно.
/// Так тест останавливает бесконечный `Loader::run` в предсказуемой точке.
#[derive(Clone)]
struct FakeClock {
    state: Arc<Mutex<ClockState>>,
    stopped: Arc<Notify>,
}

struct ClockState {
    now: DateTime<Utc>,
    stopped: bool,
    /// Ожидание дольше этого момента остановит часы на нём.
    stop_at: Option<DateTime<Utc>>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ClockState {
                now: Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap(),
                stopped: false,
                stop_at: None,
            })),
            stopped: Arc::new(Notify::new()),
        }
    }

    fn stop(&self) {
        self.state.lock().unwrap().stopped = true;
        self.stopped.notify_one();
    }

    /// Часы остановятся на `moment`, если загрузчик станет ждать дольше него.
    fn stop_at(&self, moment: DateTime<Utc>) {
        self.state.lock().unwrap().stop_at = Some(moment);
    }

    /// Дожидается остановки часов.
    async fn stopped(&self) {
        self.stopped.notified().await;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        self.state.lock().unwrap().now
    }

    async fn sleep_until(&self, deadline: DateTime<Utc>) {
        let stopped = {
            let mut state = self.state.lock().unwrap();
            match state.stop_at {
                _ if state.stopped => {}
                Some(moment) if deadline > moment => {
                    state.now = state.now.max(moment);
                    state.stopped = true;
                    self.stopped.notify_one();
                }
                _ => state.now = state.now.max(deadline),
            }
            state.stopped
        };
        if stopped {
            std::future::pending::<()>().await;
        }
    }
}

/// Запрос страницы, который загрузчик отправил источнику.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Request {
    before: Option<i64>,
    at: DateTime<Utc>,
}

/// Поддельный канал: отдаёт страницы по `before` из своих сообщений,
/// выполняет сценарий и ведёт журнал запросов.
#[derive(Clone)]
struct FakeChannel(Arc<Mutex<Channel>>);

struct Channel {
    clock: FakeClock,
    /// Текст сообщения по его ID.
    messages: BTreeMap<i64, String>,
    requests: Vec<Request>,
    /// `before` запросов, которые один раз завершатся ошибкой.
    fail_once_at: Vec<i64>,
    /// `before` запросов, на которые источник один раз не ответит.
    hang_once_at: Vec<i64>,
    /// `before` запросов, на которые Telegram один раз ответит
    /// `FLOOD_WAIT` с этим числом секунд.
    flood_wait_once_at: HashMap<i64, u32>,
    /// Сообщения, которые появятся в канале прямо перед ответом на запрос
    /// с этим `before` (один раз).
    publish_during: HashMap<i64, RangeInclusive<i64>>,
    /// Сигналы остановки, которые подаются при получении запроса с этим
    /// `before` (`None` — без границы): запрос обслуживается как обычно.
    stop_at: HashMap<Option<i64>, oneshot::Sender<()>>,
    /// Запросы с этим `before` остаются без ответа; отправитель
    /// сообщает, что источник получил запрос.
    hang_at: HashMap<i64, oneshot::Sender<()>>,
    /// После этого числа запросов источник останавливает часы и больше не
    /// отвечает.
    stop_after: Option<usize>,
}

impl FakeChannel {
    fn new(clock: &FakeClock) -> Self {
        Self(Arc::new(Mutex::new(Channel {
            clock: clock.clone(),
            messages: BTreeMap::new(),
            requests: Vec::new(),
            fail_once_at: Vec::new(),
            hang_once_at: Vec::new(),
            flood_wait_once_at: HashMap::new(),
            publish_during: HashMap::new(),
            stop_at: HashMap::new(),
            hang_at: HashMap::new(),
            stop_after: None,
        })))
    }

    fn publish(&self, ids: RangeInclusive<i64>) {
        self.0.lock().unwrap().publish(ids);
    }

    fn edit(&self, id: i64, text: &str) {
        self.0.lock().unwrap().messages.insert(id, text.to_owned());
    }

    /// Запрос с этим `before` один раз завершится ошибкой источника.
    fn fail_once_at(&self, before: i64) {
        self.0.lock().unwrap().fail_once_at.push(before);
    }

    /// На запрос с этим `before` источник один раз не ответит вовсе.
    fn hang_once_at(&self, before: i64) {
        self.0.lock().unwrap().hang_once_at.push(before);
    }

    /// На запрос с этим `before` источник один раз ответит `FloodWait(seconds)`.
    fn flood_wait_once_at(&self, before: i64, seconds: u32) {
        self.0
            .lock()
            .unwrap()
            .flood_wait_once_at
            .insert(before, seconds);
    }

    /// Сообщения `ids` появятся в канале, пока загрузчик ждёт ответа на
    /// запрос с этим `before`.
    fn publish_during_request(&self, before: i64, ids: RangeInclusive<i64>) {
        self.0.lock().unwrap().publish_during.insert(before, ids);
    }

    /// Вместо ответа на запрос, следующий за `requests`-м, источник
    /// остановит часы и зависнет; в журнал этот запрос не попадёт.
    fn stop_after(&self, requests: usize) {
        self.0.lock().unwrap().stop_after = Some(requests);
    }

    /// Сигнал остановки загрузчика, который подаётся, когда источник
    /// получает запрос с этим `before`.
    fn stop_at_request(&self, before: Option<i64>) -> impl Future<Output = ()> + Send + 'static {
        let (stop, stopped) = oneshot::channel();
        self.0.lock().unwrap().stop_at.insert(before, stop);
        async move {
            stopped.await.unwrap();
        }
    }

    /// Запрос с этим `before` останется без ответа. Возвращённый
    /// приёмник срабатывает, когда источник получил этот запрос.
    fn hang_at_request(&self, before: i64) -> oneshot::Receiver<()> {
        let (received, receiver) = oneshot::channel();
        self.0.lock().unwrap().hang_at.insert(before, received);
        receiver
    }

    fn requests(&self) -> Vec<Request> {
        self.0.lock().unwrap().requests.clone()
    }

    /// `before` всех запросов по порядку.
    fn befores(&self) -> Vec<Option<i64>> {
        self.requests().iter().map(|r| r.before).collect()
    }
}

impl Channel {
    fn publish(&mut self, ids: RangeInclusive<i64>) {
        for id in ids {
            self.messages.insert(id, format!("сообщение {id}"));
        }
    }

    /// Ответ на запрос; `None` — источник оставляет запрос без ответа.
    fn fetch_page(
        &mut self,
        before: Option<i64>,
        limit: u32,
    ) -> Option<Result<Vec<Message>, HistoryError>> {
        if self.stop_after == Some(self.requests.len()) {
            self.clock.stop();
            return None;
        }
        let at = self.clock.now();
        self.requests.push(Request { before, at });
        if let Some(received) = before.and_then(|b| self.hang_at.remove(&b)) {
            // Запрос остаётся без ответа, только пока стоит время: иначе
            // загрузчик дождался бы таймаута.
            self.clock.stop();
            received.send(()).unwrap();
            return None;
        }
        let hang = self.hang_once_at.iter().position(|&b| Some(b) == before);
        if let Some(i) = hang {
            self.hang_once_at.remove(i);
            return None;
        }
        Some(self.reply(before, limit))
    }

    fn reply(&mut self, before: Option<i64>, limit: u32) -> Result<Vec<Message>, HistoryError> {
        if let Some(ids) = before.and_then(|b| self.publish_during.remove(&b)) {
            self.publish(ids);
        }
        if let Some(stop) = self.stop_at.remove(&before) {
            stop.send(()).unwrap();
        }
        if let Some(seconds) = before.and_then(|b| self.flood_wait_once_at.remove(&b)) {
            return Err(HistoryError::FloodWait(seconds));
        }
        let fail_once = self.fail_once_at.iter().position(|&b| Some(b) == before);
        if let Some(i) = fail_once {
            self.fail_once_at.remove(i);
            return Err(HistoryError::Other("источник недоступен".into()));
        }
        let upper = before.unwrap_or(i64::MAX);
        Ok(self
            .messages
            .range(..upper)
            .rev()
            .take(limit as usize)
            .map(|(&id, text)| Message {
                id,
                payload: payload(id, text),
                payload_schema: PAYLOAD_SCHEMA.to_owned(),
            })
            .collect())
    }
}

impl HistorySource for FakeChannel {
    async fn fetch_page(
        &self,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<Message>, HistoryError> {
        let response = self.0.lock().unwrap().fetch_page(before, limit);
        match response {
            Some(page) => page,
            None => pending().await,
        }
    }
}

fn payload(id: i64, text: &str) -> serde_json::Value {
    json!({ "id": id, "message": text })
}

fn settings() -> Settings {
    Settings {
        page_size: PAGE_SIZE,
        request_delay: TimeDelta::seconds(2),
        poll_interval: TimeDelta::minutes(10),
        retry_delay: TimeDelta::minutes(1),
    }
}

/// Новый экземпляр загрузчика поверх БД теста.
fn loader(
    pool: &PgPool,
    channel: &FakeChannel,
    clock: &FakeClock,
) -> Loader<FakeChannel, FakeClock> {
    Loader::new(
        Storage::from_pool(pool.clone()),
        channel.clone(),
        clock.clone(),
        settings(),
    )
}

/// Процесс загрузчика, как его запускает команда: блокировка единственного
/// экземпляра, затем подключение к источнику и проходы до остановки.
async fn run_loader(
    pool: &PgPool,
    channel: &FakeChannel,
    clock: &FakeClock,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let lock = loader::lock(&Storage::from_pool(pool.clone())).await?;
    loader(pool, channel, clock).run(lock, shutdown).await
}

async fn stored_ids(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar!("SELECT message_id FROM raw_posts ORDER BY message_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn newest_fetched_id(pool: &PgPool) -> Option<i64> {
    sqlx::query_scalar!("SELECT newest_fetched_id FROM ingestion_state")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn first_pass_on_empty_database_saves_whole_history(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=8);

    loader(&pool, &channel, &clock).run_pass().await.unwrap();

    // 8 7 6 | 5 4 3 | 2 1 | пустая страница — начало канала.
    assert_eq!(channel.befores(), [None, Some(6), Some(3), Some(1)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=8));
    assert_eq!(newest_fetched_id(&pool).await, Some(8));

    let row = sqlx::query!("SELECT payload, payload_schema FROM raw_posts WHERE message_id = 5")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.payload, payload(5, "сообщение 5"));
    assert_eq!(row.payload_schema, PAYLOAD_SCHEMA);
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn next_pass_saves_two_pages_of_new_messages_and_stops_at_saved_ones(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    let mut loader = loader(&pool, &channel, &clock);
    loader.run_pass().await.unwrap();
    let first_pass_requests = channel.requests().len();

    channel.publish(10..=14);
    loader.run_pass().await.unwrap();

    // 14 13 12 | 11 10 9 — на второй странице есть 9 ≤ newest_fetched_id.
    assert_eq!(channel.befores()[first_pass_requests..], [None, Some(12)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=14));
    assert_eq!(newest_fetched_id(&pool).await, Some(14));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn pass_reports_only_blocks_it_saved(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    let mut loader = loader(&pool, &channel, &clock);
    assert_eq!(loader.run_pass().await.unwrap(), 9);

    channel.publish(10..=14);
    // 14 13 12 | 11 10 9 — уже сохранённое 9 не считается.
    assert_eq!(loader.run_pass().await.unwrap(), 5);
    // Новых сообщений нет: 14 13 12 уже сохранены.
    assert_eq!(loader.run_pass().await.unwrap(), 0);
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn interruption_before_last_page_commit_is_finished_by_next_run(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    loader(&pool, &channel, &clock).run_pass().await.unwrap();
    channel.publish(10..=14);
    channel.fail_once_at(12);

    loader(&pool, &channel, &clock)
        .run_pass()
        .await
        .unwrap_err();
    assert_eq!(newest_fetched_id(&pool).await, Some(9));

    let before_restart = channel.requests().len();
    loader(&pool, &channel, &clock).run_pass().await.unwrap();

    assert_eq!(channel.befores()[before_restart..], [Some(12)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=14));
    assert_eq!(newest_fetched_id(&pool).await, Some(14));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn new_loader_resumes_unfinished_first_pass(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    channel.fail_once_at(4);

    // 9 8 7 | 6 5 4 | ошибка на третьей странице.
    loader(&pool, &channel, &clock)
        .run_pass()
        .await
        .unwrap_err();
    assert_eq!(newest_fetched_id(&pool).await, None);

    let before_restart = channel.requests().len();
    loader(&pool, &channel, &clock).run_pass().await.unwrap();

    assert_eq!(channel.befores()[before_restart..], [Some(4), Some(1)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=9));
    assert_eq!(newest_fetched_id(&pool).await, Some(9));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn new_loader_resumes_unfinished_next_pass(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    loader(&pool, &channel, &clock).run_pass().await.unwrap();
    channel.publish(10..=16);
    channel.fail_once_at(14);

    // 16 15 14 | ошибка на второй странице.
    loader(&pool, &channel, &clock)
        .run_pass()
        .await
        .unwrap_err();
    assert_eq!(newest_fetched_id(&pool).await, Some(9));

    // Пока загрузчик стоял, вышли новые сообщения: они новее первой страницы
    // прерванного прохода и достанутся следующему.
    channel.publish(17..=18);
    let before_restart = channel.requests().len();
    loader(&pool, &channel, &clock).run_pass().await.unwrap();

    // 13 12 11 | 10 9 8.
    assert_eq!(channel.befores()[before_restart..], [Some(14), Some(11)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=16));
    assert_eq!(newest_fetched_id(&pool).await, Some(16));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn message_received_again_with_edited_text_keeps_first_payload(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=7);
    let mut loader = loader(&pool, &channel, &clock);
    loader.run_pass().await.unwrap();

    channel.edit(7, "исправленный текст");
    channel.publish(8..=8);
    // 8 7 6 — сообщение 7 приходит повторно.
    loader.run_pass().await.unwrap();

    let stored = sqlx::query_scalar!("SELECT payload FROM raw_posts WHERE message_id = 7")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, payload(7, "сообщение 7"));
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=8));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn messages_published_during_pass_are_saved_by_next_pass(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    // 9 8 7 | во время второго запроса выходят 10 и 11.
    channel.publish_during_request(7, 10..=11);
    let mut loader = loader(&pool, &channel, &clock);

    loader.run_pass().await.unwrap();
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=9));
    assert_eq!(newest_fetched_id(&pool).await, Some(9));

    loader.run_pass().await.unwrap();
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=11));
    assert_eq!(newest_fetched_id(&pool).await, Some(11));
}

async fn flood_wait_until(pool: &PgPool) -> Option<DateTime<Utc>> {
    sqlx::query_scalar!("SELECT flood_wait_until FROM ingestion_state")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn flood_wait_is_stored_and_next_request_waits_for_it(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=5);
    channel.flood_wait_once_at(3, 600);

    loader(&pool, &channel, &clock).run_pass().await.unwrap();

    // 5 4 3 | FLOOD_WAIT на второй странице | 2 1 | пустая страница.
    let delay = settings().request_delay;
    let flood_at = t0 + delay;
    let until = flood_at + TimeDelta::seconds(600);
    assert_eq!(flood_wait_until(&pool).await, Some(until));
    assert_eq!(
        channel.requests(),
        [
            Request {
                before: None,
                at: t0
            },
            Request {
                before: Some(3),
                at: flood_at
            },
            Request {
                before: Some(3),
                at: until
            },
            Request {
                before: Some(1),
                at: until + delay
            },
        ]
    );
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=5));
    assert_eq!(newest_fetched_id(&pool).await, Some(5));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn new_loader_waits_for_flood_wait_stored_by_previous_one(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=2);
    let until = clock.now() + TimeDelta::minutes(38);
    sqlx::query!("UPDATE ingestion_state SET flood_wait_until = $1", until)
        .execute(&pool)
        .await
        .unwrap();

    loader(&pool, &channel, &clock).run_pass().await.unwrap();

    assert_eq!(
        channel.requests()[0],
        Request {
            before: None,
            at: until
        }
    );
    assert_eq!(stored_ids(&pool).await, [1, 2]);
}

/// Последняя ошибка загрузчика из `worker_state`.
#[derive(Debug, PartialEq)]
struct LastError {
    text: Option<String>,
    at: Option<DateTime<Utc>>,
    next_attempt_at: Option<DateTime<Utc>>,
}

async fn last_error(pool: &PgPool) -> LastError {
    sqlx::query_as!(
        LastError,
        "SELECT last_error AS text, last_error_at AS at, next_attempt_at FROM worker_state"
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn source_error_is_recorded_and_pass_continues_after_next_attempt(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=8);
    channel.fail_once_at(6);
    // 8 7 6 | ошибка | 5 4 3 | 2 1 | пустая страница | следующий проход.
    channel.stop_after(6);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    let delay = settings().request_delay;
    let failed_at = t0 + delay;
    let next_attempt_at = failed_at + settings().retry_delay;
    let error = last_error(&pool).await;
    assert!(
        error
            .text
            .as_deref()
            .unwrap()
            .contains("источник недоступен"),
        "{error:?}"
    );
    assert_eq!(error.at, Some(failed_at));
    assert_eq!(error.next_attempt_at, Some(next_attempt_at));
    assert_eq!(
        channel.requests()[1..=3],
        [
            Request {
                before: Some(6),
                at: failed_at
            },
            Request {
                before: Some(6),
                at: next_attempt_at
            },
            Request {
                before: Some(3),
                at: next_attempt_at + delay
            },
        ]
    );
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=8));
    assert_eq!(newest_fetched_id(&pool).await, Some(8));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn request_without_answer_times_out_and_is_retried(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=5);
    channel.hang_once_at(3);
    // 5 4 3 | нет ответа | 2 1 | пустая страница | следующий проход.
    channel.stop_after(5);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    let sent_at = t0 + settings().request_delay;
    let timed_out_at = sent_at + TimeDelta::seconds(30);
    let next_attempt_at = timed_out_at + settings().retry_delay;
    let error = last_error(&pool).await;
    assert!(error.text.as_deref().unwrap().contains("30"), "{error:?}");
    assert_eq!(error.at, Some(timed_out_at));
    assert_eq!(error.next_attempt_at, Some(next_attempt_at));
    assert_eq!(
        channel.requests()[1..=2],
        [
            Request {
                before: Some(3),
                at: sent_at
            },
            Request {
                before: Some(3),
                at: next_attempt_at
            },
        ]
    );
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=5));
    assert_eq!(newest_fetched_id(&pool).await, Some(5));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn database_error_is_recorded_and_loader_keeps_retrying(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=8);
    // Страница 5 4 3 не сохранится, пока ограничение на месте.
    sqlx::query!("ALTER TABLE raw_posts ADD CONSTRAINT reject_5 CHECK (message_id <> 5)")
        .execute(&pool)
        .await
        .unwrap();
    channel.stop_after(4);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    let failed_at = t0 + settings().request_delay;
    let retry = settings().retry_delay;
    assert_eq!(
        channel.requests(),
        [
            Request {
                before: None,
                at: t0
            },
            Request {
                before: Some(6),
                at: failed_at
            },
            Request {
                before: Some(6),
                at: failed_at + retry
            },
            Request {
                before: Some(6),
                at: failed_at + retry * 2
            },
        ]
    );
    let error = last_error(&pool).await;
    assert!(
        error.text.as_deref().unwrap().contains("reject_5"),
        "{error:?}"
    );
    assert_eq!(error.at, Some(failed_at + retry * 2));
    assert_eq!(error.next_attempt_at, Some(failed_at + retry * 3));
    assert_eq!(stored_ids(&pool).await, [6, 7, 8]);
    assert_eq!(newest_fetched_id(&pool).await, None);
}

async fn last_heartbeat_at(pool: &PgPool) -> Option<DateTime<Utc>> {
    sqlx::query_scalar!("SELECT last_heartbeat_at FROM worker_state")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn last_successful_request_at(pool: &PgPool) -> Option<DateTime<Utc>> {
    sqlx::query_scalar!("SELECT last_successful_request_at FROM ingestion_state")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Heartbeat пишется не реже этого в любом состоянии загрузчика.
const HEARTBEAT_INTERVAL: TimeDelta = TimeDelta::seconds(15);

/// Heartbeat записан не раньше чем за [`HEARTBEAT_INTERVAL`] до `moment`.
async fn assert_heartbeat_fresh_at(pool: &PgPool, moment: DateTime<Utc>) {
    let heartbeat = last_heartbeat_at(pool).await.unwrap();
    assert!(
        moment - HEARTBEAT_INTERVAL <= heartbeat && heartbeat <= moment,
        "heartbeat {heartbeat} на момент {moment}"
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn heartbeat_and_last_successful_request_are_updated(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=2);
    // 2 1 | пустая страница | остановка на запросе следующего прохода.
    channel.stop_after(2);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    let last_answer_at = t0 + settings().request_delay;
    assert_eq!(
        last_successful_request_at(&pool).await,
        Some(last_answer_at)
    );
    assert_heartbeat_fresh_at(&pool, clock.now()).await;
    assert_eq!(clock.now(), last_answer_at + settings().poll_interval);
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn heartbeat_stays_fresh_during_long_flood_wait(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=2);
    // 2 1 | FLOOD_WAIT на три часа.
    channel.flood_wait_once_at(1, 3 * 60 * 60);
    let moment = t0 + TimeDelta::minutes(97) + TimeDelta::seconds(7);
    clock.stop_at(moment);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    assert_eq!(clock.now(), moment);
    assert_heartbeat_fresh_at(&pool, moment).await;
    assert_eq!(channel.befores(), [None, Some(1)]);
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn heartbeat_stays_fresh_while_waiting_for_next_attempt(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=2);
    // 2 1 | ошибка.
    channel.fail_once_at(1);
    let failed_at = t0 + settings().request_delay;
    let moment = failed_at + settings().retry_delay - TimeDelta::seconds(1);
    clock.stop_at(moment);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    assert_eq!(clock.now(), moment);
    assert_heartbeat_fresh_at(&pool, moment).await;
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn requests_are_paced_and_passes_repeat_with_poll_interval(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=5);
    // Первый проход — 3 запроса, второй и третий — по одному.
    channel.stop_after(5);

    run_loader(&pool, &channel, &clock, clock.stopped())
        .await
        .unwrap();

    let delay = settings().request_delay;
    let interval = settings().poll_interval;
    let first_pass_end = t0 + delay * 2;
    assert_eq!(
        channel.requests(),
        [
            Request {
                before: None,
                at: t0
            },
            Request {
                before: Some(3),
                at: t0 + delay
            },
            Request {
                before: Some(1),
                at: first_pass_end
            },
            Request {
                before: None,
                at: first_pass_end + interval
            },
            Request {
                before: None,
                at: first_pass_end + interval * 2
            },
        ]
    );
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn stop_signal_ends_loader_after_current_page_and_next_run_resumes(pool: PgPool) {
    let clock = FakeClock::new();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    // 9 8 7 | сигнал приходит, пока источник отвечает на второй запрос.
    let stop = channel.stop_at_request(Some(7));

    run_loader(&pool, &channel, &clock, stop).await.unwrap();

    // Ответ на текущий запрос сохранён, новых запросов не было.
    assert_eq!(channel.befores(), [None, Some(7)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(4..=9));
    assert_eq!(newest_fetched_id(&pool).await, None);

    // 3 2 1 | пустая страница — проход завершён; сигнал приходит во время
    // первого запроса следующего прохода.
    let stop = channel.stop_at_request(None);
    run_loader(&pool, &channel, &clock, stop).await.unwrap();

    assert_eq!(channel.befores(), [None, Some(7), Some(4), Some(1), None]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=9));
    assert_eq!(newest_fetched_id(&pool).await, Some(9));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn stop_signal_during_failed_request_ends_loader_without_retry(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=9);
    // 9 8 7 | ошибка, пока приходит сигнал остановки.
    channel.fail_once_at(7);
    let stop = channel.stop_at_request(Some(7));

    run_loader(&pool, &channel, &clock, stop).await.unwrap();

    // Ошибка записана; паузы перед повтором и самого повтора не было.
    assert_eq!(channel.befores(), [None, Some(7)]);
    assert_eq!(clock.now(), t0 + settings().request_delay);
    let error = last_error(&pool).await;
    assert!(
        error
            .text
            .as_deref()
            .unwrap()
            .contains("источник недоступен"),
        "{error:?}"
    );
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(7..=9));
}

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn second_loader_exits_without_requests_while_first_is_running(pool: PgPool) {
    let clock = FakeClock::new();
    // У первого экземпляра свои часы: зависший запрос их останавливает.
    let first_clock = FakeClock::new();
    let first_channel = FakeChannel::new(&first_clock);
    first_channel.publish(1..=9);
    // Первый экземпляр ждёт ответа на второй запрос прохода.
    let first_waits = first_channel.hang_at_request(7);
    let (stop_first, first_stopped) = oneshot::channel::<()>();
    let first = tokio::spawn({
        let (pool, channel, clock) = (pool.clone(), first_channel.clone(), first_clock.clone());
        async move {
            let stop = async {
                first_stopped.await.unwrap();
            };
            run_loader(&pool, &channel, &clock, stop).await
        }
    });
    first_waits.await.unwrap();

    let second_channel = FakeChannel::new(&clock);
    second_channel.publish(1..=9);
    let error = run_loader(&pool, &second_channel, &clock, pending())
        .await
        .unwrap_err();

    assert!(matches!(error, Error::AlreadyRunning), "{error:?}");
    assert_eq!(second_channel.requests(), []);

    // Первый останавливается, не дождавшись ответа; новый экземпляр берёт
    // блокировку и продолжает проход.
    stop_first.send(()).unwrap();
    first.await.unwrap().unwrap();
    let third_channel = FakeChannel::new(&clock);
    third_channel.publish(1..=9);
    let stop_third = third_channel.stop_at_request(Some(1));
    run_loader(&pool, &third_channel, &clock, stop_third)
        .await
        .unwrap();

    assert_eq!(third_channel.befores(), [Some(7), Some(4), Some(1)]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=9));
    assert_eq!(newest_fetched_id(&pool).await, Some(9));
}

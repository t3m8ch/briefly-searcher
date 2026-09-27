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
use tokio::sync::oneshot;

const PAYLOAD_SCHEMA: &str = "fake-source v1";
const PAGE_SIZE: u32 = 3;

/// Часы, которые не спят, а переводят время на момент окончания ожидания.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<DateTime<Utc>>>);

impl FakeClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(
            Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap(),
        )))
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }

    async fn sleep_until(&self, deadline: DateTime<Utc>) {
        let mut now = self.0.lock().unwrap();
        *now = (*now).max(deadline);
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
    /// Сообщения, которые появятся в канале прямо перед ответом на запрос
    /// с этим `before` (один раз).
    publish_during: HashMap<i64, RangeInclusive<i64>>,
    /// Все запросы после этого числа завершаются ошибкой.
    fail_after: Option<usize>,
    /// Сигналы остановки, которые подаются при получении запроса с этим
    /// `before` (`None` — без границы): запрос обслуживается как обычно.
    stop_at: HashMap<Option<i64>, oneshot::Sender<()>>,
    /// Запросы с этим `before` остаются без ответа; отправитель
    /// сообщает, что источник получил запрос.
    hang_at: HashMap<i64, oneshot::Sender<()>>,
}

impl FakeChannel {
    fn new(clock: &FakeClock) -> Self {
        Self(Arc::new(Mutex::new(Channel {
            clock: clock.clone(),
            messages: BTreeMap::new(),
            requests: Vec::new(),
            fail_once_at: Vec::new(),
            publish_during: HashMap::new(),
            fail_after: None,
            stop_at: HashMap::new(),
            hang_at: HashMap::new(),
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

    /// Сообщения `ids` появятся в канале, пока загрузчик ждёт ответа на
    /// запрос с этим `before`.
    fn publish_during_request(&self, before: i64, ids: RangeInclusive<i64>) {
        self.0.lock().unwrap().publish_during.insert(before, ids);
    }

    /// Все запросы после `requests`-го завершатся ошибкой источника.
    fn fail_after(&self, requests: usize) {
        self.0.lock().unwrap().fail_after = Some(requests);
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
        let at = self.clock.now();
        self.requests.push(Request { before, at });
        if let Some(received) = before.and_then(|b| self.hang_at.remove(&b)) {
            received.send(()).unwrap();
            return None;
        }
        if let Some(ids) = before.and_then(|b| self.publish_during.remove(&b)) {
            self.publish(ids);
        }
        if let Some(stop) = self.stop_at.remove(&before) {
            stop.send(()).unwrap();
        }
        let fail_once = self.fail_once_at.iter().position(|&b| Some(b) == before);
        if let Some(i) = fail_once {
            self.fail_once_at.remove(i);
        }
        let failing = self.fail_after.is_some_and(|n| self.requests.len() > n);
        if fail_once.is_some() || failing {
            return Some(Err(HistoryError::Other("источник недоступен".into())));
        }
        let upper = before.unwrap_or(i64::MAX);
        Some(Ok(self
            .messages
            .range(..upper)
            .rev()
            .take(limit as usize)
            .map(|(&id, text)| Message {
                id,
                payload: payload(id, text),
                payload_schema: PAYLOAD_SCHEMA.to_owned(),
            })
            .collect()))
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

#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]
async fn requests_are_paced_and_passes_repeat_with_poll_interval(pool: PgPool) {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let channel = FakeChannel::new(&clock);
    channel.publish(1..=5);
    // Первый проход — 3 запроса, второй — 1, запрос третьего прохода
    // завершается ошибкой и останавливает загрузчик.
    channel.fail_after(4);

    run_loader(&pool, &channel, &clock, pending())
        .await
        .unwrap_err();

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
async fn second_loader_exits_without_requests_while_first_is_running(pool: PgPool) {
    let clock = FakeClock::new();
    let first_channel = FakeChannel::new(&clock);
    first_channel.publish(1..=9);
    // Первый экземпляр ждёт ответа на второй запрос прохода.
    let first_waits = first_channel.hang_at_request(7);
    let (stop_first, first_stopped) = oneshot::channel::<()>();
    let first = tokio::spawn({
        let (pool, channel, clock) = (pool.clone(), first_channel.clone(), clock.clone());
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

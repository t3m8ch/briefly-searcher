//! Проход загрузчика на поддельном источнике и поддельных часах поверх
//! настоящей PostgreSQL. Тесты смотрят только на содержимое БД и журнал
//! запросов источника.

use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};

use briefly_searcher::clock::Clock;
use briefly_searcher::loader::{Loader, Settings};
use briefly_searcher_storage::Storage;
use briefly_searcher_telegram::{HistoryError, HistorySource, Message};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use serde_json::json;
use sqlx::PgPool;

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
    offset_id: i64,
    at: DateTime<Utc>,
}

/// Поддельный канал: отдаёт страницы по `offset_id` из своих сообщений,
/// выполняет сценарий и ведёт журнал запросов.
#[derive(Clone)]
struct FakeChannel(Arc<Mutex<Channel>>);

struct Channel {
    clock: FakeClock,
    /// Текст сообщения по его ID.
    messages: BTreeMap<i64, String>,
    requests: Vec<Request>,
    /// `offset_id` запросов, которые один раз завершатся ошибкой.
    fail_once_at: Vec<i64>,
    /// Сообщения, которые появятся в канале прямо перед ответом на запрос
    /// с этим `offset_id` (один раз).
    publish_during: HashMap<i64, RangeInclusive<i64>>,
    /// Все запросы после этого числа завершаются ошибкой.
    fail_after: Option<usize>,
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
        })))
    }

    fn publish(&self, ids: RangeInclusive<i64>) {
        self.0.lock().unwrap().publish(ids);
    }

    fn edit(&self, id: i64, text: &str) {
        self.0.lock().unwrap().messages.insert(id, text.to_owned());
    }

    /// Запрос с этим `offset_id` один раз завершится ошибкой источника.
    fn fail_once_at(&self, offset_id: i64) {
        self.0.lock().unwrap().fail_once_at.push(offset_id);
    }

    /// Сообщения `ids` появятся в канале, пока загрузчик ждёт ответа на
    /// запрос с этим `offset_id`.
    fn publish_during_request(&self, offset_id: i64, ids: RangeInclusive<i64>) {
        self.0.lock().unwrap().publish_during.insert(offset_id, ids);
    }

    /// Все запросы после `requests`-го завершатся ошибкой источника.
    fn fail_after(&self, requests: usize) {
        self.0.lock().unwrap().fail_after = Some(requests);
    }

    fn requests(&self) -> Vec<Request> {
        self.0.lock().unwrap().requests.clone()
    }

    /// `offset_id` всех запросов по порядку.
    fn offsets(&self) -> Vec<i64> {
        self.requests().iter().map(|r| r.offset_id).collect()
    }
}

impl Channel {
    fn publish(&mut self, ids: RangeInclusive<i64>) {
        for id in ids {
            self.messages.insert(id, format!("сообщение {id}"));
        }
    }

    fn fetch_page(&mut self, offset_id: i64, limit: u32) -> Result<Vec<Message>, HistoryError> {
        let at = self.clock.now();
        self.requests.push(Request { offset_id, at });
        if let Some(ids) = self.publish_during.remove(&offset_id) {
            self.publish(ids);
        }
        let fail_once = self.fail_once_at.iter().position(|&o| o == offset_id);
        if let Some(i) = fail_once {
            self.fail_once_at.remove(i);
        }
        let failing = self.fail_after.is_some_and(|n| self.requests.len() > n);
        if fail_once.is_some() || failing {
            return Err(HistoryError::Other("источник недоступен".into()));
        }
        let upper = if offset_id == 0 { i64::MAX } else { offset_id };
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
    async fn fetch_page(&self, offset_id: i64, limit: u32) -> Result<Vec<Message>, HistoryError> {
        self.0.lock().unwrap().fetch_page(offset_id, limit)
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
    assert_eq!(channel.offsets(), [0, 6, 3, 1]);
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
    assert_eq!(channel.offsets()[first_pass_requests..], [0, 12]);
    assert_eq!(stored_ids(&pool).await, Vec::from_iter(1..=14));
    assert_eq!(newest_fetched_id(&pool).await, Some(14));
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

    assert_eq!(channel.offsets()[before_restart..], [12]);
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

    assert_eq!(channel.offsets()[before_restart..], [4, 1]);
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
    assert_eq!(channel.offsets()[before_restart..], [14, 11]);
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

    loader(&pool, &channel, &clock).run().await.unwrap_err();

    let delay = settings().request_delay;
    let interval = settings().poll_interval;
    let first_pass_end = t0 + delay * 2;
    assert_eq!(
        channel.requests(),
        [
            Request {
                offset_id: 0,
                at: t0
            },
            Request {
                offset_id: 3,
                at: t0 + delay
            },
            Request {
                offset_id: 1,
                at: first_pass_end
            },
            Request {
                offset_id: 0,
                at: first_pass_end + interval
            },
            Request {
                offset_id: 0,
                at: first_pass_end + interval * 2
            },
        ]
    );
}

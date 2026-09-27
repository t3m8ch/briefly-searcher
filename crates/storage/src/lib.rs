//! Хранилище PostgreSQL — единственный crate, который обращается к БД.
//!
//! Здесь живут схема, миграции (`migrations/`) и все SQL-запросы. Остальные
//! crates вызывают функции этого crate и сами SQL не пишут.

use chrono::{DateTime, Utc};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::{Connection, PgConnection, PgPool, Postgres, Transaction};

/// Миграции, вшитые в бинарник. Открыты для тестов других crates:
/// `#[sqlx::test(migrator = "briefly_searcher_storage::MIGRATOR")]`.
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Ошибка обращения к хранилищу.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("не удалось подключиться к PostgreSQL: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("не удалось применить миграции: {0}")]
    Migrate(#[from] MigrateError),
    #[error("ошибка запроса к PostgreSQL: {0}")]
    Query(#[from] sqlx::Error),
}

/// Состояние прохода, выведенное из `ingestion_state` и `raw_posts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassState {
    /// Наибольший `message_id`, до которого все сообщения канала сохранены;
    /// `None`, пока первый проход не завершён.
    pub newest_fetched_id: Option<i64>,
    /// Граница `before`, с которой продолжать незавершённый проход:
    /// `min(message_id)` среди строк новее `newest_fetched_id` (при `None` —
    /// среди всех строк). `None`, если незавершённого прохода нет.
    pub resume_before: Option<i64>,
}

/// Сообщение Telegram для вставки в `raw_posts`.
#[derive(Clone, Copy, Debug)]
pub struct RawMessage<'a> {
    /// ID сообщения в Telegram.
    pub message_id: i64,
    /// Декодированный объект сообщения (ADR-0001).
    pub payload: &'a serde_json::Value,
    /// Чем записан `payload`: версия crate и TL layer.
    pub payload_schema: &'a str,
}

/// Сводка для веб-админки: состояние загрузки одним согласованным снимком.
///
/// Секретов (сессии Telegram) и `payload` сообщений в сводке нет.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminSummary {
    /// Момент снимка по часам PostgreSQL; с ним сравниваются остальные времена.
    pub now: DateTime<Utc>,
    /// Наибольший `message_id`, до которого все сообщения канала сохранены;
    /// `None`, пока первый проход не завершён.
    pub newest_fetched_id: Option<i64>,
    /// Докуда дошёл незавершённый проход: `min(message_id)` среди строк новее
    /// `newest_fetched_id` (при `None` — среди всех строк). `None`, если
    /// незавершённого прохода нет.
    pub pass_reached_id: Option<i64>,
    /// Число сообщений в `raw_posts`.
    pub raw_posts_count: i64,
    /// Срок паузы после `FLOOD_WAIT`; может быть уже в прошлом.
    pub flood_wait_until: Option<DateTime<Utc>>,
    /// Последний успешный запрос к Telegram.
    pub last_successful_request_at: Option<DateTime<Utc>>,
    /// Последний heartbeat основного цикла загрузчика.
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    /// Последняя ошибка загрузчика; после успешных запросов не очищается.
    pub last_error: Option<String>,
    /// Когда случилась последняя ошибка.
    pub last_error_at: Option<DateTime<Utc>>,
    /// Когда загрузчик повторит попытку после ошибки.
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// Ключ session-level advisory-блокировки единственного экземпляра
/// загрузчика. БД обслуживает одно приложение, поэтому ключ — просто
/// константа: байты слова `loader`, дополненные нулями до 8 байт.
const LOADER_LOCK_KEY: i64 = i64::from_be_bytes(*b"\0\0loader");

/// Advisory-блокировка единственного экземпляра загрузчика.
///
/// Держится на выделенном соединении, а не на соединении из пула: пул может
/// закрыть или пересоздать своё соединение, и блокировка бы молча пропала.
/// Если процесс падает, PostgreSQL снимает блокировку вместе с сессией.
#[derive(Debug)]
pub struct LoaderLock {
    connection: PgConnection,
}

impl LoaderLock {
    /// Снимает блокировку и закрывает её соединение. Блокировку можно и
    /// просто уронить, но тогда PostgreSQL снимет её не сразу, а когда
    /// заметит закрытие соединения.
    pub async fn release(mut self) -> Result<(), Error> {
        sqlx::query_scalar!(
            r#"SELECT pg_advisory_unlock($1) AS "unlocked!""#,
            LOADER_LOCK_KEY
        )
        .fetch_one(&mut self.connection)
        .await?;
        self.connection.close().await?;
        Ok(())
    }
}

/// Пул соединений с PostgreSQL.
#[derive(Clone, Debug)]
pub struct Storage {
    pool: PgPool,
}

impl Storage {
    /// Подключается к PostgreSQL по URL из конфигурации.
    pub async fn connect(database_url: &str) -> Result<Self, Error> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(Error::Connect)?;
        Ok(Self::from_pool(pool))
    }

    /// Оборачивает готовый пул, например пул тестовой БД `#[sqlx::test]`.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Применяет все ещё не применённые миграции.
    ///
    /// Вызывается только разовой командой `migrate`; долгоживущие процессы
    /// схему не меняют.
    pub async fn migrate(&self) -> Result<(), Error> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Берёт блокировку единственного экземпляра загрузчика на отдельном
    /// соединении с теми же параметрами, что и у пула. `None` — если её
    /// держит другой экземпляр.
    pub async fn try_lock_loader(&self) -> Result<Option<LoaderLock>, Error> {
        let mut connection = PgConnection::connect_with(&self.pool.connect_options())
            .await
            .map_err(Error::Connect)?;
        let locked = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) AS "locked!""#,
            LOADER_LOCK_KEY
        )
        .fetch_one(&mut connection)
        .await?;
        if locked {
            Ok(Some(LoaderLock { connection }))
        } else {
            connection.close().await?;
            Ok(None)
        }
    }

    /// Выводит состояние прохода из сохранённых данных.
    pub async fn pass_state(&self) -> Result<PassState, Error> {
        // Проход сохраняет только сообщения не новее своей первой страницы и
        // идёт вниз без пропусков, поэтому строки новее newest_fetched_id —
        // ровно то, что успел сохранить незавершённый проход.
        //
        // ID сообщений Telegram положительны, поэтому NULL заменяется на 0.
        // Условие без OR попадает в Index Cond: min() читает одну запись
        // первичного ключа (Index Only Scan), а не фильтрует весь диапазон
        // до newest_fetched_id.
        let state = sqlx::query_as!(
            PassState,
            r#"
            SELECT
                s.newest_fetched_id,
                (
                    SELECT min(p.message_id)
                    FROM raw_posts p
                    WHERE p.message_id > COALESCE(s.newest_fetched_id, 0)
                ) AS resume_before
            FROM ingestion_state s
            "#
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(state)
    }

    /// Сохраняет страницу прохода одной транзакцией и возвращает, сколько
    /// сообщений записано впервые.
    ///
    /// Уже сохранённые сообщения не переписываются: остаётся впервые
    /// сохранённый `payload`.
    pub async fn save_page(
        &self,
        page: &[RawMessage<'_>],
        fetched_at: DateTime<Utc>,
    ) -> Result<u64, Error> {
        let mut tx = self.pool.begin().await?;
        let saved = insert_raw_messages(&mut tx, page, fetched_at).await?;
        tx.commit().await?;
        Ok(saved)
    }

    /// Сохраняет последнюю страницу прохода и в той же транзакции сдвигает
    /// `newest_fetched_id` на наибольший сохранённый `message_id`. Возвращает,
    /// сколько сообщений страницы записано впервые.
    pub async fn finish_pass(
        &self,
        last_page: &[RawMessage<'_>],
        fetched_at: DateTime<Utc>,
    ) -> Result<u64, Error> {
        let mut tx = self.pool.begin().await?;
        let saved = insert_raw_messages(&mut tx, last_page, fetched_at).await?;
        sqlx::query!(
            "UPDATE ingestion_state SET newest_fetched_id = (SELECT max(message_id) FROM raw_posts)"
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(saved)
    }

    /// Собирает сводку для веб-админки.
    pub async fn admin_summary(&self) -> Result<AdminSummary, Error> {
        // Один оператор видит один снимок данных (READ COMMITTED), поэтому
        // счётчик и граница прохода не разойдутся посреди сохранения страницы.
        let summary = sqlx::query_as!(
            AdminSummary,
            r#"
            SELECT
                now() AS "now!",
                i.newest_fetched_id,
                -- Как в pass_state: без OR min() читает одну запись индекса.
                (SELECT min(message_id) FROM raw_posts
                  WHERE message_id > COALESCE(i.newest_fetched_id, 0)
                ) AS pass_reached_id,
                (SELECT count(*) FROM raw_posts) AS "raw_posts_count!",
                i.flood_wait_until,
                i.last_successful_request_at,
                w.last_heartbeat_at,
                w.last_error,
                w.last_error_at,
                w.next_attempt_at
            FROM ingestion_state i CROSS JOIN worker_state w
            "#
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(summary)
    }
}

/// Вставляет сообщения без перезаписи и возвращает, сколько из них новые.
async fn insert_raw_messages(
    tx: &mut Transaction<'_, Postgres>,
    messages: &[RawMessage<'_>],
    fetched_at: DateTime<Utc>,
) -> Result<u64, Error> {
    let mut inserted = 0;
    for message in messages {
        inserted += sqlx::query!(
            "INSERT INTO raw_posts (message_id, payload, payload_schema, fetched_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (message_id) DO NOTHING",
            message.message_id,
            message.payload,
            message.payload_schema,
            fetched_at,
        )
        .execute(&mut **tx)
        .await?
        .rows_affected();
    }
    Ok(inserted)
}

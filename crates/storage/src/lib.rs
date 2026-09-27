//! Хранилище PostgreSQL — единственный crate, который обращается к БД.
//!
//! Здесь живут схема, миграции (`migrations/`) и все SQL-запросы. Остальные
//! crates вызывают функции этого crate и сами SQL не пишут.

use chrono::{DateTime, Utc};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::{PgPool, Postgres, Transaction};

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
    /// `offset_id`, с которого продолжать незавершённый проход:
    /// `min(message_id)` среди строк новее `newest_fetched_id` (при `None` —
    /// среди всех строк). `None`, если незавершённого прохода нет.
    pub resume_offset_id: Option<i64>,
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
                ) AS resume_offset_id
            FROM ingestion_state s
            "#
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(state)
    }

    /// Сохраняет страницу прохода одной транзакцией.
    ///
    /// Уже сохранённые сообщения не переписываются: остаётся впервые
    /// сохранённый `payload`.
    pub async fn save_page(
        &self,
        page: &[RawMessage<'_>],
        fetched_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await?;
        insert_raw_messages(&mut tx, page, fetched_at).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Сохраняет последнюю страницу прохода и в той же транзакции сдвигает
    /// `newest_fetched_id` на наибольший сохранённый `message_id`.
    pub async fn finish_pass(
        &self,
        last_page: &[RawMessage<'_>],
        fetched_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await?;
        insert_raw_messages(&mut tx, last_page, fetched_at).await?;
        sqlx::query!(
            "UPDATE ingestion_state SET newest_fetched_id = (SELECT max(message_id) FROM raw_posts)"
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn insert_raw_messages(
    tx: &mut Transaction<'_, Postgres>,
    messages: &[RawMessage<'_>],
    fetched_at: DateTime<Utc>,
) -> Result<(), Error> {
    for message in messages {
        sqlx::query!(
            "INSERT INTO raw_posts (message_id, payload, payload_schema, fetched_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (message_id) DO NOTHING",
            message.message_id,
            message.payload,
            message.payload_schema,
            fetched_at,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

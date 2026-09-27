//! Хранилище PostgreSQL — единственный crate, который обращается к БД.
//!
//! Здесь живут схема, миграции (`migrations/`) и все SQL-запросы. Остальные
//! crates вызывают функции этого crate и сами SQL не пишут.

use sqlx::PgPool;
use sqlx::migrate::{MigrateError, Migrator};

/// Миграции, вшитые в бинарник.
static MIGRATOR: Migrator = sqlx::migrate!();

/// Ошибка обращения к хранилищу.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("не удалось подключиться к PostgreSQL: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("не удалось применить миграции: {0}")]
    Migrate(#[from] MigrateError),
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
}

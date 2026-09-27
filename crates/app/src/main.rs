//! Бинарник Briefly Searcher: разовые административные команды и долгоживущие процессы.

mod config;

use anyhow::Context;
use briefly_storage::Storage;
use clap::{Parser, Subcommand};
use envconfig::Envconfig;
use tracing_subscriber::EnvFilter;

use crate::config::DatabaseConfig;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Применить миграции схемы к БД из DATABASE_URL и завершиться.
    Migrate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv()?;

    // Логи — поток событий в stdout (12-factor, фактор XI); уровень задаёт RUST_LOG.
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    match Cli::parse().command {
        Command::Migrate => migrate().await,
    }
}

async fn migrate() -> anyhow::Result<()> {
    let config = DatabaseConfig::init_from_env()
        .context("не удалось прочитать конфигурацию из окружения")?;
    let storage = Storage::connect(&config.database_url).await?;
    tracing::info!("применяю миграции");
    storage.migrate().await.context("команда migrate")?;
    tracing::info!("миграции применены");
    Ok(())
}

/// Подгружает локальный `.env`, если он есть: удобство разработки.
/// Уже заданные переменные окружения имеют приоритет над файлом.
fn load_dotenv() -> anyhow::Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(error) if error.not_found() => Ok(()),
        Err(error) => Err(error).context("не удалось прочитать .env"),
    }
}

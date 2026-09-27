//! Бинарник Briefly: разовые административные команды и долгоживущие процессы.

mod config;

use anyhow::Context;
use briefly_storage::Storage;
use clap::{Parser, Subcommand};
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
    let config = DatabaseConfig::from_env()?;
    let storage = Storage::connect(&config.database_url).await?;
    tracing::info!("применяю миграции");
    storage.migrate().await.context("команда migrate")?;
    tracing::info!("миграции применены");
    Ok(())
}

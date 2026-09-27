//! Бинарник Briefly Searcher: разовые административные команды и долгоживущие процессы.

mod config;

use anyhow::Context;
use briefly_searcher::web;
use briefly_searcher_storage::Storage;
use clap::{Parser, Subcommand};
use envconfig::Envconfig;
use tracing_subscriber::EnvFilter;

use crate::config::{DatabaseConfig, WebConfig};

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
    /// Запустить веб-админку только для чтения на WEB_ADDR (по умолчанию 127.0.0.1:3000).
    Web,
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
        Command::Web => serve_web().await,
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

async fn serve_web() -> anyhow::Result<()> {
    let config =
        WebConfig::init_from_env().context("не удалось прочитать конфигурацию из окружения")?;
    let storage = Storage::connect(&config.database.database_url).await?;
    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("не удалось занять адрес {}", config.addr))?;
    tracing::info!(addr = %config.addr, "веб-админка: http://{}", config.addr);
    web::serve(listener, storage, shutdown_signal())
        .await
        .context("веб-сервер")?;
    tracing::info!("веб-админка остановлена");
    Ok(())
}

/// Завершается по SIGINT или SIGTERM.
async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "не удалось подписаться на SIGINT");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "не удалось подписаться на SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("получен сигнал остановки, дорабатываю текущие запросы");
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

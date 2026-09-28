//! Бинарник Briefly Searcher: разовые административные команды и долгоживущие процессы.

mod config;

use anyhow::Context;
use briefly_searcher::clock::SystemClock;
use briefly_searcher::loader::{self, Loader};
use briefly_searcher::web;
use briefly_searcher_storage::Storage;
use briefly_searcher_telegram::{WebFeed, WebFeedSettings};
use chrono::TimeDelta;
use clap::{Parser, Subcommand};
use envconfig::Envconfig;
use tracing_subscriber::EnvFilter;

use crate::config::{DatabaseConfig, LoaderConfig, WebConfig};

/// Адрес Telegram, у которого загрузчик читает веб-ленту `/s/<канал>`.
const TELEGRAM_BASE_URL: &str = "https://t.me";

/// Сколько блоков загрузчик просит у ленты за раз. Лента отдаёт около 20
/// сообщений и размер страницы не принимает, поэтому это лишь верхняя граница.
const PAGE_SIZE: u32 = 100;

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
    /// Загружать веб-ленту канала TELEGRAM_CHANNEL в БД, повторяя проходы
    /// с интервалом опроса.
    Loader,
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
        Command::Loader => run_loader().await,
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

async fn run_loader() -> anyhow::Result<()> {
    let config =
        LoaderConfig::init_from_env().context("не удалось прочитать конфигурацию из окружения")?;
    let storage = Storage::connect(&config.database.database_url).await?;
    // Блокировка — до создания источника: второй экземпляр завершается, не
    // обратившись к Telegram.
    let lock = loader::lock(&storage).await.inspect_err(|error| {
        tracing::error!(%error, "загрузчик не запущен");
    })?;
    let source = WebFeed::new(WebFeedSettings {
        base_url: TELEGRAM_BASE_URL.to_owned(),
        channel: config.channel.clone(),
        flood_wait_secs: config.flood_wait_secs,
    })
    .map_err(|error| anyhow::anyhow!(error))
    .context("не удалось создать источник на веб-ленте")?;
    let settings = loader::Settings {
        page_size: PAGE_SIZE,
        request_delay: TimeDelta::seconds(config.request_delay_secs.into()),
        poll_interval: TimeDelta::seconds(config.poll_interval_secs.into()),
    };
    tracing::info!(
        channel = %config.channel,
        poll_interval_secs = config.poll_interval_secs,
        request_delay_secs = config.request_delay_secs,
        flood_wait_secs = config.flood_wait_secs,
        "загрузчик запущен"
    );
    // До устойчивости к ошибкам загрузчик завершается на первой ошибке
    // источника или БД. Блокировку `run` снимает сам при любом исходе.
    Loader::new(storage, source, SystemClock, settings)
        .run(lock, shutdown_signal())
        .await
        .inspect_err(|error| tracing::error!(%error, "загрузчик остановлен ошибкой"))
        .context("команда loader")?;
    tracing::info!("загрузчик остановлен");
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
    if !config.addr.ip().is_loopback() {
        // Проверка Host пропускает только localhost и loopback-адреса.
        tracing::warn!(
            addr = %config.addr,
            "адрес не loopback, но админка отвечает только на запросы к localhost"
        );
    }
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
    tracing::info!("получен сигнал остановки, завершаю текущую работу");
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

//! Команда `loader` бинарника: запуск с неполной конфигурацией и запуск
//! второго экземпляра.

use std::process::{Command, Output};

use briefly_searcher::loader;
use briefly_searcher_storage::Storage;

/// Прокси, через который любой HTTPS-запрос сразу завершается ошибкой: так
/// тест видит, что загрузчик не обращался к Telegram.
const DEAD_PROXY: &str = "http://127.0.0.1:9";

/// Запускает `briefly-searcher loader` только с окружением `env`.
fn run_loader(env: &[(&str, &str)]) -> Output {
    // Бинарник ищет `.env` в рабочем каталоге и выше, поэтому запускается
    // вне репозитория: только окружение теста, без `.env` разработчика.
    let workdir = std::env::temp_dir().join("briefly-searcher-loader-command");
    std::fs::create_dir_all(&workdir).unwrap();
    Command::new(env!("CARGO_BIN_EXE_briefly-searcher"))
        .arg("loader")
        .current_dir(&workdir)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

#[test]
fn loader_without_channel_exits_with_clear_error() {
    let output = run_loader(&[("DATABASE_URL", "postgres://localhost/briefly_searcher")]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TELEGRAM_CHANNEL"),
        "в ошибке нет имени переменной: {stderr}"
    );
}

#[tokio::test]
async fn second_loader_exits_with_clear_error_without_requests_to_telegram() {
    // Блокировка advisory — на всю БД, поэтому тест берёт её в БД из
    // DATABASE_URL, как и запущенный следом бинарник. Если её уже держит
    // загрузчик разработчика, второй экземпляр тем более не нужен.
    let database_url = dotenvy::var("DATABASE_URL").unwrap();
    let storage = Storage::connect(&database_url).await.unwrap();
    let lock = loader::lock(&storage).await.ok();

    let output = run_loader(&[
        ("DATABASE_URL", &database_url),
        ("TELEGRAM_CHANNEL", "brieflyru"),
        ("HTTPS_PROXY", DEAD_PROXY),
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("загрузчик уже запущен"),
        "второй экземпляр завершился не из-за блокировки: {stderr}"
    );
    if let Some(lock) = lock {
        lock.release().await.unwrap();
    }
}

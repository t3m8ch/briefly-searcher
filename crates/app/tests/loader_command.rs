//! Команда `loader` бинарника: запуск с неполной конфигурацией.

use std::process::Command;

#[test]
fn loader_without_channel_exits_with_clear_error() {
    // Бинарник ищет `.env` в рабочем каталоге и выше, поэтому запускается
    // вне репозитория: только окружение теста, без `.env` разработчика.
    let workdir = std::env::temp_dir().join("briefly-searcher-loader-command");
    std::fs::create_dir_all(&workdir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_briefly-searcher"))
        .arg("loader")
        .current_dir(&workdir)
        .env_clear()
        .env("DATABASE_URL", "postgres://localhost/briefly_searcher")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TELEGRAM_CHANNEL"),
        "в ошибке нет имени переменной: {stderr}"
    );
}

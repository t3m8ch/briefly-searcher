//! Команда `loader` бинарника: запуск с неполной конфигурацией.

use std::process::Command;

#[test]
fn loader_without_channel_exits_with_clear_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_briefly-searcher"))
        .arg("loader")
        // Только окружение теста: без `.env` рабочего каталога и без
        // переменных разработчика.
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
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

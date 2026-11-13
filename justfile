set dotenv-load

# Список команд
default:
    @just --list

# Собрать приложение
build:
    SQLX_OFFLINE=true heavy -- cargo build --workspace

# Проверить компиляцию всех targets
check:
    SQLX_OFFLINE=true heavy -- cargo check --workspace --all-targets

# Запустить тесты (нужна PostgreSQL из DATABASE_URL)
test *args:
    SQLX_OFFLINE=true heavy -- cargo test --workspace {{args}}

# Проверить код Clippy
clippy:
    SQLX_OFFLINE=true heavy -- cargo clippy --workspace --all-targets

# Проверить форматирование
fmt:
    cargo fmt --all -- --check

# Обновить кэш SQL-запросов (нужны sqlx-cli и живая БД)
sqlx-prepare:
    heavy -- cargo sqlx prepare --workspace -- --all-targets

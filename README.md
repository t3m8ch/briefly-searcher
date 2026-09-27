# briefly-searcher

Briefly Searcher — поиск по каналу @brieflyru. План — [docs/scraper-plan.md](docs/scraper-plan.md), глоссарий — [CONTEXT.md](CONTEXT.md).

## Crates

- `crates/telegram` — доступ к Telegram;
- `crates/storage` — PostgreSQL: схема, миграции и все SQL-запросы;
- `crates/app` — бинарник `briefly` с командами.

## Разработка

Конфигурация — только переменные окружения; локально их удобно держать в `.env`: бинарник подхватывает его при запуске, уже заданные переменные окружения важнее файла. `.env` в Git не попадает.

```sh
DATABASE_URL=postgres://postgres:postgres@localhost/briefly
```

```sh
cargo run -- migrate          # применить миграции к БД из DATABASE_URL
cargo test --workspace        # тесты #[sqlx::test] создают временные БД через DATABASE_URL
```

Логи пишутся в stdout; уровень задаёт `RUST_LOG` (по умолчанию `info`).

Проект собирается без живой БД по кэшу запросов из `.sqlx/`. После изменения SQL-запросов обновите кэш ([`sqlx-cli`](https://crates.io/crates/sqlx-cli)):

```sh
cargo sqlx prepare --workspace -- --all-targets
```

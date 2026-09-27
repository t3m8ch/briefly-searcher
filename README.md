# Briefly Searcher

Briefly Searcher — поиск по каналу @brieflyru. План — [docs/scraper-plan.md](docs/scraper-plan.md), глоссарий — [CONTEXT.md](CONTEXT.md).

## Crates

- `crates/telegram` — доступ к Telegram;
- `crates/storage` — PostgreSQL: схема, миграции и все SQL-запросы;
- `crates/app` — бинарник `briefly-searcher` с командами и веб-админкой (`src/web`, шаблон `templates/admin.html`, вшитый htmx 2.0.11 в `assets/`).

## Разработка

Конфигурация — только переменные окружения; локально их удобно держать в `.env`: бинарник подхватывает его при запуске, уже заданные переменные окружения важнее файла. `.env` в Git не попадает.

```sh
DATABASE_URL=postgres://postgres:postgres@localhost/briefly_searcher
TELEGRAM_API_ID=123456        # приложение с https://my.telegram.org
TELEGRAM_API_HASH=...         # секрет
```

```sh
cargo run -- migrate          # применить миграции к БД из DATABASE_URL
cargo run -- web              # веб-админка только для чтения на http://127.0.0.1:3000
cargo run -- login            # войти в аккаунт Telegram и сохранить сессию в БД
cargo test --workspace        # тесты #[sqlx::test] создают временные БД через DATABASE_URL
```

Адрес веб-админки задаёт `WEB_ADDR` (по умолчанию `127.0.0.1:3000`). Админка отвечает только на запросы с `Host` вида `localhost` или loopback-адреса (защита от DNS rebinding), поэтому открывать её нужно по такому адресу, например `http://localhost:3000`.

`login` спрашивает номер телефона, код входа и, если включена двухэтапная проверка, пароль 2FA. Сессия Telegram хранится в таблице `telegram_session`, а не файлом; она попадает в БД только после успешного входа и заменяет прежнюю. Сессия, `api_id` и `api_hash` — секреты: в логи они не пишутся.

Логи пишутся в stdout; уровень задаёт `RUST_LOG` (по умолчанию `info`).

Проект собирается без живой БД по кэшу запросов из `.sqlx/`. После изменения SQL-запросов обновите кэш ([`sqlx-cli`](https://crates.io/crates/sqlx-cli)):

```sh
cargo sqlx prepare --workspace -- --all-targets
```

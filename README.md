# Briefly Searcher

Briefly Searcher — поиск по каналу @brieflyru. План — [docs/scraper-plan.md](docs/scraper-plan.md), глоссарий — [CONTEXT.md](CONTEXT.md).

## Crates

- `crates/telegram` — доступ к Telegram: источник истории на веб-ленте `t.me/s` (записанные страницы для тестов — в `tests/fixtures/`);
- `crates/storage` — PostgreSQL: схема, миграции и все SQL-запросы;
- `crates/app` — бинарник `briefly-searcher` с командами и веб-админкой (`src/web`, шаблон `templates/admin.html`, вшитый htmx 2.0.11 в `assets/`).

## Разработка

Конфигурация — только переменные окружения; локально их удобно держать в `.env`: бинарник подхватывает его при запуске, уже заданные переменные окружения важнее файла. `.env` в Git не попадает.

```sh
DATABASE_URL=postgres://postgres:postgres@localhost/briefly_searcher
TELEGRAM_CHANNEL=brieflyru
```

```sh
cargo run -- migrate          # применить миграции к БД из DATABASE_URL
cargo run -- loader           # загружать веб-ленту t.me/s/$TELEGRAM_CHANNEL в raw_posts
cargo run -- web              # веб-админка только для чтения на http://127.0.0.1:3000
cargo test --workspace        # тесты #[sqlx::test] создают временные БД через DATABASE_URL
```

`loader` и `web` — отдельные долгоживущие процессы: загрузчик сохраняет блоки веб-ленты, админка показывает, как идёт загрузка. Переменные окружения `loader`:

| Переменная | По умолчанию | Что задаёт |
| --- | --- | --- |
| `DATABASE_URL` | — (обязательна) | адрес PostgreSQL |
| `TELEGRAM_CHANNEL` | — (обязательна) | имя канала без `@`, например `brieflyru` |
| `POLL_INTERVAL_SECS` | `600` | пауза между концом прохода и началом следующего, с |
| `REQUEST_DELAY_SECS` | `2` | пауза между ответом ленты и следующим запросом, с |
| `FLOOD_WAIT_SECS` | `60` | пауза при ответе `429`, с; `Retry-After` не читается. Пока загрузчик на `429` завершается, как и на других ошибках |

Загрузчик работает в одном экземпляре на БД: второй `loader` завершается с ошибкой «загрузчик уже запущен», не обращаясь к Telegram. По SIGTERM/SIGINT (Ctrl+C) загрузчик дописывает текущую страницу и выходит. Пока он завершается и на первой ошибке источника или БД; тогда его нужно перезапустить — он продолжит незавершённый проход с места остановки.

Адрес веб-админки задаёт `WEB_ADDR` (по умолчанию `127.0.0.1:3000`). Админка отвечает только на запросы с `Host` вида `localhost` или loopback-адреса (защита от DNS rebinding), поэтому открывать её нужно по такому адресу, например `http://localhost:3000`.

Логи пишутся в stdout; уровень задаёт `RUST_LOG` (по умолчанию `info`).

Проект собирается без живой БД по кэшу запросов из `.sqlx/`. После изменения SQL-запросов обновите кэш ([`sqlx-cli`](https://crates.io/crates/sqlx-cli)):

```sh
cargo sqlx prepare --workspace -- --all-targets
```

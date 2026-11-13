# Briefly Searcher

Briefly Searcher — поиск по каналу @brieflyru. План — [docs/scraper-plan.md](docs/scraper-plan.md), глоссарий — [CONTEXT.md](CONTEXT.md).

## Crates

- `crates/telegram` — доступ к Telegram: источник истории на веб-ленте `t.me/s` (записанные страницы для тестов — в `tests/fixtures/`);
- `crates/storage` — PostgreSQL: схема, миграции и все SQL-запросы;
- `crates/app` — бинарник `briefly-searcher` с командами и веб-админкой (`src/web`, шаблон `templates/admin.html`, вшитый htmx 2.0.11 в `assets/`).

## Разработка

Команды разработки описаны в `justfile`. Установите [just](https://just.systems/man/en/installation.html) и [heavy](https://github.com/mokevnin/heavy) (нужны и на CI, если он запускает эти команды):

```sh
cargo install just --locked
cargo install heavy --version 0.1.2 --locked
```

`just` показывает список команд. Сборка, проверка компиляции, тесты, Clippy и обновление SQL-кэша запускаются через `heavy`: все worktree и агенты на машине делят общий пул слотов. Число слотов задаётся через `HEAVY_SLOTS`; используйте одно значение во всех проектах, работающих с общим пулом. `heavy --status` показывает занятые слоты. Проверка форматирования идёт без очереди.

Конфигурация — только переменные окружения; локально их удобно держать в `.env`: бинарник и `just` подхватывают его при запуске, уже заданные переменные окружения важнее файла. `.env` в Git не попадает.

```sh
DATABASE_URL=postgres://postgres:postgres@localhost/briefly_searcher
TELEGRAM_CHANNEL=brieflyru
```

```sh
just build                   # собрать приложение через heavy
just check                   # проверить компиляцию всех targets
just test                    # тесты #[sqlx::test] создают временные БД через DATABASE_URL
just clippy                  # проверить код Clippy
just fmt                     # проверить форматирование
target/debug/briefly-searcher migrate  # применить миграции к БД из DATABASE_URL
target/debug/briefly-searcher loader   # загружать веб-ленту в raw_posts
target/debug/briefly-searcher web      # админка на http://127.0.0.1:3000
```

`loader` и `web` — отдельные долгоживущие процессы: загрузчик сохраняет блоки веб-ленты, админка показывает, как идёт загрузка. Они запускаются вне `heavy`, чтобы не занимать слот на всё время работы. Переменные окружения `loader`:

| Переменная | По умолчанию | Что задаёт |
| --- | --- | --- |
| `DATABASE_URL` | — (обязательна) | адрес PostgreSQL |
| `TELEGRAM_CHANNEL` | — (обязательна) | имя канала без `@`, например `brieflyru` |
| `POLL_INTERVAL_SECS` | `600` | пауза между концом прохода и началом следующего, с |
| `REQUEST_DELAY_SECS` | `2` | пауза между ответом ленты и следующим запросом, с |
| `FLOOD_WAIT_SECS` | `60` | пауза при ответе `429`, с; `Retry-After` не читается. Срок паузы пишется в БД, и до него загрузчик, в том числе перезапущенный, к ленте не обращается |

Загрузчик работает в одном экземпляре на БД: второй `loader` завершается с ошибкой «загрузчик уже запущен», не обращаясь к Telegram. По SIGTERM/SIGINT (Ctrl+C) загрузчик дописывает текущую страницу и выходит. Из-за ошибок источника или БД он не завершается: записывает ошибку (её видно в админке) и через минуту повторяет попытку, продолжая проход с места остановки. Запрос, на который лента не ответила за 30 с, тоже считается ошибкой.

Адрес веб-админки задаёт `WEB_ADDR` (по умолчанию `127.0.0.1:3000`). Админка отвечает только на запросы с `Host` вида `localhost` или loopback-адреса (защита от DNS rebinding), поэтому открывать её нужно по такому адресу, например `http://localhost:3000`.

Логи пишутся в stdout; уровень задаёт `RUST_LOG` (по умолчанию `info`).

Рецепты сборки, проверки компиляции, тестов и Clippy задают `SQLX_OFFLINE=true`: SQL-запросы проверяются по кэшу `.sqlx/` без подключения к БД. Для выполнения интеграционных тестов PostgreSQL всё равно нужна. После изменения SQL-запросов обновите кэш ([`sqlx-cli`](https://crates.io/crates/sqlx-cli), нужна живая БД):

```sh
just sqlx-prepare
```

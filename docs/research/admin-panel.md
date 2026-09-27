# Веб-админка (issue #6): UI-референсы и способ реализации

Предмет: команда `web` из [issue #6](https://github.com/t3m8ch/briefly-searcher/issues/6). Это локальная сводка загрузчика, доступная только на чтение: `GET /` отдаёт страницу, `GET /status` отдаёт HTML-фрагмент, HTMX хранится в репозитории. Поля сводки заданы в [плане, раздел «Веб-админка»](../scraper-plan.md#3-веб-админка). Способ построения спека уже зафиксировала: готовый HTML, без JSON API и без собственного JavaScript. Поэтому здесь не выбирается «SPA или сервер», а разбирается, как сделать это хорошо в стеке проекта и как должна выглядеть одна страница.

**Ответ:** `axum` 0.8 и шаблоны `askama`, где один шаблон содержит `{% block status %}`. `/` рендерит шаблон целиком, `/status` рендерит только этот блок. HTMX 2.0.11 вшит в бинарник через `include_str!`. Данные даёт один запрос хранилища с `now()` из БД. Страница одна: заголовок состояния, баннер паузы, блок ошибки, блок прохода и список «ключ — значение». Всё прочее, что обычно бывает в админках (таблицы, фильтры, навигация, действия), в часть 1 не входит.

Слова «**Вывод:**» отмечают собственные выводы, прямо в источниках не записанные.

## Что показывать: поля и их источники

| Что на экране | Откуда (схема [`20260927000000_loader_schema.sql`](../../crates/storage/migrations/20260927000000_loader_schema.sql)) |
| --- | --- |
| Heartbeat загрузчика | `worker_state.last_heartbeat_at` |
| Последняя ошибка, её время, следующая попытка | `worker_state.last_error`, `last_error_at`, `next_attempt_at` |
| Пауза Telegram | `ingestion_state.flood_wait_until` |
| Последний успешный запрос | `ingestion_state.last_successful_request_at` |
| История загружена, до какого ID | `ingestion_state.newest_fetched_id IS NOT NULL` и само значение |
| Идёт ли проход и до какого ID он дошёл | `min(message_id)` среди строк `raw_posts` новее `newest_fetched_id` (при `NULL` — среди всех строк) ([план, раздел 1](../scraper-plan.md#1-получение-сообщений)) |
| Число сохранённых сообщений | `count(*)` по `raw_posts` |

Термины [глоссария](../../CONTEXT.md): строка `raw_posts` — это **сообщение** Telegram, а не пост и не статья, потому что каждая фотография альбома — отдельное сообщение. Счётчик на странице поэтому должен называться «сообщений», а не «постов» или «статей».

Готового поля «состояние загрузчика» в схеме нет. **Вывод:** его нужно выводить в функции хранилища или в обработчике из полей выше, например так (порядок проверки важен):

1. `last_heartbeat_at IS NULL` — «загрузчик ещё не запускался»;
2. `now() - last_heartbeat_at` больше порога — «загрузчик не отвечает»;
3. `flood_wait_until > now()` — «пауза Telegram до …»;
4. `next_attempt_at > now()` и есть `last_error` — «ошибка, повтор в …»;
5. иначе — «работает».

Проверка heartbeat стоит раньше паузы и ошибки: иначе загрузчик, упавший во время `FLOOD_WAIT`, до конца паузы выглядел бы как «пауза Telegram до …», хотя процесса уже нет.

### Порог «загрузчик не отвечает» (решено)

Если heartbeat пишется только при работе со страницами, порог должен перекрывать самую долгую законную тишину: интервал опроса плюс `FLOOD_WAIT`, который Telegram может назначить на часы. С таким порогом мёртвый загрузчик заметят через часы, и вынос порога в окружение этого не исправит. Поэтому порог определяется тем, как загрузчик пишет heartbeat, а не настройкой.

Требования к загрузчику (перенести в его тикет):

- heartbeat пишется не реже раза в 15 с **в любом состоянии**, в том числе во время ожидания `flood_wait_until` и между проходами: ожидание — это цикл тиков, а не один долгий `sleep`, и каждый тик обновляет `last_heartbeat_at`;
- heartbeat пишет основной цикл загрузчика, а не отдельная фоновая задача: отдельная задача продолжала бы писать heartbeat при зависшем основном цикле;
- у вызова Telegram есть таймаут (например, 30 с): зависший запрос становится ошибкой в `last_error`, а не тишиной.

Тогда наибольший законный разрыв между heartbeat — таймаут запроса плюс интервал heartbeat (около 45 с), и порог — **константа в коде около 90 с**, а не переменная окружения:

- частота heartbeat — свойство кода загрузчика, а не развёртывания; в списке конфигурации ([план, фактор III](../scraper-plan.md#5-эксплуатация-12-factor)) порога нет;
- загрузчик и админка — один бинарник, поэтому интервал heartbeat и порог лежат рядом в одном модуле, и админка проверяет ровно то, что обещает загрузчик;
- отдельная переменная позволила бы задать порог меньше интервала heartbeat и получить ложное «не отвечает»;
- интервал опроса и длина `FLOOD_WAIT` на порог больше не влияют.

Heartbeat отвечает только на вопрос «процесс жив и его цикл крутится». Продвижение работы видно отдельно, по `last_successful_request_at` и `newest_fetched_id`.

Все сравнения лучше делать с `now()` из того же SQL-запроса: тогда веб-процесс и БД не расходятся по часам. Кроме того, в `READ COMMITTED` один оператор видит согласованный снимок данных на момент своего начала ([PostgreSQL: Read Committed](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED)). Значит, запрос с подзапросами к `ingestion_state`, `worker_state` и `raw_posts` не покажет, например, счётчик от одной страницы прохода, а `min(message_id)` — от другой. Набросок:

```sql
SELECT now() AS now,
       i.newest_fetched_id, i.flood_wait_until, i.last_successful_request_at,
       w.last_heartbeat_at, w.last_error, w.last_error_at, w.next_attempt_at,
       (SELECT count(*) FROM raw_posts) AS raw_posts_count,
       (SELECT min(message_id) FROM raw_posts
         WHERE i.newest_fetched_id IS NULL OR message_id > i.newest_fetched_id) AS pass_reached_id
FROM ingestion_state i CROSS JOIN worker_state w;
```

**Вывод:** при истории в десятки тысяч строк `count(*)` раз в несколько секунд не должен быть проблемой, но это не измерялось. Если окажется медленно, можно кэшировать или приблизить значение. Запрос живёт в crate хранилища и пишется через `query_as!`, как требует [план, раздел 4](../scraper-plan.md#4-границы-rust-проекта).

## UI-референсы (Mobbin)

Искал экраны статуса фоновых задач, прогресса импорта или бэкфилла, баннеров паузы, деталей интеграции со статусом и индикатора «последний раз видели». Сложных админских паттернов (таблицы с фильтрами, массовые действия, очереди модерации) не искал: в issue #6 одна запись состояния и нет изменяющих маршрутов.

1. **Customer.io — Workspace Performance** ([Mobbin](https://mobbin.com/screens/b9743841-5a29-4454-bc39-133d7b0d7d9c)). Слева крупная карточка общего состояния: иконка, слово «Healthy», одна поясняющая фраза («Everything is running smoothly. No issues detected.») и строка «Last updated today, 5:14:16 PM». Справа список подсистем с цветным статусом. **Что взять:** карточку-заголовок, где слово состояния крупно, одна фраза пояснения и **абсолютное время обновления сводки**. Этот штамп заодно даёт индикацию устаревания данных без JavaScript (см. ниже).
2. **Customer.io — детали интеграции** ([Mobbin](https://mobbin.com/screens/dac79f39-b619-4919-96cf-552530760314)). Панель «Details» — вертикальный список «ключ — значение»: Status с цветной точкой («No recent data»), «Data last received: -», «Created 3 minutes ago». Отсутствующее значение показано прочерком, а не пустым местом и не нулём. **Что взять:** форму основного блока сводки. Это `<dl>`, а не таблица и не плитки: полей около десяти, и одна запись.
3. **Braintrust — Brainstore backfill status** ([Mobbin](https://mobbin.com/screens/ed269b7f-be0a-45f9-8279-6226ab185f8a)). Экран состояния бэкфилла: крупное «100% complete», под ним «Last backfilled just now», зелёная метка «Live», затем «1 segments / 10 chunks / 2,693 rows». **Что взять:** отдельный блок «Загрузка истории» с крупной главной строкой и строкой абсолютных счётчиков под ней. **Чего не брать:** процент. Для нас его не на чем считать: «43 000+» знаменателем использовать нельзя ([план, раздел 3](../scraper-plan.md#3-веб-админка)), а ID идут с пропусками ([раздел 1](../scraper-plan.md#1-получение-сообщений)). Вместо процента — «Первый проход: дошёл до ID N (сохранено M сообщений)», после завершения — «История загружена, `newest_fetched_id` = N».
4. **Stripe — «Payouts paused until requirements are met»** ([Mobbin](https://mobbin.com/screens/5102fc01-2b89-4170-93da-ef0e5d824dc1)). Во всю ширину над содержимым — красный баннер с иконкой, жирным заголовком о паузе и строкой причины. **Что взять:** баннер паузы Telegram над сводкой: «Пауза Telegram до 14:32:10 (FLOOD_WAIT)». Он виден только при `flood_wait_until > now()`. Кнопки действия из референса не нужны: изменяющих маршрутов нет.
5. **OpenAI Platform — батч в статусе Failed** ([Mobbin](https://mobbin.com/screens/012bdbf4-f102-496c-95bd-81f4e4fec126)). Детальная панель: бейдж «Failed», поле «Errors» с текстом ошибки моноширинным шрифтом, «Created at», ниже хронология «Batch created → Batch failed» со временем. **Что взять:** блок последней ошибки — текст в `<pre>`/`<code>` (ошибки загрузчика технические), время ошибки и «следующая попытка в …». Это ровно критерий issue: «её текст, время и время следующей попытки».
6. **Supabase — обзор проекта** ([Mobbin](https://mobbin.com/screens/2e0ffd2d-c588-4513-bd84-d3effa2e31ed)). Ряд мини-плиток «STATUS: Healthy», «LAST BACKUP: No backups», «RECENT BRANCH: No branches». Пустое состояние записано словами прямо в плитке, а не отдельной страницей-заглушкой. **Что взять:** для `NULL`-полей писать смысл («ещё не было», «не завершён»), а не «NULL» и не пустую строку. Особенно для свежей БД, где загрузчик ещё не запускался.

Дополнительно:
- как **не** надо: Literal показывает «0 out of 0 books imported» до того, как знаменатель известен ([Mobbin](https://mobbin.com/screens/d42b996a-a56d-48a0-97da-76bec5b44a3b));
- HubSpot ставит «--» для ещё не посчитанных счётчиков импорта ([Mobbin](https://mobbin.com/screens/36aa37c2-a52f-4303-a648-31245b8b7666));
- Deel держит под заголовком компактные «Connected» и «Synced: Dec 17th, 2025 06:22 PM», а проблему выносит отдельным баннером ([Mobbin](https://mobbin.com/screens/7b7a9f07-cad9-4c17-807a-d9e2df6f45ac)).

Что намеренно не переносится: боковая навигация, поиск, кнопки «Refresh» / «Manual sync» / «Optimize» и таблицы с фильтрами из тех же экранов. **Вывод:** в части 1 страница одна и только читает данные. Навигация понадобится, когда в части 2 появятся поиск и действия ([план, раздел 3](../scraper-plan.md#3-веб-админка)).

### Устаревшие данные без JavaScript

Критерий issue: «если сервер не ответил или вернул ошибку, на экране остаются последние данные». **Вывод:** последние данные на экране сами по себе вводят в заблуждение, если не видно, что они старые. Без собственного JS помогают две вещи:
- во фрагменте рендерится «Сводка на 14:30:05» (время `now()` из запроса). Если опрос перестал проходить, штамп перестаёт меняться (как в референсе 1);
- все времена показываются абсолютными («heartbeat 14:30:01»), а не «3 с назад»: относительное время, отрендеренное сервером, при сбое опроса застынет и будет врать. Относительную подсказку можно ставить рядом только в дополнение.

## Реализация

### Варианты

| Вариант | Соответствие issue #6 |
| --- | --- |
| **axum + askama + HTMX** | Подходит. Рекомендуется (ниже). |
| axum + maud + HTMX | Подходит. Равноценная альтернатива askama. |
| axum + minijinja + HTMX | Подходит, но шаблоны разбираются во время выполнения. |
| SPA (например, React-admin) | Не подходит. React-admin — фреймворк для SPA «on top of REST/GraphQL APIs», работает через *Data Providers* к API ([README](https://github.com/marmelab/react-admin/blob/master/README.md)). Спека запрещает JSON API и собственный JS. |
| Внешний DB-клиент к PostgreSQL (pgAdmin, Adminer и т. п.) | Не подходит. **Вывод:** универсальный клиент показывает любые таблицы, включая `telegram_session`, а её нельзя выводить в админке ([план, раздел 2](../scraper-plan.md#2-хранилище)). К тому же это второй процесс со своей конфигурацией вне бинарника (ADR-0002) и без производных полей вроде «идёт ли проход». |

Rust-crates «готовых админок» не рассматривались: в спеке одна страница из фиксированных полей, генерировать CRUD не над чем.

### Веб-сервер: axum 0.8

- Текущая версия — `axum` 0.8.9 ([crates.io API](https://crates.io/api/v1/crates/axum), проверено 2026-09-27; репозиторий [tokio-rs/axum](https://github.com/tokio-rs/axum)). Проект уже на tokio, sqlx использует `runtime-tokio` ([Cargo.toml](../../Cargo.toml)).
- **Корректное завершение** (критерий «по SIGTERM/SIGINT дорабатывает текущие запросы»): `axum::serve(listener, app).with_graceful_shutdown(signal)`. Официальный пример ждёт `tokio::signal::ctrl_c()` или `signal::unix::signal(SignalKind::terminate())` через `tokio::select!`; комментарий там: «Graceful shutdown will wait for outstanding requests to complete. Add a timeout so requests don't hang forever», и для этого ставится `tower_http::timeout::TimeoutLayer` ([examples/graceful-shutdown](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/examples/graceful-shutdown/src/main.rs); [`Serve::with_graceful_shutdown`](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/axum/src/serve/mod.rs)).
- **Features tokio:** сейчас в workspace включены только `macros` и `rt-multi-thread` ([Cargo.toml](../../Cargo.toml)). Для `tokio::signal` нужна feature `signal`, для `tokio::net::TcpListener` — `net` ([tokio/src/lib.rs](https://github.com/tokio-rs/tokio/blob/master/tokio/src/lib.rs)). Feature `tokio` у axum сама включает `tokio/net` ([axum/Cargo.toml](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/axum/Cargo.toml)), так что добавить нужно в первую очередь `signal`.
- В default features axum есть `json` ([axum/Cargo.toml](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/axum/Cargo.toml)). **Вывод:** выключать их не обязательно: запрет JSON API касается маршрутов, а не зависимостей.
- **Тесты** (критерий «HTTP-запросы к роутеру над тестовой БД»): официальный пример вызывает `Router` без сети через `tower::ServiceExt::oneshot(Request)`, а тело читает `http_body_util::BodyExt` ([examples/testing](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/examples/testing/src/main.rs); dev-зависимости `tower` с feature `util` и `http-body-util` — в [examples/templates/Cargo.toml](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/examples/templates/Cargo.toml)). **Вывод:** в связке с `#[sqlx::test]` тест получает `PgPool`, заполняет `ingestion_state`/`worker_state` через `UPDATE`, строит роутер через `Storage::from_pool` и проверяет HTML ответа `/status`. Так и предлагает issue: состояние готовится прямо в БД.

### Шаблоны: askama (рекомендуется) или maud

- **askama** 0.16.1 ([crates.io API](https://crates.io/api/v1/crates/askama), выпуск 2026-09-04; [askama-rs/askama](https://github.com/askama-rs/askama), MIT OR Apache-2.0). Jinja-подобный синтаксис; «generates type-safe Rust code from your templates at compile time» ([README](https://github.com/askama-rs/askama/blob/main/README.md)). Шаблоны ищутся в `templates/` относительно корня crate ([book: configuration](https://github.com/askama-rs/askama/blob/main/book/src/configuration.md)). HTML экранируется по умолчанию для расширений `html`, `htm`, `xml`, `j2`, `jinja`, `jinja2` (там же, раздел «Escapers»).
- **Главное для issue:** атрибут `#[template(path = "...", block = "status")]` рендерит только один блок шаблона: «useful when you need to decompose your template for partial rendering, without needing to extract the partial into a separate template». Вариант `blocks = [...]` генерирует методы `as_<block>()` ([book: creating templates](https://github.com/askama-rs/askama/blob/main/book/src/creating_templates.md); [template syntax](https://github.com/askama-rs/askama/blob/main/book/src/template_syntax.md)). **Вывод:** `/` и `/status` рендерят один и тот же файл `admin.html` из одной структуры сводки, поэтому разметка фрагмента в них одинакова. Это прямо ложится на требование issue «данные обоих маршрутов даёт одна функция».
- Интеграция с axum: крейты `askama_axum` удалены («The integration crates were removed… use `template.render()`», [book: upgrading](https://github.com/askama-rs/askama/blob/main/book/src/upgrading.md)). Есть два пути: свой `IntoResponse` (как в [axum examples/templates](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/examples/templates/src/main.rs), где пример закреплён на `askama = "0.12"`) или `#[derive(askama_web::WebTemplate)]` с feature `axum-0.8` ([askama_web README](https://github.com/askama-rs/askama_web/blob/main/README.md); `askama_web` 0.16.0 по [crates.io API](https://crates.io/api/v1/crates/askama_web)). Книга askama рекомендует свой тип ошибки, а `askama_web` — «if you don't need custom / stylized error messages» ([book: frameworks](https://github.com/askama-rs/askama/blob/main/book/src/frameworks.md)). **Вывод:** для `/status` нужен свой ответ 5xx при ошибке хранилища (см. HTMX ниже), поэтому хватит маленького собственного `IntoResponse` без лишней зависимости.
- **maud** 0.27.0 ([crates.io API](https://crates.io/api/v1/crates/maud), выпуск 2025-02-02; последний коммит в [lambda-fairy/maud](https://github.com/lambda-fairy/maud) — 2026-05-25). Макрос `html!` компилирует разметку в Rust-код, опечатки ловятся при компиляции ([docs/index](https://github.com/lambda-fairy/maud/blob/main/docs/content/index.md)). «Any HTML special characters are escaped by default» ([splices-toggles](https://github.com/lambda-fairy/maud/blob/main/docs/content/splices-toggles.md)). Feature `axum` даёт `IntoResponse` для `Markup` ([web-frameworks](https://github.com/lambda-fairy/maud/blob/main/docs/content/web-frameworks.md)) и зависит от `axum-core` 0.5 ([maud/Cargo.toml](https://github.com/lambda-fairy/maud/blob/main/maud/Cargo.toml)) — ту же версию использует axum 0.8.9 ([axum/Cargo.toml](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/axum/Cargo.toml)). Фрагмент здесь — просто функция `fn status(&Summary) -> Markup`, которую вызывают оба обработчика. **Вывод:** по возможностям равноценен askama. Выбор — дело вкуса: HTML в отдельных файлах (askama) или разметка в Rust (maud). Выпуски maud выходят реже.
- **minijinja** 2.24.0 ([crates.io API](https://crates.io/api/v1/crates/minijinja)) экранирует по расширению `html`/`htm`/`xml` ([defaults.rs](https://github.com/mitsuhiko/minijinja/blob/main/minijinja/src/defaults.rs)). Есть пример для axum ([examples/templates-minijinja](https://github.com/tokio-rs/axum/tree/axum-v0.8.9/examples/templates-minijinja)). Это движок времени выполнения: ошибки шаблона всплывают при рендере. **Вывод:** проект проверяет SQL при компиляции (`query!`), и шаблоны с проверкой при компиляции лучше соответствуют этому подходу.

Экранирование важно: текст `last_error` приходит от Telegram или из ошибок библиотек. Документация HTMX прямо требует «Escape All User Content» и предупреждает, что внедрённый HTML с атрибутами `hx-*` становится исполняемым поведением ([htmx docs, Security](https://github.com/bigskysoftware/htmx/blob/master/www/content/docs.md)). Поэтому никаких `|safe` / `PreEscaped` для полей сводки.

### HTMX: версия, вшивание, поведение при ошибках

- **Версия.** На npm `latest` = 2.0.11 (2026-09-22), `next` = 4.0.0 (2026-08-28); лицензия 0BSD ([npm registry](https://registry.npmjs.org/htmx.org); [LICENSE](https://github.com/bigskysoftware/htmx/blob/master/LICENSE)). Лицензия позволяет положить `htmx.min.js` в репозиторий. Документация 2.x описывает именно этот способ («Download a copy… add it to the appropriate directory in your project») и советует подумать о «not using CDNs in production» ([docs.md, Installing](https://github.com/bigskysoftware/htmx/blob/master/www/content/docs.md)).
- **Опрос.** `hx-trigger="every 5s"` — «Every 2 seconds, issue a GET to /news and load the response into the div» в примере документации 2.x ([docs.md, Polling](https://github.com/bigskysoftware/htmx/blob/master/www/content/docs.md)). В htmx 4 синтаксис `every <time>` сохранён ([patterns/04-polling.md, ветка `four`](https://github.com/bigskysoftware/htmx/blob/four/www/src/content/patterns/04-polling.md)).
- **Критерий «при ошибке остаются последние данные» зависит от версии HTMX.**
  - htmx 2: по умолчанию `{code:"[45]..", swap: false, error:true}` — ответы 4xx/5xx не вставляются в страницу. При обрыве соединения срабатывает `htmx:sendError`, и вставлять тоже нечего ([docs.md, Response Handling](https://github.com/bigskysoftware/htmx/blob/master/www/content/docs.md)). Значит, достаточно, чтобы `/status` при ошибке хранилища отвечал 5xx, а не 200 с текстом ошибки.
  - htmx 4: «htmx 4 swaps all HTTP responses. Only 204 and 304 do not swap… htmx 2 did not swap 4xx and 5xx responses». Прежнее поведение возвращается через `htmx.config.noSwap = [204, 304, '4xx', '5xx']` или атрибутом `hx-status:5xx="swap:none"` ([whats-new-in-htmx-4.md, ветка `four`](https://github.com/bigskysoftware/htmx/blob/four/www/src/content/docs/whats-new-in-htmx-4.md)).
  - **Вывод:** взять htmx 2.0.11 (тег `latest`) и отвечать 5xx. Если выбрать 4.x, в разметку нужно добавить `hx-status:5xx="swap:none"` и держать эту деталь в тесте разметки `/`. В обоих случаях поведение браузера автоматически не тестируется (так в issue), поэтому выбранную версию и атрибут стоит записать комментарием в шаблоне.
- **Разметка.** **Вывод:** атрибуты опроса ставятся на внешний элемент в `/`: `<section id="status" hx-get="/status" hx-trigger="every 5s" hx-swap="innerHTML">`. `/status` отдаёт только внутреннее содержимое. Тогда фрагмент не несёт `hx-*` и не зависит от различий 2.x/4.x в `outerHTML`/`outerMorph`, а при первой загрузке `/` блок уже заполнен тем же `{% block status %}`.
- **Отдача файла.** **Вывод:** `include_str!("…/htmx.min.js")` и маршрут вида `GET /htmx.min.js` с `Content-Type: text/javascript`. Файл попадает в бинарник, и релиз остаётся «бинарник + окружение», без каталога статики рядом ([ADR-0002](../adr/0002-twelve-factor-app.md); [план, раздел 5, фактор V](../scraper-plan.md#5-эксплуатация-12-factor)). CSS так же можно вшить через `<style>` в шаблоне.
- В 2.x `htmx.config.selfRequestsOnly` по умолчанию `true`: запросы только к своему домену ([docs.md, Configuration](https://github.com/bigskysoftware/htmx/blob/master/www/content/docs.md)).

### Доступ и безопасность

- Аутентификации в спеке нет. Защита — привязка к loopback по умолчанию и адрес из окружения (issue #6, [план, фактор VII](../scraper-plan.md#5-эксплуатация-12-factor)). Конфигурацию можно описать как в [`config.rs`](../../crates/app/src/config.rs): `envconfig` поддерживает `default = "…"` ([envconfig README](https://github.com/greyblake/envconfig-rs/blob/master/README.md)). Например, `WEB_ADDR` со значением по умолчанию `127.0.0.1:<порт>` (имя и порт — открытый вопрос).
- **DNS rebinding.** По NCC Group, HTTP-сервер без HTTPS, без аутентификации и без проверки `Host` уязвим для DNS rebinding. Для сервиса на loopback допустимые `Host` — только `localhost` и loopback-адреса с портом, например `127.0.0.1:3000` и `localhost:3000` ([NCC Group Singularity: Preventing DNS Rebinding Attacks](https://github.com/nccgroup/singularity/wiki/Preventing-DNS-Rebinding-Attacks)). План требует проверку `Host`/`Origin` только для будущих `POST` части 2 ([план, раздел 3](../scraper-plan.md#3-веб-админка)). **Вывод:** в части 1 чужой сайт через rebinding смог бы прочитать только сводку, где секретов нет. Проверка `Host` — дешёвый middleware; его можно заложить сразу, чтобы в части 2 не забыть.
- Секреты: сводка по построению не читает `telegram_session`, а `last_error` не должен содержать секретов — это требование к загрузчику ([план, `worker_state`](../scraper-plan.md#2-хранилище)). **Вывод:** тест «на странице нет содержимого `telegram_session`» полезен как регрессионный: вставить в таблицу известную строку и проверить, что её нет в ответах `/` и `/status`.

## Рекомендация и MVP-экран

**Подход:** подкоманда `web` в `crates/app` (модуль `web`, шаблоны в `crates/app/templates/`) → `axum` 0.8 + `askama` с `block` + вшитый htmx 2.0.11. Одна функция хранилища `admin_summary()` → структура, где все поля — `Option<…>`, плюс `now`. `/status` при ошибке хранилища отвечает 5xx. Завершение — `with_graceful_shutdown` по SIGINT/SIGTERM. Тесты — `#[sqlx::test]` + `oneshot`.

Одна страница, сверху вниз:

| Блок | Показывается | Референс |
| --- | --- | --- |
| Заголовок состояния: слово («Работает» / «Пауза Telegram» / «Ошибка, повтор в …» / «Не отвечает» / «Ещё не запускался»), одна фраза и «Сводка на ЧЧ:ММ:СС» | всегда | Customer.io Workspace Performance ([1](https://mobbin.com/screens/b9743841-5a29-4454-bc39-133d7b0d7d9c)) |
| Баннер паузы: «Пауза Telegram до …» | `flood_wait_until > now()` | Stripe ([4](https://mobbin.com/screens/5102fc01-2b89-4170-93da-ef0e5d824dc1)) |
| Последняя ошибка: текст в `<pre>`, время, «следующая попытка в …» | `last_error` не `NULL` | OpenAI Platform ([5](https://mobbin.com/screens/012bdbf4-f102-496c-95bd-81f4e4fec126)) |
| Загрузка истории: «Первый проход: дошёл до ID N» / «Проход: дошёл до ID N» / «История загружена, `newest_fetched_id` = N»; под этим «Сохранено M сообщений», без процента | всегда | Braintrust ([3](https://mobbin.com/screens/ed269b7f-be0a-45f9-8279-6226ab185f8a)); анти-пример Literal ([↗](https://mobbin.com/screens/d42b996a-a56d-48a0-97da-76bec5b44a3b)) |
| Детали `<dl>`: heartbeat, последний успешный запрос, `newest_fetched_id`, сообщений в `raw_posts`; пустые значения — словами | всегда | Customer.io details ([2](https://mobbin.com/screens/dac79f39-b619-4919-96cf-552530760314)), Supabase ([6](https://mobbin.com/screens/2e0ffd2d-c588-4513-bd84-d3effa2e31ed)) |

## Открытые вопросы

1. **Как показывать ошибку, которая уже прошла.** Если `last_successful_request_at > last_error_at`, ошибку показывать приглушённо («была в …») или не показывать совсем? Критерий issue требует показывать её «после ошибки», но не говорит, как долго.
2. **Часовой пояс.** JS нет, поэтому пояс выбирает сервер. Показывать UTC с явной пометкой или брать пояс из окружения?
3. **Имя переменной и порт** адреса веб-сервера (`WEB_ADDR`?) и интервал опроса UI (5 с?). Отдельная переменная для интервала вряд ли нужна.
4. **htmx 2.0.11 или 4.0.0?** 4.x уже вышла под тегом `next`, но меняет обработку ошибок (см. выше). Рекомендация — 2.x.
5. **Проверка `Host` сразу в #6** или, как в плане, только вместе с `POST`-действиями части 2?
6. **askama или maud:** шаблоны в `.html`-файлах или разметка в Rust-коде.

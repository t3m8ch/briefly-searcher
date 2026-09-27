-- Схема части 1 (загрузчик): см. docs/scraper-plan.md, раздел «Хранилище».

-- По одной строке на каждое сообщение Telegram целевого канала.
-- payload после вставки не переписывается (ON CONFLICT DO NOTHING).
CREATE TABLE raw_posts (
    message_id     BIGINT      PRIMARY KEY,
    payload        JSONB       NOT NULL,
    payload_schema TEXT        NOT NULL,
    fetched_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Единственная запись: singleton-ключ допускает только значение TRUE.
CREATE TABLE ingestion_state (
    singleton                  BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    -- Наибольший message_id, до которого все сообщения канала сохранены;
    -- NULL, пока первый проход не завершён.
    newest_fetched_id          BIGINT      NULL,
    -- Срок ожидания после ошибки Telegram FLOOD_WAIT_X.
    flood_wait_until           TIMESTAMPTZ NULL,
    last_successful_request_at TIMESTAMPTZ NULL
);

INSERT INTO ingestion_state DEFAULT VALUES;

-- Единственная запись с сериализованной сессией grammers. Секрет.
-- Строки нет, пока не выполнена команда login.
CREATE TABLE telegram_session (
    singleton  BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    session    BYTEA       NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Единственная запись о состоянии загрузчика для веб-админки.
CREATE TABLE worker_state (
    singleton         BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    last_heartbeat_at TIMESTAMPTZ NULL,
    last_error        TEXT        NULL,
    last_error_at     TIMESTAMPTZ NULL,
    next_attempt_at   TIMESTAMPTZ NULL
);

INSERT INTO worker_state DEFAULT VALUES;

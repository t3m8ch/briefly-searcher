-- Производные данные полного пересчёта (ADR-0004).
CREATE TABLE articles (
    id BIGINT PRIMARY KEY REFERENCES raw_posts(message_id),
    title TEXT NULL,
    search_text TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('single', 'complete', 'incomplete')),
    declared_parts INT NULL CHECK (declared_parts > 0),
    published_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE posts (
    message_id BIGINT PRIMARY KEY REFERENCES raw_posts(message_id),
    article_id BIGINT NOT NULL REFERENCES articles(id),
    part_no INT NULL CHECK (part_no > 0),
    text TEXT NOT NULL,
    published_at TIMESTAMPTZ NOT NULL,
    forwarded_from TEXT NULL
);
CREATE TABLE normalizer_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    normalized_up_to BIGINT NULL,
    last_recalculation_started_at TIMESTAMPTZ NULL,
    last_recalculation_duration_ms BIGINT NULL CHECK (last_recalculation_duration_ms >= 0),
    last_heartbeat_at TIMESTAMPTZ NULL,
    last_error TEXT NULL,
    last_error_at TIMESTAMPTZ NULL
);
INSERT INTO normalizer_state DEFAULT VALUES;

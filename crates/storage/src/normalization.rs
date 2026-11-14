//! Чтение среза блоков и атомарная замена производных данных.

use std::time::Instant;

use briefly_searcher_normalizer::{Article, ArticleStatus, Normalized, Post};
use briefly_searcher_telegram::Message;
use chrono::{DateTime, Utc};
use sqlx::{Connection, PgConnection, Postgres, Transaction};

use crate::{Error, Storage};

const NORMALIZER_LOCK_KEY: i64 = i64::from_be_bytes(*b"normaliz");

/// Session-level блокировка на выделенном соединении.
pub struct NormalizerLock {
    connection: PgConnection,
}

impl NormalizerLock {
    pub async fn release(self) -> Result<(), Error> {
        // Закрытие сессии снимает все её advisory-блокировки.
        self.connection.close().await?;
        Ok(())
    }
}

/// Состояние последнего успешно опубликованного пересчёта и диагностика процесса.
#[derive(Debug, sqlx::FromRow)]
pub struct NormalizerState {
    pub normalized_up_to: Option<i64>,
    pub last_recalculation_started_at: Option<DateTime<Utc>>,
    pub last_recalculation_duration_ms: Option<i64>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
}

impl Storage {
    pub async fn try_lock_normalizer(&self) -> Result<Option<NormalizerLock>, Error> {
        let mut connection = PgConnection::connect_with(&self.pool.connect_options())
            .await
            .map_err(Error::Connect)?;
        let locked = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_lock($1) AS "locked!""#,
            NORMALIZER_LOCK_KEY
        )
        .fetch_one(&mut connection)
        .await?;
        if locked {
            Ok(Some(NormalizerLock { connection }))
        } else {
            connection.close().await?;
            Ok(None)
        }
    }

    pub async fn newest_fetched_id(&self) -> Result<Option<i64>, Error> {
        Ok(
            sqlx::query_scalar!("SELECT newest_fetched_id FROM ingestion_state")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Граница передаётся явно: загруженный позже блок не попадёт в пересчёт.
    /// Сравнение с NULL не выбирает ни одной строки.
    pub async fn normalization_input(&self, boundary: Option<i64>) -> Result<Vec<Message>, Error> {
        Ok(sqlx::query_as!(Message, "SELECT message_id AS id, payload, payload_schema FROM raw_posts WHERE message_id <= $1 ORDER BY message_id", boundary)
            .fetch_all(&self.pool).await?)
    }

    pub async fn normalizer_state(&self) -> Result<NormalizerState, Error> {
        Ok(sqlx::query_as!(NormalizerState, "SELECT normalized_up_to, last_recalculation_started_at, last_recalculation_duration_ms, last_heartbeat_at, last_error, last_error_at FROM normalizer_state")
            .fetch_one(&self.pool).await?)
    }

    pub async fn record_normalizer_heartbeat(&self, at: DateTime<Utc>) -> Result<(), Error> {
        sqlx::query!("UPDATE normalizer_state SET last_heartbeat_at = $1", at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Текст ошибки должен быть безопасной диагностикой, без payload и секретов.
    pub async fn record_normalizer_error(
        &self,
        text: &str,
        at: DateTime<Utc>,
    ) -> Result<(), Error> {
        sqlx::query!(
            "UPDATE normalizer_state SET last_error = $1, last_error_at = $2",
            text,
            at
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Обе таблицы и граница публикуются одной транзакцией. При любой ошибке
    /// (включая FK/дубликат ID) прежний результат остаётся целиком.
    pub async fn replace_normalized(
        &self,
        result: &Normalized,
        boundary: Option<i64>,
        started_at: DateTime<Utc>,
        duration_ms: i64,
    ) -> Result<(), Error> {
        let saving_started = Instant::now();
        let mut tx = self.pool.begin().await?;

        sqlx::query!("TRUNCATE posts, articles")
            .execute(&mut *tx)
            .await?;
        insert_articles(&mut tx, &result.articles).await?;
        insert_posts(&mut tx, &result.posts).await?;

        let saving_duration_ms =
            i64::try_from(saving_started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let duration_ms = duration_ms.saturating_add(saving_duration_ms);

        sqlx::query!(
            r#"
            UPDATE normalizer_state
            SET normalized_up_to = $1,
                last_recalculation_started_at = $2,
                last_recalculation_duration_ms = $3
            "#,
            boundary,
            started_at,
            duration_ms,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

async fn insert_articles(
    tx: &mut Transaction<'_, Postgres>,
    articles: &[Article],
) -> Result<(), Error> {
    let ids: Vec<_> = articles.iter().map(|a| a.id).collect();
    let titles: Vec<_> = articles.iter().map(|a| a.title.as_deref()).collect();
    let texts: Vec<_> = articles.iter().map(|a| a.search_text.as_str()).collect();
    let statuses: Vec<_> = articles
        .iter()
        .map(|a| match a.status {
            ArticleStatus::Single => "single",
            ArticleStatus::Complete => "complete",
            ArticleStatus::Incomplete => "incomplete",
        })
        .collect();
    let totals: Vec<Option<i32>> = articles
        .iter()
        .map(|a| a.declared_parts.map(i32::try_from).transpose())
        .collect::<Result<_, _>>()?;
    let dates: Vec<_> = articles.iter().map(|a| a.published_at).collect();
    sqlx::query!(
        r#"
        INSERT INTO articles (id, title, search_text, status, declared_parts, published_at)
        SELECT * FROM UNNEST(
            $1::bigint[], $2::text[], $3::text[],
            $4::text[], $5::int[], $6::timestamptz[]
        )
        "#,
        &ids,
        &titles as &[Option<&str>],
        &texts as &[&str],
        &statuses as &[&str],
        &totals as &[Option<i32>],
        &dates,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_posts(tx: &mut Transaction<'_, Postgres>, posts: &[Post]) -> Result<(), Error> {
    let ids: Vec<_> = posts.iter().map(|p| p.message_id).collect();
    let article_ids: Vec<_> = posts.iter().map(|p| p.article_id).collect();
    let parts: Vec<Option<i32>> = posts
        .iter()
        .map(|p| p.part_no.map(i32::try_from).transpose())
        .collect::<Result<_, _>>()?;
    let texts: Vec<_> = posts.iter().map(|p| p.text.as_str()).collect();
    let dates: Vec<_> = posts.iter().map(|p| p.published_at).collect();
    let forwards: Vec<_> = posts.iter().map(|p| p.forwarded_from.as_deref()).collect();
    sqlx::query!(
        r#"
        INSERT INTO posts (message_id, article_id, part_no, text, published_at, forwarded_from)
        SELECT * FROM UNNEST(
            $1::bigint[], $2::bigint[], $3::int[],
            $4::text[], $5::timestamptz[], $6::text[]
        )
        "#,
        &ids,
        &article_ids,
        &parts as &[Option<i32>],
        &texts as &[&str],
        &dates,
        &forwards as &[Option<&str>],
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

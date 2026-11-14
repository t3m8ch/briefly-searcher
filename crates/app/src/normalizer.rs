//! Полный пересчёт при старте и при росте границы завершённой загрузки.

use std::{future::Future, pin::pin, time::Instant};

use briefly_searcher_normalizer::normalize;
use briefly_searcher_storage::{NormalizerLock, Storage};
use briefly_searcher_telegram::parse_message;
use chrono::TimeDelta;

use crate::clock::Clock;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("нормализатор уже запущен: advisory-блокировку держит другой экземпляр")]
    AlreadyRunning,
    #[error(transparent)]
    Storage(#[from] briefly_searcher_storage::Error),
    #[error(transparent)]
    Parse(#[from] briefly_searcher_telegram::ParseError),
}

pub async fn lock(storage: &Storage) -> Result<NormalizerLock, Error> {
    storage
        .try_lock_normalizer()
        .await?
        .ok_or(Error::AlreadyRunning)
}

pub struct Normalizer<C> {
    storage: Storage,
    clock: C,
    poll_interval: TimeDelta,
}

impl<C: Clock> Normalizer<C> {
    pub fn new(storage: Storage, clock: C, poll_interval: TimeDelta) -> Self {
        assert!(
            poll_interval > TimeDelta::zero(),
            "интервал опроса должен быть положительным"
        );
        Self {
            storage,
            clock,
            poll_interval,
        }
    }

    /// Держит блокировку весь срок работы. Сигнал прерывает ожидание или
    /// текущую асинхронную операцию; незавершённая транзакция откатывается.
    pub async fn run(
        &self,
        lock: NormalizerLock,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        let mut shutdown = pin!(shutdown);
        let mut force = true;
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                result = self.poll(force) => match result {
                    Ok(_) => force = false,
                    Err(error) => tracing::error!(%error, "пересчёт прерван, повтор на следующем опросе"),
                }
            }
            let deadline = self.clock.now() + self.poll_interval;
            let stopped = loop {
                let now = self.clock.now();
                if now >= deadline {
                    break false;
                }
                tokio::select! {
                    biased;
                    () = &mut shutdown => break true,
                    () = self.clock.sleep_until(deadline.min(now + TimeDelta::seconds(15))) => {}
                }
                tokio::select! {
                    biased;
                    () = &mut shutdown => break true,
                    result = self.storage.record_normalizer_heartbeat(self.clock.now()) => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "не удалось записать heartbeat нормализатора");
                        }
                    }
                }
            };
            if stopped {
                break;
            }
        }
        lock.release().await?;
        Ok(())
    }

    /// Один опрос; `force` применяет правила независимо от сохранённой границы.
    /// Ошибка записывается отдельно от транзакции публикации результата.
    async fn poll(&self, force: bool) -> Result<bool, Error> {
        let result = self.recalculate_if_needed(force).await;
        if let Err(error) = &result {
            // Подробности ошибок БД могут содержать значения строк. В состоянии
            // оставляем безопасное описание, а не payload или текст публикации.
            let text = match error {
                Error::Storage(_) => "ошибка хранилища при пересчёте".to_owned(),
                _ => error.to_string(),
            };
            if let Err(error) = self
                .storage
                .record_normalizer_error(&text, self.clock.now())
                .await
            {
                tracing::error!(%error, "не удалось записать ошибку нормализатора");
            }
        }
        result
    }

    async fn recalculate_if_needed(&self, force: bool) -> Result<bool, Error> {
        self.storage
            .record_normalizer_heartbeat(self.clock.now())
            .await?;
        let boundary = self.storage.newest_fetched_id().await?;
        let state = self.storage.normalizer_state().await?;
        if !force && boundary <= state.normalized_up_to {
            return Ok(false);
        }
        let started_at = self.clock.now();
        let started = Instant::now();
        let input = self.storage.normalization_input(boundary).await?;
        let messages = input
            .iter()
            .map(parse_message)
            .collect::<Result<Vec<_>, _>>()?;
        let normalized = normalize(&messages);
        self.storage
            .replace_normalized(
                &normalized,
                boundary,
                started_at,
                started.elapsed().as_millis().min(i64::MAX as u128) as i64,
            )
            .await?;
        tracing::info!(
            ?boundary,
            posts = normalized.posts.len(),
            articles = normalized.articles.len(),
            "пересчёт завершён"
        );
        Ok(true)
    }
}

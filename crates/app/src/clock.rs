//! Часы загрузчика: текущее время и ожидание до момента.
//!
//! Все моменты времени, которые загрузчик записывает в БД, он берёт отсюда,
//! а не из SQL-функции `now()`; тесты подставляют поддельные часы, которые
//! не спят, а переводят время.

use std::future::Future;

use chrono::{DateTime, Utc};

pub trait Clock {
    /// Текущее время.
    fn now(&self) -> DateTime<Utc>;

    /// Ждёт, пока наступит `deadline`; если момент уже прошёл, возвращается сразу.
    fn sleep_until(&self, deadline: DateTime<Utc>) -> impl Future<Output = ()> + Send;
}

/// Системные часы и таймер tokio.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    async fn sleep_until(&self, deadline: DateTime<Utc>) {
        if let Ok(duration) = (deadline - Utc::now()).to_std() {
            tokio::time::sleep(duration).await;
        }
    }
}

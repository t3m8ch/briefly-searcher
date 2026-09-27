//! Конфигурация только из переменных окружения (12-factor, фактор III).

use std::net::SocketAddr;

use envconfig::Envconfig;

/// Подключение к PostgreSQL — общая часть конфигурации всех команд.
#[derive(Debug, Envconfig)]
pub struct DatabaseConfig {
    #[envconfig(from = "DATABASE_URL")]
    pub database_url: String,
}

/// Конфигурация команды `web`.
#[derive(Debug, Envconfig)]
pub struct WebConfig {
    #[envconfig(nested)]
    pub database: DatabaseConfig,
    /// Адрес, который слушает веб-админка; по умолчанию только loopback.
    #[envconfig(from = "WEB_ADDR", default = "127.0.0.1:3000")]
    pub addr: SocketAddr,
}

/// Конфигурация команды `loader`.
#[derive(Debug, Envconfig)]
pub struct LoaderConfig {
    #[envconfig(nested)]
    pub database: DatabaseConfig,
    /// Имя канала без `@`, например `brieflyru`.
    #[envconfig(from = "TELEGRAM_CHANNEL")]
    pub channel: String,
    /// Пауза между концом прохода и началом следующего, в секундах.
    #[envconfig(from = "POLL_INTERVAL_SECS", default = "600")]
    pub poll_interval_secs: u32,
    /// Пауза между ответом ленты и следующим запросом, в секундах.
    #[envconfig(from = "REQUEST_DELAY_SECS", default = "2")]
    pub request_delay_secs: u32,
    /// Собственная пауза при ответе `429`, в секундах: `Retry-After` не читается.
    #[envconfig(from = "FLOOD_WAIT_SECS", default = "60")]
    pub flood_wait_secs: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn loader_env(extra: &[(&str, &str)]) -> HashMap<String, String> {
        [
            ("DATABASE_URL", "postgres://localhost/briefly_searcher"),
            ("TELEGRAM_CHANNEL", "brieflyru"),
        ]
        .iter()
        .chain(extra)
        .map(|&(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
    }

    #[test]
    fn loader_defaults_follow_spec() {
        let config = LoaderConfig::init_from_hashmap(&loader_env(&[])).unwrap();
        assert_eq!(config.channel, "brieflyru");
        assert_eq!(
            config.database.database_url,
            "postgres://localhost/briefly_searcher"
        );
        assert_eq!(config.poll_interval_secs, 600);
        assert_eq!(config.request_delay_secs, 2);
        assert_eq!(config.flood_wait_secs, 60);
    }

    #[test]
    fn loader_pacing_comes_from_environment() {
        let env = loader_env(&[
            ("POLL_INTERVAL_SECS", "300"),
            ("REQUEST_DELAY_SECS", "5"),
            ("FLOOD_WAIT_SECS", "120"),
        ]);
        let config = LoaderConfig::init_from_hashmap(&env).unwrap();
        assert_eq!(config.poll_interval_secs, 300);
        assert_eq!(config.request_delay_secs, 5);
        assert_eq!(config.flood_wait_secs, 120);
    }

    #[test]
    fn missing_channel_is_an_error() {
        let mut env = loader_env(&[]);
        env.remove("TELEGRAM_CHANNEL");
        let error = LoaderConfig::init_from_hashmap(&env).unwrap_err();
        assert_eq!(
            error,
            envconfig::Error::EnvVarMissing {
                name: "TELEGRAM_CHANNEL"
            }
        );
    }

    #[test]
    fn reads_database_url() {
        let env = HashMap::from([(
            "DATABASE_URL".to_owned(),
            "postgres://localhost/briefly_searcher".to_owned(),
        )]);
        let config = DatabaseConfig::init_from_hashmap(&env).unwrap();
        assert_eq!(config.database_url, "postgres://localhost/briefly_searcher");
    }

    #[test]
    fn web_listens_on_loopback_by_default() {
        let env = HashMap::from([(
            "DATABASE_URL".to_owned(),
            "postgres://localhost/briefly_searcher".to_owned(),
        )]);
        let config = WebConfig::init_from_hashmap(&env).unwrap();
        assert_eq!(config.addr, "127.0.0.1:3000".parse().unwrap());
        assert_eq!(
            config.database.database_url,
            "postgres://localhost/briefly_searcher"
        );
    }

    #[test]
    fn web_address_comes_from_environment() {
        let env = HashMap::from([
            (
                "DATABASE_URL".to_owned(),
                "postgres://localhost/briefly_searcher".to_owned(),
            ),
            ("WEB_ADDR".to_owned(), "[::1]:8080".to_owned()),
        ]);
        let config = WebConfig::init_from_hashmap(&env).unwrap();
        assert_eq!(config.addr, "[::1]:8080".parse().unwrap());
    }

    #[test]
    fn missing_database_url_is_an_error() {
        let error = DatabaseConfig::init_from_hashmap(&HashMap::new()).unwrap_err();
        assert_eq!(
            error,
            envconfig::Error::EnvVarMissing {
                name: "DATABASE_URL"
            }
        );
    }
}

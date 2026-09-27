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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

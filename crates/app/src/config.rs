//! Конфигурация только из переменных окружения (12-factor, фактор III).

use envconfig::Envconfig;

/// Подключение к PostgreSQL — общая часть конфигурации всех команд.
#[derive(Debug, Envconfig)]
pub struct DatabaseConfig {
    #[envconfig(from = "DATABASE_URL")]
    pub database_url: String,
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

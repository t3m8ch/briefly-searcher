//! Конфигурация только из переменных окружения (12-factor, фактор III).

/// Ошибка чтения конфигурации.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("переменная окружения {0} не задана")]
    Missing(&'static str),
    #[error("переменная окружения {0} содержит не UTF-8")]
    NotUnicode(&'static str),
}

/// Подключение к PostgreSQL — общая часть конфигурации всех команд.
#[derive(Debug)]
pub struct DatabaseConfig {
    pub database_url: String,
}

impl DatabaseConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(std::env::var)
    }

    fn from_lookup(
        lookup: impl Fn(&'static str) -> Result<String, std::env::VarError>,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            database_url: required(&lookup, "DATABASE_URL")?,
        })
    }
}

fn required(
    lookup: &impl Fn(&'static str) -> Result<String, std::env::VarError>,
    name: &'static str,
) -> Result<String, ConfigError> {
    match lookup(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => Err(ConfigError::Missing(name)),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::NotUnicode(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::VarError;

    #[test]
    fn reads_database_url() {
        let config = DatabaseConfig::from_lookup(|name| match name {
            "DATABASE_URL" => Ok("postgres://localhost/briefly".into()),
            _ => Err(VarError::NotPresent),
        })
        .unwrap();
        assert_eq!(config.database_url, "postgres://localhost/briefly");
    }

    #[test]
    fn missing_database_url_is_an_error() {
        let error = DatabaseConfig::from_lookup(|_| Err(VarError::NotPresent)).unwrap_err();
        assert_eq!(error, ConfigError::Missing("DATABASE_URL"));
        assert_eq!(
            error.to_string(),
            "переменная окружения DATABASE_URL не задана"
        );
    }

    #[test]
    fn empty_database_url_is_an_error() {
        let error = DatabaseConfig::from_lookup(|_| Ok(String::new())).unwrap_err();
        assert_eq!(error, ConfigError::Missing("DATABASE_URL"));
    }
}

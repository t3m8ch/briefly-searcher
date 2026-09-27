//! Вход в аккаунт Telegram: номер, код и при необходимости пароль 2FA.

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use briefly_searcher_storage::Storage;
use grammers_client::{Client, InvocationError, SenderPool, SignInError};

use crate::session::{DbSession, SessionError};

/// `api_hash` приложения с my.telegram.org. Секрет: `Debug` его не показывает.
#[derive(Clone)]
pub struct ApiHash(String);

impl ApiHash {
    /// Значение для запроса к Telegram. Не выводить в логи.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl FromStr for ApiHash {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Infallible> {
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for ApiHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiHash(<скрыт>)")
    }
}

/// Ответы человека, который входит в аккаунт.
pub trait LoginPrompt {
    /// Номер телефона аккаунта в международном формате.
    fn phone(&mut self) -> std::io::Result<String>;
    /// Код входа, который Telegram прислал в приложение или по SMS.
    /// `retry` — предыдущий код был неверным.
    fn code(&mut self, retry: bool) -> std::io::Result<String>;
    /// Пароль двухэтапной проверки. `retry` — предыдущий пароль был неверным.
    fn password(&mut self, hint: Option<&str>, retry: bool) -> std::io::Result<String>;
}

/// Ошибка входа в аккаунт.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("не удалось прочитать ответ: {0}")]
    Prompt(#[from] std::io::Error),
    #[error("ошибка Telegram: {0}")]
    Telegram(#[from] InvocationError),
    #[error("Telegram отклонил вход: {0}")]
    Rejected(Box<SignInError>),
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// Проходит вход в аккаунт и сохраняет сессию в БД, заменяя прежнюю.
///
/// Сессия попадает в БД только после успешного входа.
pub async fn login(
    storage: Storage,
    api_id: i32,
    api_hash: &ApiHash,
    prompt: &mut impl LoginPrompt,
) -> Result<(), LoginError> {
    let session = Arc::new(DbSession::fresh(storage));
    let SenderPool { runner, handle, .. } = SenderPool::new(Arc::clone(&session), api_id);
    let client = Client::new(handle);
    let runner = tokio::spawn(runner.run());

    let result = sign_in(&client, api_hash, prompt).await;
    if result.is_ok() {
        session.save().await?;
    }
    client.disconnect();
    // Ошибка задачи соединений здесь уже ничего не меняет: вход завершён или
    // провалился раньше.
    let _ = runner.await;
    result
}

async fn sign_in(
    client: &Client,
    api_hash: &ApiHash,
    prompt: &mut impl LoginPrompt,
) -> Result<(), LoginError> {
    let phone = prompt.phone()?;
    let token = client
        .request_login_code(phone.trim(), api_hash.expose())
        .await?;
    let mut code = prompt.code(false)?;
    let mut password_token = loop {
        match client.sign_in(&token, code.trim()).await {
            Ok(_) => return Ok(()),
            Err(SignInError::InvalidCode) => code = prompt.code(true)?,
            Err(SignInError::PasswordRequired(password_token)) => break password_token,
            Err(SignInError::Other(error)) => return Err(error.into()),
            Err(error) => return Err(LoginError::Rejected(Box::new(error))),
        }
    };
    let mut retry = false;
    loop {
        let password = prompt.password(password_token.hint(), retry)?;
        match client.check_password(password_token, password).await {
            Ok(_) => return Ok(()),
            Err(SignInError::InvalidPassword(token)) => {
                password_token = token;
                retry = true;
            }
            Err(SignInError::Other(error)) => return Err(error.into()),
            Err(error) => return Err(LoginError::Rejected(Box::new(error))),
        }
    }
}

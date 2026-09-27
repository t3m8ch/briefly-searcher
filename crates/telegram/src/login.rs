//! Вход в аккаунт Telegram: номер, код и при необходимости пароль 2FA.

use std::fmt;
use std::sync::Arc;

use briefly_searcher_storage::Storage;
use grammers_client::{Client, InvocationError, SenderPool, SignInError};

use crate::session::{DbSession, SessionError};

/// Приложение Telegram с my.telegram.org. Секрет: `Debug` не показывает
/// ни `api_id`, ни `api_hash`, чтобы они не попали в логи.
#[derive(Clone)]
pub struct ApiCredentials {
    pub api_id: i32,
    pub api_hash: String,
}

impl fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiCredentials(<скрыты>)")
    }
}

/// Ответы человека, который входит в аккаунт.
pub trait LoginPrompt {
    /// Номер телефона аккаунта в международном формате, без пробелов по краям.
    fn phone(&mut self) -> std::io::Result<String>;
    /// Код входа, который Telegram прислал в приложение или по SMS, без
    /// пробелов по краям.
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

impl From<SignInError> for LoginError {
    fn from(error: SignInError) -> Self {
        match error {
            SignInError::Other(error) => Self::Telegram(error),
            error => Self::Rejected(Box::new(error)),
        }
    }
}

/// Проходит вход в аккаунт и сохраняет сессию в БД, заменяя прежнюю.
///
/// Сессия попадает в БД только после успешного входа.
pub async fn login(
    storage: Storage,
    credentials: &ApiCredentials,
    prompt: &mut impl LoginPrompt,
) -> Result<(), LoginError> {
    let session = Arc::new(DbSession::fresh(storage));
    let SenderPool { runner, handle, .. } =
        SenderPool::new(Arc::clone(&session), credentials.api_id);
    let client = Client::new(handle);
    let runner = tokio::spawn(runner.run());

    let result = match sign_in(&client, &credentials.api_hash, prompt).await {
        Ok(()) => session.save().await.map_err(LoginError::from),
        Err(error) => Err(error),
    };
    client.disconnect();
    // Ошибка задачи соединений здесь уже ничего не меняет: вход завершён или
    // провалился раньше.
    let _ = runner.await;
    result
}

async fn sign_in(
    client: &Client,
    api_hash: &str,
    prompt: &mut impl LoginPrompt,
) -> Result<(), LoginError> {
    let phone = prompt.phone()?;
    let token = client.request_login_code(&phone, api_hash).await?;
    let mut code = prompt.code(false)?;
    let mut password_token = loop {
        match client.sign_in(&token, &code).await {
            Ok(_) => return Ok(()),
            Err(SignInError::InvalidCode) => code = prompt.code(true)?,
            Err(SignInError::PasswordRequired(password_token)) => break password_token,
            Err(error) => return Err(error.into()),
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
            Err(error) => return Err(error.into()),
        }
    }
}

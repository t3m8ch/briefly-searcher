//! Ответы на вопросы команды `login` из терминала.

use std::io::{self, BufRead, Write};

use briefly_searcher_telegram::LoginPrompt;

/// Спрашивает в терминале; пароль 2FA читается без эха.
pub struct TerminalPrompt;

impl LoginPrompt for TerminalPrompt {
    fn phone(&mut self) -> io::Result<String> {
        ask("Номер телефона аккаунта (+79991234567): ")
    }

    fn code(&mut self, retry: bool) -> io::Result<String> {
        if retry {
            ask("Неверный код, введите ещё раз: ")
        } else {
            ask("Код входа из Telegram: ")
        }
    }

    fn password(&mut self, hint: Option<&str>, retry: bool) -> io::Result<String> {
        let hint = hint
            .map(|h| format!(" (подсказка: {h})"))
            .unwrap_or_default();
        let question = if retry {
            "Неверный пароль 2FA"
        } else {
            "Пароль 2FA"
        };
        rpassword::prompt_password(format!("{question}{hint}: "))
    }
}

fn ask(question: &str) -> io::Result<String> {
    let mut stdout = io::stdout();
    stdout.write_all(question.as_bytes())?;
    stdout.flush()?;
    let mut answer = String::new();
    if io::stdin().lock().read_line(&mut answer)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "ввод закончился раньше, чем вход",
        ));
    }
    Ok(answer)
}

//! Доступ к Telegram: MTProto-клиент `grammers` и trait `HistorySource`.
//!
//! Crate не обращается к БД: сессию Telegram он читает и сохраняет через
//! crate хранилища. Содержимое появится вместе с командой `login`.

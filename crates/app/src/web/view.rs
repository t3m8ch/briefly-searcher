//! Что показать по сводке: состояние загрузчика и журнал событий.

use briefly_searcher_storage::AdminSummary;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};

/// Heartbeat старше этого — загрузчик «не отвечает». Загрузчик пишет heartbeat
/// не реже раза в 15 с в любом состоянии, а у запроса к Telegram таймаут 30 с;
/// порог берётся с запасом от их суммы (docs/research/admin-panel.md).
const HEARTBEAT_STALE_AFTER: TimeDelta = TimeDelta::seconds(90);

/// Сводка в виде, готовом для шаблона.
pub struct StatusView {
    pub now: Moment,
    pub loader: LoaderView,
    pub history: HistoryView,
    /// «21 902 сообщения».
    pub raw_posts: String,
    /// `None`, пока первый проход не завершён.
    pub newest_fetched_id: Option<String>,
    /// События позже `now`, от дальних к ближним.
    pub future: Vec<Event>,
    /// События не позже `now`, от свежих к старым.
    pub past: Vec<Event>,
}

/// Момент времени: UTC для `<time datetime>` и текста, подсказка относительно `now`.
pub struct Moment {
    pub iso: String,
    pub utc: String,
    pub relative: String,
}

/// Цвет кружка и слова состояния.
#[derive(Clone, Copy)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Idle,
}

impl Tone {
    /// CSS-класс цвета.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Ok => "t-ok",
            Tone::Warn => "t-warn",
            Tone::Bad => "t-bad",
            Tone::Idle => "t-dim",
        }
    }
}

/// Состояние загрузчика: слово в заголовке и пояснение одной фразой.
pub struct LoaderView {
    pub tone: Tone,
    pub title: &'static str,
    pub why: &'static str,
}

/// Строка про загрузку истории: «Первый проход: дошёл до ID N — …».
pub struct HistoryView {
    pub label: &'static str,
    pub main: String,
    pub detail: String,
}

impl HistoryView {
    fn new(newest_fetched_id: Option<i64>, pass_reached_id: Option<i64>) -> Self {
        // Процента нет: полный объём истории заранее неизвестен.
        match (newest_fetched_id, pass_reached_id) {
            (None, None) => Self {
                label: "История",
                main: "не начата".to_owned(),
                detail: "загрузка ещё не начиналась".to_owned(),
            },
            (None, Some(reached)) => Self {
                label: "Первый проход",
                main: format!("дошёл до ID {}", group_digits(reached)),
                detail: "сколько осталось до начала канала — неизвестно".to_owned(),
            },
            (Some(newest), Some(reached)) => Self {
                label: "История",
                main: format!("загружена до ID {}", group_digits(newest)),
                detail: format!("проход за новыми дошёл до ID {}", group_digits(reached)),
            },
            (Some(newest), None) => Self {
                label: "История",
                main: format!("загружена до ID {}", group_digits(newest)),
                detail: "незавершённого прохода нет".to_owned(),
            },
        }
    }
}

/// Событие журнала: над линией «сейчас» — будущее, под ней — прошлое.
pub struct Event {
    pub at: Moment,
    pub what: &'static str,
    pub tone: Tone,
    /// Строка `└ …` под событием.
    pub note: Option<Note>,
}

/// Пояснение под событием: текст ошибки или «загрузчик молчит».
pub struct Note {
    pub text: String,
    pub tone: Tone,
}

impl StatusView {
    /// Выводит состояние загрузчика, строку истории и журнал из сводки.
    pub fn new(summary: &AdminSummary) -> Self {
        let now = summary.now;
        let pause_active = summary.flood_wait_until.is_some_and(|until| until > now);
        // Загрузчик не очищает last_error; ошибка прошла, если после неё был успешный запрос.
        let error_past = match (summary.last_error_at, summary.last_successful_request_at) {
            (Some(error_at), Some(success_at)) => success_at > error_at,
            _ => false,
        };
        let error_current = summary.last_error.is_some() && !error_past;
        let silent = summary
            .last_heartbeat_at
            .is_some_and(|heartbeat| now - heartbeat > HEARTBEAT_STALE_AFTER);

        // Молчащий heartbeat проверяется раньше паузы и ошибки: упавший во время
        // FLOOD_WAIT загрузчик иначе до конца паузы выглядел бы живым.
        let loader = match summary.last_heartbeat_at {
            None => LoaderView {
                tone: Tone::Idle,
                title: "ещё не запускался",
                why: "Heartbeat не записан ни разу. Запустите команду loader.",
            },
            Some(_) if silent => LoaderView {
                tone: Tone::Bad,
                title: "не отвечает",
                why: "Heartbeat давно не обновлялся: процесс остановлен, упал или завис.",
            },
            _ if pause_active => LoaderView {
                tone: Tone::Warn,
                title: "пауза Telegram",
                why: "Telegram попросил подождать (FLOOD_WAIT). Загрузчик жив и ждёт.",
            },
            _ if error_current && summary.next_attempt_at.is_some_and(|at| at > now) => {
                LoaderView {
                    tone: Tone::Bad,
                    title: "ошибка, ждёт повтора",
                    why: "Последний запрос не удался. Загрузчик повторит его сам.",
                }
            }
            _ => LoaderView {
                tone: Tone::Ok,
                title: "работает",
                why: "Процесс жив, основной цикл крутится.",
            },
        };

        let mut events = Vec::new();
        if let Some(until) = summary.flood_wait_until.filter(|_| pause_active) {
            events.push(JournalEvent::new(
                until,
                "Закончится пауза Telegram (FLOOD_WAIT)",
                Tone::Warn,
            ));
        }
        if let Some(at) = summary.next_attempt_at.filter(|_| error_current) {
            events.push(JournalEvent::new(
                at,
                "Следующая попытка запроса",
                Tone::Bad,
            ));
        }
        if let Some(at) = summary.last_heartbeat_at {
            events.push(if silent {
                JournalEvent {
                    note: Some(Note {
                        text: "с тех пор загрузчик молчит".to_owned(),
                        tone: Tone::Bad,
                    }),
                    ..JournalEvent::new(at, "Heartbeat загрузчика", Tone::Bad)
                }
            } else {
                JournalEvent::new(at, "Heartbeat загрузчика", Tone::Ok)
            });
        }
        if let Some(at) = summary.last_successful_request_at {
            events.push(JournalEvent::new(
                at,
                "Успешный запрос к Telegram",
                Tone::Ok,
            ));
        }
        if let (Some(error), Some(at)) = (&summary.last_error, summary.last_error_at) {
            let (what, tone) = if error_past {
                ("Ошибка — после неё были успешные запросы", Tone::Idle)
            } else {
                ("Ошибка", Tone::Bad)
            };
            events.push(JournalEvent {
                note: Some(Note {
                    text: error.clone(),
                    tone,
                }),
                ..JournalEvent::new(at, what, tone)
            });
        }

        // Журнал от будущего к прошлому; линия «сейчас» делит его на две части.
        events.sort_by(|a, b| b.at.cmp(&a.at));
        let (future, past) = events.into_iter().partition(|event| event.at > now);
        let into_view = |events: Vec<JournalEvent>| {
            events
                .into_iter()
                .map(|event| Event {
                    at: Moment::new(event.at, now),
                    what: event.what,
                    tone: event.tone,
                    note: event.note,
                })
                .collect()
        };

        Self {
            now: Moment::new(now, now),
            loader,
            history: HistoryView::new(summary.newest_fetched_id, summary.pass_reached_id),
            raw_posts: format!(
                "{} {}",
                group_digits(summary.raw_posts_count),
                plural(
                    summary.raw_posts_count,
                    ["сообщение", "сообщения", "сообщений"]
                ),
            ),
            newest_fetched_id: summary.newest_fetched_id.map(group_digits),
            future: into_view(future),
            past: into_view(past),
        }
    }
}

/// Событие журнала, пока время ещё не отформатировано относительно `now`.
struct JournalEvent {
    at: DateTime<Utc>,
    what: &'static str,
    tone: Tone,
    note: Option<Note>,
}

impl JournalEvent {
    fn new(at: DateTime<Utc>, what: &'static str, tone: Tone) -> Self {
        Self {
            at,
            what,
            tone,
            note: None,
        }
    }
}

impl Moment {
    fn new(at: DateTime<Utc>, now: DateTime<Utc>) -> Self {
        Self {
            iso: at.to_rfc3339_opts(SecondsFormat::Secs, true),
            utc: at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            relative: relative(at - now),
        }
    }
}

/// «5 с назад», «через 47 мин», «7 ч 12 мин назад».
fn relative(delta: TimeDelta) -> String {
    let seconds = delta.num_seconds().unsigned_abs();
    let amount = match seconds {
        0..60 => format!("{seconds} с"),
        60..3600 => format!("{} мин", seconds / 60),
        _ => format!("{} ч {} мин", seconds / 3600, seconds % 3600 / 60),
    };
    if delta > TimeDelta::zero() {
        format!("через {amount}")
    } else {
        format!("{amount} назад")
    }
}

/// Разбивает число на группы по три цифры неразрывным пробелом: «43 870».
fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push('\u{a0}');
        }
        grouped.push(digit);
    }
    if n < 0 {
        grouped.insert(0, '-');
    }
    grouped
}

/// Форма слова для числа: [«сообщение», «сообщения», «сообщений»].
fn plural(n: i64, forms: [&'static str; 3]) -> &'static str {
    let n = n.unsigned_abs();
    match (n % 10, n % 100) {
        (_, 11..=14) => forms[2],
        (1, _) => forms[0],
        (2..=4, _) => forms[1],
        _ => forms[2],
    }
}

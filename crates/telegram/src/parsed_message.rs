//! Разбор сохранённого блока в данные для нормализатора, без БД и сети.

use chrono::{DateTime, Utc};
use scraper::{ElementRef, Html, Selector};

use crate::Message;

/// Тип публикации в блоке веб-ленты.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Text,
    Photo,
    Video,
    Album,
    Poll,
    Service,
}

/// Представление блока, не зависящее от формата источника.
/// Альбом остаётся одним блоком с общей подписью.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedMessage {
    pub id: i64,
    pub published_at: DateTime<Utc>,
    pub kind: MessageKind,
    /// ID элементов альбома; пустой список, если веб-лента не показала медиа.
    pub album_ids: Vec<i64>,
    pub text: String,
    pub reply_to: Option<i64>,
    pub forwarded_from: Option<String>,
}

/// Ошибка разбора; исходный payload в текст ошибки не включается.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("неизвестная схема payload: {0}")]
    UnknownSchema(String),
    #[error("блок {id}: {reason}")]
    InvalidBlock { id: i64, reason: &'static str },
}

/// Разбирает сохранённый блок по `payload_schema`.
///
/// # Errors
/// Неизвестная схема, отсутствие HTML или ожидаемой разметки, некорректные
/// ID и дата публикации. Пустой текст распознанного блока допустим.
pub fn parse_message(message: &Message) -> Result<ParsedMessage, ParseError> {
    if message.payload_schema != "t.me/s html v1" {
        return Err(ParseError::UnknownSchema(message.payload_schema.clone()));
    }
    parse_web_block(message)
}

fn parse_web_block(message: &Message) -> Result<ParsedMessage, ParseError> {
    let parse = || {
        let html = message
            .payload
            .get("html")
            .and_then(|v| v.as_str())
            .ok_or("нет HTML")?;
        let document = Html::parse_fragment(html);
        let root = message_root(&document)?;
        let (channel, id) = message_identity(root, message.id)?;
        let published_at = publication_date(root)?;
        let kind = message_kind(root)?;
        let album_ids = if kind == MessageKind::Album {
            album_message_ids(root, channel, id)?
        } else {
            Vec::new()
        };
        Ok(ParsedMessage {
            id,
            published_at,
            kind,
            album_ids,
            text: message_text(root, kind)?,
            reply_to: reply_target(root, channel)?,
            forwarded_from: forwarding_source(root)?,
        })
    };
    parse().map_err(|reason| ParseError::InvalidBlock {
        id: message.id,
        reason,
    })
}

fn message_root(document: &Html) -> Result<ElementRef<'_>, &'static str> {
    let blocks = selector(".tgme_widget_message");
    let mut roots = document.select(&blocks);
    let root = roots.next().ok_or("нет блока сообщения")?;
    if roots.next().is_some() {
        return Err("ожидался один блок сообщения");
    }
    Ok(root)
}

fn message_identity(root: ElementRef<'_>, expected_id: i64) -> Result<(&str, i64), &'static str> {
    let post = root.attr("data-post").ok_or("нет data-post")?;
    let (channel, id) = post.split_once('/').ok_or("неверный data-post")?;
    let id = positive_id(id).ok_or("неверный ID")?;
    if channel.is_empty() || id != expected_id {
        return Err("ID блока не совпадает с ID записи");
    }
    Ok((channel, id))
}

fn publication_date(root: ElementRef<'_>) -> Result<DateTime<Utc>, &'static str> {
    root.select(&selector(".tgme_widget_message_date time[datetime]"))
        .next()
        .and_then(|element| element.attr("datetime"))
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
        .map(|date| date.with_timezone(&Utc))
        .ok_or("нет корректной даты публикации")
}

fn message_kind(root: ElementRef<'_>) -> Result<MessageKind, &'static str> {
    if root
        .value()
        .has_class("service_message", scraper::CaseSensitivity::CaseSensitive)
    {
        return Ok(MessageKind::Service);
    }
    // Сначала специфические типы: подпись у фото, видео и альбома
    // использует тот же CSS-класс, что и обычное текстовое сообщение.
    for (css, kind) in [
        (".tgme_widget_message_poll", MessageKind::Poll),
        (".tgme_widget_message_grouped_wrap", MessageKind::Album),
        (".tgme_widget_message_video_wrap", MessageKind::Video),
        (".tgme_widget_message_photo_wrap", MessageKind::Photo),
        (".tgme_widget_message_text", MessageKind::Text),
    ] {
        if root.select(&selector(css)).next().is_some() {
            return Ok(kind);
        }
    }
    Err("неизвестный тип блока")
}

fn album_message_ids(
    root: ElementRef<'_>,
    channel: &str,
    id: i64,
) -> Result<Vec<i64>, &'static str> {
    let mut ids = Vec::new();
    for link in root.select(&selector(".grouped_media_wrap")) {
        let href = link.attr("href").ok_or("нет ссылки элемента альбома")?;
        let target = href
            .strip_suffix("?single")
            .ok_or("неверная ссылка элемента альбома")?;
        let item = linked_id(target, channel).ok_or("неверный ID элемента альбома")?;
        if !ids.contains(&item) {
            ids.push(item);
        }
    }
    // Веб-лента иногда отдаёт пустую сетку медиа с доступной подписью.
    // Состав неизвестен: не выдумываем ID, но сохраняем видимый текст.
    if !ids.is_empty() && (ids.len() < 2 || ids.first() != Some(&id)) {
        return Err("неполный состав альбома");
    }
    Ok(ids)
}

fn message_text(root: ElementRef<'_>, kind: MessageKind) -> Result<String, &'static str> {
    let mut text = String::new();
    let mut found = false;
    for element in root.select(&selector(".tgme_widget_message_text")) {
        if !is_message_text(element) {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        found = true;
        visible_text(element, &mut text);
    }
    if kind == MessageKind::Text && !found {
        return Err("нет текста публикации");
    }
    Ok(text.trim().to_owned())
}

fn is_message_text(element: ElementRef<'_>) -> bool {
    // Внешний div уже включает вложенную подпись альбома. Цитата
    // ответа и превью не относятся к тексту публикации.
    !element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|parent| {
            parent.value().classes().any(|class| {
                matches!(
                    class,
                    "tgme_widget_message_text" | "tgme_widget_message_reply" | "link_preview"
                )
            })
        })
}

fn reply_target(root: ElementRef<'_>, channel: &str) -> Result<Option<i64>, &'static str> {
    let Some(reply) = root.select(&selector(".tgme_widget_message_reply")).next() else {
        return Ok(None);
    };
    let id = reply
        .attr("href")
        .and_then(|href| linked_id(href, channel))
        .ok_or("неверная ссылка reply")?;
    Ok(Some(id))
}

fn forwarding_source(root: ElementRef<'_>) -> Result<Option<String>, &'static str> {
    let Some(forwarded) = root
        .select(&selector(".tgme_widget_message_forwarded_from"))
        .next()
    else {
        return Ok(None);
    };
    let name = forwarded
        .select(&selector(".tgme_widget_message_forwarded_from_name"))
        .next()
        .ok_or("нет источника пересылки")?;
    let mut text = String::new();
    visible_text(name, &mut text);
    let name = text.trim();
    if name.is_empty() {
        return Err("пустой источник пересылки");
    }
    Ok(Some(name.to_owned()))
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("статический CSS-селектор")
}

fn positive_id(s: &str) -> Option<i64> {
    s.parse().ok().filter(|&id| id > 0)
}

fn linked_id(href: &str, channel: &str) -> Option<i64> {
    let path = href
        .strip_prefix("https://t.me/")
        .or_else(|| href.strip_prefix("http://t.me/"))?;
    let (target_channel, id) = path.split_once('/')?;
    target_channel
        .eq_ignore_ascii_case(channel)
        .then(|| positive_id(id))
        .flatten()
}

fn visible_text(element: ElementRef<'_>, output: &mut String) {
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            output.push_str(text);
        } else if let Some(element) = ElementRef::wrap(child) {
            if element
                .value()
                .classes()
                .any(|c| matches!(c, "link_preview" | "tgme_widget_message_reply" | "copyonly"))
                || matches!(element.value().name(), "script" | "style")
            {
                continue;
            }
            if element.value().name() == "br" {
                output.push('\n');
            } else {
                visible_text(element, output);
            }
        }
    }
}

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
    let invalid = |reason| ParseError::InvalidBlock {
        id: message.id,
        reason,
    };
    let html = message
        .payload
        .get("html")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("нет HTML"))?;
    let document = Html::parse_fragment(html);
    let blocks = selector(".tgme_widget_message");
    let mut roots = document.select(&blocks);
    let root = roots.next().ok_or_else(|| invalid("нет блока сообщения"))?;
    if roots.next().is_some() {
        return Err(invalid("ожидался один блок сообщения"));
    }
    let post = root
        .attr("data-post")
        .ok_or_else(|| invalid("нет data-post"))?;
    let (channel, id) = post
        .split_once('/')
        .ok_or_else(|| invalid("неверный data-post"))?;
    let id = positive_id(id).ok_or_else(|| invalid("неверный ID"))?;
    if channel.is_empty() || id != message.id {
        return Err(invalid("ID блока не совпадает с ID записи"));
    }
    let published_at = root
        .select(&selector(".tgme_widget_message_date time[datetime]"))
        .next()
        .and_then(|e| e.attr("datetime"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .ok_or_else(|| invalid("нет корректной даты публикации"))?
        .with_timezone(&Utc);
    let has = |css| root.select(&selector(css)).next().is_some();
    let kind = if root
        .value()
        .has_class("service_message", scraper::CaseSensitivity::CaseSensitive)
    {
        MessageKind::Service
    } else if has(".tgme_widget_message_poll") {
        MessageKind::Poll
    } else if has(".tgme_widget_message_grouped_wrap") {
        MessageKind::Album
    } else if has(".tgme_widget_message_video_wrap") {
        MessageKind::Video
    } else if has(".tgme_widget_message_photo_wrap") {
        MessageKind::Photo
    } else if has(".tgme_widget_message_text") {
        MessageKind::Text
    } else {
        return Err(invalid("неизвестный тип блока"));
    };
    let mut album_ids = Vec::new();
    if kind == MessageKind::Album {
        for link in root.select(&selector(".grouped_media_wrap[href]")) {
            let href = link.attr("href").unwrap();
            let target = href
                .strip_suffix("?single")
                .ok_or_else(|| invalid("неверная ссылка элемента альбома"))?;
            let item = linked_id(target, channel)
                .ok_or_else(|| invalid("неверный ID элемента альбома"))?;
            if !album_ids.contains(&item) {
                album_ids.push(item);
            }
        }
        if album_ids.len() < 2 || album_ids.first() != Some(&id) {
            return Err(invalid("неполный состав альбома"));
        }
    }
    let mut text = String::new();
    let mut text_blocks = 0;
    {
        for element in root.select(&selector(".tgme_widget_message_text")) {
            // Внешний div уже включает вложенную подпись альбома. Цитата
            // ответа и превью не относятся к тексту публикации.
            if element
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|parent| {
                    parent.value().classes().any(|c| {
                        matches!(
                            c,
                            "tgme_widget_message_text"
                                | "tgme_widget_message_reply"
                                | "link_preview"
                        )
                    })
                })
            {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text_blocks += 1;
            visible_text(element, &mut text);
        }
    }
    if kind == MessageKind::Text && text_blocks == 0 {
        return Err(invalid("нет текста публикации"));
    }
    let reply_to = root
        .select(&selector(".tgme_widget_message_reply"))
        .next()
        .map(|e| {
            e.attr("href")
                .and_then(|h| linked_id(h, channel))
                .ok_or_else(|| invalid("неверная ссылка reply"))
        })
        .transpose()?;
    let forwarded_from = root
        .select(&selector(".tgme_widget_message_forwarded_from"))
        .next()
        .map(|e| {
            let name = e
                .select(&selector(".tgme_widget_message_forwarded_from_name"))
                .next()
                .ok_or_else(|| invalid("нет источника пересылки"))?;
            let mut text = String::new();
            visible_text(name, &mut text);
            let name = text.trim();
            if name.is_empty() {
                return Err(invalid("пустой источник пересылки"));
            }
            Ok(name.to_owned())
        })
        .transpose()?;
    Ok(ParsedMessage {
        id,
        published_at,
        kind,
        album_ids,
        text: text.trim().to_owned(),
        reply_to,
        forwarded_from,
    })
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

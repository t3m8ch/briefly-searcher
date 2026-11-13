//! Чистый пересчёт разобранных блоков в посты и статьи для поиска.

mod marker;
mod similarity;

use briefly_searcher_telegram::{MessageKind, ParsedMessage};
use chrono::{DateTime, Duration, Utc};
use marker::Marker;

/// Результат полного пересчёта; обе коллекции упорядочены по ID.
#[derive(Debug, PartialEq, Eq)]
pub struct Normalized {
    pub posts: Vec<Post>,
    pub articles: Vec<Article>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Post {
    pub message_id: i64,
    pub article_id: i64,
    /// Порядковый номер после исправления опечаток; у одиночного поста отсутствует.
    pub part_no: Option<u32>,
    pub text: String,
    pub published_at: DateTime<Utc>,
    pub forwarded_from: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Article {
    pub id: i64,
    pub title: Option<String>,
    pub search_text: String,
    pub status: ArticleStatus,
    pub declared_parts: Option<u32>,
    pub published_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArticleStatus {
    Single,
    Complete,
    Incomplete,
}

/// Строит статьи из всего среза истории. Вход может быть не отсортирован;
/// каждый ID должен встречаться один раз. Исходные сообщения не меняются.
pub fn normalize(messages: &[ParsedMessage]) -> Normalized {
    let mut messages: Vec<_> = messages.iter().collect();
    messages.sort_by_key(|m| m.id);
    let mut posts: Vec<Post> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut articles: Vec<BuildingArticle> = Vec::new();
    for message in messages {
        if matches!(message.kind, MessageKind::Service | MessageKind::Poll)
            || message.text.trim().is_empty()
        {
            continue;
        }
        let text = strip_footer(&message.text);
        let marker = Marker::parse(&text);
        open.retain(|&index| {
            let article = &articles[index];
            message.published_at.signed_duration_since(article.last_at) <= Duration::hours(24)
                && article
                    .declared_parts
                    .is_none_or(|total| article.last_ordinal() < total)
        });
        let target = marker.as_ref().and_then(|marker| {
            open.iter()
                .rev()
                .copied()
                .find(|&index| articles[index].accepts(marker, message.published_at))
        });
        let target = target.or_else(|| {
            let marker = marker.as_ref().filter(|m| m.number == 2)?;
            let reply = message.reply_to?;
            let post_index = posts.binary_search_by_key(&reply, |p| p.message_id).ok()?;
            if posts[post_index].part_no.is_some() {
                return None;
            }
            let index = articles.binary_search_by_key(&reply, |a| a.id).ok()?;
            let article = &mut articles[index];
            article.title = Some(marker.title.clone());
            article.normalized_title = normalize_title(&marker.title);
            article.parts[0] = Some(1);
            posts[post_index].part_no = Some(1);
            Some(index)
        });
        let target = target.unwrap_or_else(|| {
            articles.push(BuildingArticle::new(message, marker.as_ref()));
            articles.len() - 1
        });
        if marker.is_some() && !open.contains(&target) {
            open.push(target);
        }
        let article = &mut articles[target];
        let part_no = marker.as_ref().map(|m| {
            let ordinal = if article.parts.is_empty() {
                m.number
            } else {
                article.last_ordinal() + 1
            };
            article.last_number = Some(m.number);
            article.last_at = message.published_at;
            article.declared_parts = article.declared_parts.max(m.total);
            ordinal
        });
        article.bodies.push(
            marker
                .as_ref()
                .map_or_else(|| text.clone(), |m| m.body.clone()),
        );
        article.parts.push(part_no);
        posts.push(Post {
            message_id: message.id,
            article_id: article.id,
            part_no,
            text,
            published_at: message.published_at,
            forwarded_from: message.forwarded_from.clone(),
        });
    }
    Normalized {
        posts,
        articles: articles.into_iter().map(BuildingArticle::finish).collect(),
    }
}

struct BuildingArticle {
    id: i64,
    title: Option<String>,
    normalized_title: String,
    published_at: DateTime<Utc>,
    last_at: DateTime<Utc>,
    last_number: Option<u32>,
    declared_parts: Option<u32>,
    parts: Vec<Option<u32>>,
    bodies: Vec<String>,
}

impl BuildingArticle {
    fn new(message: &ParsedMessage, marker: Option<&Marker>) -> Self {
        let title = marker.map(|m| m.title.clone());
        Self {
            id: message.id,
            normalized_title: title.as_deref().map(normalize_title).unwrap_or_default(),
            title,
            published_at: message.published_at,
            last_at: message.published_at,
            last_number: None,
            declared_parts: None,
            parts: vec![],
            bodies: vec![],
        }
    }

    fn accepts(&self, marker: &Marker, at: DateTime<Utc>) -> bool {
        let age = at.signed_duration_since(self.last_at);
        ((marker.number > 1
            && self.last_number == Some(marker.number - 1)
            && (Duration::zero()..=Duration::hours(24)).contains(&age))
            || (self.last_number == Some(marker.number)
                && (Duration::zero()..=Duration::hours(3)).contains(&age)))
            && similarity::ratio(&self.normalized_title, &normalize_title(&marker.title)) >= 0.54
    }

    fn last_ordinal(&self) -> u32 {
        self.parts.last().copied().flatten().unwrap_or(0)
    }

    fn finish(self) -> Article {
        let status = if self.title.is_none() {
            ArticleStatus::Single
        } else if self
            .parts
            .iter()
            .enumerate()
            .all(|(i, n)| *n == Some(i as u32 + 1))
            && self
                .declared_parts
                .is_none_or(|total| self.parts.len() as u32 >= total)
            && (self.parts.len() > 1 || self.declared_parts.is_some())
        {
            ArticleStatus::Complete
        } else {
            ArticleStatus::Incomplete
        };
        let search_text = self
            .title
            .iter()
            .cloned()
            .chain(self.bodies)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        Article {
            id: self.id,
            title: self.title,
            search_text,
            status,
            declared_parts: self.declared_parts,
            published_at: self.published_at,
        }
    }
}

fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_footer(text: &str) -> String {
    let text = text.trim();
    let (body, last) = text.rsplit_once('\n').unwrap_or(("", text));
    let footer = last.split('|').map(str::trim).collect::<Vec<_>>().join("|");
    let known = matches!(
        footer.as_str(),
        "Подписаться."
            | "Подписаться"
            | "Подписаться|Сайт"
            | "Подписаться|Сайт|"
            | "Подписаться|Сайт|Х"
            | "Подпишись на Briefly и RTVI"
            | "@brieflyru"
            | "@rusbri"
            | "Подпишись."
            | "Подписывайтесь на NEWS.ru"
            | "Подпишись на RTVI в Telegram|MAX"
            | "Подпишись на RTVI в Telegram|МАКС"
    );
    if known {
        body.trim_end().to_owned()
    } else {
        text.to_owned()
    }
}

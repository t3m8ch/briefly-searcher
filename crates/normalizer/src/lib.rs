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
    let mut normalizer = Normalizer::default();
    for message in messages {
        let has_visible_text = !message.text.trim().is_empty();
        let is_publication = !matches!(message.kind, MessageKind::Service | MessageKind::Poll);
        if is_publication && has_visible_text {
            normalizer.add_message(message);
        }
    }
    normalizer.finish()
}

/// Состояние одного пересчёта. Индексы открытых статей ссылаются в articles;
/// сами статьи не удаляются и сохраняют порядок создания (по ID).
#[derive(Default)]
struct Normalizer {
    posts: Vec<Post>,
    articles: Vec<BuildingArticle>,
    open_articles: Vec<usize>,
}

impl Normalizer {
    fn add_message(&mut self, message: &ParsedMessage) {
        let text = strip_footer(&message.text);
        let marker = Marker::parse(&text);
        self.close_finished_articles(message.published_at);
        let article_index = self.find_or_create_article(message, marker.as_ref());
        let article = &mut self.articles[article_index];
        let post = article.add_post(message, text, marker.as_ref());
        self.posts.push(post);
    }

    fn close_finished_articles(&mut self, at: DateTime<Utc>) {
        self.open_articles
            .retain(|&index| self.articles[index].is_open_at(at));
    }

    fn find_or_create_article(
        &mut self,
        message: &ParsedMessage,
        marker: Option<&Marker>,
    ) -> usize {
        if let Some(marker) = marker {
            if let Some(index) = self.find_open_article(marker, message.published_at) {
                return index;
            }
            if let Some(index) = self.restore_first_part_from_reply(message, marker) {
                self.open_articles.push(index);
                return index;
            }
        }

        let index = self.articles.len();
        self.articles.push(BuildingArticle::new(message, marker));
        if marker.is_some() {
            self.open_articles.push(index);
        }
        index
    }

    fn find_open_article(&self, marker: &Marker, at: DateTime<Utc>) -> Option<usize> {
        self.open_articles
            .iter()
            .rev()
            .copied()
            .find(|&index| self.articles[index].accepts(marker, at))
    }

    fn restore_first_part_from_reply(
        &mut self,
        message: &ParsedMessage,
        marker: &Marker,
    ) -> Option<usize> {
        if marker.number != 2 {
            return None;
        }
        let reply_id = message.reply_to?;
        let post_index = self
            .posts
            .binary_search_by_key(&reply_id, |post| post.message_id)
            .ok()?;
        if self.posts[post_index].part_no.is_some() {
            return None;
        }
        // Пост без маркера пока образует одиночную статью с тем же ID.
        let article_index = self
            .articles
            .binary_search_by_key(&reply_id, |article| article.id)
            .ok()?;
        self.articles[article_index].restore_first_part(marker);
        self.posts[post_index].part_no = Some(1);
        Some(article_index)
    }

    fn finish(self) -> Normalized {
        Normalized {
            posts: self.posts,
            articles: self
                .articles
                .into_iter()
                .map(BuildingArticle::finish)
                .collect(),
        }
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

    fn is_open_at(&self, at: DateTime<Utc>) -> bool {
        let within_time_window = at.signed_duration_since(self.last_at) <= Duration::hours(24);
        let expects_more_parts = self
            .declared_parts
            .is_none_or(|total| self.last_ordinal() < total);
        within_time_window && expects_more_parts
    }

    fn restore_first_part(&mut self, marker: &Marker) {
        self.title = Some(marker.title.clone());
        self.normalized_title = normalize_title(&marker.title);
        self.parts[0] = Some(1);
    }

    fn add_post(&mut self, message: &ParsedMessage, text: String, marker: Option<&Marker>) -> Post {
        let (part_no, body) = match marker {
            Some(marker) => {
                let ordinal = self.next_ordinal(marker);
                self.last_number = Some(marker.number);
                self.last_at = message.published_at;
                self.declared_parts = self.declared_parts.max(marker.total);
                (Some(ordinal), marker.body.clone())
            }
            None => (None, text.clone()),
        };
        self.parts.push(part_no);
        self.bodies.push(body);
        Post {
            message_id: message.id,
            article_id: self.id,
            part_no,
            text,
            published_at: message.published_at,
            forwarded_from: message.forwarded_from.clone(),
        }
    }

    fn next_ordinal(&self, marker: &Marker) -> u32 {
        if self.parts.is_empty() {
            marker.number
        } else {
            self.last_ordinal() + 1
        }
    }

    fn accepts(&self, marker: &Marker, at: DateTime<Utc>) -> bool {
        let age = at.signed_duration_since(self.last_at);
        let continues_numbering = marker.number > 1
            && self.last_number == Some(marker.number - 1)
            && (Duration::zero()..=Duration::hours(24)).contains(&age);
        let repeats_number = self.last_number == Some(marker.number)
            && (Duration::zero()..=Duration::hours(3)).contains(&age);
        if !continues_numbering && !repeats_number {
            return false;
        }
        similarity::ratio(&self.normalized_title, &normalize_title(&marker.title)) >= 0.54
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

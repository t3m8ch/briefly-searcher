use briefly_searcher_normalizer::{ArticleStatus, normalize};
use briefly_searcher_telegram::{MessageKind, ParsedMessage};
use chrono::{TimeZone, Utc};

fn message(id: i64, text: &str) -> ParsedMessage {
    ParsedMessage {
        id,
        text: text.into(),
        published_at: Utc.timestamp_opt(1_700_000_000 + id * 60, 0).unwrap(),
        kind: MessageKind::Text,
        album_ids: vec![],
        reply_to: None,
        forwarded_from: None,
    }
}

#[test]
fn single_post_loses_only_known_footer() {
    let result = normalize(&[message(1, "Текст\n\nПодписаться.")]);
    assert_eq!(result.posts.len(), 1);
    assert_eq!(result.posts[0].text, "Текст");
    assert_eq!(result.articles[0].search_text, "Текст");
    assert_eq!(result.articles[0].title, None);
    assert_eq!(result.articles[0].status, ArticleStatus::Single);
}

#[test]
fn parts_form_one_search_document_in_id_order() {
    let result = normalize(&[
        message(2, "Тема. Часть 2/2.\nВторая\nПодписаться."),
        message(1, "Тема. Часть 1/2.\nПервая\nПодписаться."),
    ]);
    assert_eq!(result.articles.len(), 1);
    assert_eq!(result.articles[0].id, 1);
    assert_eq!(result.articles[0].title.as_deref(), Some("Тема"));
    assert_eq!(result.articles[0].search_text, "Тема\n\nПервая\n\nВторая");
    assert_eq!(result.articles[0].status, ArticleStatus::Complete);
    assert_eq!(
        result
            .posts
            .iter()
            .map(|p| (p.article_id, p.part_no))
            .collect::<Vec<_>>(),
        [(1, Some(1)), (1, Some(2))]
    );
}

#[test]
fn numbering_typos_and_reply_are_repaired() {
    let duplicate = normalize(&[
        message(1, "Тема. Часть 1/3.\nА"),
        message(2, "Тема. Часть 2/3.\nБ"),
        message(3, "Тема. Часть 2/3.\nВ"),
    ]);
    assert_eq!(duplicate.articles.len(), 1);
    assert_eq!(duplicate.posts[2].part_no, Some(3));
    assert_eq!(duplicate.articles[0].status, ArticleStatus::Complete);
    let mut second = message(2, "Тема (часть 2): Второй текст");
    second.reply_to = Some(1);
    let reply = normalize(&[message(1, "Первая без маркера"), second]);
    assert_eq!(reply.articles.len(), 1);
    assert_eq!(reply.posts[0].part_no, Some(1));
    assert_eq!(
        reply.articles[0].search_text,
        "Тема\n\nПервая без маркера\n\nВторой текст"
    );
}

#[test]
fn footer_catalog_is_exact_and_only_last_line_is_removed() {
    for footer in [
        "Подписаться.",
        "Подписаться",
        "Подписаться | Сайт | Х",
        " Подписаться|Сайт ",
        "Подписаться |Сайт",
        "Подписаться | Сайт |",
        "Подпишись на Briefly и RTVI",
        "@brieflyru",
        "@rusbri",
        "Подпишись.",
        "Подписывайтесь на NEWS.ru",
        "Подпишись на RTVI в Telegram | MAX",
        "Подпишись на RTVI в Telegram | МАКС",
    ] {
        let result = normalize(&[message(
            1,
            &format!("Начало\nПодписаться.\nКонец\n{footer}"),
        )]);
        assert_eq!(
            result.posts[0].text, "Начало\nПодписаться.\nКонец",
            "{footer}"
        );
    }
    for unknown in [
        "Подписаться на автора",
        "Подписаться | Неизвестный сайт",
        "Последняя строка",
    ] {
        let text = format!("Текст\n{unknown}");
        assert_eq!(normalize(&[message(1, &text)]).posts[0].text, text);
    }
}

#[test]
fn marker_grammar_and_search_text_preserve_inline_content() {
    for marker in [
        "Часть 1/1.",
        "ЧАСТЬ 1\\1:",
        "часть 1//1)",
        "Часть 1.",
        "Часть 1:",
        "часть 1)",
        "Часть 1",
    ] {
        let result = normalize(&[message(1, &format!("Тема ({marker}\nТело"))]);
        assert!(result.posts[0].part_no.is_some(), "{marker}");
        assert_eq!(result.articles[0].title.as_deref(), Some("Тема"));
    }
    let result = normalize(&[message(1, "Тема (часть 1): Начало\n\nПродолжение")]);
    assert_eq!(
        result.articles[0].search_text,
        "Тема\n\nНачало\n\nПродолжение"
    );
    assert_eq!(result.articles[0].status, ArticleStatus::Incomplete);
    for text in [
        "часть 2023 года",
        "части 10-го корпуса",
        "часть 21-го пакета",
        "часть 3 бюджета",
        "запчасть 1.",
        "Часть 0.",
        "Часть 31.",
        "Часть 1а.",
        "Тема\nЧасть 1/2.",
    ] {
        assert_eq!(
            normalize(&[message(1, text)]).articles[0].status,
            ArticleStatus::Single,
            "{text}"
        );
    }
    for (prefix_len, expected) in [
        (399, ArticleStatus::Incomplete),
        (400, ArticleStatus::Single),
    ] {
        let prefix = format!("{} ", "я".repeat(prefix_len - 1));
        assert_eq!(
            normalize(&[message(1, &format!("{prefix}Часть 1."))]).articles[0].status,
            expected
        );
    }
    assert_eq!(
        normalize(&[message(1, "Тема часть 3. Уточнение часть 1/1.")]).posts[0].part_no,
        Some(1)
    );
}

#[test]
fn time_windows_and_latest_open_article_prevent_wrong_merges() {
    for (seconds, count) in [(86400, 1), (86401, 2), (-1, 2)] {
        let first = message(1, "Тема часть 1/2.");
        let mut second = message(2, "Тема часть 2/2.");
        second.published_at = first.published_at + chrono::Duration::seconds(seconds);
        assert_eq!(
            normalize(&[first, second]).articles.len(),
            count,
            "{seconds}"
        );
    }
    for (seconds, count) in [(10800, 1), (10801, 2)] {
        let first = message(1, "Тема часть 1.");
        let mut second = message(2, "Тема часть 1.");
        second.published_at = first.published_at + chrono::Duration::seconds(seconds);
        assert_eq!(normalize(&[first, second]).articles.len(), count);
    }
    let result = normalize(&[
        message(1, "Тема часть 1/2."),
        message(2, "Другая тема часть 1/2."),
        message(3, "Другая тема часть 2/2."),
    ]);
    assert_eq!(result.posts[2].article_id, 2);
    let closed = normalize(&[message(1, "Тема часть 1/1."), message(2, "Тема часть 2/2.")]);
    assert_eq!(closed.articles.len(), 2);
}

#[test]
fn title_similarity_ignores_case_yo_and_punctuation_but_separates_different_titles() {
    let same = normalize(&[
        message(1, "«ЁЛКА» *часть 1/2."),
        message(2, "елка часть 2/2."),
    ]);
    assert_eq!(same.articles.len(), 1);
    let different = normalize(&[
        message(1, "АБВГД часть 1/2."),
        message(2, "ЕЖЗИК часть 2/2."),
    ]);
    assert_eq!(different.articles.len(), 2);
}

#[test]
fn status_detects_missing_start_and_conflicting_total() {
    let result = normalize(&[
        message(1, "Тема часть 1/4."),
        message(2, "Тема часть 2/3."),
        message(3, "Тема часть 3/3."),
    ]);
    assert_eq!(result.articles[0].declared_parts, Some(4));
    assert_eq!(result.articles[0].status, ArticleStatus::Incomplete);
    let missing = normalize(&[message(1, "Тема часть 2."), message(2, "Тема часть 3.")]);
    assert_eq!(missing.articles.len(), 1);
    assert_eq!(missing.articles[0].status, ArticleStatus::Incomplete);
    let no_total = normalize(&[message(1, "Тема часть 1."), message(2, "Тема часть 2.")]);
    assert_eq!(no_total.articles[0].status, ArticleStatus::Complete);
}

#[test]
fn only_visible_publications_become_posts_and_forwarding_is_preserved() {
    for kind in [
        MessageKind::Text,
        MessageKind::Photo,
        MessageKind::Video,
        MessageKind::Album,
        MessageKind::Poll,
        MessageKind::Service,
    ] {
        let mut post = message(1, "Подпись");
        post.kind = kind;
        post.forwarded_from = Some("RTVI".into());
        let result = normalize(&[post]);
        if matches!(kind, MessageKind::Poll | MessageKind::Service) {
            assert!(result.posts.is_empty());
        } else {
            assert_eq!(result.posts[0].forwarded_from.as_deref(), Some("RTVI"));
        }
    }
    assert!(normalize(&[message(1, " \n ")]).posts.is_empty());
    assert_eq!(normalize(&[message(1, "Подписаться.")]).posts.len(), 1);
}

#[test]
fn unknown_footer_spacing_does_not_remove_content() {
    let text = "Текст\nПод писаться";
    assert_eq!(normalize(&[message(1, text)]).posts[0].text, text);
}

#[test]
fn time_window_does_not_round_down_fractional_seconds() {
    let first = message(1, "Тема часть 1/2.");
    let mut second = message(2, "Тема часть 2/2.");
    second.published_at = first.published_at + chrono::Duration::milliseconds(86_400_001);
    assert_eq!(normalize(&[first, second]).articles.len(), 2);
}

#[test]
fn continuation_after_duplicate_uses_editorial_number_and_corrected_ordinal() {
    let result = normalize(&[
        message(1, "Тема часть 1/4."),
        message(2, "Тема часть 2/4."),
        message(3, "Тема часть 2/4."),
        message(4, "Тема часть 3/4."),
    ]);
    assert_eq!(result.articles.len(), 1);
    assert_eq!(
        result.posts.iter().map(|p| p.part_no).collect::<Vec<_>>(),
        [Some(1), Some(2), Some(3), Some(4)]
    );
    assert_eq!(result.articles[0].status, ArticleStatus::Complete);
}

#[test]
fn inline_content_keeps_its_own_punctuation() {
    let result = normalize(&[message(1, "Тема (часть 1): ...не всё ясно")]);
    assert_eq!(result.articles[0].search_text, "Тема\n\n...не всё ясно");
}

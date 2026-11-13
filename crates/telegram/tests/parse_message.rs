use briefly_searcher_telegram::{Message, MessageKind, parse_message};

fn block(id: i64, html: &str) -> Message {
    Message {
        id,
        payload: serde_json::json!({"html": html}),
        payload_schema: "t.me/s html v1".into(),
    }
}

#[test]
fn album_has_one_caption_and_all_message_ids() {
    let parsed =
        parse_message(&block(562, include_str!("fixtures/blocks/album-562.html"))).unwrap();
    assert_eq!(parsed.id, 562);
    assert_eq!(parsed.kind, MessageKind::Album);
    assert_eq!(parsed.album_ids, [562, 563, 564, 565]);
    assert_eq!(
        parsed.text,
        "NYT опубликовал фото бывших украинских военнослужащих- пациентов психиатрической больницы. Теперь они тяжело больные люди.\n\nПодписаться."
    );
}

#[test]
fn text_keeps_line_breaks_link_labels_and_publication_date() {
    let parsed = parse_message(&block(
        47354,
        include_str!("fixtures/blocks/text-47354.html"),
    ))
    .unwrap();
    assert_eq!(parsed.kind, MessageKind::Text);
    assert_eq!(
        parsed.published_at.to_rfc3339(),
        "2026-10-03T07:00:04+00:00"
    );
    assert!(parsed.text.starts_with("Зеленский попросил Трампа заблокировать российско-китайский аналог «Starlink» — The Financial Times.\nПрезидент Украины"));
    assert!(parsed.text.ends_with("Подписаться"));
    assert!(!parsed.text.contains("https://"));
    assert!(parsed.album_ids.is_empty());
    assert_eq!(parsed.reply_to, None);
    assert_eq!(parsed.forwarded_from, None);
}

#[test]
fn reply_keeps_target_but_excludes_quoted_text() {
    let parsed = parse_message(&block(
        4645,
        include_str!("fixtures/blocks/reply-4645.html"),
    ))
    .unwrap();
    assert_eq!(parsed.reply_to, Some(4644));
    assert!(
        parsed
            .text
            .starts_with("🇬🇧Лоуренс Фридман (часть 2): Именно вопрос")
    );
    assert!(
        !parsed
            .text
            .contains("В течение нескольких месяцев Путину говорили")
    );
}

#[test]
fn preview_keeps_link_labels_but_not_preview_or_urls() {
    let parsed = parse_message(&block(
        47340,
        include_str!("fixtures/blocks/preview-47340.html"),
    ))
    .unwrap();
    assert_eq!(parsed.kind, MessageKind::Text);
    assert!(
        parsed
            .text
            .contains("Хотя ракета, запущенная накануне, имеет ограниченную дальность")
    );
    assert!(!parsed.text.contains("BRIEFLY RU"));
    assert!(!parsed.text.contains("https://t.me/brieflyru/47285"));
}

#[test]
fn photo_and_forwarded_video_are_recognized() {
    let photo = parse_message(&block(
        47353,
        include_str!("fixtures/blocks/photo-47353.html"),
    ))
    .unwrap();
    assert_eq!(photo.kind, MessageKind::Photo);
    assert!(photo.text.starts_with("Новейшая российская пропаганда?"));
    let video = parse_message(&block(
        45999,
        include_str!("fixtures/blocks/video-45999.html"),
    ))
    .unwrap();
    assert_eq!(video.kind, MessageKind::Video);
    assert_eq!(video.forwarded_from.as_deref(), Some("RTVI"));
    assert!(video.text.starts_with("Интересные материалы за 2 сентября"));
}

#[test]
fn polls_and_service_messages_are_recognized() {
    let poll = parse_message(&block(
        40738,
        include_str!("fixtures/blocks/poll-40738.html"),
    ))
    .unwrap();
    assert_eq!(poll.kind, MessageKind::Poll);
    assert!(poll.text.is_empty());
    for (id, html) in [
        (1, include_str!("fixtures/blocks/service-1.html")),
        (34715, include_str!("fixtures/blocks/service-34715.html")),
    ] {
        let service = parse_message(&block(id, html)).unwrap();
        assert_eq!(service.kind, MessageKind::Service);
        if id == 1 {
            assert_eq!(service.text, "Channel created");
        }
    }
}

#[test]
fn unknown_schema_is_an_explicit_error() {
    let mut message = block(1, "");
    message.payload_schema = "future v2".into();
    assert_eq!(
        parse_message(&message),
        Err(briefly_searcher_telegram::ParseError::UnknownSchema(
            "future v2".into()
        ))
    );
}

#[test]
fn invalid_payload_and_unexpected_markup_are_errors() {
    let html = include_str!("fixtures/blocks/album-562.html");
    let mut missing_payload = block(562, html);
    missing_payload.payload = serde_json::json!({"html": null});
    assert!(parse_message(&missing_payload).is_err());
    for damaged in [
        "<div>unfamiliar markup</div>".to_owned(),
        html.replace("data-post=", "data-unknown="),
        html.replace("brieflyru/562\"", "brieflyru/0\""),
        html.replace("datetime=", "invalid-date="),
        html.replace("?single", "?unknown"),
        format!("{html}{html}"),
    ] {
        assert!(parse_message(&block(562, &damaged)).is_err());
    }
    assert!(parse_message(&block(563, html)).is_err());
}

#[test]
fn album_without_item_links_is_an_error_instead_of_partial_success() {
    assert!(
        parse_message(&block(
            35361,
            include_str!("fixtures/blocks/empty-album-35361.html")
        ))
        .is_err()
    );
    let html = include_str!("fixtures/blocks/album-562.html")
        .replace("href=\"https://t.me/brieflyru/563?single\"", "");
    assert!(parse_message(&block(562, &html)).is_err());
}

#[test]
fn recognized_photo_without_caption_has_empty_text() {
    let html = "<div class=\"tgme_widget_message\" data-post=\"brieflyru/9\"><a class=\"tgme_widget_message_photo_wrap\"></a><a class=\"tgme_widget_message_date\"><time datetime=\"2026-10-03T07:00:04Z\"></time></a></div>";
    let parsed = parse_message(&block(9, html)).unwrap();
    assert_eq!(parsed.kind, MessageKind::Photo);
    assert!(parsed.text.is_empty());
}

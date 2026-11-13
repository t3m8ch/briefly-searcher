use std::{fs, path::Path};

use briefly_searcher_normalizer::{ArticleStatus, normalize};
use briefly_searcher_telegram::{Message, parse_message};
use serde_json::{Value, json};

#[test]
fn candidates_from_history_match_reviewed_documents() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in [
        "complete",
        "interleaved",
        "inline",
        "duplicate",
        "reworded",
        "reply",
        "republication",
        "column",
        "conflict",
        "3661",
        "7016",
        "44349",
        "562",
        "service",
    ] {
        let expected: Value =
            serde_json::from_str(&fs::read_to_string(root.join(format!("{name}.json"))).unwrap())
                .unwrap();
        let messages = expected["blocks"].as_array().unwrap().iter().map(|id| {
            let id = id.as_i64().unwrap();
            parse_message(&Message { id, payload: json!({"html": fs::read_to_string(root.join(format!("{id}.html"))).unwrap()}), payload_schema: "t.me/s html v1".into() }).unwrap()
        }).collect::<Vec<_>>();
        let result = normalize(&messages);
        let posts: Vec<_> = result.posts.iter().map(|p| json!({"id":p.message_id,"article_id":p.article_id,"part_no":p.part_no,"text":p.text})).collect();
        let articles: Vec<_> = result.articles.iter().map(|a| json!({"id":a.id,"title":a.title,"status":match a.status {ArticleStatus::Single=>"single",ArticleStatus::Complete=>"complete",ArticleStatus::Incomplete=>"incomplete"},"declared_parts":a.declared_parts,"search_text":a.search_text})).collect();
        assert_eq!(json!(posts), expected["posts"], "posts: {name}");
        assert_eq!(json!(articles), expected["articles"], "articles: {name}");
        for post in &result.posts {
            let source = messages.iter().find(|m| m.id == post.message_id).unwrap();
            assert_eq!(post.published_at, source.published_at);
            assert_eq!(post.forwarded_from, source.forwarded_from);
        }
        for article in &result.articles {
            assert_eq!(
                article.published_at,
                messages
                    .iter()
                    .find(|m| m.id == article.id)
                    .unwrap()
                    .published_at
            );
        }
    }
}

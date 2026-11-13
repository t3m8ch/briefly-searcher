//! Диагностический прогон по внешнему pg_dump, никогда не запускается по умолчанию.

use briefly_searcher_normalizer::{ArticleStatus, normalize};
use briefly_searcher_telegram::{Message, parse_message};
use std::{
    env,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Instant,
};

#[test]
#[ignore = "нужны NORMALIZER_DUMP и pg_restore (или PG_RESTORE)"]
fn full_dump_summary() {
    let path =
        env::var_os("NORMALIZER_DUMP").expect("NORMALIZER_DUMP: путь к pg_dump custom-format");
    let started = Instant::now();
    let mut restore =
        Command::new(env::var_os("PG_RESTORE").unwrap_or_else(|| "pg_restore".into()))
            .args(["--data-only", "--table=raw_posts", "--file=-"])
            .arg(path)
            .stdout(Stdio::piped())
            .spawn()
            .expect("запустить pg_restore");
    let mut messages = vec![];
    let mut parse_errors = vec![];
    let mut in_copy = false;
    let mut blocks = 0;
    for line in BufReader::new(restore.stdout.take().unwrap()).lines() {
        let line = line.expect("читать дамп");
        if line.starts_with("COPY public.raw_posts ") {
            assert!(
                line.starts_with(
                    "COPY public.raw_posts (message_id, payload, payload_schema, fetched_at)"
                ),
                "неизвестный порядок колонок raw_posts"
            );
            in_copy = true;
            continue;
        }
        if line == "\\." {
            in_copy = false;
        }
        if !in_copy {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 4);
        let message = Message {
            id: fields[0].parse().unwrap(),
            payload: serde_json::from_str(&copy_text(fields[1])).unwrap(),
            payload_schema: copy_text(fields[2]),
        };
        blocks += 1;
        match parse_message(&message) {
            Ok(parsed) => messages.push(parsed),
            Err(error) => parse_errors.push(error.to_string()),
        }
    }
    assert!(
        restore.wait().unwrap().success(),
        "pg_restore завершился с ошибкой"
    );
    assert!(blocks > 0, "в дампе нет raw_posts");
    let parse_time = started.elapsed();
    let result = normalize(&messages);
    let count = |status| {
        result
            .articles
            .iter()
            .filter(|a| a.status == status)
            .count()
    };
    println!(
        "blocks={blocks} parsed={} parse_errors={} posts={} single={} complete={} incomplete={} parse_time={parse_time:?} total_time={:?}",
        messages.len(),
        parse_errors.len(),
        result.posts.len(),
        count(ArticleStatus::Single),
        count(ArticleStatus::Complete),
        count(ArticleStatus::Incomplete),
        started.elapsed()
    );
    for error in parse_errors {
        println!("parse_error: {error}");
    }
}

fn copy_text(text: &str) -> String {
    let mut chars = text.chars();
    let mut decoded = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            decoded.push(c);
            continue;
        }
        decoded.push(match chars.next().expect("COPY escape") {
            '\\' => '\\',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'v' => '\u{b}',
            other => panic!("неожиданный COPY escape: {other}"),
        });
    }
    decoded
}

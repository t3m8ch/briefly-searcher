//! Маркер ищется только в первой строке, по символьной границе 400.

pub(super) struct Marker {
    pub number: u32,
    pub total: Option<u32>,
    pub title: String,
    pub body: String,
}

impl Marker {
    pub fn parse(text: &str) -> Option<Self> {
        let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
        let chars: Vec<_> = first.char_indices().collect();
        let mut found = None;
        for (position, &(start, _)) in chars.iter().enumerate().take(400) {
            let tail = &first[start..];
            if !tail
                .chars()
                .take(5)
                .flat_map(char::to_lowercase)
                .eq("часть".chars())
            {
                continue;
            }
            if position > 0 && chars[position - 1].1.is_alphanumeric() {
                continue;
            }
            let after = &tail["часть".len()..];
            if !after.starts_with(char::is_whitespace) {
                continue;
            }
            let after = after.trim_start();
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            let Ok(number) = after[..digits].parse::<u32>() else {
                continue;
            };
            if !(1..=30).contains(&number) {
                continue;
            }
            let mut suffix = &after[digits..];
            let mut total = None;
            if suffix.starts_with('/') || suffix.starts_with('\\') {
                suffix = suffix
                    .strip_prefix("//")
                    .or_else(|| suffix.strip_prefix('/'))
                    .or_else(|| suffix.strip_prefix('\\'))
                    .expect("разделитель общего числа");
                let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
                total = suffix[..digits].parse::<u32>().ok().filter(|n| *n > 0);
                if total.is_none() {
                    continue;
                }
                suffix = &suffix[digits..];
            }
            if !suffix.is_empty()
                && !suffix.starts_with(['.', ':', ')'])
                && !suffix.trim().is_empty()
            {
                continue;
            }
            let title = first[..start]
                .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '(' | ',' | '.' | ':'))
                .to_owned();
            let suffix = suffix.strip_prefix(')').unwrap_or(suffix);
            let inline = suffix
                .strip_prefix(['.', ':'])
                .unwrap_or(suffix)
                .trim_start();
            let body = [inline, rest.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            found = Some(Self {
                number,
                total,
                title,
                body,
            });
        }
        found
    }
}

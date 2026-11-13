//! Маркер ищется только в первой строке, по символьной границе 400.

pub(super) struct Marker {
    pub number: u32,
    pub total: Option<u32>,
    pub title: String,
    pub body: String,
}

impl Marker {
    pub fn parse(text: &str) -> Option<Self> {
        let (first_line, remaining_lines) = text.split_once('\n').unwrap_or((text, ""));
        let mut last_marker = None;

        for (start, _) in first_line.char_indices().take(400) {
            let previous_char = first_line[..start].chars().next_back();
            if previous_char.is_some_and(char::is_alphanumeric) {
                continue;
            }
            if let Some(marker) = Self::parse_at(first_line, remaining_lines, start) {
                last_marker = Some(marker);
            }
        }
        last_marker
    }

    /// Разбирает один кандидат: «часть N», при наличии разделителя — «часть N/M».
    fn parse_at(first_line: &str, remaining_lines: &str, start: usize) -> Option<Self> {
        let after_keyword = strip_part_keyword(&first_line[start..])?;
        let (number, mut suffix) = parse_positive_number(after_keyword)?;
        if number > 30 {
            return None;
        }

        let total = if let Some(after_separator) = strip_total_separator(suffix) {
            let (total, remainder) = parse_positive_number(after_separator)?;
            suffix = remainder;
            Some(total)
        } else {
            None
        };
        if !suffix.trim().is_empty() && !suffix.starts_with(['.', ':', ')']) {
            return None;
        }

        Some(Self {
            number,
            total,
            title: title_before_marker(&first_line[..start]),
            body: body_after_marker(suffix, remaining_lines),
        })
    }
}

/// Слово «часть» без учёта регистра, за которым обязательно идёт пробел.
fn strip_part_keyword(text: &str) -> Option<&str> {
    let matches_keyword = text
        .chars()
        .take(5)
        .flat_map(char::to_lowercase)
        .eq("часть".chars());
    if !matches_keyword {
        return None;
    }
    let after_keyword = &text["часть".len()..];
    if !after_keyword.starts_with(char::is_whitespace) {
        return None;
    }
    Some(after_keyword.trim_start())
}

/// Число в начале строки и ещё не разобранный остаток.
fn parse_positive_number(text: &str) -> Option<(u32, &str)> {
    let digit_count = text.bytes().take_while(u8::is_ascii_digit).count();
    let number = text[..digit_count].parse().ok()?;
    if number == 0 {
        return None;
    }
    Some((number, &text[digit_count..]))
}

fn strip_total_separator(text: &str) -> Option<&str> {
    text.strip_prefix("//")
        .or_else(|| text.strip_prefix('/'))
        .or_else(|| text.strip_prefix('\\'))
}

fn title_before_marker(prefix: &str) -> String {
    prefix
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '(' | ',' | '.' | ':'))
        .to_owned()
}

fn body_after_marker(suffix: &str, remaining_lines: &str) -> String {
    // Снимаем только пунктуацию маркера. Например, после «(часть 1):»
    // многоточие в «...не всё ясно» уже принадлежит содержанию.
    let suffix = suffix.strip_prefix(')').unwrap_or(suffix);
    let inline_text = suffix
        .strip_prefix(['.', ':'])
        .unwrap_or(suffix)
        .trim_start();
    let remaining_lines = remaining_lines.trim();
    match (inline_text.is_empty(), remaining_lines.is_empty()) {
        (true, _) => remaining_lines.to_owned(),
        (_, true) => inline_text.to_owned(),
        (false, false) => format!("{inline_text}\n\n{remaining_lines}"),
    }
}

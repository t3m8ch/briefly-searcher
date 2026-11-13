//! Ratcliff/Obershelp: рекурсивно совпадающие непрерывные подстроки.
//! При равной длине выбирается самая ранняя позиция в первом, затем во втором
//! заголовке, как в SequenceMatcher (без эвристики popular characters).

pub(super) fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut ranges = vec![(0, a.len(), 0, b.len())];
    let mut matched = 0;
    while let Some((a_start, a_end, b_start, b_end)) = ranges.pop() {
        let mut previous = vec![0; b_end - b_start + 1];
        let (mut best_a, mut best_b, mut length) = (a_start, b_start, 0);
        for (i, ac) in a.iter().enumerate().take(a_end).skip(a_start) {
            let mut current = vec![0; previous.len()];
            for (j, bc) in b.iter().enumerate().take(b_end).skip(b_start) {
                if ac == bc {
                    let size = previous[j - b_start] + 1;
                    current[j - b_start + 1] = size;
                    if size > length {
                        (best_a, best_b, length) = (i + 1 - size, j + 1 - size, size);
                    }
                }
            }
            previous = current;
        }
        if length == 0 {
            continue;
        }
        matched += length;
        if best_a > a_start && best_b > b_start {
            ranges.push((a_start, best_a, b_start, best_b));
        }
        if best_a + length < a_end && best_b + length < b_end {
            ranges.push((best_a + length, a_end, best_b + length, b_end));
        }
    }
    2.0 * matched as f64 / (a.len() + b.len()) as f64
}

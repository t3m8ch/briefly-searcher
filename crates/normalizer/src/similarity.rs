//! Ratcliff/Obershelp: находим самый длинный общий непрерывный фрагмент,
//! затем повторяем поиск слева и справа от него. Доля совпадения равна
//! удвоенному числу совпавших символов, делённому на суммарную длину строк.
//!
//! При равной длине выбираем самый ранний фрагмент в первом, затем во втором
//! заголовке, как в SequenceMatcher (без эвристики popular characters).

pub(super) fn ratio(first: &str, second: &str) -> f64 {
    let first: Vec<_> = first.chars().collect();
    let second: Vec<_> = second.chars().collect();
    let total_length = first.len() + second.len();
    if total_length == 0 {
        return 1.0;
    }
    let matched_characters = count_matching_characters(&first, &second);
    2.0 * matched_characters as f64 / total_length as f64
}

struct TextPair<'a>(&'a [char], &'a [char]);

impl<'a> TextPair<'a> {
    fn unmatched_sides(&self, block: &MatchingBlock) -> [Self; 2] {
        let before = Self(&self.0[..block.first_start], &self.1[..block.second_start]);
        let after = Self(
            &self.0[block.first_start + block.length..],
            &self.1[block.second_start + block.length..],
        );
        [before, after]
    }

    fn has_both_texts(&self) -> bool {
        !self.0.is_empty() && !self.1.is_empty()
    }
}

#[derive(Default)]
struct MatchingBlock {
    first_start: usize,
    second_start: usize,
    length: usize,
}

fn count_matching_characters(first: &[char], second: &[char]) -> usize {
    let mut pending = vec![TextPair(first, second)];
    let mut matched_characters = 0;
    while let Some(pair) = pending.pop() {
        let Some(block) = longest_common_block(pair.0, pair.1) else {
            continue;
        };
        matched_characters += block.length;
        pending.extend(
            pair.unmatched_sides(&block)
                .into_iter()
                .filter(TextPair::has_both_texts),
        );
    }
    matched_characters
}

fn longest_common_block(first: &[char], second: &[char]) -> Option<MatchingBlock> {
    let mut longest = MatchingBlock::default();
    let mut previous_lengths = vec![0; second.len() + 1];
    for (first_index, first_char) in first.iter().enumerate() {
        let mut current_lengths = vec![0; second.len() + 1];
        for (second_index, second_char) in second.iter().enumerate() {
            if first_char != second_char {
                continue;
            }
            // Длина общего фрагмента, заканчивающегося на этих двух символах:
            // совпадение на предыдущих позициях продолжается ещё на один символ.
            let length = previous_lengths[second_index] + 1;
            current_lengths[second_index + 1] = length;
            // Строгое сравнение сохраняет самый ранний фрагмент при равенстве.
            if length > longest.length {
                longest = MatchingBlock {
                    first_start: first_index + 1 - length,
                    second_start: second_index + 1 - length,
                    length,
                };
            }
        }
        previous_lengths = current_lengths;
    }
    (longest.length > 0).then_some(longest)
}

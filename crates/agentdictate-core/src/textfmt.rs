use std::sync::OnceLock;

/// Formats whole seconds for overview usage labels such as `3m 07s`.
///
/// The desktop overview uses explicit unit suffixes because the value appears
/// beside other labeled activity totals.
#[must_use]
pub fn format_duration_words(seconds: u64) -> String {
    format!("{}m {:02}s", seconds / 60, seconds % 60)
}

/// Formats a measured duration for compact workspace rows such as `3:07`.
///
/// Workspace history rounds measured seconds and clamps negative values before
/// displaying the compact clock form.
#[must_use]
pub fn format_duration_clock(seconds: f64) -> String {
    let seconds = seconds.round().max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Counts stable ASCII history tokens, including underscores and one internal
/// apostrophe or hyphen.
///
/// Runtime history and usage persistence use this narrower expression to keep
/// stored metrics compatible with existing records.
#[must_use]
pub fn count_words_ascii_history(text: &str) -> u64 {
    static WORD_EXPRESSION: OnceLock<regex::Regex> = OnceLock::new();
    WORD_EXPRESSION
        .get_or_init(|| {
            regex::Regex::new(r"[A-Za-z0-9_]+(?:[-'][A-Za-z0-9_]+)?")
                .expect("word-count expression is valid")
        })
        .find_iter(text)
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_words_keep_explicit_units() {
        assert_eq!(format_duration_words(0), "0m 00s");
        assert_eq!(format_duration_words(187), "3m 07s");
        assert_eq!(format_duration_words(3_607), "60m 07s");
    }

    #[test]
    fn duration_clock_rounds_and_clamps_measured_seconds() {
        assert_eq!(format_duration_clock(-1.0), "0:00");
        assert_eq!(format_duration_clock(186.49), "3:06");
        assert_eq!(format_duration_clock(186.5), "3:07");
    }

    #[test]
    fn history_word_counts_keep_internal_apostrophes_and_hyphens() {
        assert_eq!(count_words_ascii_history("don't ship-it"), 2);
        assert_eq!(count_words_ascii_history("你好 world"), 1);
    }
}

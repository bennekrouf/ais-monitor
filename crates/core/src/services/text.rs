//! Character-safe string shortening.
//!
//! Rust's `&s[..n]` indexes *bytes* and panics when `n` lands inside a
//! multi-byte character. Every ad-hoc `format!("{}…", &s[..8])` in a UI is
//! therefore a latent crash waiting for the first non-ASCII input — and in a
//! Dioxus render a panic unwinds through the VirtualDom and takes the window
//! with it. These helpers count characters and never panic.

/// First `max` characters of `s`, with an ellipsis appended if anything was
/// dropped. Returns `s` unchanged when it already fits.
pub fn head(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Shorten to at most `max` characters by eliding the middle, keeping `tail`
/// characters of the end — the shape that keeps a URL's host *and* its last
/// path segment readable.
pub fn middle_elide(s: &str, max: usize, tail: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    // Guard the arithmetic rather than trusting callers: `tail >= max` would
    // otherwise underflow the head width.
    let tail = tail.min(max.saturating_sub(1));
    let head_len = max.saturating_sub(tail);
    let head: String = s.chars().take(head_len).collect();
    let end: String = s.chars().skip(len - tail).collect();
    format!("{head}…{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_string_is_returned_unchanged() {
        assert_eq!(head("abc", 8), "abc");
        assert_eq!(middle_elide("https://x.test/a", 80, 30), "https://x.test/a");
    }

    #[test]
    fn a_long_string_is_shortened() {
        assert_eq!(head("abcdefghij", 4), "abcd…");
        assert_eq!(middle_elide(&"a".repeat(200), 70, 30).chars().count(), 71);
    }

    /// The whole reason these exist: `&s[..40]` on this input panics, because
    /// byte 40 is in the middle of a character.
    #[test]
    fn a_multibyte_boundary_does_not_panic() {
        let url = format!("https://example.test/{}", "é".repeat(60));
        assert!(url.len() > 80, "input must be long enough to be elided");
        let out = middle_elide(&url, 70, 30);
        assert!(out.contains('…'));
        assert_eq!(out.chars().count(), 71);
        assert_eq!(head(&"é".repeat(20), 8).chars().count(), 9);
    }

    /// Emoji are several bytes and one character; the count is characters.
    #[test]
    fn counting_is_by_character_not_byte() {
        assert_eq!(head("🚀🚀🚀🚀", 2), "🚀🚀…");
    }

    #[test]
    fn a_tail_wider_than_the_budget_does_not_underflow() {
        let out = middle_elide("abcdefghijklmnop", 4, 99);
        assert!(out.chars().count() <= 5, "got {out}");
    }
}

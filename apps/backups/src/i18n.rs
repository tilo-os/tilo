//! Backups' Fluent catalogue: every string that carries a count.
//!
//! The catalogue is embedded with `include_str!`, so a capture, a unit test
//! and a live session resolve the same strings with no runtime file I/O.

use ui::ArgValue;

ui::embedded_catalog!(pub fn, ["en-US"]);

/// Resolve a counted string: `count` picks the plural form and is offered
/// to the message both as `$count` and, digit-grouped, as `$files` and
/// `$snapshots`; `size` fills `$size`.
pub fn counted(key: &str, count: u64, size: &str) -> String {
    let grouped = group_digits(count);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a count only selects a plural form; exactness past 2^52 is irrelevant"
    )]
    let number = count as f64;
    CATALOG.t_args(
        key,
        &[
            ("count", ArgValue::Number(number)),
            ("files", ArgValue::Text(grouped.clone())),
            ("snapshots", ArgValue::Text(grouped)),
            ("size", ArgValue::Text(size.to_owned())),
        ],
    )
}

/// `1840` → `1,840`.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_strings_choose_their_plural_form() {
        assert_eq!(t("backups-health-ok"), "No failed backups");
        let one = counted("backups-health-attention", 1, "");
        let two = counted("backups-health-attention", 2, "");
        assert!(one.contains("One backup needs"), "{one}");
        assert!(two.contains("backups need"), "{two}");
        let files = counted("backups-changed-files", 1840, "1.1 GB");
        assert!(files.contains("1,840 files"), "{files}");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }
}

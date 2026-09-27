//! Char ↔ byte offset conversion for UTF-8 document text.
//!
//! Shared by the page widget (caret geometry, selection paint) and the find
//! bar (match indices). Both need the same rule: a mid-code-unit offset is
//! treated as the nearest preceding char boundary, never as "zero" or a panic.

/// Byte index of the `char_off`-th Unicode scalar, or `text.len()` if the
/// offset is past the end.
pub(crate) fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices()
        .nth(char_off)
        .map_or(text.len(), |(b, _)| b)
}

/// Number of Unicode scalars that start before `byte_off`.
///
/// Iterator-based rather than `text[..b]`: `byte_off` isn't guaranteed to land
/// on a char boundary, and this counts every char that starts before it
/// without ever slicing through one.
pub(crate) fn byte_to_char(text: &str, byte_off: usize) -> usize {
    let b = byte_off.min(text.len());
    text.char_indices().take_while(|&(i, _)| i < b).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trip() {
        let text = "hello";
        assert_eq!(char_to_byte(text, 0), 0);
        assert_eq!(char_to_byte(text, 5), 5);
        assert_eq!(byte_to_char(text, 0), 0);
        assert_eq!(byte_to_char(text, 5), 5);
    }

    #[test]
    fn multibyte_char_and_mid_code_unit() {
        let text = "a😀b";
        assert_eq!(char_to_byte(text, 1), 1);
        assert_eq!(char_to_byte(text, 2), 5);
        assert_eq!(byte_to_char(text, 1), 1);
        assert_eq!(byte_to_char(text, 5), 2);
        // Mid-emoji byte: count the emoji as started, not as zero.
        assert_eq!(byte_to_char(text, 3), 2);
    }
}

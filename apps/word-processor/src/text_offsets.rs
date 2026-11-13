//! Char and byte offsets in UTF-8 document text: a mid-code-unit offset counts as the
//! nearest preceding char boundary, never as zero or a panic.

pub(crate) fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices()
        .nth(char_off)
        .map_or(text.len(), |(b, _)| b)
}

/// Number of Unicode scalars that start before `byte_off`, which need not land on a
/// char boundary.
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
        assert_eq!(byte_to_char(text, 3), 2);
    }
}

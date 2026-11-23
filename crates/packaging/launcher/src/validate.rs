//! Frozen-shape validators shared by the retained-view and installed-copy protocols.

/// A lower-case 64-character hex SHA-256.
pub(crate) fn digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A canonical lower-case hyphenated UUID, exactly as the `uuid` crate prints it.
pub(crate) fn uuid(text: &str) -> bool {
    uuid::Uuid::parse_str(text).is_ok_and(|id| id.to_string() == text)
}

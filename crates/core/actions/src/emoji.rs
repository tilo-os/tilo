//! The name a workspace emoji is written by.

/// The longest workspace emoji name, in characters.
pub const MAX_CUSTOM_NAME: usize = 32;

/// The name of the workspace emoji `emoji` writes as `:name:`, when it is
/// one.
#[must_use]
pub fn custom_emoji_name(emoji: &str) -> Option<&str> {
    emoji
        .strip_prefix(':')
        .and_then(|rest| rest.strip_suffix(':'))
        .filter(|name| valid_custom_emoji_name(name))
}

/// Whether `name` can name a workspace emoji: 2 to 32 ASCII letters, digits
/// and underscores, as Discord names its custom emoji, so the same name
/// works on both sides of the bridge.
#[must_use]
pub fn valid_custom_emoji_name(name: &str) -> bool {
    (2..=MAX_CUSTOM_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

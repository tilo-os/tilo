//! The key a drive names (`{"key": "Enter"}`): the toolkit's one key spelling.

use winit::keyboard::Key;

/// The key `spec` names: `Enter`, `Tab`, `Escape`, the arrows, `F1`–`F12`, `Home`, `End`, … or, failing
/// those, the character itself.
pub(super) fn parse_key(spec: &str) -> Key {
    crate::keys::parse(spec)
}

//! The key an X11 or Wayland keysym names.
//!
//! A host that reads the keyboard itself (X11, Wayland, a KMS session through
//! libinput and xkbcommon) delivers a press as a keysym; the vocabulary is
//! winit's own, and a build that runs no such host has no keysyms.

use winit::keyboard::{Key, KeyLocation};

/// The key `keysym` names and where on the keyboard it sits; `None` where
/// this build has no X11 or Wayland host, so no keysym names anything here.
///
/// A keysym naming no key winit knows is [`Key::Unidentified`]: the caller
/// takes the key's text instead.
#[cfg(target_os = "linux")]
#[must_use]
pub fn keysym_key(keysym: u32) -> Option<(Key, KeyLocation)> {
    Some(winit::platform::scancode::keysym_to_key(keysym))
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub const fn keysym_key(_keysym: u32) -> Option<(Key, KeyLocation)> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use winit::keyboard::NamedKey;

    #[test]
    fn a_named_keysym_names_the_key_winit_names() {
        let (key, _) = keysym_key(0xff0d).expect("no host lacks keysyms here");
        assert_eq!(key, Key::Named(NamedKey::Enter));
    }

    #[test]
    fn a_typed_character_is_no_named_key() {
        let (key, _) = keysym_key(u32::from('с')).expect("no host lacks keysyms here");
        assert!(matches!(key, Key::Unidentified(_)), "{key:?}");
    }
}

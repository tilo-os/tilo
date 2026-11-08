//! Dead keys and the Compose key: what a run of keys types together.
//! xkb turns a key into a keysym, and some keysyms type nothing on their own: `dead_acute` and
//! `Multi_key` open a sequence that the keys after them finish (`´` then `e` is `é` , Compose then
//! `o` `c` is `©` ). A client holding the keymap answers that itself, from the same compose table;
//! a hosted surface types through the keys the compositor resolves, so the compositor answers for
//! it here, from the compose table of the session's locale — the sequences the person's own apps
//! know.

use std::ffi::OsString;

use smithay::input::keyboard::{Keysym, xkb};

/// The session locale's compose table, and the sequence in progress.
pub(crate) struct Compose {
    /// The table is compiled from it.
    _context: xkb::Context,
    state: xkb::compose::State,
}

impl Compose {
    /// The compose table for the locale the session runs in; `None` when there is none for it, and
    /// keys then type as they are.
    pub(crate) fn for_session_locale() -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let locale = session_locale();
        let table =
            xkb::compose::Table::new_from_locale(&context, &locale, xkb::compose::COMPILE_NO_FLAGS)
                .inspect_err(|()| {
                    tracing::warn!(
                        ?locale,
                        "no compose table for the session's locale: dead keys and the Compose key \
                 type nothing"
                    );
                })
                .ok()?;
        Some(Self {
            _context: context,
            state: xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS),
        })
    }

    /// What pressing the key with `keysym` types, given the character the layout gives it on its
    /// own: nothing while a sequence is open, the text the sequence finishes with, and the key's
    /// own character otherwise — a key that does not continue the sequence types as it is, as it
    /// does in the person's apps.
    pub(crate) fn typed(&mut self, keysym: Keysym, own: Option<char>) -> Option<String> {
        use xkb::compose::{FeedResult, Status};
        if self.state.feed(keysym) == FeedResult::Ignored {
            // Modifiers and keys with no character take no part in a sequence, and leave the open
            // one alone.
            return own.map(String::from);
        }
        match self.state.status() {
            Status::Nothing => own.map(String::from),
            Status::Composing => None,
            Status::Composed => {
                let text = self
                    .state
                    .utf8()
                    .filter(|text| !text.is_empty())
                    .or_else(|| {
                        self.state
                            .keysym()
                            .and_then(Keysym::key_char)
                            .map(String::from)
                    });
                self.state.reset();
                text
            }
            Status::Cancelled => {
                self.state.reset();
                own.map(String::from)
            }
        }
    }
}

/// The locale the session runs in, as the compose table is looked up by.
fn session_locale() -> OsString {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| OsString::from("C"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of this test's own, so what a sequence types does not depend on the locale data the
    /// machine running it has.
    fn compose() -> Compose {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let sequences = "# a dead key and the Compose key, as the locale's table has them\n\
                        <dead_acute> <e> : \"é\"\n\
                        <Multi_key> <o> <c> : \"©\"\n";
        let table = xkb::compose::Table::new_from_buffer(
            &context,
            sequences,
            "C",
            xkb::compose::FORMAT_TEXT_V1,
            xkb::compose::COMPILE_NO_FLAGS,
        )
        .expect("the table compiles");
        Compose {
            _context: context,
            state: xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS),
        }
    }

    #[test]
    fn a_dead_key_and_the_key_it_accents_type_together() {
        let mut compose = compose();
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_dead_acute), None),
            None
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_e), Some('e')),
            Some("é".to_owned())
        );
    }

    #[test]
    fn a_compose_key_opens_the_sequence_the_keys_after_it_finish() {
        let mut compose = compose();
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_Multi_key), None),
            None
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_o), Some('o')),
            None
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_c), Some('c')),
            Some("©".to_owned())
        );
    }

    #[test]
    fn a_key_the_sequence_does_not_know_types_its_own_character() {
        let mut compose = compose();
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_dead_acute), None),
            None
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_x), Some('x')),
            Some("x".to_owned()),
            "the accent takes nothing the letter typed"
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_a), Some('a')),
            Some("a".to_owned()),
            "and leaves nothing open behind"
        );
    }

    #[test]
    fn a_modifier_between_the_keys_of_a_sequence_leaves_it_open() {
        let mut compose = compose();
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_dead_acute), None),
            None
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_Shift_L), None),
            None
        );
        assert_eq!(
            compose.typed(xkb::Keysym::new(xkb::keysyms::KEY_e), Some('e')),
            Some("é".to_owned())
        );
    }
}

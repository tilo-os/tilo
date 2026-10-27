//! Asking for someone's card: a click on a person anywhere (an author in
//! Chat, a member in its list) opens the one card the desktop shows for
//! them, with who they are, what they are doing, their id, Message and
//! Follow.
//!
//! An app knows people by their root key (hex), the id that stays theirs on
//! every device; it asks here ([`show`]) and the desktop hosting it takes
//! the ask on its next frame ([`take`]), anchoring the card where the
//! pointer is. Only the latest ask counts.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Someone whose card was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// Their root key (hex), or another id the host knows them by.
    pub person: String,
    /// The name the asker shows them by, for a card of someone not here.
    pub name: String,
}

static ASKED: Mutex<Option<Asked>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn asked() -> std::sync::MutexGuard<'static, Option<Asked>> {
    ASKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open `person`'s card, as `name` shows them.
pub fn show(person: &str, name: &str) {
    *asked() = Some(Asked {
        person: person.to_owned(),
        name: name.to_owned(),
    });
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// The card asked for since the last take, if any.
pub fn take() -> Option<Asked> {
    asked().take()
}

/// Changes with every ask: a host wakes for it.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_latest_ask_is_taken_once() {
        let before = super::generation();
        super::show("ada-key", "Ada");
        super::show("bo-key", "Bo");
        assert!(super::generation() > before);
        let asked = super::take().expect("asked");
        assert_eq!(
            (asked.person.as_str(), asked.name.as_str()),
            ("bo-key", "Bo")
        );
        assert!(super::take().is_none());
    }
}

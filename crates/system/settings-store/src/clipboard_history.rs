//! This device's clipboard history, and any file paste it was in the middle
//! of.
//!
//! Copies are kept so they survive the session and a restart: the history
//! the person can paste from again, with what they pinned. A copy is one
//! format's bytes, text as UTF-8 or one binary format; secret copies never
//! reach this section (the compositor drops them), so nothing here needs to
//! decide what may be kept.
//!
//! A paste of files is a journal, not a wish: every item is written down
//! before it is touched and marked done after, so a session that dies in the
//! middle of one finishes it when it starts again instead of leaving half a
//! folder behind unexplained.
//!
//! ```json
//! "clipboard": {
//!   "history": { "copies": [{ "mime": "text/plain;charset=utf-8",
//!                             "bytes": "aGVsbG8=", "created_ms": 1761900000000,
//!                             "pinned": true, "from": null }] },
//!   "paste": { "moving": true, "destination": "/home/Documents",
//!              "started_ms": 1761900000000,
//!              "items": [{ "source": "/home/a.txt",
//!                          "target": "/home/Documents/a.txt",
//!                          "state": "pending" }] }
//! }
//! ```

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::Key;

/// The section holding this device's clipboard.
pub const SECTION: &str = "clipboard";

/// The most copies the history keeps.
pub const MAX_COPIES: usize = 50;
/// The largest one copy's payload the history keeps, in bytes.
pub const MAX_COPY_BYTES: usize = 256 * 1024;
/// The largest payload the whole history keeps, in bytes.
pub const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
/// The most files one paste carries.
pub const MAX_PASTE_ITEMS: usize = 4096;
/// The longest path one paste names.
pub const MAX_PATH_BYTES: usize = 4096;

/// One copy in the history: one format's bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredCopy {
    /// The format its bytes are in.
    pub mime: String,
    /// Its bytes, base64: the document is JSON.
    pub bytes: String,
    /// When it was copied, in Unix milliseconds.
    pub created_ms: u64,
    /// Whether the person keeps it when the history is cleared.
    pub pinned: bool,
    /// The device it came from, when another of the person's devices shared
    /// it; absent for a copy made here.
    #[serde(default)]
    pub from: Option<String>,
}

impl StoredCopy {
    /// The copy with payload bytes `bytes` of format `mime`.
    #[must_use]
    pub fn of(mime: &str, bytes: &[u8], created_ms: u64, pinned: bool, from: Option<&str>) -> Self {
        Self {
            mime: mime.to_owned(),
            bytes: BASE64.encode(bytes),
            created_ms,
            pinned,
            from: from.map(str::to_owned),
        }
    }

    /// The payload bytes; empty when the stored text is not base64.
    #[must_use]
    pub fn payload(&self) -> Vec<u8> {
        BASE64.decode(self.bytes.as_bytes()).unwrap_or_default()
    }
}

/// The copies kept, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredHistory {
    #[serde(default)]
    pub copies: Vec<StoredCopy>,
}

impl StoredHistory {
    /// The copies this section holds, in order, with what could not be kept
    /// (an unknown field, a payload over the limit) dropped.
    #[must_use]
    pub fn kept(&self) -> Vec<StoredCopy> {
        let mut total = 0usize;
        let mut kept = Vec::new();
        for copy in &self.copies {
            let bytes = copy.payload();
            let size = bytes.len();
            if copy.mime.is_empty()
                || size == 0
                || size > MAX_COPY_BYTES
                || total.saturating_add(size) > MAX_HISTORY_BYTES
            {
                continue;
            }
            total = total.saturating_add(size);
            kept.push(copy.clone());
            if kept.len() == MAX_COPIES {
                break;
            }
        }
        kept
    }
}

/// Whether one item of a paste has been carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemState {
    /// Written down, not yet copied.
    Pending,
    /// Copied to its target; for a move, not yet taken from its source.
    Copied,
    /// A move's source is gone.
    Moved,
}

impl ItemState {
    /// Whether the item's target has its bytes.
    #[must_use]
    pub const fn copied(self) -> bool {
        matches!(self, Self::Copied | Self::Moved)
    }
}

/// One file of a paste.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredItem {
    pub source: String,
    pub target: String,
    pub state: ItemState,
}

/// A paste of files, as far as it got: written before its first item is
/// touched and after each one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPaste {
    /// Whether the sources go away once they are copied.
    pub moving: bool,
    /// The folder the items land in.
    pub destination: String,
    pub items: Vec<StoredItem>,
    /// When it started, in Unix milliseconds.
    pub started_ms: u64,
}

impl StoredPaste {
    /// Whether any item is still to do.
    #[must_use]
    pub fn unfinished(&self) -> bool {
        self.items.iter().any(|item| match item.state {
            ItemState::Pending => true,
            ItemState::Copied => self.moving,
            ItemState::Moved => false,
        })
    }

    /// The paste with `item` (by its place) in `state`.
    #[must_use]
    pub fn with(&self, index: usize, state: ItemState) -> Self {
        let mut paste = self.clone();
        if let Some(item) = paste.items.get_mut(index) {
            item.state = state;
        }
        paste
    }
}

/// The history, newest first.
pub const HISTORY: Key<StoredHistory> = Key::new(SECTION, "history", StoredHistory::default);

/// The paste in flight, if one is.
pub const PASTE: Key<Option<StoredPaste>> = Key::new(SECTION, "paste", || None);

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(text: &str, pinned: bool) -> StoredCopy {
        StoredCopy::of(
            "text/plain;charset=utf-8",
            text.as_bytes(),
            1_761_900_000_000,
            pinned,
            None,
        )
    }

    #[test]
    fn a_copy_survives_the_document_and_pinned_ones_are_marked() {
        let sealed = StoredHistory {
            copies: vec![copy("hello", true), copy("bye", false)],
        };
        let document = serde_json::to_value(&sealed).expect("stored");
        let back: StoredHistory = serde_json::from_value(document).expect("read");
        assert_eq!(back, sealed);
        assert_eq!(back.kept()[0].payload(), b"hello");
        assert!(back.kept()[0].pinned);
        assert_eq!(back.kept()[1].payload(), b"bye");
    }

    #[test]
    fn what_cannot_be_kept_is_dropped_and_the_rest_stays_in_order() {
        let huge = StoredCopy::of("image/png", &vec![b'x'; MAX_COPY_BYTES + 1], 1, false, None);
        let empty = StoredCopy::of("text/plain", b"", 2, false, None);
        let nameless = StoredCopy::of("", b"x", 3, false, None);
        let not_base64 = StoredCopy {
            mime: "text/plain".into(),
            bytes: "not base64!".into(),
            created_ms: 4,
            pinned: false,
            from: None,
        };
        let history = StoredHistory {
            copies: vec![
                copy("first", true),
                huge,
                empty,
                nameless,
                not_base64,
                copy("last", false),
            ],
        };
        let kept = history.kept();
        let texts: Vec<String> = kept
            .iter()
            .map(|copy| String::from_utf8_lossy(&copy.payload()).into_owned())
            .collect();
        assert_eq!(texts, ["first", "last"]);
    }

    #[test]
    fn a_paste_is_unfinished_exactly_while_it_has_something_left() {
        let item = |state| StoredItem {
            source: "/home/a.txt".into(),
            target: "/home/Documents/a.txt".into(),
            state,
        };
        let paste = |moving, state| StoredPaste {
            moving,
            destination: "/home/Documents".into(),
            items: vec![item(state)],
            started_ms: 1_761_900_000_000,
        };
        assert!(paste(false, ItemState::Pending).unfinished());
        assert!(!paste(false, ItemState::Copied).unfinished());
        assert!(paste(true, ItemState::Copied).unfinished());
        assert!(!paste(true, ItemState::Moved).unfinished());
        assert_eq!(
            paste(true, ItemState::Copied).with(0, ItemState::Moved),
            paste(true, ItemState::Moved)
        );
        let document = serde_json::to_value(paste(true, ItemState::Pending)).expect("stored");
        assert_eq!(
            serde_json::from_value::<StoredPaste>(document).expect("read"),
            paste(true, ItemState::Pending)
        );
    }

    #[test]
    fn the_journal_and_the_history_are_this_devices() {
        assert_eq!(HISTORY.section(), SECTION);
        assert_eq!(PASTE.section(), SECTION);
        assert!(HISTORY.default_value().copies.is_empty());
        assert_eq!(PASTE.default_value(), None);
    }
}

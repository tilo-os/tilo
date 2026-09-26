//! The per-user recently-used list.

use serde::{Deserialize, Serialize};

use crate::SkinTone;

/// Recently used emoji, most recent first, deduplicated and capped.
///
/// Two spellings of one emoji — a different skin tone, or with and without
/// U+FE0F — count as the same entry, and the latest spelling wins. The type
/// serialises as `{"capacity": n, "items": [...]}` for private per-user sync;
/// a deserialised value is re-normalised, so a hand-edited or merged list
/// still honours the cap and dedup rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Wire")]
pub struct Recents {
    capacity: usize,
    items: Vec<String>,
}

#[derive(Deserialize)]
struct Wire {
    capacity: usize,
    #[serde(default)]
    items: Vec<String>,
}

impl From<Wire> for Recents {
    fn from(wire: Wire) -> Self {
        let mut recents = Self::new(wire.capacity);
        // Oldest first, so the first stored item ends up most recent.
        for item in wire.items.iter().rev() {
            recents.record(item);
        }
        recents
    }
}

impl Default for Recents {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

/// Identity for dedup: no tone modifiers, no presentation selectors.
fn identity(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|&c| c != '\u{FE0F}' && SkinTone::from_modifier(c).is_none())
}

impl Recents {
    /// Four rows of a typical eight-column picker.
    pub const DEFAULT_CAPACITY: usize = 32;

    /// An empty list holding at most `capacity` (at least one) emoji.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            items: Vec::new(),
        }
    }

    /// Mark `emoji` as just used: it moves to the front, replacing any other
    /// spelling of it, and the oldest entry falls off past the cap.
    pub fn record(&mut self, emoji: &str) {
        let emoji = emoji.trim();
        if emoji.is_empty() {
            return;
        }
        self.items
            .retain(|item| !identity(item).eq(identity(emoji)));
        self.items.insert(0, emoji.to_owned());
        self.items.truncate(self.capacity);
    }

    /// Drop `emoji` (any spelling of it).
    pub fn forget(&mut self, emoji: &str) {
        self.items
            .retain(|item| !identity(item).eq(identity(emoji)));
    }

    /// The emoji, most recent first.
    #[must_use]
    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// The cap.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing has been recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

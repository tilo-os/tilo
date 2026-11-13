//! The per-user recently-used list.

use serde::{Deserialize, Serialize};

use crate::SkinTone;

/// Recently used emoji, most recent first, deduplicated and capped. Two
/// spellings of one emoji — a different skin tone, or with and without U+FE0F —
/// count as one entry, and the latest spelling wins. Serialises as
/// `{"capacity": n, "items": [...]}`; a deserialised value is re-normalised.
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

fn identity(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|&c| c != '\u{FE0F}' && SkinTone::from_modifier(c).is_none())
}

impl Recents {
    pub const DEFAULT_CAPACITY: usize = 32;

    /// An empty list holding at most `capacity` (at least one) emoji.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            items: Vec::new(),
        }
    }

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

    pub fn forget(&mut self, emoji: &str) {
        self.items
            .retain(|item| !identity(item).eq(identity(emoji)));
    }

    #[must_use]
    pub fn items(&self) -> &[String] {
        &self.items
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

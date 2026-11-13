//! What the person has learned of the desktop's own controls, so each hint teaches one thing once,
//! whichever of their devices they are on; and whether hints show at all.

use crate::Key;
use std::collections::BTreeMap;

pub const SECTION: &str = "control_hints";

pub const LEARNED: Key<Learned> = Key::new(SECTION, "learned", Learned::default);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Learned {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Hints the person has shown they know, by name, sorted.
    #[serde(default)]
    pub known: Vec<String>,
    #[serde(default)]
    pub shown: BTreeMap<String, u8>,
}

fn enabled_by_default() -> bool {
    true
}

impl Default for Learned {
    fn default() -> Self {
        Self {
            enabled: true,
            known: Vec::new(),
            shown: BTreeMap::new(),
        }
    }
}

impl Learned {
    #[must_use]
    pub fn knows(&self, id: &str) -> bool {
        self.known.iter().any(|known| known == id)
    }

    #[must_use]
    pub fn showings(&self, id: &str) -> u8 {
        self.shown.get(id).copied().unwrap_or(0)
    }

    pub fn learn(&mut self, id: &str) {
        if !self.knows(id) {
            self.known.push(id.to_owned());
            self.known.sort();
        }
    }

    pub fn note_shown(&mut self, id: &str) {
        let count = self.shown.entry(id.to_owned()).or_insert(0);
        *count = count.saturating_add(1);
    }

    /// Fold in the record as another device left it: what either knows is
    /// known and the higher count of showings stands. Whether hints show is
    /// the other record's: the newer one is folded in.
    pub fn merge(&mut self, other: &Self) {
        for id in &other.known {
            self.learn(id);
        }
        for (id, count) in &other.shown {
            let mine = self.shown.entry(id.clone()).or_insert(0);
            *mine = (*mine).max(*count);
        }
        self.enabled = other.enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_keeps_what_either_device_learned() {
        let mut here = Learned::default();
        here.learn("move");
        here.note_shown("open");
        let mut there = Learned::default();
        there.learn("open");
        there.note_shown("open");
        there.note_shown("open");
        there.enabled = false;
        here.merge(&there);
        assert!(here.knows("move") && here.knows("open"));
        assert_eq!(here.showings("open"), 2);
        assert!(!here.enabled);
    }
}

//! Type-ahead in a list: typing the first letters of a name moves to it.
//!
//! Every list of names a person scans (a folder in Files, the file chooser)
//! answers typing the same way: letters typed in quick succession spell the
//! start of a name and move to the first row it begins; after a pause the
//! next letter starts over. Typing one letter again and again steps through
//! the rows that begin with it. The list keeps its rows and its cursor; this
//! keeps what was typed and says where to go.

use crate::time::{Duration, Instant};

/// A pause longer than this starts a new name.
pub const PAUSE: Duration = Duration::from_millis(1000);

/// What has been typed into a list so far.
#[derive(Clone, Debug, Default)]
pub struct TypeAhead {
    typed: String,
    last: Option<Instant>,
}

impl TypeAhead {
    /// Add `text` (a key's characters) to what is being typed at `now`,
    /// starting over after a pause.
    pub fn type_text(&mut self, text: &str, now: Instant) {
        if self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) > PAUSE)
        {
            self.typed.clear();
        }
        self.typed.extend(text.chars().flat_map(char::to_lowercase));
        self.last = Some(now);
    }

    /// Forget what was typed: the list changed under it.
    pub fn reset(&mut self) {
        self.typed.clear();
        self.last = None;
    }

    /// What has been typed, lowercased.
    #[must_use]
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// Whether a name is being typed at `now`: something was typed and the
    /// pause that starts over has not passed. A space continues such a name;
    /// otherwise it is the list's own key.
    #[must_use]
    pub fn is_typing(&self, now: Instant) -> bool {
        !self.typed.is_empty()
            && self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) <= PAUSE)
    }

    /// The row to move to among `labels`, given the row the cursor is on.
    ///
    /// A longer name refines from the current row, which stays put while it
    /// still matches; one letter repeated steps to the next row beginning
    /// with it. Matching ignores case and wraps around. `None` when no row
    /// begins with what was typed.
    #[must_use]
    pub fn target<'a>(
        &self,
        labels: impl IntoIterator<Item = &'a str>,
        current: Option<usize>,
    ) -> Option<usize> {
        let labels: Vec<&str> = labels.into_iter().collect();
        let n = labels.len();
        let first = self.typed.chars().next()?;
        let repeated = self.typed.chars().all(|c| c == first);
        let prefix = if repeated {
            first.to_string()
        } else {
            self.typed.clone()
        };
        let start = match current {
            // One letter, or the same letter again: the next row with it.
            Some(current) if repeated => current.saturating_add(1),
            Some(current) => current,
            None => 0,
        };
        (0..n)
            .map(|step| start.saturating_add(step) % n)
            .find(|&index| {
                labels
                    .get(index)
                    .is_some_and(|label| label.to_lowercase().starts_with(&prefix))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 5] = ["Apple", "apricot", "Banana", "berry", "Cherry"];

    #[test]
    fn typing_spells_a_name_and_a_pause_starts_over() {
        let t0 = crate::time::now();
        let mut typing = TypeAhead::default();
        typing.type_text("a", t0);
        assert_eq!(typing.target(NAMES, None), Some(0));
        typing.type_text("p", t0);
        typing.type_text("r", t0);
        assert_eq!(typing.target(NAMES, Some(0)), Some(1), "refines to apricot");
        typing.type_text("c", t0 + PAUSE + Duration::from_millis(1));
        assert_eq!(typing.typed(), "c", "a pause starts a new name");
        assert_eq!(typing.target(NAMES, Some(1)), Some(4));
        typing.type_text("x", t0 + PAUSE + Duration::from_millis(2));
        assert_eq!(typing.target(NAMES, Some(4)), None);
        assert!(typing.is_typing(t0 + PAUSE + Duration::from_millis(3)));
        assert!(
            !typing.is_typing(t0 + PAUSE + PAUSE + Duration::from_millis(3)),
            "after a pause nothing is being typed"
        );
    }

    #[test]
    fn one_letter_again_steps_through_its_rows() {
        let t0 = crate::time::now();
        let mut typing = TypeAhead::default();
        typing.type_text("B", t0);
        assert_eq!(typing.target(NAMES, Some(0)), Some(2));
        typing.type_text("b", t0);
        assert_eq!(typing.target(NAMES, Some(2)), Some(3));
        typing.type_text("b", t0);
        assert_eq!(typing.target(NAMES, Some(3)), Some(2), "wraps around");
    }
}

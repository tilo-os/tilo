//! Progress through a request of many items, told per item.
//!
//! A request like "go through today's applications" is the same work done
//! for each of several items. The model says where it is on the `invoke`
//! that does an item's work (`progress`: how many items are done, of how
//! many, and what it just did), and the person reads it in the issue's
//! thread as one line per item ("3 of 8: reviewed Jane Doe"), apart from the
//! step notes. Each line is told once, when the count or the item changes.

use serde_json::Value;

use crate::tools::PROGRESS;
/// The most items a request is read as having.
const MOST_ITEMS: u64 = 10_000;
/// The longest item text a line keeps, in characters.
const MOST_ITEM_CHARS: usize = 160;

/// Where a many-item request is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) done: u64,
    pub(crate) of: u64,
    pub(crate) item: String,
}

impl Progress {
    /// The `progress` of an `invoke`'s input, when it says one that reads:
    /// at least one item, no more done than there are.
    pub(crate) fn of_input(input: &Value) -> Option<Self> {
        let progress = input.get(PROGRESS)?;
        let done = progress.get("done")?.as_u64()?;
        let of = progress.get("of")?.as_u64()?;
        let item: String = progress
            .get("item")?
            .as_str()?
            .trim()
            .chars()
            .take(MOST_ITEM_CHARS)
            .collect();
        (of > 0 && of <= MOST_ITEMS && done <= of && !item.is_empty()).then_some(Self {
            done,
            of,
            item,
        })
    }

    /// The line the person reads.
    pub(crate) fn line(&self) -> String {
        format!("{} of {}: {}", self.done, self.of, self.item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn progress_reads_as_one_line_per_item_and_refuses_what_cannot_be() {
        let read = |progress: Value| Progress::of_input(&json!({ PROGRESS: progress }));
        let third = read(json!({"done": 3, "of": 8, "item": " reviewed Jane Doe "})).unwrap();
        assert_eq!(third.line(), "3 of 8: reviewed Jane Doe");
        assert!(read(json!({"done": 9, "of": 8, "item": "x"})).is_none());
        assert!(read(json!({"done": 0, "of": 0, "item": "x"})).is_none());
        assert!(read(json!({"done": 1, "of": 2, "item": "  "})).is_none());
        assert!(read(json!({"done": 1, "of": 2})).is_none());
        assert!(Progress::of_input(&json!({})).is_none());
    }
}

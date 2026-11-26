//! Where an action's write landed in a field: the widget reports the text and
//! the geometry of a byte range, and the presence layer draws the wash and
//! caret ([`tilo_ui_base::presence::capability::WrittenPlace`]).
use super::TextInput;
use automerge::{AutoCommit, ObjId};

impl TextInput {
    #[must_use]
    pub fn bound_at(mut self, locate: impl Fn(&AutoCommit) -> Option<ObjId> + 'static) -> Self {
        self.locate_text = Some(Box::new(locate));
        self
    }
}

/// Where byte `at` of `old` stands in `new`, the one span where the two differ being someone
/// else's edit: before it, it stays; after it, it moves with the text; inside it, it lands at
/// the end of what they wrote.
pub(super) fn through_change(old: &str, new: &str, at: usize) -> usize {
    if old == new {
        return at;
    }
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| old.len().min(new.len()), |((index, _), _)| index);
    let (old_rest, new_rest) = (
        old.get(prefix..).unwrap_or_default(),
        new.get(prefix..).unwrap_or_default(),
    );
    let suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let (old_end, new_end) = (
        old.len().saturating_sub(suffix),
        new.len().saturating_sub(suffix),
    );
    if at <= prefix {
        at
    } else if at >= old_end {
        new_end.saturating_add(at.saturating_sub(old_end))
    } else {
        new_end
    }
}

//! A value whose every change is counted: it is read freely, and changed only through
//! [`Edited::edit`], which counts the change. What publishes a value only when it changed compares
//! the count it last published at with [`Edited::edits`] instead of the value itself, which may be
//! as big as a whole desktop.

use std::ops::Deref;

/// `T`, changed only through [`Edited::edit`].
#[derive(Debug, Default)]
pub struct Edited<T> {
    value: T,
    edits: u64,
}

impl<T> Edited<T> {
    pub const fn new(value: T) -> Self {
        Self { value, edits: 0 }
    }

    /// The value, to change: counted as a change whether or not the caller changes it.
    pub fn edit(&mut self) -> &mut T {
        self.edits = self.edits.wrapping_add(1);
        &mut self.value
    }

    /// Let `edit` change the value, counted as a change when `edit` says it changed it: for what
    /// passes over the value on every frame and changes it only now and then.
    pub fn edit_when(&mut self, edit: impl FnOnce(&mut T) -> bool) -> bool {
        let changed = edit(&mut self.value);
        if changed {
            self.edits = self.edits.wrapping_add(1);
        }
        changed
    }

    /// Put `value` in place of the one held, as a change.
    pub fn set(&mut self, value: T) {
        *self.edit() = value;
    }

    /// How many times the value was handed out to change: the same count means the same value.
    pub const fn edits(&self) -> u64 {
        self.edits
    }
}

impl<T> Deref for Edited<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::Edited;

    #[test]
    fn only_an_edit_moves_the_count() {
        let mut held = Edited::new(vec![1]);
        assert_eq!(held.len(), 1);
        assert_eq!(held.edits(), 0);
        held.edit().push(2);
        held.set(vec![3]);
        assert!(!held.edit_when(|_| false));
        assert!(held.edit_when(|held| {
            held.push(4);
            true
        }));
        assert_eq!(*held, vec![3, 4]);
        assert_eq!(held.edits(), 3);
    }
}

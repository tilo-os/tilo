//! A value the clones of its owner share until one of them changes it:
//! [`Shared`].
//!
//! A view may keep the state it was shown while the reader brings a copy of
//! it up to date ([`Resource::copy_state`](crate::Resource::copy_state)).
//! The containers in that state hold their contents in a [`Shared`], so the
//! copy is a pointer per container and only what a change touches is copied.

use std::sync::{Arc, OnceLock};

/// A value its owner's clones share; the one that changes it while another
/// still holds it changes a copy.
///
/// Sharing adds no `Clone` bound to the owner's API: [`share`](Self::share),
/// called where the value is known to clone (the owner's own `Clone`),
/// records how to copy it beside it, and [`make_mut`](Self::make_mut)
/// copies through that record.
pub(crate) struct Shared<T>(Arc<Held<T>>);

struct Held<T> {
    value: T,
    /// How to copy `value`, recorded by the first [`Shared::share`]: before
    /// it, nothing else holds the value and nothing needs a copy.
    copy: OnceLock<fn(&T) -> T>,
}

impl<T: Default> Clone for Held<T> {
    /// Run only for a value another holder still shares, which
    /// [`Shared::share`] made and recorded the copy for. Without a record
    /// nothing shared it and there is nothing to keep: the empty value.
    fn clone(&self) -> Self {
        let copy = self.copy.get().copied();
        Self {
            value: copy.map_or_else(T::default, |copy| copy(&self.value)),
            copy: copy.map_or_else(OnceLock::new, OnceLock::from),
        }
    }
}

impl<T> Shared<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Arc::new(Held {
            value,
            copy: OnceLock::new(),
        }))
    }

    pub(crate) fn get(&self) -> &T {
        &self.0.value
    }

    /// Whether `self` and `other` hold the same value, not equal ones.
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T: Clone> Shared<T> {
    /// Another holder of the same value: a pointer, no copy.
    pub(crate) fn share(&self) -> Self {
        let copy: fn(&T) -> T = T::clone;
        let _ = self.0.copy.set(copy);
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Shared<T> {
    /// The value, to change: copied first when another holder still shares
    /// it, so that one keeps seeing it as it was.
    pub(crate) fn make_mut(&mut self) -> &mut T {
        &mut Arc::make_mut(&mut self.0).value
    }

    /// The value, taken: copied when another holder still shares it.
    pub(crate) fn into_inner(self) -> T {
        Arc::unwrap_or_clone(self.0).value
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[cfg(test)]
mod tests {
    use super::Shared;

    #[test]
    fn a_change_copies_only_what_another_holder_shares() {
        let mut first = Shared::new(vec![1, 2, 3]);
        // Not shared yet: changed in place.
        first.make_mut().push(4);
        let mut second = first.share();
        assert_eq!(second.get().as_ptr(), first.get().as_ptr());
        assert!(first.same(&second));
        second.make_mut().push(5);
        assert!(!first.same(&second));
        assert_eq!(first.get(), &[1, 2, 3, 4]);
        assert_eq!(second.get(), &[1, 2, 3, 4, 5]);
        // The copy shares again, through the copy it carries.
        let third = second.share();
        assert_eq!(third.into_inner(), vec![1, 2, 3, 4, 5]);
        assert_eq!(second.into_inner(), vec![1, 2, 3, 4, 5]);
    }
}

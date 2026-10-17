//! A copy-on-write value: cloning shares it, the first mutable borrow of a
//! shared copy detaches it (`Arc::make_mut`).
//!
//! Every admission decides in a copy of the whole peer and installs the copy
//! only once every check passed. A long-running room holds hundreds of
//! certificates and records, each record with its channel state: copying
//! all of that per write made every write cost the size of the room. Behind
//! `Shared`, a copy costs a pointer per part, and a write copies only the
//! parts it changes.
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
    /// Whether `a` and `b` are copies of one value no copy changed since.
    pub(crate) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }
}
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}
impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}
impl<'a, T> IntoIterator for &'a Shared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&*self.0).into_iter()
    }
}
impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other) || *self.0 == *other.0
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for Shared<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_shares_until_written_and_a_write_leaves_the_original() {
        let original = Shared::new(vec![1, 2, 3]);
        let mut copy = original.clone();
        assert!(Shared::ptr_eq(&original, &copy));
        assert_eq!(*copy, [1, 2, 3]);
        copy.push(4);
        assert!(!Shared::ptr_eq(&original, &copy));
        assert_eq!(*original, [1, 2, 3]);
        assert_eq!(*copy, [1, 2, 3, 4]);
    }
}

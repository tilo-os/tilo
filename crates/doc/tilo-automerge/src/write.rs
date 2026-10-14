//! Writes that skip a value the document already holds.
//!
//! Writing an unchanged value is still an operation: it lands in history as
//! a row of its own, and two peers exporting the same state to each other
//! never fall quiet. These writers compare first and touch the document only
//! when the value differs.

use automerge::transaction::Transactable;
use automerge::{AutomergeError, ObjId, Prop, ScalarValue};

use crate::ReadTyped;

/// Change-only writers for every [`Transactable`].
pub trait WriteTyped: Transactable {
    /// Put the string `value` at `prop` of `obj` unless it is already there.
    ///
    /// # Errors
    ///
    /// Returns the [`AutomergeError`] of a put that fails.
    fn put_str_if_changed(
        &mut self,
        obj: &ObjId,
        prop: impl Into<Prop>,
        value: &str,
    ) -> Result<(), AutomergeError> {
        let prop = prop.into();
        if self.str_at(obj, prop.clone()).as_deref() == Some(value) {
            return Ok(());
        }
        self.put(obj, prop, value)
    }

    /// Put the unsigned integer `value` at `prop` of `obj` unless it is
    /// already there.
    ///
    /// # Errors
    ///
    /// Returns the [`AutomergeError`] of a put that fails.
    fn put_uint_if_changed(
        &mut self,
        obj: &ObjId,
        prop: impl Into<Prop>,
        value: u64,
    ) -> Result<(), AutomergeError> {
        let prop = prop.into();
        if self.uint_at(obj, prop.clone()) == Some(value) {
            return Ok(());
        }
        self.put(obj, prop, ScalarValue::Uint(value))
    }

    /// Put `value` at `prop` of `obj` when it is `Some` and differs; delete
    /// the entry when it is `None` and a value is present.
    ///
    /// # Errors
    ///
    /// Returns the [`AutomergeError`] of a put or delete that fails.
    fn put_opt_str_if_changed(
        &mut self,
        obj: &ObjId,
        prop: impl Into<Prop>,
        value: Option<&str>,
    ) -> Result<(), AutomergeError> {
        let prop = prop.into();
        match value {
            Some(value) => self.put_str_if_changed(obj, prop, value),
            None if self.get(obj, prop.clone())?.is_some() => self.delete(obj, prop),
            None => Ok(()),
        }
    }
}

impl<T: Transactable + ?Sized> WriteTyped for T {}

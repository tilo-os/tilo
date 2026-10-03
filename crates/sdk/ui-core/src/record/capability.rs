//! A root widget that chooses which record its document is, and owns work
//! scoped to that record.

use crate::widget::Widget;
use crate::widget::capability::{capability, capability_mut};

/// A root that picks and releases its record.
pub trait RecordScoped: Widget {
    /// Which record this widget's document should be, so opening a second
    /// file checks out a different document instead of overwriting the
    /// first. `None` keeps the default package-level document.
    fn record_intent(&self) -> Option<super::RecordCheckout> {
        None
    }

    /// Inverse of [`Self::record_intent`]. A widget that publishes presence
    /// must fold this into its presence key: two instances of one app are
    /// two records, and a key that only names the field collides.
    fn set_record_scope(&mut self, _record: &super::RecordId) {}

    /// Detach every receiver, queued result and deferred document write owned
    /// by the current record before a host retains this root under another
    /// record owner. `Ok(())` certifies no later callback applies old-owner
    /// work to a replacement document.
    ///
    /// # Errors
    /// Returns a refusal when the widget cannot yet detach every continuation.
    fn release_record_scope(&mut self) -> Result<(), String> {
        Err(REFUSAL.into())
    }
}

const REFUSAL: &str =
    "This application cannot release retained work from its current document owner";

/// `widget`'s record intent.
pub fn record_intent(widget: &(impl Widget + ?Sized)) -> Option<super::RecordCheckout> {
    capability::<dyn RecordScoped>(widget)?.record_intent()
}

/// Tell `widget` which record it is.
pub fn set_record_scope(widget: &mut (impl Widget + ?Sized), record: &super::RecordId) {
    if let Some(scoped) = capability_mut::<dyn RecordScoped>(widget) {
        scoped.set_record_scope(record);
    }
}

/// Release `widget`'s record-owned work; opaque roots refuse.
///
/// # Errors
/// Returns the widget's refusal.
pub fn release_record_scope(widget: &mut (impl Widget + ?Sized)) -> Result<(), String> {
    capability_mut::<dyn RecordScoped>(widget)
        .map_or_else(|| Err(REFUSAL.into()), RecordScoped::release_record_scope)
}

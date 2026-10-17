//! Compatibility boundary: native read authority lives in the backend, while
//! the frontend observes abandoned-child retirement on its canonical runtime.
use super::migration_intent::MigrationIntent;
use crate::model::TargetDrive;
use imager_backend::setup::read_media as native;
use std::{io, sync::atomic::AtomicBool};

pub(crate) use native::maybe_run;

pub(crate) struct ReadOnlyMediaGrant {
    native: Option<native::ReadOnlyMediaGrant>,
    retirement_key: String,
}
impl std::ops::Deref for ReadOnlyMediaGrant {
    type Target = native::ReadOnlyMediaGrant;
    fn deref(&self) -> &Self::Target {
        self.native.as_ref().expect("live selected-medium custody")
    }
}
impl Drop for ReadOnlyMediaGrant {
    fn drop(&mut self) {
        // First close the session and transfer custody to the backend registry;
        // scheduling failure must never discard native ownership.
        drop(self.native.take());
        super::journey::prepared_media::observe_read_medium(self.retirement_key.clone());
    }
}
fn wrap(
    target: &TargetDrive,
    result: io::Result<native::ReadOnlyMediaGrant>,
) -> io::Result<ReadOnlyMediaGrant> {
    let retirement_key = format!("target:{}", target.stable_id.as_deref().unwrap_or(""));
    match result {
        Ok(native) => Ok(ReadOnlyMediaGrant {
            native: Some(native),
            retirement_key,
        }),
        Err(error) => {
            // Validation can fail after the backend has assembled and retired
            // a grant. Observe that custody even when no grant reaches the UI.
            if native::retirement_pending(&retirement_key) {
                super::journey::prepared_media::observe_read_medium(retirement_key);
            }
            Err(error)
        }
    }
}
pub(crate) fn admit(
    target: &TargetDrive,
    intent: &MigrationIntent,
    cancel: &AtomicBool,
) -> io::Result<ReadOnlyMediaGrant> {
    wrap(target, native::admit(target, intent, cancel))
}
pub(crate) fn admit_applications(
    target: &TargetDrive,
    intent: &MigrationIntent,
    cancel: &AtomicBool,
) -> io::Result<ReadOnlyMediaGrant> {
    wrap(target, native::admit_applications(target, intent, cancel))
}
pub(crate) fn admit_boot(
    target: &TargetDrive,
    cancel: &AtomicBool,
) -> io::Result<ReadOnlyMediaGrant> {
    wrap(target, native::admit_boot(target, cancel))
}

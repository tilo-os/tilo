//! Application readiness context over the native bounded capacity transport.

use super::probe::{self, HostFacts};

pub(crate) fn maybe_run(args: &[String]) -> Result<Option<i32>, String> {
    imager_backend::staging::capacity_probe::maybe_run(args, probe::probe)
}

pub(in crate::setup) fn review() -> HostFacts {
    review_with_cancel(&std::sync::atomic::AtomicBool::new(false))
}

pub(in crate::setup) fn review_with_cancel(cancel: &std::sync::atomic::AtomicBool) -> HostFacts {
    let mut facts = imager_backend::staging::capacity_probe::review_with_cancel(cancel);
    probe::add_shared_blockers(&mut facts);
    facts
}

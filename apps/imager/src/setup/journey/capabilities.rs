//! Admission for the guided prepared-installation pipeline, not raw image writes.

#[derive(Clone, Copy)]
pub(super) struct GuidedCapabilities {
    pub(super) prepared_installation: bool,
}

impl GuidedCapabilities {
    pub(super) const fn native() -> Self {
        // Raw Imager has macOS/Windows writers. The guided identity/source
        // bundle producers and all three installation handoffs are Linux-only.
        Self {
            prepared_installation: cfg!(target_os = "linux"),
        }
    }
}

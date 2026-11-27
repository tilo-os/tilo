//! Negative-only ownership for an actual SDK mount; no Source or reader authority.
use super::{RetirementHandle, RetirementOwner};

/// Negative ownership consumed before the real component factory.
pub struct MountRetirement {
    owner: RetirementOwner,
    app: Option<RootRetirementPrelude>,
}
impl MountRetirement {
    pub fn new(owner: RetirementOwner) -> Self {
        let app = RootRetirementPrelude(owner.handle());
        Self {
            owner,
            app: Some(app),
        }
    }
    pub(crate) fn retire(&self) {
        self.owner.retire();
    }
    pub(crate) fn take_app(&mut self) -> Option<RootRetirementPrelude> {
        self.app.take()
    }
}

/// Direct App/assembly field.
pub struct RootRetirementPrelude(RetirementHandle);
impl RootRetirementPrelude {
    pub fn retire(&self) {
        self.0.retire();
    }
}
impl Drop for RootRetirementPrelude {
    fn drop(&mut self) {
        self.retire();
    }
}

/// Borrowed construction guard; private, negative-only, with no check/callback API.
pub struct MountConstructionRetirement<'a> {
    retirement: Option<&'a MountRetirement>,
}
impl<'a> MountConstructionRetirement<'a> {
    pub(crate) fn new(retirement: Option<&'a MountRetirement>) -> Self {
        Self { retirement }
    }
    pub(crate) fn disarm(mut self) {
        self.retirement = None;
    }
}
impl Drop for MountConstructionRetirement<'_> {
    fn drop(&mut self) {
        if let Some(retirement) = self.retirement {
            retirement.retire();
        }
    }
}

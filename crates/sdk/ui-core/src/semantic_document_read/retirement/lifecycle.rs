//! Negative-only custody for an actual SDK mount; no Source or reader authority.
#[cfg(not(target_arch = "wasm32"))]
use super::RetirementHandle;
use super::RetirementOwner;

/// Negative custody consumed before the real component factory.
/// No Live association or semantic reader can be created by this value.
pub struct MountRetirement {
    owner: RetirementOwner,
    #[cfg(not(target_arch = "wasm32"))]
    app: Option<RootRetirementPrelude>,
}
impl MountRetirement {
    pub fn new(owner: RetirementOwner) -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            app: Some(RootRetirementPrelude(owner.handle())),
            owner,
        }
    }
    pub(crate) fn retire(&self) {
        self.owner.retire();
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn take_app(&mut self) -> Option<RootRetirementPrelude> {
        self.app.take()
    }
}

/// Direct App/assembly field. Never copied through CallbackBinding or a widget.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct RootRetirementPrelude(RetirementHandle);
#[cfg(not(target_arch = "wasm32"))]
impl RootRetirementPrelude {
    pub(crate) fn retire(&self) {
        self.0.retire();
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for RootRetirementPrelude {
    fn drop(&mut self) {
        self.retire();
    }
}

/// Borrowed construction guard; private, negative-only, with no gate/callback API.
pub(crate) struct MountConstructionRetirement<'a> {
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

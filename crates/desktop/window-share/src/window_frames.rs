//! Where a shared window's frames come from when a share encodes "the
//! window as the frame store holds it" ([`crate::flatpak_share`]'s window
//! staging): the compositor's frame store, which lives above this crate.
//! The compositor provides it once ([`provide`]); until it has, no window
//! frame is held, as an empty store would answer.

use std::sync::OnceLock;

use neighbour_wire::SurfaceId;

use crate::encoder::nv12::Region;

/// A surface's most recent frame: tightly packed RGBA at its size, and its
/// place in the surface's sequence of frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
}

/// The compositor's store of the frames its clients' surfaces committed.
pub trait WindowFrames: Send + Sync {
    /// A copy of `surface`'s most recent frame, if it holds one.
    fn frame(&self, surface: SurfaceId) -> Option<WindowFrame>;

    /// The parts of `surface` that changed between frames `previous` and
    /// `current`, when it can tell; `None` means treat the whole frame as
    /// changed.
    fn changes(&self, surface: SurfaceId, previous: u64, current: u64) -> Option<Vec<Region>>;
}

static PROVIDED: OnceLock<&'static dyn WindowFrames> = OnceLock::new();

/// Make `frames` the store window shares read from. The first call wins:
/// a process has one compositor frame store.
pub fn provide(frames: &'static dyn WindowFrames) {
    let _ = PROVIDED.set(frames);
}

/// The store window shares read from, once the compositor provided it.
pub(crate) fn provided() -> Option<&'static dyn WindowFrames> {
    PROVIDED.get().copied()
}

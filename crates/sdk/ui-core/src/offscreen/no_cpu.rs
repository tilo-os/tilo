//! Drawing a picture where no GPU adapter is, on a browser page: it has no
//! CPU rasterizer, so no picture is drawn. The native toolkit uses `cpu.rs`.

use vello::Scene;
use vello::peniko::Color;

/// Nothing: a browser page has no CPU rasterizer to draw `scene` with.
pub(super) fn render(_: &Scene, _: u32, _: u32, _: Color) -> Option<Result<Vec<u8>, String>> {
    None
}

/// A browser page has no CPU rasterizer, so no surface's frames are drawn on
/// one: a host there draws through the GPU path or not at all.
pub struct CpuSurface;

impl CpuSurface {
    /// No surface: this toolkit draws nothing on the CPU.
    #[must_use]
    pub const fn new(_width: u32, _height: u32, _threads: u16) -> Option<Self> {
        None
    }

    /// No size: nothing was drawn.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (0, 0)
    }

    /// No pixels: nothing was drawn.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &[]
    }

    /// Nothing is drawn where a page has no CPU rasterizer.
    pub fn draw(&mut self, _scene: &Scene, _regions: &[(u32, u32, u32, u32)]) -> bool {
        false
    }
}

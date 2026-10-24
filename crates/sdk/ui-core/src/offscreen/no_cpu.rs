//! Drawing a picture where no GPU adapter is, on a browser page: it has no
//! CPU rasterizer, so no picture is drawn. The native toolkit uses `cpu.rs`.

use vello::Scene;
use vello::peniko::Color;

/// Nothing: a browser page has no CPU rasterizer to draw `scene` with.
pub(super) fn render(_: &Scene, _: u32, _: u32, _: Color) -> Option<Result<Vec<u8>, String>> {
    None
}

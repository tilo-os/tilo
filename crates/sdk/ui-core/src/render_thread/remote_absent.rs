//! Windows drawn by the room's render service, where there is none: only a
//! Linux session runs one, so elsewhere no window asks for it and every
//! window draws on its own process's device. The runner and the text
//! renderer ask the same questions on every target.

use std::path::Path;
use std::sync::Arc;

use vello::peniko::FontData;
use winit::window::Window;

use super::RenderThreadHandle;

/// Never: no render service exists on this target.
pub fn requested() -> bool {
    false
}

/// Never: no render service exists on this target.
pub fn active() -> bool {
    false
}

/// Nothing to note: no render service maps font files here.
pub fn note_font_file(_font: &FontData, _path: &Path) {}

/// Nothing to connect to.
pub fn prepare() {}

/// No render service draws this window.
///
/// # Errors
///
/// Always: this target has no render service.
pub(crate) fn spawn(
    _window: Arc<Window>,
    _transparent: bool,
    _wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<RenderThreadHandle, String> {
    Err("this platform has no render service".to_owned())
}

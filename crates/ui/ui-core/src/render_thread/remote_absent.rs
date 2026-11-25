//! Windows drawn by the workspace's render service, where there is none: only a Linux session runs one,
//! so elsewhere no window asks for it and every window draws on its own process's device. The runner
//! asks the same question on every target.

use std::sync::Arc;

use winit::window::Window;

use super::RenderThreadHandle;

/// Never: no render service exists on this target.
pub fn requested() -> bool {
    false
}

/// Nothing to connect to.
pub fn prepare() {}

/// No render service draws this window. # Errors Always: this target has no render service.
pub(crate) fn spawn(
    _window: Arc<Window>,
    _transparent: bool,
    _wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<RenderThreadHandle, String> {
    Err("this platform has no render service".to_owned())
}

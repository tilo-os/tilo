//! Horizontal scrolling a window has no use for, handed back to a compositor
//! that moves its window strip with it.
use winit::window::Window;

/// Tell the compositor that the sideways scroll `window` is receiving moved
/// nothing in it. A host without the protocol returns false.
pub fn horizontal_unused(window: &Window) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.horizontal_scroll_unused()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        false
    }
}

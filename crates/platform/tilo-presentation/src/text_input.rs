//! Optional original-field acknowledgements beside standard native IME delivery.
//! These calls carry no transcript and grant no microphone or input ownership.
use winit::window::Window;

/// Publish multiline intent when the native input protocol supports it.
pub fn set_multiline(window: &Window, multiline: bool) {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.set_ime_multiline(multiline);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, multiline);
}

/// Advertise an opaque field stamp. Missing native protocol support returns false.
pub fn publish(window: &Window, snapshot: Option<[u8; 32]>) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.private_text_input(snapshot)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, snapshot);
        false
    }
}

/// Resolve exactly the original compositor operation; unsupported hosts do nothing.
pub fn acknowledge(window: &Window, serial: u32, outcome: u32) {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.acknowledge_private_text_input(serial, outcome);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, serial, outcome);
}

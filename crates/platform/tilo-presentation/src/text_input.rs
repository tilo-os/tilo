//! Optional original-field acknowledgements beside standard native IME delivery:
//! no transcript, and no microphone or input ownership.
use winit::window::Window;

pub fn set_content_type(window: &Window, hint: u32, purpose: u32) {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.set_ime_content_type(Some(winit::platform::wayland::ImeContentType {
            hint,
            purpose,
        }));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, hint, purpose);
}

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

pub fn acknowledge(window: &Window, serial: u32, outcome: u32) {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        window.acknowledge_private_text_input(serial, outcome);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, serial, outcome);
}

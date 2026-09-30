//! The page's `HostServices`: each platform crate's web backend — the
//! clipboard of trusted clipboard events, `<input type=file>` choosers,
//! downloads, `window.open` and the Notifications API. Speech is silent
//! (`tts::NullSpeaker`); media controls, location and screen capture are
//! unavailable.
//!
//! Browsers grant pickers, popups and permission prompts only inside a user
//! gesture. Every call comes from the app's handling of an input event, which
//! winit dispatches from the DOM listener, so it runs within that gesture's
//! activation.

/// Install the page's services as the process default.
pub fn install() {
    ui::host::install_default(ui::host::HostServices::system(None));
}

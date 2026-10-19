//! Binary entry point for the Tilo on-screen keyboard: the
//! [`osk::OskShell`] app, serving the session as its keyboard
//! ([`osk::serve`]). `--shots` and `--shots-dir` capture its scenarios;
//! `--purpose KIND` starts it for a kind of field, `--input-method` /
//! `--no-input-method` require or refuse the input method, and
//! `--debug-hit-areas` paints the effective tap targets.
//!
//! ## Tilo integration
//!
//! The session compositor supervises this signed service when the persisted
//! accessibility preference is enabled. It gives the service a dedicated,
//! unexported Wayland socket. Only connections accepted on that socket can see
//! `input-method-v2` or `virtual-keyboard-v1`; copying the app-id on the public
//! display grants no input authority.
//!
//! The main xdg-toplevel announces the stable [`osk::KEYBOARD_APP_ID`]. The
//! compositor requires both that id and the private-socket credential before it
//! promotes the surface out of the workspace tree into a bottom floating
//! overlay. Input-method activation makes the initially empty keyboard visible;
//! leaving the focused text field suppresses it again. Pointer focus may enter
//! the keyboard, but its taps deliberately do not take keyboard focus from the
//! edited field.
//!
//! The in-progress word is shown as input-method preedit and committed with
//! `commit_string` at word boundaries. A narrowly gated virtual keyboard sends
//! the editing keys — Backspace outside a composing word (winit's
//! `text-input-v3` client does not consume `delete_surrounding_text`) and
//! Return, so single-line fields submit. [`osk::OrderedRouter`] keeps the two
//! connections in order. On other compositors, the binary may use its
//! standalone virtual-keyboard fallback, which types the composing word
//! directly; Tilo never advertises that privileged global on its public
//! display.
//!
//! Swipe decoding and prediction are live: the real FUTO model is selected
//! when its runtime assets are available, otherwise the in-tree geometric
//! decoder provides the same candidate UI.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The emoji catalogue comes from its engine library, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_emoji_engine;

fn main() {
    osk::serve();
    ui::framework::run::<osk::OskShell>();
}

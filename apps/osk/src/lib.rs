//! qOS on-screen keyboard (OSK) — app + layout/rendering/input model.
//!
//! This is the first-class on-screen keyboard for touch and pointer use, designed to also
//! be the foundation for the future qOS **smartphone** keyboard. The sizing
//! model is responsive (it fills the available surface width and derives key
//! size from it), so the *same* code serves a narrow phone surface and a wide
//! desktop surface — only the surface dimensions change.
//!
//! ## Scope of this crate
//! * Data-driven [`KeyboardLayout`]s (QWERTY, AZERTY, symbols; more are data).
//! * Input-type layouts selected by [`InputPurpose`] — a number pad, dialpad,
//!   PIN pad, and email/URL keyboards — so a focused field gets the right keys.
//! * Pure, responsive geometry ([`key_rects`]).
//! * The [`OskWidget`] — drawing, pointer input, shift/caps, symbol layer,
//!   backspace — plus a customizable [`OskToolbarItem`] row.
//!
//! ## Swipe-to-type + prediction (shipped here)
//! * [`dict`] — a compact, embedded, offline English word-frequency
//!   dictionary (~30k words).
//! * [`swipe`] — a SHARK²-style geometric + dictionary swipe decoder
//!   ([`swipe::GeometricSwipeModel`]). The preferred FUTO/ExecuTorch model is
//!   left as the clean [`swipe::SwipeModel`] trait seam (see that module for
//!   why the geometric path ships instead).
//! * [`predict`] — an offline n-gram next-word predictor (unigram + bigram).
//! * The [`OskWidget`] captures pointer drags as a gesture path, decodes
//!   swipes, and renders a tappable **suggestion bar** above the keys.
//!
//! ## Text injection (client half, shipped here)
//! * [`inject`] — a [`TextInjector`] trait with a [`VirtualKeyboardInjector`]
//!   (wtype-style `virtual-keyboard-v1`) and a [`NullInjector`] fallback. The
//!   binary wires the widget's `on_key` callback to the injector so every
//!   committed [`KeyAction`] is typed into the focused surface. The compositor
//!   *server* half (the layer-shell role + advertising the protocol) is a
//!   separate concern.
//!
//! ## Explicitly out of scope (left as clean seams)
//! * **Compositor server half** — the `wlr-layer-shell` surface role and the
//!   compositor side of `virtual-keyboard-v1` / `input-method-v2`.
//! * **Neural swipe** — the FUTO `swipe-library` (GPL C++ + `ExecuTorch` `.pte`
//!   model). Drop a `FutoSwipeModel: SwipeModel` in and the wiring is
//!   unchanged.
//!
//! See [`widget`] for where text injection and swipe plug in.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]

/// The Wayland app-id the on-screen keyboard identifies itself with. The
/// compositor matches a client's app-id against this to recognise the keyboard
/// and treat its surface as the floating, compositor-managed keyboard overlay
/// rather than an ordinary tiled window. Must stay in step with the
/// compositor's matching constant.
///
/// Setting this on the actual Wayland surface still needs a way to pass an
/// app-id through `ui::App` (winit's `WindowAttributesExtWayland::with_name`),
/// which the toolkit does not expose yet; see the crate-level notes in
/// `main.rs` for what that takes.
pub const KEYBOARD_APP_ID: &str = "org.qos.Keyboard";

pub mod dict;
pub mod futo;
pub mod imclient;
pub mod inject;
pub mod layout;
pub mod predict;
pub mod suggest;
pub mod swipe;
pub mod toolbar;
pub mod widget;

pub use dict::{Dictionary, Entry};
pub use futo::FutoSwipeModel;
pub use imclient::{
    ImEvent, ImState, ImTransition, InputMethodClient, purpose_from_wire, try_input_method,
};
pub use inject::{
    NullInjector, Symbol, TextInjector, VirtualKeyboardInjector, action_symbols, build_keymap,
    default_injector,
};
pub use layout::{InputPurpose, Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
pub use predict::Predictor;
pub use suggest::{Suggestion, suggestion_rects};
pub use swipe::{Candidate, GeometricSwipeModel, SwipeModel, is_swipe};
pub use toolbar::{OskToolbarAction, OskToolbarItem, toolbar_rects};
pub use widget::{OskWidget, input_purpose_from_name, input_purpose_name};

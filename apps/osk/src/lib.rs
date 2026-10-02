//! Tilo on-screen keyboard (OSK) — app + layout/rendering/input model.
//!
//! This is the first-class on-screen keyboard for touch and pointer use, designed to also
//! be the foundation for the future Tilo **smartphone** keyboard. The sizing
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
//! * An [`emoji_panel`] behind the emoji key — group tabs, recents, search
//!   typed with the keys, skin tones — and emoji chips for words that name one.
//!
//! ## Swipe-to-type + prediction (shipped here)
//! * [`dict`] — compact, embedded, offline word-frequency dictionaries
//!   (English ~40k words, French ~50k), picked by the letter layout.
//! * [`swipe`] — a SHARK²-style geometric + dictionary swipe decoder
//!   ([`swipe::GeometricSwipeModel`]). The preferred FUTO/ExecuTorch model is
//!   left as the clean [`swipe::SwipeModel`] trait seam (see that module for
//!   why the geometric path ships instead).
//! * [`predict`] — an offline n-gram next-word predictor (unigram + bigram).
//! * The [`OskWidget`] captures pointer drags as a gesture path, decodes
//!   swipes, and renders a tappable **suggestion bar** above the keys.
//!
//! ## Text injection and session integration
//! * [`imclient`] is the preferred `input-method-v2` commit path and drives
//!   focus-based visibility plus content-purpose layouts.
//! * [`output`] is the ordered preedit/commit/key stream the widget emits and
//!   the routing that keeps it ordered across both connections.
//! * [`inject`] provides the `virtual-keyboard-v1` client for editing keys
//!   (Backspace, Return), plus a standalone fallback for other compositors.
//! * The Tilo compositor exposes both privileged globals only on its
//!   process-credential-gated OSK socket and floats the stable
//!   [`KEYBOARD_APP_ID`] surface outside the workspace tile tree.
//!
//! The neural FUTO decoder is used when its runtime libraries and model assets
//! are present; otherwise the geometric decoder provides the same UI without
//! pretending neural inference is available.
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
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// The Wayland app-id the on-screen keyboard identifies itself with. The
/// compositor matches a client's app-id against this to recognise the keyboard
/// and treat its surface as the floating, compositor-managed keyboard overlay
/// rather than an ordinary tiled window. Must stay in step with the
/// compositor's matching constant.
///
/// Stable package/window identity. The compositor only promotes this id to
/// the keyboard overlay when it also arrived on the private OSK Wayland
/// socket; the string alone never grants input-method privileges.
pub const KEYBOARD_APP_ID: &str = "so.tilo.osk";

pub mod autocorrect;
pub mod dict;
pub mod emoji_panel;
pub mod futo;
mod i18n;
pub mod imclient;
pub mod inject;
pub mod layout;
pub mod output;
pub mod predict;
pub mod resource;
pub mod suggest;
pub mod swipe;
pub mod toolbar;
pub mod touch_model;
pub mod widget;

pub use autocorrect::{Autocorrector, KeyAdjacency};
pub use dict::{Dictionary, Entry, Language};
pub use emoji_panel::{EmojiPanel, PanelHit, PanelMode};
pub use futo::FutoSwipeModel;
pub use imclient::{
    ImEvent, ImState, ImTransition, InputMethodClient, purpose_from_wire, try_input_method,
};
pub use inject::{
    NullInjector, Symbol, TextInjector, VirtualKeyboardInjector, build_keymap, default_injector,
};
pub use layout::{InputPurpose, Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
pub use output::{Channel, OrderedRouter, OskOutput, PreeditEmulator, Step, TextMirror};
pub use predict::Predictor;
pub use suggest::{Suggestion, suggestion_rects};
pub use swipe::{Candidate, GeometricSwipeModel, SwipeModel, is_swipe};
pub use toolbar::{OskToolbarAction, OskToolbarItem, toolbar_rects};
pub use touch_model::{NextKeyDistribution, TouchKey, TouchModel};
pub use widget::{OskWidget, input_purpose_from_name, input_purpose_name};

pub mod framework_history;
mod presentation;

pub mod actions;
pub use resource::{Osk, OskShell};

//! The on-screen keyboard for touch and pointer use: data-driven
//! [`KeyboardLayout`]s, an input-purpose layout per field, offline swipe decoding
//! and prediction, and the compositor paths that inject what is typed. Its sizing
//! fills the surface it is given, so a phone and a wide desktop share one widget.

// An app runs in its own sandboxed process, so an integer that wraps here costs
// one app rather than the desktop. The lints that produce a crash stay denied.
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
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// The app-id the compositor recognises the keyboard by, and only when the client
/// also arrived on the private OSK socket: the string alone grants no privileges.
/// Must stay in step with the compositor's constant.
pub const KEYBOARD_APP_ID: &str = "so.tilo.osk";

pub mod autocorrect;
pub mod dict;
pub mod emoji_panel;
pub mod futo;
mod i18n;
pub mod imclient;
pub mod inject;
#[doc(no_inline)]
pub use tilo_keyboard_map::touch as layout;
pub mod learned;
pub mod output;
pub mod predict;
pub mod resource;
pub mod service;
mod session;
pub mod suggest;
pub mod surface;
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
    Keypress, Modifiers, NullInjector, Symbol, TextInjector, VirtualKeyboardInjector, build_keymap,
    default_injector,
};
pub use layout::{InputPurpose, Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
pub use output::{Channel, OrderedRouter, OskOutput, PreeditEmulator, Step, TextMirror};
pub use predict::Predictor;
pub use suggest::{Suggestion, suggestion_rects};
pub use swipe::{Candidate, GeometricSwipeModel, SwipeModel, is_swipe};
pub use toolbar::{
    Latch, ModifierKey, ModifierLatches, OskToolbarAction, OskToolbarItem, toolbar_rects,
};
pub use touch_model::{NextKeyDistribution, TouchKey, TouchModel};
pub use widget::{OskWidget, input_purpose_from_name, input_purpose_name};

mod presentation;

pub mod actions;
pub use actions::SetPreference;
pub use resource::{Osk, OskPreferences, OskShell};
pub use service::serve;
pub use surface::OskSurface;

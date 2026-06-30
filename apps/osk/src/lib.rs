//! qOS on-screen keyboard (OSK) — app + layout/rendering/input model.
//!
//! This is workstream **W8** of the accessibility plan
//! (`docs/superpowers/specs/2026-06-30-accessibility-design.md` §3): the
//! first-class on-screen keyboard for touch and pointer use, designed to also
//! be the foundation for the future qOS **smartphone** keyboard. The sizing
//! model is responsive (it fills the available surface width and derives key
//! size from it), so the *same* code serves a narrow phone surface and a wide
//! desktop surface — only the surface dimensions change.
//!
//! ## Scope of this crate (W8)
//! * Data-driven [`KeyboardLayout`]s (QWERTY, AZERTY, symbols; more are data).
//! * Pure, responsive geometry ([`key_rects`]).
//! * The [`OskWidget`] — drawing, pointer input, shift/caps, symbol layer,
//!   backspace — plus a customizable [`OskToolbarItem`] row.
//!
//! ## W9 — swipe-to-type + prediction (shipped here)
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
//! ## W7 — text injection (client half, shipped here)
//! * [`inject`] — a [`TextInjector`] trait with a [`VirtualKeyboardInjector`]
//!   (wtype-style `virtual-keyboard-v1`) and a [`NullInjector`] fallback. The
//!   binary wires the widget's `on_key` callback to the injector so every
//!   committed [`KeyAction`] is typed into the focused surface. The compositor
//!   *server* half (the layer-shell role + advertising the protocol) is a
//!   separate workstream.
//!
//! ## Explicitly out of scope (left as clean seams)
//! * **W7 (server)** — the `wlr-layer-shell` surface role and the compositor
//!   side of `virtual-keyboard-v1` / `input-method-v2`.
//! * **W9a (neural)** — the FUTO `swipe-library` (GPL C++ + ExecuTorch `.pte`
//!   model). Drop a `FutoSwipeModel: SwipeModel` in and the wiring is
//!   unchanged.
//!
//! See [`widget`] for where W7 and W9 plug in.

pub mod dict;
pub mod futo;
pub mod inject;
pub mod layout;
pub mod predict;
pub mod suggest;
pub mod swipe;
pub mod toolbar;
pub mod widget;

pub use dict::{Dictionary, Entry};
pub use futo::FutoSwipeModel;
pub use inject::{
    NullInjector, Symbol, TextInjector, VirtualKeyboardInjector, action_symbols, build_keymap,
    default_injector,
};
pub use layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
pub use predict::Predictor;
pub use suggest::{Suggestion, suggestion_rects};
pub use swipe::{Candidate, GeometricSwipeModel, SwipeModel, is_swipe};
pub use toolbar::{OskToolbarAction, OskToolbarItem, toolbar_rects};
pub use widget::OskWidget;

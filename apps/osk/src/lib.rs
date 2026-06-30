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
//! ## Explicitly out of scope (left as clean seams)
//! * **W7** — the compositor `input-method-v2` / `virtual-keyboard` protocols
//!   and text injection into the focused surface. The widget exposes its
//!   output via [`OskWidget::on_key`] + [`OskWidget::take_pending_text`]; a
//!   host wires that to the IM client.
//! * **W9a** — FUTO swipe-to-type (GPL C++ `swipe-library` via FFI).
//! * **W9b** — on-device next-word prediction.
//!
//! See [`widget`] for where W7 and W9 plug in.

pub mod layout;
pub mod toolbar;
pub mod widget;

pub use layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
pub use toolbar::{OskToolbarAction, OskToolbarItem, toolbar_rects};
pub use widget::OskWidget;

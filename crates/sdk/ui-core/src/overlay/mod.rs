//! The window's top layer: what modal and anchored surfaces share.
//!
//! A modal Panel ([`crate::widget::ModalDialog`]) and an anchored one
//! ([`crate::widget::Popover`]) paint in the host's top-layer pass
//! ([`crate::widget::Widget::draw_overlay`]), after the app and the window's
//! own chrome, so a modal's scrim covers the whole window. This module holds
//! what they have in common: the keyboard [`FocusTrap`] and the [`scrim`]
//! painters.

pub mod focus_trap;
pub mod scrim;

pub use focus_trap::FocusTrap;

//! The on-screen keyboard's own work, with no user interface: the layouts it types on, the tap a
//! finger meant, the words it offers and corrects, and the words a swipe traces.
//!
//! The keyboard's app draws keys and sends what a person did; this crate answers what that means.
//! It owns no state of a person's: what typing taught the keyboard is handed in with each ask, so
//! the app keeps it and this crate stays a pure answer (which is what lets the same asks be replayed
//! and tested without a device).
//!
//! Layouts are data ([`layout`]), so a new language or purpose is a row. The gaps a layout leaves
//! for accents, the odds of the next letter and the words a gesture could be are computed here.

pub mod autocorrect;
pub mod dict;
pub mod futo;
pub mod layout;
mod num;
pub mod predict;
pub mod service;
pub mod swipe;
pub mod touch_model;

pub use autocorrect::{Autocorrector, KeyAdjacency};
pub use dict::{Dictionary, Entry, Language};
pub use futo::FutoSwipeModel;
pub use layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
pub use predict::Predictor;
pub use swipe::{Candidate, GeometricSwipeModel, SwipeModel, is_swipe};
pub use touch_model::{KeyOffset, NextKeyDistribution, TouchKey, TouchModel};

/// The app whose engine this is: the on-screen keyboard, which asks these ops and no other's.
pub const APP: &str = "so.tilo.osk";

/// This crate as the keyboard's engine: every ask of [`service::call`], answered for the app that
/// draws the keys.
struct Keyboard;

impl tilolang_tilo::engine::Provider for Keyboard {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        service::call(op, input)
    }
}

/// Register this crate as the keyboard's engine: what a host calls once at its start, before the
/// keyboard asks. The keyboard itself is the session's own view, so the session's process calls
/// this; a process that runs the app alone calls it too, as every host registers the engines it
/// links.
pub fn register() {
    tilolang_tilo::engine::register(APP, std::sync::Arc::new(Keyboard));
}

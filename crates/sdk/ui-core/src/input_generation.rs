//! How much input the tree has been handed since a frame drew it.
//!
//! A widget is clean until something it reads changes. Most widgets say so
//! themselves, by tracking what they hold and marking it dirty. The rest
//! read what reaches them from outside themselves — an event they handled,
//! a document revision their root synced — and paint it without keeping a
//! flag of their own. This counter is that reading for them: it moves when
//! input is delivered or a model the tree reads changes.
//!
//! Only drawings that read outside state go stale with it
//! ([`crate::outside_paint`]): input alone changes nothing a widget keeps in
//! the tree, and treating every cache as stale on it would cost the frames
//! that copy what they keep on screen.
//!
//! A host also compares the generation its last drawn frame read against
//! this one: input handed to the tree after that frame makes the next one
//! due, so what a widget repaints from it reaches the screen.

use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Input reached the tree: an event was delivered, or a model it reads
/// changed.
pub fn note() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The input generation now: draw a frame against it and keep that drawing
/// only while it still reads the same.
#[must_use]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

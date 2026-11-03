//! How much input and model change the tree has been handed since a frame
//! drew it.
//!
//! A widget is clean until something it reads changes. Most widgets say so
//! themselves, by tracking what they hold and marking it dirty. The rest
//! read what reaches them from outside themselves — an event they handled,
//! a document revision their root synced — and paint it without keeping a
//! flag of their own. These two readings are for them.
//!
//! [`generation`] moves when input is delivered. A widget's own fields
//! stand still across an event it did not take up, so only drawings that
//! read outside state go stale with it ([`crate::outside_paint`]): treating
//! every cache as stale on input would cost the frames that copy what they
//! keep on screen.
//!
//! [`model`] moves when a model the tree reads changes. That reaches
//! widgets through the sync pass, so a cache kept across it would hold a
//! drawing from before the change: every cache drawn with it is stale.
//!
//! A host also compares the generations its last drawn frame read against
//! the current ones: a change handed to the tree after that frame makes the
//! next one due, so what a widget repaints from it reaches the screen.

use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static MODEL: AtomicU64 = AtomicU64::new(0);

/// Input reached the tree: an event was delivered.
pub fn note() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The input generation now: draw a frame against it and keep that drawing
/// only while it still reads the same.
#[must_use]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// A model the tree reads changed.
pub fn note_model() {
    MODEL.fetch_add(1, Ordering::Relaxed);
}

/// The model generation now: a drawing kept from another one painted a
/// document that has since changed.
#[must_use]
pub fn model() -> u64 {
    MODEL.load(Ordering::Relaxed)
}

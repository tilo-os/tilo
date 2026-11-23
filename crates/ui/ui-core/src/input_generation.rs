//! How much input and model change the tree has been handed since a frame drew it. A widget is clean
//! until something it reads changes.

use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static MODEL: AtomicU64 = AtomicU64::new(0);

/// Input reached the tree: an event was delivered.
pub fn note() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The input generation now: draw a frame against it and keep that drawing only while it still reads
/// the same.
#[must_use]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// A model the tree reads changed.
pub fn note_model() {
    MODEL.fetch_add(1, Ordering::Relaxed);
}

/// The model generation now: a drawing kept from another one painted a document that has since changed.
#[must_use]
pub fn model() -> u64 {
    MODEL.load(Ordering::Relaxed)
}

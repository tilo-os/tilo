//! How much input the tree has been handed since a frame drew it.
//!
//! A widget is clean until something it reads changes. Most widgets say so
//! themselves, by tracking what they hold and marking it dirty. The rest
//! read what reaches them from outside themselves — an event they handled,
//! a document revision their root synced — and paint it without keeping a
//! flag of their own. This counter is that reading for them: it moves when
//! input is delivered or a model the tree reads changes, so a drawing a
//! frame kept from an earlier frame is only reused while it still stands.

use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Input reached the tree: an event was handled, or a model it reads
/// changed. Drawings kept from earlier frames hold what it changed.
pub fn note() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The input generation now: draw a frame against it and keep that drawing
/// only while it still reads the same.
#[must_use]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

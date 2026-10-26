// Copyright 2026 the Tilo Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Images that are another image changed in a few rects (Tilo patch).
//!
//! A web page's frame is a new image every time the page paints, though a
//! blinking caret changes a few hundred of its millions of pixels. Whoever
//! makes such a frame says which image it follows and where it differs
//! ([`register_image_delta`]); an image cache that holds the earlier image
//! hands its atlas slot to the new one and uploads only those rects. A cache
//! that does not hold it (another renderer, an evicted slot) uploads the new
//! image whole, as without a delta.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Deltas kept, newest last: a few frames each of several pages.
const KEPT: usize = 64;

/// A rect of an image, in its pixels: x, y, width, height.
pub type ImageRect = [u32; 4];

struct Delta {
    to: u64,
    from: u64,
    rects: Arc<[ImageRect]>,
}

static DELTAS: Mutex<VecDeque<Delta>> = Mutex::new(VecDeque::new());

/// Say that the image whose blob id is `to` has the same size and format as
/// the one whose blob id is `from` and the same pixels outside `rects`.
pub fn register_image_delta(from: u64, to: u64, rects: &[ImageRect]) {
    let mut deltas = DELTAS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    deltas.retain(|delta| delta.to != to);
    if deltas.len() >= KEPT {
        deltas.pop_front();
    }
    deltas.push_back(Delta {
        to,
        from,
        rects: rects.into(),
    });
}

/// The image `to` follows and the rects where they differ, if known.
pub fn image_delta(to: u64) -> Option<(u64, Arc<[ImageRect]>)> {
    let deltas = DELTAS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    deltas
        .iter()
        .rev()
        .find(|delta| delta.to == to)
        .map(|delta| (delta.from, delta.rects.clone()))
}

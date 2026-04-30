//! Test-target re-export of `quick_ui::offscreen` so existing
//! integration tests keep their `support::offscreen::OffscreenRenderer`
//! import path working. New code (binaries, examples, downstream
//! crates) should import directly from `quick_ui::offscreen`.

#![allow(unused_imports)]

pub use quick_ui::offscreen::OffscreenRenderer;

//! Test-target re-export of `ui::offscreen` so existing
//! integration tests keep their `support::offscreen::OffscreenRenderer`
//! import path working. New code (binaries, examples, downstream
//! crates) should import directly from `ui::offscreen`.

#![allow(unused_imports)]

pub use ui::offscreen::OffscreenRenderer;

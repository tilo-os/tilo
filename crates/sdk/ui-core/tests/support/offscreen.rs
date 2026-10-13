//! Test-target re-export of `tilo_ui::offscreen` so existing
//! integration tests keep their `support::offscreen::OffscreenRenderer`
//! import path working. New code (binaries, examples, downstream
//! crates) should import directly from `tilo_ui::offscreen`.

#![allow(unused_imports)]

pub use tilo_ui::offscreen::OffscreenRenderer;

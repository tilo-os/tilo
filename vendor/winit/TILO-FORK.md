# tilo-winit

Tilo's fork of the crates.io `winit` 0.30.13, published as `tilo-winit` so the
Tilo SDK can be published with it. The library is still named `winit`, so code
keeps writing `winit::…`. Upstream: https://github.com/rust-windowing/winit
(Apache-2.0, unchanged).

The additions the SDK uses are `KeyEvent::synthetic`, Wayland drag sources
(`start_drag`) and swipe gestures, and a macOS quit request
(`set_quit_request_window`); `vendor/PATCHES.md` and the standalone history
`vendor-upstream/winit-0.30.13` in the Tilo repository list them commit by
commit. When a winit release carries them, the SDK moves back to `winit` and
this fork ends.

Inside the Tilo workspace, crates.io crates that depend on `winit` get it
through a facade (`crates/forks/winit`) that re-exports this crate, so they
share its types.

# tilo-winit

Tilo's fork of the crates.io `winit` 0.30.13, published as `tilo-winit` so the
Tilo SDK can be published with it. The library is still named `winit`, so code
keeps writing `winit::…`. Upstream: https://github.com/rust-windowing/winit
(Apache-2.0, unchanged).

The additions the SDK uses are `KeyEvent::synthetic`, Wayland drag sources
(`start_drag`), swipe gestures, announcing a frame drawn off the event-loop
thread (`Window::announce_frame`), Wayland pen tablets
(`WindowEvent::PenSample` before each pointer event a stylus makes), and a
macOS quit request (`set_quit_request_window`); `vendor/PATCHES.md` and the
standalone history `vendor-upstream/winit-0.30.13` in the Tilo repository list
them commit by commit. When a winit release carries them, the SDK moves back to
`winit` and this fork ends.

`WindowEvent::PenSample` is a variant upstream does not have, so a crates.io
crate that matches every `WindowEvent` does not build against this fork. The
workspace has none: Smithay's `backend_winit`, the one that did, is not
enabled.

//! `word-processor` — the runnable word-processor app: the framework app over
//! a rich-text record.
//!
//! Usage:
//!   word-processor [FILE]                         open FILE's own record (imported the first time)
//!   word-processor --shots OUT [--scenario NAME]  render one headless frame to OUT.png
//!
//! Collaboration: launched from the shell, the compositor's broker is
//! reached through the `TILO_SVC_COLLAB_FD` descriptor the launcher injects —
//! nothing to configure. A raw process started outside a session has no
//! collaboration service; the descriptor is inherited, not a socket path an
//! app can fabricate.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

// Thin build only: pull the shared toolkit dylib
// (`libruntime_dylib.so`) into the link graph. It statically contains
// the `ui` toolkit this app links -- and with it wgpu, vello,
// cosmic-text, automerge, taffy and fluent -- so with `-C
// prefer-dynamic` rustc serves `ui` and libstd from the dylib instead
// of baking ~18 MB of `.text` into this binary. Naming the crate here
// is what keeps rustc from eliminating it as unused. Compiled out in
// normal (static) builds.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(word_processor::framework::WordProcessorApp);

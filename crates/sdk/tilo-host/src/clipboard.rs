//! Clipboard backends — how a `ui` process accesses the selection:
//! `tilo_clipboard` under the toolkit's module path.
//!
//! `ui::text_selection`'s verbs resolve to the clipboard of the current
//! [`crate::host::HostServices`]: a host-installed one (the compositor hosts
//! several apps in-process and mirrors its own selection for them), this
//! target's [`system`] clipboard, or an [`InProcessClipboard`] when the host
//! offers none.

pub use tilo_clipboard::*;

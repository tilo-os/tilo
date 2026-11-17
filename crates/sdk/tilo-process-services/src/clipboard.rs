//! Clipboard backends — how a `ui` process accesses the selection:
//! `tilo_clipboard` under the toolkit's module path.
//!
//! `ui::text_selection`'s verbs resolve to the clipboard of the
//! current [`crate::process::ProcessServices`]: a installed by the process one (the compositor processes
//! several apps in-process and mirrors its own selection for them), this
//! target's [`system`] clipboard, or an [`InProcessClipboard`] when the process
//! offers none.

pub use tilo_clipboard::*;

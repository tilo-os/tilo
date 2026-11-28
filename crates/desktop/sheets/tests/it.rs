#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "dictation.rs"]
mod dictation;
#[path = "directory.rs"]
mod directory;
#[path = "display_confirm.rs"]
mod display_confirm;
#[path = "displays.rs"]
mod displays;
#[path = "invite.rs"]
mod invite;
#[path = "lock.rs"]
mod lock;
#[path = "painted.rs"]
mod painted;
#[path = "power_hold.rs"]
mod power_hold;
#[path = "resource_labels.rs"]
mod resource_labels;
#[path = "shortcut_guide.rs"]
mod shortcut_guide;
#[path = "workspace_settings.rs"]
mod workspace_settings;

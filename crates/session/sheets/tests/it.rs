// One test binary instead of one per file; #[path] keeps the files in place.
#[path = "add_app.rs"]
mod add_app;
#[path = "delete_workspace.rs"]
mod delete_workspace;
#[path = "dictation.rs"]
mod dictation;
#[path = "directory.rs"]
mod directory;
#[path = "display_confirm.rs"]
mod display_confirm;
#[path = "displays.rs"]
mod displays;
#[path = "file_picker.rs"]
mod file_picker;
#[path = "invite.rs"]
mod invite;
#[path = "lock.rs"]
mod lock;
#[path = "portal.rs"]
mod portal;
#[path = "power.rs"]
mod power;
#[path = "resource_labels.rs"]
mod resource_labels;
#[path = "shortcut_guide.rs"]
mod shortcut_guide;
#[path = "uninstall.rs"]
mod uninstall;
#[path = "workspace_settings.rs"]
mod workspace_settings;

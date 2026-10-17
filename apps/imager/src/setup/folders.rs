//! Folder scan DTOs and the frontend folder picker.
pub use imager_backend::folders::*;

/// Open the host's folder chooser without blocking the UI thread.
pub fn pick_folder() -> ui::PortalChannel {
    ui::PickRequest::directory("Choose folder", "Choose").send()
}

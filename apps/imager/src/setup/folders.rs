//! The frontend folder picker; folder scan DTOs live in the backend.

/// Open the host's folder chooser without blocking the UI thread.
pub fn pick_folder() -> ui::PortalChannel {
    ui::PickRequest::directory("Choose folder", "Choose").send()
}

//! The content store the artwork's pictures are kept in: the one the host
//! lends the window, joined to the artwork's route, so a picture placed here
//! reaches every device that opens the artwork. Every window has its own.
//!
//! The framework hands it to the window's code wherever it runs — a view,
//! an action's arguments being built, an action applying — through the
//! window's scope; work that leaves the window's thread (a background
//! decode) is handed the store when it starts.

use tilo_content::ContentStore;

/// The store of the window whose code is running. Outside a window (a
/// unit test of a model function), an empty one in memory.
pub fn store() -> ContentStore {
    ui::tilo_resource::content::current().unwrap_or_else(ContentStore::memory)
}

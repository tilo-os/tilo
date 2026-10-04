//! The content store the artwork's pictures are kept in: the one the host
//! lends the window (`cx.content()`), joined to the artwork's route, so a
//! picture placed here reaches every device that opens the artwork.
//!
//! Pictures are read and stored where no view reaches — an action's
//! arguments, a background decode — so the window's store is adopted here
//! when the app is launched and when its view is built.

use std::sync::{OnceLock, RwLock};

use tilo_content::ContentStore;

fn slot() -> &'static RwLock<Option<ContentStore>> {
    static SLOT: OnceLock<RwLock<Option<ContentStore>>> = OnceLock::new();
    SLOT.get_or_init(RwLock::default)
}

/// Keep the pictures in `store` from now on; `None` leaves the current one.
pub fn adopt(store: Option<&ContentStore>) {
    if let Some(store) = store
        && let Ok(mut slot) = slot().write()
    {
        *slot = Some(store.clone());
    }
}

/// The store the artwork's pictures are kept in. Before a host lent one
/// (a unit test), an in-memory store of this process.
pub fn store() -> ContentStore {
    static MEMORY: OnceLock<ContentStore> = OnceLock::new();
    slot()
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| MEMORY.get_or_init(ContentStore::memory).clone())
}

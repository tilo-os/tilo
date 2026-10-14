//! The content store of the window whose code is running.
//!
//! A record names its binaries (a picture, an attachment) by hash; the bytes
//! live in the content store the host lends the window, joined to the
//! record's route. Every window has its own: two windows of one app in two
//! rooms — in one process, as the shell mounts apps — keep their content
//! apart. The host registers a window's store against the window's wake
//! scope ([`register`]), the scope the toolkit enters whenever it runs the
//! window's code; [`current`] answers the store of the scope entered now.
//!
//! That is how code no view reaches gets it: an action applying
//! ([`ActionCx::content`](crate::ActionCx::content)), an action's arguments
//! being built, the window's views and handlers. Work that leaves the
//! window's thread captures the store when it starts, as it captures its
//! wake handle.

use std::cell::RefCell;

use tilo_content::ContentStore;
use tilo_wake::WakeHandle;

thread_local! {
    static STORES: RefCell<Vec<(WakeHandle, ContentStore)>> = const { RefCell::new(Vec::new()) };
}

/// Keep `store` as the content store of the window whose wake scope
/// `window` names, in place of any it had. Windows that closed are
/// forgotten.
pub fn register(window: &WakeHandle, store: ContentStore) {
    STORES.with(|stores| {
        let mut stores = stores.borrow_mut();
        stores.retain(|(handle, _)| handle.is_active() && !handle.same_open_scope(window));
        stores.push((window.clone(), store));
    });
}

/// The content store of the window whose code is running: the one
/// registered for the wake scope entered now. `None` outside a window, or
/// for one whose host lent no store.
#[must_use]
pub fn current() -> Option<ContentStore> {
    let here = WakeHandle::new();
    STORES.with(|stores| {
        stores
            .borrow()
            .iter()
            .find(|(handle, _)| handle.same_open_scope(&here))
            .map(|(_, store)| store.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_window_reads_its_own_store() {
        let (one, two) = (tilo_wake::WakeScope::new(), tilo_wake::WakeScope::new());
        let (first, second) = (ContentStore::memory(), ContentStore::memory());
        register(&one.handle(), first.clone());
        register(&two.handle(), second);
        let hash = first.put(b"in the first window").unwrap();
        assert!(current().is_none(), "no window runs");
        {
            let _entered = one.enter();
            assert!(current().unwrap().has(&hash));
        }
        {
            let _entered = two.enter();
            assert!(!current().unwrap().has(&hash));
        }
    }
}

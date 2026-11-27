//! The content store each window a host serves reads its record's content from: where the pictures
//! the host loads for the window ([`tilo_ui_base::image_store::ServedWindow`]) are read and, when
//! they are not here yet, asked for, on the window's own content requests.

use std::cell::RefCell;

use tilo_content::ContentStore;
use tilo_ui_base::WakeHandle;
use tilo_ui_base::image_store::{ServedWindow, is_served};

thread_local! {
    /// The content of each served window, by its wake scope, for as long as the scope is open.
    static CONTENT: RefCell<Vec<(WakeHandle, ContentStore)>> = const { RefCell::new(Vec::new()) };
}

/// `window` is built, reading its record's content from `content`: the pictures its host loads for
/// it are read, and asked for, there. A window no host serves keeps nothing here.
pub fn window_built(window: &WakeHandle, content: Option<ContentStore>) {
    CONTENT.with_borrow_mut(|kept| {
        kept.retain(|(kept, _)| kept.same_open_scope(kept) && !kept.same_open_scope(window));
        if let Some(content) = content.filter(|_| is_served(window)) {
            kept.push((window.clone(), content));
        }
    });
}

/// The content store `served` reads its record's content from, once the window is built.
#[must_use]
pub fn content_of(served: &ServedWindow) -> Option<ContentStore> {
    CONTENT.with_borrow(|kept| {
        kept.iter()
            .find(|(kept, _)| kept.same_open_scope(served.window()))
            .map(|(_, content)| content.clone())
    })
}

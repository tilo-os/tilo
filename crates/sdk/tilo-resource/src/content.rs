//! The content store of the window whose code is running.
//!
//! A record names its binaries (a picture, an attachment) by hash; the bytes
//! live in the content store the host lends the window, joined to the
//! record's route. Every window has its own: two windows of one app in two
//! workspaces — in one process, as the shell mounts apps — keep their content
//! apart. The host registers a window's store against the window's wake
//! scope ([`register`]), the scope the toolkit enters whenever it runs the
//! window's code; [`current`] answers the store of the scope entered now.
//!
//! That is how code no view reaches gets it: an action's arguments being
//! built where it is invoked, the window's views and handlers. An action
//! applying keeps nothing here: it stores the references its arguments
//! carry. Work that leaves the
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

/// Why a content reference an action was given does not resolve here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentRefusal {
    /// The value is no `tilo.content:<hash>` reference.
    #[error(
        "{0:?} is not a content reference: give the tilo.content:<hash> a put or another step answered"
    )]
    NotAReference(String),
    /// No window runs, or its host lent it no store.
    #[error("this window has no content store to read {0} from")]
    NoStore(String),
    /// The window's store is not a workspace's, so a hash would name content
    /// from anywhere on this machine.
    #[error("{0} cannot be read here: this window's content store does not name a workspace")]
    NotInWorkspace(String),
    /// The workspace does not reference the content, or this device does not
    /// hold it.
    #[error("{0} is not readable in this workspace: put the content into this workspace first")]
    NotReadable(String),
}

/// The content reference a `tilo.content:<reference>` names, when `value` is one.
#[must_use]
pub fn reference_hash(value: &str) -> Option<&str> {
    tilo_actions::content_hash(value.trim()).filter(|hash| tilo_content::valid_reference(hash))
}

/// The bytes the content reference `address` names, read from the store of
/// the window whose code is running.
///
/// A hash is never a capability by itself: only a store that answers for
/// one workspace ([`tilo_content::ContentStore::workspace_scoped`]) resolves
/// a reference, and it serves an address only when that workspace references
/// it. So a caller can name only content readable in the workspace it acts
/// in.
///
/// # Errors
///
/// A [`ContentRefusal`] saying why, in words for whoever passed it.
pub fn read_reference(address: &str) -> Result<Vec<u8>, ContentRefusal> {
    let hash =
        reference_hash(address).ok_or_else(|| ContentRefusal::NotAReference(address.to_owned()))?;
    read_hash(hash)
}

/// [`read_reference`] for the hash itself, for a caller that accepted it in
/// another spelling.
///
/// # Errors
///
/// As [`read_reference`].
pub fn read_hash(hash: &str) -> Result<Vec<u8>, ContentRefusal> {
    let named = tilo_actions::content_resource(hash).unwrap_or_else(|| hash.to_owned());
    if !tilo_content::valid_reference(hash) {
        return Err(ContentRefusal::NotAReference(named));
    }
    let store = current().ok_or_else(|| ContentRefusal::NoStore(named.clone()))?;
    if !store.workspace_scoped() {
        return Err(ContentRefusal::NotInWorkspace(named));
    }
    // The store verifies what it reads against the address, and opens
    // sealed content with the reference's key.
    store.read(hash).ok_or(ContentRefusal::NotReadable(named))
}

thread_local! {
    static INVOKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `build` as the side invoking an action builds it from its arguments:
/// the data a [`Bytes`](crate::Bytes) argument carries is kept, sealed, in
/// the window's store there, and the action carries the reference. Where an
/// action replays, its arguments already are references.
pub(crate) fn invoking<T>(build: impl FnOnce() -> T) -> T {
    let before = INVOKING.with(|invoking| invoking.replace(true));
    let action = build();
    INVOKING.with(|invoking| invoking.set(before));
    action
}

/// Whether an action is being built where it is invoked ([`invoking`]).
pub(crate) fn is_invoking() -> bool {
    INVOKING.with(std::cell::Cell::get)
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

    fn decode(value: serde_json::Value) -> Result<crate::Bytes, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    /// Where an action is invoked, data its argument carries inline, or by a
    /// reference to content the window's workspace holds, is kept sealed in
    /// the window's store and named by its reference; any other hash is
    /// refused. Where it replays, its stored form is read as it is.
    #[test]
    fn a_bytes_argument_is_kept_sealed_where_the_action_is_invoked() {
        let window = tilo_wake::WakeScope::new();
        let workspace = ContentStore::memory_in_workspace().sealing();
        register(&window.handle(), workspace.clone());
        let picture = vec![9_u8; 4096];
        let held = workspace.put(&picture).unwrap();
        let reference = tilo_actions::content_resource(&held).unwrap();
        let elsewhere = tilo_content::hash_bytes(b"content kept elsewhere");
        let _entered = window.enter();

        assert_eq!(
            invoking(|| decode(serde_json::json!([1, 2, 3]))).unwrap(),
            crate::Bytes::Inline(vec![1, 2, 3])
        );
        let kept = invoking(|| decode(serde_json::json!(picture))).unwrap();
        let sealed = kept.reference().unwrap();
        assert!(tilo_content::is_sealed(&sealed), "{sealed}");
        assert_eq!(kept.read(&workspace), Some(picture.clone()));
        assert!(
            !workspace.has(&tilo_content::hash_bytes(&picture)),
            "never kept in the clear"
        );
        let named = invoking(|| decode(serde_json::json!(reference))).unwrap();
        assert_eq!(named.reference().as_deref(), Some(held.as_str()));
        assert_eq!(named.hash(), *blake3::hash(&picture).as_bytes());

        // The reference alone names the same content: an operation that
        // collects content takes a bare reference, sealed or plain, and the
        // hash content was named by before it was sealed still reads where
        // the bytes were kept so.
        let bare = invoking(|| decode(serde_json::json!(held))).unwrap();
        assert_eq!(bare.hash(), *blake3::hash(&picture).as_bytes());
        assert_eq!(bare.read(&workspace), Some(picture.clone()));
        let plain = ContentStore::memory_in_workspace();
        let plain_hash = plain.put(b"kept in the clear").unwrap();
        register(&window.handle(), plain.clone());
        let earlier = invoking(|| decode(serde_json::json!(plain_hash))).unwrap();
        assert_eq!(earlier.read(&plain), Some(b"kept in the clear".to_vec()));

        let missing = format!("tilo.content:{elsewhere}");
        let refused = invoking(|| decode(serde_json::json!(missing.clone()))).unwrap_err();
        assert!(
            refused.contains(&ContentRefusal::NotReadable(missing).to_string()),
            "{refused}"
        );

        // Replaying, the stored form reads back as it is; a reference to
        // read is no stored form, and inline data is an earlier version's.
        let stored = serde_json::to_value(&kept).unwrap();
        assert_eq!(decode(stored).unwrap(), kept);
        assert!(decode(serde_json::json!(reference)).is_err());
        assert_eq!(
            decode(serde_json::json!(picture)).unwrap(),
            crate::Bytes::of(&picture)
        );
    }

    /// A store that is not a workspace's resolves no reference, even to
    /// content it holds: a hash is never a capability by itself.
    #[test]
    fn a_store_outside_a_workspace_resolves_no_reference() {
        let window = tilo_wake::WakeScope::new();
        let machine = ContentStore::memory();
        register(&window.handle(), machine.clone());
        let held = machine.put(b"on this machine").unwrap();
        let reference = tilo_actions::content_resource(&held).unwrap();
        assert_eq!(
            read_reference(&reference),
            Err(ContentRefusal::NoStore(reference.clone())),
            "outside the window"
        );
        let _entered = window.enter();
        assert_eq!(
            read_reference(&reference),
            Err(ContentRefusal::NotInWorkspace(reference.clone()))
        );
    }
}

//! The page's profile stores: one [`tilo_peer_state::store::Store`] per
//! profile namespace, its files in `IndexedDB` (`web/peer-files.mjs`), held
//! in memory and committed later, in order, by a worker
//! (`web/peer-files-worker.mjs`, [`crate::page_files`]).
//!
//! What a store keeps is what the page's certified peer keeps between
//! sessions ([`crate::peer_files::PagePeerStore`]) and the page's own values.
//! [`flush_peer_store`] resolves when every change made before it is durable.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Function, Promise, Uint8Array};
use tilo_peer_state::store::{Appetite, Backend, Durable, Store};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::page_files::{Committer, PageBackend};
use crate::peer_files::{self, PagePeerStore};

/// An error the page can show: what went wrong, as an `Error` with its stack.
fn error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

/// What a commit's rejection said, kept as an error of the store's.
fn rejected(value: &JsValue) -> io::Error {
    let name = js_sys::Reflect::get(value, &"name".into())
        .ok()
        .and_then(|name| name.as_string());
    let message = js_sys::Reflect::get(value, &"message".into())
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| "the commit failed".into());
    io::Error::other(name.map_or_else(|| message.clone(), |name| format!("{name}: {message}")))
}

/// Hands each commit to the page's worker: `commit(packed) => Promise`,
/// resolving once its transaction completed; `pause() => Promise` resolves
/// once the page ran what waits for it.
struct WorkerCommits {
    commit: Function,
    pause: Function,
}

/// What `called` promised, once it settles.
fn settled(called: Result<JsValue, JsValue>, what: &'static str) -> Durable {
    Box::pin(async move {
        let promise: Promise = called
            .map_err(|value| rejected(&value))?
            .dyn_into()
            .map_err(|_| io::Error::other(format!("the page's {what} must return a promise")))?;
        JsFuture::from(promise)
            .await
            .map(drop)
            .map_err(|value| rejected(&value))
    })
}

impl Committer for WorkerCommits {
    fn commit(&self, commit: Vec<u8>) -> Durable {
        settled(
            self.commit
                .call1(&JsValue::NULL, &Uint8Array::from(commit.as_slice())),
            "commit",
        )
    }

    fn pause(&self) -> Durable {
        settled(self.pause.call0(&JsValue::NULL), "pause")
    }

    fn later(&self, commit: Durable) {
        // A commit that failed keeps its changes for the next one, and the
        // page hears of it through the commit it handed over.
        wasm_bindgen_futures::spawn_local(async move {
            let _ = commit.await;
        });
    }
}

/// An open profile namespace's store.
struct PageFiles {
    store: Arc<Store>,
    backend: Arc<PageBackend<WorkerCommits>>,
}

thread_local! {
    static OPEN: RefCell<HashMap<(String, String), Rc<PageFiles>>> = RefCell::new(HashMap::new());
}

fn opened(profile: &str, device: &str) -> Result<Rc<PageFiles>, JsValue> {
    OPEN.with_borrow(|open| open.get(&(profile.to_owned(), device.to_owned())).cloned())
        .ok_or_else(|| error("open the profile's peer store before using it"))
}

/// The store of the open namespace `namespace` (`{profile, device}` JSON),
/// as the record of `user`'s peer.
pub(crate) fn peer_store(
    namespace: &str,
    user: &[u8; 32],
    channel_vault: [u8; 32],
) -> Result<PagePeerStore, JsError> {
    #[derive(serde::Deserialize)]
    struct Namespace {
        profile: String,
        device: String,
    }
    let namespace: Namespace = serde_json::from_str(namespace).map_err(crate::peer::js_error)?;
    let files = opened(&namespace.profile, &namespace.device).map_err(|_| {
        crate::peer::js_error("open the profile's storage before starting its peer")
    })?;
    Ok(PagePeerStore::new(
        Arc::clone(&files.store),
        user,
        channel_vault,
    ))
}

/// Open the store of the profile namespace `(profile, device)` over the
/// files its database keeps (`rows`, packed as [`crate::page_files::unpack`]
/// reads them), committing what it changes through `commit`
/// (`(packed) => Promise`) and letting the page run, between the slices of a
/// large commit, through `pause` (`() => Promise`).
///
/// # Errors
///
/// The namespace is open already, or its files are not a store.
#[wasm_bindgen]
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "a store takes a shared backend; in the page it stays on its one thread"
)]
pub fn open_peer_store(
    profile: &str,
    device: &str,
    rows: &[u8],
    commit: Function,
    pause: Function,
) -> Result<(), JsValue> {
    let key = (profile.to_owned(), device.to_owned());
    if OPEN.with_borrow(|open| open.contains_key(&key)) {
        return Err(error("the profile's peer store is open already"));
    }
    let backend = PageBackend::open(rows, WorkerCommits { commit, pause }).map_err(error)?;
    let store = Store::open_on(Arc::clone(&backend) as Arc<dyn Backend>, Appetite::StoreAll)
        .map_err(error)?;
    OPEN.with_borrow_mut(|open| {
        open.insert(
            key,
            Rc::new(PageFiles {
                store: Arc::new(store),
                backend,
            }),
        )
    });
    Ok(())
}

/// Resolve once every change the namespace's store made before this is
/// durable, committing what is not.
///
/// # Errors
///
/// The namespace is not open, or the commit failed; its changes are
/// committed again by the next.
#[wasm_bindgen]
pub async fn flush_peer_store(profile: String, device: String) -> Result<(), JsValue> {
    let files = opened(&profile, &device)?;
    files.backend.durable().await.map_err(error)
}

/// Close the namespace's store, once everything it changed is durable.
///
/// # Errors
///
/// The namespace is not open, or holds a change not committed yet.
#[wasm_bindgen]
pub fn close_peer_store(profile: &str, device: &str) -> Result<(), JsValue> {
    if !opened(profile, device)?.backend.settled() {
        return Err(error(
            "the profile's peer store holds changes not committed yet",
        ));
    }
    OPEN.with_borrow_mut(|open| open.remove(&(profile.to_owned(), device.to_owned())));
    Ok(())
}

/// A small page-side value kept in the profile (for example a peer's public
/// certificates), or `None` when absent.
///
/// # Errors
///
/// Rejects an unknown namespace or a value that does not read.
#[wasm_bindgen]
pub fn profile_value(profile: &str, device: &str, name: &str) -> Result<Option<Vec<u8>>, JsValue> {
    peer_files::value(&opened(profile, device)?.store, name).map_err(error)
}

/// Keep (or with `None`, let go) a page-side value in the profile.
///
/// # Errors
///
/// Rejects an unknown namespace or a refused write.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives optional JavaScript bytes only as an owned value"
)]
pub fn set_profile_value(
    profile: &str,
    device: &str,
    name: &str,
    bytes: Option<Vec<u8>>,
) -> Result<(), JsValue> {
    peer_files::keep_value(&opened(profile, device)?.store, name, bytes.as_deref()).map_err(error)
}

/// The certificate and server pin (JSON) the saved peer entry in
/// `(profile, device)` for `user_public_key_hex` is bound to, if any.
///
/// # Errors
///
/// Rejects an unknown namespace or an unreadable store.
#[wasm_bindgen(js_name = savedPeerCertificate)]
pub fn saved_peer_certificate(
    profile: &str,
    device: &str,
    user_public_key_hex: &str,
) -> Result<Option<String>, JsValue> {
    peer_files::saved_certificate(&opened(profile, device)?.store, user_public_key_hex)
        .map_err(error)
}

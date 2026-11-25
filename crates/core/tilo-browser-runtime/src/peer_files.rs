//! What a page keeps in its profile's store, by name: its certified peer's
//! record, channel state and the workspaces it holds with their runs and
//! parts under `peer/<user>/`, and the page's own values under `page/`.
//! The names are the store's state ([`Store::keep_state`]), whatever files
//! hold it: on a page, its database ([`crate::page_files`]).

use std::sync::Arc;

use tilo_peer_state::store::{Store, StoreError};
use tilo_session_client::{PeerEntry, PeerStore};

/// A peer's record in its profile's store, each part of it as the store's
/// state under `peer/<user>/`.
pub(crate) struct PagePeerStore {
    store: Arc<Store>,
    /// `peer/<user>/`.
    under: String,
    /// Seals the channel state. The page keeps it in the platform secret
    /// store, never in this one.
    channel_vault: [u8; 32],
}

/// Where the record of the peer of the person whose key is `user` is kept.
fn peer_record(user_hex: &str) -> String {
    format!("peer/{}/record", user_hex.to_ascii_lowercase())
}

impl PagePeerStore {
    /// The record of `user`'s peer in `store`.
    pub(crate) fn new(store: Arc<Store>, user: &[u8; 32], channel_vault: [u8; 32]) -> Self {
        Self {
            store,
            under: format!("peer/{}/", hex::encode(user)),
            channel_vault,
        }
    }

    /// The state `name`; the files are in memory, so only a name the store
    /// refuses fails to read, and reads as none.
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.store
            .state(&format!("{}{name}", self.under))
            .ok()
            .flatten()
    }

    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.store
            .keep_state(&format!("{}{name}", self.under), bytes)
            .map_err(|error: StoreError| error.to_string())
    }
}

impl PeerStore for PagePeerStore {
    fn load(&self) -> Option<PeerEntry> {
        postcard::from_bytes(&self.get("record")?).ok()
    }

    fn save(&self, record: &PeerEntry) -> Result<(), String> {
        let bytes = postcard::to_stdvec(record).map_err(|error| error.to_string())?;
        self.put("record", &bytes)
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        self.get("channels")
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        self.put("channels", bytes)
    }

    fn load_workspaces(&self) -> Option<Vec<u8>> {
        self.get("workspaces")
    }

    fn keeps_workspaces(&self) -> bool {
        true
    }

    fn save_workspaces(&self, bytes: &[u8]) -> Result<(), String> {
        self.put("workspaces", bytes)
    }

    fn load_workspaces_part(&self, name: &str) -> Option<Vec<u8>> {
        self.get(&format!("parts/{name}"))
    }

    fn save_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.put(&format!("parts/{name}"), bytes)
    }

    fn append_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let mut part = self.get(&format!("parts/{name}")).unwrap_or_default();
        part.extend_from_slice(bytes);
        self.put(&format!("parts/{name}"), &part)
    }

    fn keeps_workspaces_parts(&self) -> bool {
        true
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.channel_vault)
    }
}

/// The certificate and server pin (JSON, `{cert, hostCert}`) the saved peer
/// record in `store` for `user_hex` is bound to, if any.
///
/// # Errors
///
/// The store cannot read it, or the pin does not encode.
pub(crate) fn saved_certificate(store: &Store, user_hex: &str) -> Result<Option<String>, String> {
    let kept = store
        .state(&peer_record(user_hex))
        .map_err(|error| error.to_string())?;
    let Some((cert, server_cert)) = kept
        .and_then(|bytes| postcard::from_bytes::<PeerEntry>(&bytes).ok())
        .and_then(|entry| entry.cert.map(|cert| (cert, entry.server_cert)))
    else {
        return Ok(None);
    };
    let held = tilo_session_client::device::Held { cert, server_cert };
    serde_json::to_string(&held)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Where the page keeps its value `name`.
fn page_value(name: &str) -> String {
    format!("page/{name}")
}

/// The page's value `name` in `store`, or `None` when absent.
///
/// # Errors
///
/// The name is refused, or the value does not read.
pub(crate) fn value(store: &Store, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
    store.state(&page_value(name))
}

/// Keep the page's value `name` in `store`, or with `None` let it go.
///
/// # Errors
///
/// The name is refused, or the store refused the write.
pub(crate) fn keep_value(
    store: &Store,
    name: &str,
    bytes: Option<&[u8]>,
) -> Result<(), StoreError> {
    match bytes {
        Some(bytes) => store.keep_state(&page_value(name), bytes),
        None => store.forget_state(&page_value(name)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;
    use crate::page_files::tests::{Database, now, store};
    use tilo_peer_state::store::Backend as _;

    fn certificate(device: u8) -> tilo_user_identity::DeviceCert {
        tilo_user_identity::DeviceCert {
            user_public_key: [0xab; 32],
            device_public_key: [device; 32],
            device_label: "page".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [device; 64],
            issuer: None,
        }
    }

    /// Everything a page's peer keeps is read back by a peer started again
    /// over what its store committed: its record (and the certificate the
    /// page asks for by the person's key), its channel state, the workspaces
    /// it holds with their parts and runs, and a run let go stays gone.
    #[test]
    fn a_page_peer_reads_back_what_it_kept() {
        let database = Database::default();
        let (backend, kept) = store(&database);
        let user = [0xab; 32];
        let peer = PagePeerStore::new(Arc::new(kept), &user, [7; 32]);
        assert_eq!(peer.load(), None);
        let record = PeerEntry {
            cert: Some(certificate(1)),
            server_cert: Some(certificate(3)),
        };
        peer.save(&record).unwrap();
        peer.save_channels(b"channels").unwrap();
        peer.save_workspaces(b"workspaces").unwrap();
        peer.save_workspaces_part("index", b"one").unwrap();
        peer.append_workspaces_part("index", b" two").unwrap();
        peer.append_workspaces_part("journal", b"first").unwrap();
        now(backend.durable()).unwrap();
        drop(peer);

        let (_, reopened) = store(&database);
        let reopened = Arc::new(reopened);
        let peer = PagePeerStore::new(Arc::clone(&reopened), &user, [7; 32]);
        assert_eq!(peer.load(), Some(record.clone()));
        assert_eq!(peer.load_channels(), Some(b"channels".to_vec()));
        assert_eq!(peer.load_workspaces(), Some(b"workspaces".to_vec()));
        assert_eq!(
            peer.load_workspaces_part("index"),
            Some(b"one two".to_vec())
        );
        assert_eq!(
            peer.load_workspaces_part("journal"),
            Some(b"first".to_vec())
        );
        assert_eq!(peer.channel_vault(), Some([7; 32]));
        assert!(peer.keeps_workspaces() && peer.keeps_workspaces_parts());

        let saved = saved_certificate(&reopened, &hex::encode_upper(user))
            .unwrap()
            .unwrap();
        let held: tilo_session_client::device::Held = serde_json::from_str(&saved).unwrap();
        assert_eq!(Some(held.cert), record.cert);
        assert_eq!(held.server_cert, record.server_cert);
        assert_eq!(
            saved_certificate(&reopened, &hex::encode([0xcd; 32])).unwrap(),
            None
        );
        let other = PagePeerStore::new(reopened, &[0xcd; 32], [7; 32]);
        assert_eq!(
            other.load(),
            None,
            "another person's peer keeps its own record"
        );
    }

    #[test]
    fn page_values_are_kept_and_let_go() {
        let database = Database::default();
        let (backend, kept) = store(&database);
        assert_eq!(value(&kept, "greeting").unwrap(), None);
        keep_value(&kept, "greeting", Some(b"hello")).unwrap();
        keep_value(&kept, "gone", Some(b"soon")).unwrap();
        keep_value(&kept, "gone", None).unwrap();
        assert!(keep_value(&kept, "../outside", Some(b"x")).is_err());
        now(backend.durable()).unwrap();
        let (_, reopened) = store(&database);
        assert_eq!(
            value(&reopened, "greeting").unwrap(),
            Some(b"hello".to_vec())
        );
        assert_eq!(value(&reopened, "gone").unwrap(), None);
    }
}

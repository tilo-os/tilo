//! The app surfaces this page decides its workspaces' writes against, by the
//! content hash each write names.
//!
//! A change carries the hash of the surface of the app version that made it,
//! in the form it is kept by, and a record keeps the version it was started
//! with. A page
//! reading a record an older version started therefore holds its changes
//! until it has that version's surface: it asks its workspaces for it by
//! hash, as it asks for a picture a record references ([`neighbour_wire::content`]),
//! keeps what answers hash to the address asked for, and decides the changes
//! it held the moment it arrives. What it runs an app with is its own
//! surface, read from the app rather than fetched.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use tilo_lock::MutexRecover as _;

/// Surfaces one page keeps at once; past it further ones are not kept. A
/// surface is a few kilobytes of compiled JSON, and a page runs a handful of
/// apps.
const MAX_SURFACES: usize = 256;

/// The surfaces this page fetched from its workspaces, by their BLAKE3 hash.
fn held() -> &'static Mutex<BTreeMap<[u8; 32], Vec<u8>>> {
    static HELD: OnceLock<Mutex<BTreeMap<[u8; 32], Vec<u8>>>> = OnceLock::new();
    HELD.get_or_init(Mutex::default)
}

/// The bytes of the surface `hash`: the one this page runs `app` with, or one
/// it fetched from a workspace.
#[must_use]
pub fn read(app: &str, hash: &str) -> Option<Vec<u8>> {
    running(app, hash).or_else(|| {
        let hash = <[u8; 32]>::try_from(hex::decode(hash).ok()?).ok()?;
        held().lock_recover().get(&hash).cloned()
    })
}

/// Whether this page holds the surface `hash` of `app`.
pub(crate) fn holds(app: &str, hash: &[u8; 32]) -> bool {
    read(app, &hex::encode(hash)).is_some()
}

/// Keep `bytes` as the surface `hash`: whether they were new and hash to the
/// address they came under.
pub(crate) fn keep(hash: &str, bytes: Vec<u8>) -> bool {
    let Some(hash) = hex::decode(hash)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        return false;
    };
    if blake3::hash(&bytes).as_bytes() != &hash {
        return false;
    }
    let mut held = held().lock_recover();
    if held.contains_key(&hash) || held.len() >= MAX_SURFACES {
        return false;
    }
    held.insert(hash, bytes);
    true
}

/// The surface this page runs `app` with, when it is `hash`.
#[cfg(feature = "toolkit")]
fn running(app: &str, hash: &str) -> Option<Vec<u8>> {
    let carried = collab_types::app_surface::hash_of(app)?;
    (hex::encode(carried) == hash)
        .then(|| collab_types::app_surface::bytes_of(app).map(<[u8]>::to_vec))
        .flatten()
}

/// A page that runs no app in its own process runs the surface of none.
#[cfg(not(feature = "toolkit"))]
fn running(_: &str, _: &str) -> Option<Vec<u8>> {
    None
}

/// The surfaces a page decides members' writes against, by the hash a write
/// names.
#[must_use]
pub fn surfaces() -> collab_types::Surfaces {
    collab_types::Surfaces::from_source(std::sync::Arc::new(
        collab_types::surface::ContentSurfaces::new(
            |record: &collab_types::WrittenDocument<'_>, hash: &str| read(record.app, hash),
        ),
    ))
}

/// The surface this page signs its own changes with: the one it runs the app
/// with.
#[cfg(feature = "toolkit")]
fn signed_surface(app: &str) -> Option<[u8; 32]> {
    collab_types::app_surface::hash_of(app)
}

/// Decide every workspace's members' writes against the surfaces this page
/// holds ([`surfaces`]): set once, before the page restores its workspaces. A
/// session started inside a host that decided its process's changes already, as a
/// device's daemon does, reads the host's surfaces after its own: the installed
/// packages and the workspace's kept content stay where the host's changes are decided.
#[cfg(feature = "toolkit")]
pub(crate) fn decide_with_kept_surfaces() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let page = surfaces();
        let surfaces = match (page.source(), tilo_peer_state::default_surfaces().source()) {
            (Some(page), Some(host)) => collab_types::Surfaces::from_source(std::sync::Arc::new(
                ThenHost(std::sync::Arc::clone(page), std::sync::Arc::clone(host)),
            )),
            _ => page,
        };
        tilo_peer_state::decide_with_surfaces(surfaces, signed_surface);
    });
}

/// The page's surfaces, then those of the host the page runs in.
#[cfg(feature = "toolkit")]
struct ThenHost(
    std::sync::Arc<dyn collab_types::receipt::SurfaceSource>,
    std::sync::Arc<dyn collab_types::receipt::SurfaceSource>,
);

#[cfg(feature = "toolkit")]
impl collab_types::receipt::SurfaceSource for ThenHost {
    fn surface(
        &self,
        document: &collab_types::WrittenDocument<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn collab_types::receipt::ReceiptSurface>> {
        self.0
            .surface(document, kind, named)
            .or_else(|| self.1.surface(document, kind, named))
    }
}

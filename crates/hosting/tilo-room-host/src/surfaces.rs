//! The app surfaces this relay decides received changes against.
//!
//! A change's op names the content hash of its app's `surface.toml`; the
//! registry serves every reviewed package's payload blobs by hash, so the
//! relay fetches a surface it has not seen from there, in the background,
//! and holds the ops naming it until it has arrived. What it fetched stays
//! here by hash, whichever version of the app it is: members read a record
//! started by an older version with that version's surface
//! ([`Workspaces::content_answer`](crate::workspaces::Workspaces::content_answer)).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// The registry relays fetch surfaces from unless `TILO_REGISTRY` names one.
pub const DEFAULT_REGISTRY: &str = "https://apps.tilo.so";

/// How long answering a member's request waits for a registry fetch: a
/// surface is a few kilobytes, and a member that waited longer asks again.
const FETCH_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What the relay has of each surface: its bytes, or a fetch under way.
type Fetched = Arc<Mutex<HashMap<String, Option<Vec<u8>>>>>;

/// The surfaces this host fetches from `registry`: one cache for deciding the
/// changes that name them and for answering the members that read them.
struct Surfaces {
    registry: String,
    runtime: Option<tokio::runtime::Handle>,
    bytes: Fetched,
    /// Signalled when a fetch finishes, so a member's request for a surface
    /// this host is still fetching is answered by that one fetch rather than
    /// by the next request.
    fetched: std::sync::Condvar,
}

/// This process's surfaces, once [`decide_receipt_from_registry`] installed
/// them.
static SURFACES: Mutex<Option<Arc<Surfaces>>> = Mutex::new(None);

/// The surface `hash` of `app` when it is the one this host runs `app`
/// with.
#[cfg(feature = "agents")]
pub fn surface_of(app: &str, hash: &str) -> Option<Vec<u8>> {
    let carried = ui::app_surface::hash_of(app)?;
    (hex::encode(carried) == hash)
        .then(|| ui::app_surface::bytes_of(app).map(<[u8]>::to_vec))
        .flatten()
}

/// A host that runs no app in its own process runs the surface of none.
#[cfg(not(feature = "agents"))]
pub const fn surface_of(_: &str, _: &str) -> Option<Vec<u8>> {
    None
}

/// The bytes of the surface `hash` of `app` this host keeps: the one it runs
/// `app` with, or one the registry fetched for a change that named it, whoever
/// ran that version. A surface it holds neither is asked of the registry, and
/// this says `None` until it arrives.
pub(crate) fn kept(app: &str, hash: &str) -> Option<Vec<u8>> {
    surface_of(app, hash).or_else(|| request(hash))
}

/// The bytes of the surface `hash` of `app` a member asked this host for,
/// waiting for the registry's answer when the host has not seen it yet: what
/// a record started by an older version of an app reads its writes with.
///
/// Blocks until the fetch lands or [`FETCH_WAIT`] passes, so it is called off
/// the async threads that serve members.
pub(crate) fn answer(app: &str, hash: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = surface_of(app, hash) {
        return Some(bytes);
    }
    let surfaces = SURFACES.lock_recover().clone()?;
    // One fetch for every member asking at once: the one already under way,
    // or this one.
    let _ = request(hash);
    let (known, _) = surfaces
        .fetched
        .wait_timeout_while(surfaces.bytes.lock_recover(), FETCH_WAIT, |known| {
            matches!(known.get(hash), Some(None))
        })
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    known.get(hash).cloned().flatten()
}

/// What this host fetched for the surface `hash`, or a fetch started for it.
fn request(hash: &str) -> Option<Vec<u8>> {
    let surfaces = SURFACES.lock_recover().clone()?;
    let mut known = surfaces.bytes.lock_recover();
    if let Some(known) = known.get(hash) {
        return known.clone();
    }
    // ignored-failure: a hash that is not one names no surface; the member asking is told so.
    let Some(blob) = hex::decode(hash)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        tracing::warn!(%hash, "a surface is asked for by a hash that is not one");
        return None;
    };
    let runtime = surfaces.runtime.clone()?;
    known.insert(hash.to_owned(), None);
    drop(known);
    let blobs = tilo_app_registry_blobs(&surfaces.registry);
    let hash = hash.to_owned();
    runtime.spawn(async move {
        let result = blobs.fetch_all(&[tilo_app_package::BlobHash(blob)]).await;
        let mut known = surfaces.bytes.lock_recover();
        if let Some(bytes) = result.ok().and_then(|mut all| all.pop()) {
            known.insert(hash, Some(bytes.to_vec()));
            drop(known);
            surfaces.fetched.notify_all();
            arrived();
        } else {
            // Asked again by the next op or member that names it.
            known.remove(&hash);
            drop(known);
            surfaces.fetched.notify_all();
        }
    });
    None
}

/// The surface the changes this host signs for `app` name: the one it runs
/// the app with, for its own writes and its bots' alike.
#[cfg(feature = "agents")]
fn signed_surface(app: &str) -> Option<[u8; 32]> {
    ui::app_surface::hash_of(app)
}

/// A relay running no app in its own process signs no app's changes.
#[cfg(not(feature = "agents"))]
const fn signed_surface(_: &str) -> Option<[u8; 32]> {
    None
}

/// Decide every change this process receives against the surfaces of the
/// apps this host runs in its own process, and others fetched by hash from
/// `registry`. Call it once, on the runtime the relay runs on, before the
/// relay starts.
pub fn decide_receipt_from_registry(registry: &str) {
    *SURFACES.lock_recover() = Some(Arc::new(Surfaces {
        registry: registry.trim_end_matches('/').to_owned(),
        runtime: tokio::runtime::Handle::try_current().ok(),
        bytes: Fetched::default(),
        fetched: std::sync::Condvar::new(),
    }));
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(Arc::new(collab_types::surface::ContentSurfaces::new(
            |record: &collab_types::Record<'_>, hash: &str| kept(record.app, hash),
        ))),
        signed_surface,
    );
}

/// How many app surfaces arrived here since the process started.
fn arrival_count() -> &'static tokio::sync::watch::Sender<u64> {
    static ARRIVALS: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> =
        std::sync::OnceLock::new();
    ARRIVALS.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// Changes each time an app surface arrives here: a change held for it can
/// be decided now. A workspace's governor replays what its relay retains then
/// ([`crate::workspaces::Workspaces`]), so a held change is never let go for good.
pub(crate) fn arrivals() -> tokio::sync::watch::Receiver<u64> {
    arrival_count().subscribe()
}

/// Why the relay refuses the change the delta `delta` carries by its author's
/// role: a Viewer's role grants nothing to write, so no document change it
/// sends is one the workspace takes, and the relay says so of a change of an
/// encrypted record too, which it cannot read. `None` for any other role, for
/// a workspace whose governor projected no standings (`standings`), and for a
/// delta that carries no document change at all.
pub(crate) fn role_refusal(
    delta: &[u8],
    standings: Option<&crate::workspace::Standings>,
    author_device: [u8; 32],
) -> Option<String> {
    // What a role says about a document change it sends: an app's record or the
    // workspace's own. A plane's key material and presence frames are no
    // document change and pass.
    let (_, _, body) = crate::coedit::retention_view(delta)?;
    if !matches!(
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body),
        Ok(neighbour_wire::NeighbourMsg::AppDocument { .. }
            | neighbour_wire::NeighbourMsg::SyncResp { .. })
    ) {
        return None;
    }
    let standing = standings?.get(&author_device)?;
    if standing.policy.active_role(&standing.person) != Some(tilo_action_policy::Role::Viewer) {
        return None;
    }
    Some(
        collab_types::ReceiptRefusal::NotGranted {
            node: tilo_action_policy::TreePath::workspace(&standing.policy.workspace).readable(),
            kind: tilo_action_policy::ActionKind::Undoable,
        }
        .to_string(),
    )
}

/// Tell whoever follows [`arrivals`] that an app surface arrived.
pub(crate) fn arrived() {
    arrival_count().send_modify(|count| *count = count.wrapping_add(1));
}

fn tilo_app_registry_blobs(registry: &str) -> app_registry::http_blobs::HttpBlobs {
    app_registry::http_blobs::HttpBlobs::new(registry.trim_end_matches('/').to_owned())
}

/// Why the relay refuses the change the delta `delta` carries, if it does:
/// a change that names no action, a cleartext op that brings claims of its
/// own, a claim to an action or field the app's surface does not publish,
/// one writing a unit outside its action's footprint, or, where the workspace's
/// governor projected its members' standings (`standings`), an action the
/// author is not granted. Ops whose surface has not arrived here yet are
/// left to the workspace's devices, which hold them. `payloads` reads the
/// document frame.
pub(crate) fn refusal(
    payloads: &dyn crate::payloads::ShellPayloads,
    delta: &[u8],
    standings: Option<&crate::workspace::Standings>,
) -> Option<String> {
    let (_, _, body) = crate::coedit::retention_view(delta)?;
    let (route, envelope) = payloads.open_document(body)?;
    let record = collab_types::Record {
        room: &route.cluster_id,
        app: &route.app_id,
        record: &route.record_id,
    };
    let author = standings.map(|standings| standings.get(envelope.author.as_bytes()));
    let allows = |node: &tilo_action_policy::TreePath, kind| match author {
        // No projection: membership already decided who may write here.
        None => true,
        Some(None) => false,
        Some(Some(standing)) => {
            standing.answer(
                node,
                kind,
                &tilo_action_policy::Context::at(tilo_clock::unix_now()),
            ) == tilo_action_policy::Answer::Allow
        }
    };
    collab_types::decide_op(
        &envelope,
        &record,
        None,
        None,
        &tilo_peer_state::default_surfaces(),
        &allows,
    )
    .err()
    .map(|refusal| refusal.to_string())
}

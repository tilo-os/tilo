//! The app surfaces this relay decides received changes against.
//!
//! A change's op names the content hash of its app's `surface.toml`; the
//! registry serves every reviewed package's payload blobs by hash, so the
//! relay fetches a surface it has not seen from there, in the background,
//! and holds the ops naming it until it has arrived.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The registry relays fetch surfaces from unless `TILO_REGISTRY` names one.
pub const DEFAULT_REGISTRY: &str = "https://apps.tilo.so";

/// What the relay has of each surface: its bytes, or a fetch under way.
type Fetched = Arc<Mutex<HashMap<String, Option<Vec<u8>>>>>;

/// Decide every change this process receives against surfaces fetched by
/// hash from `registry`. Call it once, on the runtime the relay runs on,
/// before the relay starts.
pub fn decide_receipt_from_registry(registry: &str) {
    let blobs = tilo_app_registry_blobs(registry);
    let fetched: Fetched = Arc::default();
    let runtime = tokio::runtime::Handle::try_current().ok();
    let read = move |_: &collab_types::Record<'_>, hash: &str| -> Option<Vec<u8>> {
        let mut known = fetched.lock().ok()?;
        match known.get(hash) {
            Some(Some(bytes)) => return Some(bytes.clone()),
            Some(None) => return None,
            None => {}
        }
        let blob: [u8; 32] = hex::decode(hash).ok()?.try_into().ok()?;
        let runtime = runtime.as_ref()?;
        known.insert(hash.to_owned(), None);
        drop(known);
        let blobs = blobs.clone();
        let fetched = Arc::clone(&fetched);
        let hash = hash.to_owned();
        runtime.spawn(async move {
            let result = blobs.fetch_all(&[tilo_app_package::BlobHash(blob)]).await;
            let Ok(mut known) = fetched.lock() else {
                return;
            };
            match result.ok().and_then(|mut all| all.pop()) {
                Some(bytes) => {
                    known.insert(hash, Some(bytes.to_vec()));
                }
                // Asked again by the next op that names it.
                None => {
                    known.remove(&hash);
                }
            }
        });
        None
    };
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(Arc::new(collab_types::surface::ContentSurfaces::new(
            read,
        ))),
        // The relay signs no app's changes.
        |_| None,
    );
}

fn tilo_app_registry_blobs(registry: &str) -> app_registry::http_blobs::HttpBlobs {
    app_registry::http_blobs::HttpBlobs::new(registry.trim_end_matches('/').to_owned())
}

/// Why the relay refuses the change the delta `delta` carries, if it does:
/// a change that names no action, a cleartext op that brings claims of its
/// own, a claim to an action or field the app's surface does not publish,
/// one writing a unit outside its action's footprint, or, where the room's
/// governor projected its members' standings (`standings`), an action the
/// author is not granted. Ops whose surface has not arrived here yet are
/// left to the room's devices, which hold them.
pub(crate) fn refusal(
    delta: &[u8],
    standings: Option<&crate::workspace::Standings>,
) -> Option<String> {
    let (_, _, body) = crate::coedit::retention_view(delta)?;
    let Ok(tilo_peer_state::NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        record_id,
        envelope,
        ..
    }) = postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(body)
    else {
        return None;
    };
    let record = collab_types::Record {
        room: &cluster_id,
        app: &app_id,
        record: &record_id,
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

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

/// The surface `hash` of `record`'s app, when this host runs the app in
/// its own process (a bot's apps) and that is the surface it runs.
#[cfg(feature = "agents")]
fn carried(record: &collab_types::Record<'_>, hash: &str) -> Option<Vec<u8>> {
    surface_of(record.app, hash)
}

/// The surface `hash` of `app` when it is the one this host runs `app`
/// with.
#[cfg(feature = "agents")]
pub(crate) fn surface_of(app: &str, hash: &str) -> Option<Vec<u8>> {
    let carried = ui::app_surface::hash_of(app)?;
    (hex::encode(carried) == hash)
        .then(|| ui::app_surface::bytes_of(app).map(<[u8]>::to_vec))
        .flatten()
}

#[cfg(not(feature = "agents"))]
const fn carried(_: &collab_types::Record<'_>, _: &str) -> Option<Vec<u8>> {
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
    let blobs = tilo_app_registry_blobs(registry);
    let fetched: Fetched = Arc::default();
    let runtime = tokio::runtime::Handle::try_current().ok();
    let read = move |record: &collab_types::Record<'_>, hash: &str| -> Option<Vec<u8>> {
        if let Some(bytes) = carried(record, hash) {
            return Some(bytes);
        }
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
                    drop(known);
                    arrived();
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
/// be decided now. A room's governor replays what its relay retains then
/// ([`crate::rooms::Rooms`]), so a held change is never let go for good.
pub(crate) fn arrivals() -> tokio::sync::watch::Receiver<u64> {
    arrival_count().subscribe()
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
/// one writing a unit outside its action's footprint, or, where the room's
/// governor projected its members' standings (`standings`), an action the
/// change's invoker is not granted. Ops whose surface has not arrived here
/// yet are left to the room's devices, which hold them. `payloads` reads the
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
    // A person editing a document is granted by their own standing; an agent
    // or a bot doing so is bounded by its delegation, which the governor put
    // in the projected standing. Who invoked a change is decided by the
    // invoker the change records, not by whoever holds the device: one
    // device runs a person's windows and their agents.
    let invoker = invocation_invoker(&envelope);
    let now = tilo_clock::unix_now();
    // `Err` where the change claims somebody else's authority: it is decided
    // by nobody's.
    let author: Option<Result<tilo_action_policy::Standing, ()>> = standings.map(|standings| {
        let standing = standings.get(envelope.author.as_bytes()).ok_or(())?;
        match claimed_subject(invoker.as_ref(), &standing.person).ok_or(())? {
            Claimed::Person => Ok(standing.clone()),
            Claimed::Through(subject) => Ok(standing.through(subject)),
        }
    });
    let allows = |node: &tilo_action_policy::TreePath, kind| match &author {
        // No projection: membership already decided who may write here.
        None => true,
        Some(Err(())) => false,
        Some(Ok(standing)) => {
            standing.answer(node, kind, &tilo_action_policy::Context::at(now))
                == tilo_action_policy::Answer::Allow
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

/// Who invoked the change `op` carries, as the first of its changes records
/// it in its history entry. A change the framework makes, or one whose
/// entry does not decode, has none, and is decided as the author acting for
/// themselves.
fn invocation_invoker(op: &collab_types::SignedOp) -> Option<tilo_actions::ActionInvoker> {
    let changes = collab_types::cleartext_changes(op).ok()??;
    changes.iter().find_map(|change| {
        change
            .message()
            .and_then(tilo_actions::ActionContext::from_change_message)
            .and_then(|context| context.invoker)
    })
}

/// Whose authority decides a change, as its invoker records it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Claimed {
    /// The person themselves: the standing as it is.
    Person,
    /// The person acting through a delegation of theirs (an agent, a bot).
    Through(tilo_action_policy::Subject),
}

/// The delegation `invoker` invokes a change through, when it invokes it
/// through one: an agent or a bot acts for the person who delegated it, so
/// the change is decided by that person's standing bounded by the
/// delegation, not by the person's own role. [`Claimed::Person`] for a
/// person, a Shortcut or an app, decided by the standing as it is.
///
/// `None` where the change claims a person other than `person`, whose
/// authority nothing here may decide.
fn claimed_subject(invoker: Option<&tilo_actions::ActionInvoker>, person: &str) -> Option<Claimed> {
    use tilo_actions::ActorKind;
    let Some(invoker) = invoker else {
        return Some(Claimed::Person);
    };
    let subject = match invoker.kind {
        ActorKind::Person | ActorKind::Shortcut | ActorKind::App => return Some(Claimed::Person),
        ActorKind::Agent => tilo_action_policy::Subject::Agent(invoker.actor.clone()),
        ActorKind::Bot => tilo_action_policy::Subject::Bot(invoker.actor.clone()),
    };
    match invoker.on_behalf_of.as_deref() {
        None => Some(Claimed::Through(subject)),
        Some(acted_for) if acted_for == person => Some(Claimed::Through(subject)),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests;

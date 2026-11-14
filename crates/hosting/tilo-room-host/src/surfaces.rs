//! The app surfaces this host decides received changes against, kept with the
//! workspace's content.
//!
//! A change's op names the content hash of its app's `surface.toml`, and the
//! change is decided against that version's surface, whoever ran it. A host
//! has the surfaces of the apps it runs in its own process; every other one is
//! content of the workspace, kept under its state directory by hash, which the
//! host asks its members for when a change names one it lacks
//! ([`crate::workspaces`]). What arrives stays, whichever version of the app it
//! is: members read a record started by an older version with that version's
//! surface
//! ([`Workspaces::content_answer`](crate::workspaces::Workspaces::content_answer)),
//! and a registry is nowhere on that path — it serves installs, never history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// How many surfaces one host keeps. A surface is a few kilobytes of
/// `surface.toml`, and a host that met more versions than this asks for the
/// rest again rather than keep an unbounded map.
const MAX_SURFACES: usize = 4096;

/// The surfaces this host has, by BLAKE3 hash: what it runs an app with, and
/// what the workspace gave it.
struct Surfaces {
    dir: PathBuf,
    bytes: Mutex<HashMap<[u8; 32], Vec<u8>>>,
}

/// This process's surfaces, once [`decide_receipt`] installed them.
static SURFACES: Mutex<Option<Arc<Surfaces>>> = Mutex::new(None);

fn installed() -> Option<Arc<Surfaces>> {
    SURFACES.lock_recover().clone()
}

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

/// The bytes of the surface `hash` of `app` this host decides its changes
/// with: the one it runs the app with, or one the workspace gave it. The
/// first is put in the content plane here, so a host that runs a later version
/// of the app still reads what was written with this one.
pub(crate) fn kept(app: &str, hash: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = surface_of(app, hash) {
        keep(hash, &bytes);
        return Some(bytes);
    }
    read(hash)
}

/// The bytes of the surface `hash` this host keeps, checked against the
/// address they are kept under.
pub(crate) fn read(hash: &str) -> Option<Vec<u8>> {
    let hash = address(hash)?;
    let surfaces = installed()?;
    if let Some(bytes) = surfaces.bytes.lock_recover().get(&hash) {
        return Some(bytes.clone());
    }
    // The plane is the workspace's, not this process's memory: a surface
    // another start kept is read here, and kept for the rest of this one.
    let bytes = std::fs::read(surfaces.dir.join(hex::encode(hash))).ok()?;
    (blake3::hash(&bytes).as_bytes() == &hash).then(|| {
        let mut known = surfaces.bytes.lock_recover();
        if known.len() < MAX_SURFACES {
            known.insert(hash, bytes.clone());
        }
        bytes
    })
}

/// Whether this host keeps the surface `hash`.
pub(crate) fn holds(hash: &str) -> bool {
    address(hash).is_some_and(|hash| {
        installed().is_some_and(|surfaces| surfaces.bytes.lock_recover().contains_key(&hash))
            || read(&hex::encode(hash)).is_some()
    })
}

/// Keep `bytes` as the surface `hash`: whether they were new to the plane and
/// hash to the address they came under. A surface that arrived is one this
/// host can decide the changes it held for
/// ([`arrivals`]).
pub(crate) fn keep(hash: &str, bytes: &[u8]) -> bool {
    let Some(hash) = address(hash) else {
        return false;
    };
    if blake3::hash(bytes).as_bytes() != &hash {
        return false;
    }
    let Some(surfaces) = installed() else {
        return false;
    };
    {
        let mut known = surfaces.bytes.lock_recover();
        if known.contains_key(&hash) || known.len() >= MAX_SURFACES {
            return false;
        }
        known.insert(hash, bytes.to_vec());
    }
    // Written beside the workspace's state, as the content it is: a start
    // after this one reads it there rather than asking for it again.
    if let Err(error) = write(&surfaces.dir, &hex::encode(hash), bytes) {
        tracing::warn!(%error, "a surface could not be written to the content plane");
    }
    arrived();
    true
}

/// Write `bytes` as `name` under `dir`, whole or not at all.
fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{name}.part"));
    std::fs::write(&part, bytes)?;
    std::fs::rename(&part, dir.join(name))
}

/// The [u8; 32] address `hash` names, when it names one.
pub(crate) fn address(hash: &str) -> Option<[u8; 32]> {
    tilo_failure::ignored!(
        "a hash that is not one names no surface this host keeps",
        hex::decode(hash)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    )
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

/// The surfaces this build publishes for the history the apps' older versions
/// wrote: one file per version of a built-in app's `surface.toml`, laid out by
/// the image's build beside its programs (`TILO_SURFACES` names another).
const BUILT_SURFACES: &str = "/usr/local/share/tilo/surfaces";

/// Publish the surfaces this build carries into the content plane `dir`, by
/// their hash: a change written by an older version of an app than this build
/// runs is decided here even when no member still has that version. Files
/// already in the plane are left as they are, so a start publishes once.
fn publish_built(dir: &Path) {
    let built = std::env::var_os("TILO_SURFACES")
        .map_or_else(|| PathBuf::from(BUILT_SURFACES), PathBuf::from);
    publish_from(&built, dir);
}

/// [`publish_built`] for the surfaces under `built`.
fn publish_from(built: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(built) else {
        return;
    };
    let (mut published, mut kept) = (0u64, 0u64);
    for entry in entries.flatten() {
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let hash = hex::encode(blake3::hash(&bytes).as_bytes());
        if dir.join(&hash).exists() {
            kept = kept.saturating_add(1);
            continue;
        }
        match write(dir, &hash, &bytes) {
            Ok(()) => published = published.saturating_add(1),
            Err(error) => {
                tracing::warn!(%error, %hash, "a surface this build carries was not published");
            }
        }
    }
    if published > 0 {
        tracing::info!(
            published,
            kept,
            "the surfaces this build carries are in the content plane"
        );
    }
}

/// Decide every change this process receives against the surfaces of the apps
/// this host runs in its own process, or keeps under `dir`, its content plane
/// for them. Call it once, on the runtime the relay runs on, before the relay
/// starts.
pub fn decide_receipt(dir: &Path) {
    publish_built(dir);
    let known = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let hash = address(entry.file_name().to_str()?)?;
            let bytes = std::fs::read(entry.path()).ok()?;
            (blake3::hash(&bytes).as_bytes() == &hash).then_some((hash, bytes))
        })
        .take(MAX_SURFACES)
        .collect();
    *SURFACES.lock_recover() = Some(Arc::new(Surfaces {
        dir: dir.to_path_buf(),
        bytes: Mutex::new(known),
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

/// Tell whoever follows [`arrivals`] that an app surface arrived.
pub(crate) fn arrived() {
    arrival_count().send_modify(|count| *count = count.wrapping_add(1));
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
    if standing
        .policy
        .active_role(standing.person_id().unwrap_or_default())
        != Some(tilo_action_policy::Role::Viewer)
    {
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
        workspace: &route.cluster_id,
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::publish_from;

    /// What the image's build laid out: one file per version of the apps'
    /// `surface.toml`, named for the app and the commit, and the same version
    /// twice when two apps shared it.
    #[test]
    fn the_surfaces_a_build_carries_are_published_by_hash_once() {
        let scratch = tempfile::tempdir().unwrap();
        let built = scratch.path().join("built");
        let plane = scratch.path().join("surfaces");
        std::fs::create_dir_all(&built).unwrap();
        let old = b"kind = \"chat.workspace\"\nversion = 1\n";
        let older = b"kind = \"chat.workspace\"\nversion = 2\n";
        std::fs::write(built.join("chat.9102a1.toml"), old).unwrap();
        std::fs::write(built.join("chat.4c1e77.toml"), older).unwrap();
        std::fs::write(built.join("slides.9102a1.toml"), old).unwrap();

        publish_from(&built, &plane);

        for (version, bytes) in [("chat v1", old.as_slice()), ("chat v2", older.as_slice())] {
            let hash = hex::encode(blake3::hash(bytes).as_bytes());
            assert_eq!(
                std::fs::read(plane.join(&hash)).unwrap(),
                bytes,
                "{version} is in the plane, by its hash"
            );
        }
        assert_eq!(
            std::fs::read_dir(&plane).unwrap().count(),
            2,
            "one file per version, not one per file"
        );

        // A start after this one publishes nothing new.
        let kept = std::fs::metadata(plane.join(hex::encode(blake3::hash(old).as_bytes())))
            .unwrap()
            .modified()
            .unwrap();
        publish_from(&built, &plane);
        assert_eq!(std::fs::read_dir(&plane).unwrap().count(), 2);
        assert_eq!(
            std::fs::metadata(plane.join(hex::encode(blake3::hash(old).as_bytes())))
                .unwrap()
                .modified()
                .unwrap(),
            kept
        );
    }
}

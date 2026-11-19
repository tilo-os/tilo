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

/// How many surfaces one host keeps. A surface is a few kilobytes, and a host
/// that met more versions than this asks for the rest again rather than keep
/// an unbounded map.
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
    let carried = collab_types::app_surface::hash_of(app)?;
    (hex::encode(carried) == hash)
        .then(|| collab_types::app_surface::bytes_of(app).map(<[u8]>::to_vec))
        .flatten()
}

/// A host that runs no app in its own process runs the surface of none.
#[cfg(not(feature = "agents"))]
pub const fn surface_of(_: &str, _: &str) -> Option<Vec<u8>> {
    None
}

/// The bytes of the surface `hash` of `app` this host decides its changes
/// with: the one it runs the app with, or one the workspace gave it. The
/// first is put in the content store here, so a host that runs a later version
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
    // The content store is the workspace's, not this process's memory: a surface
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

/// Keep `bytes` as the surface `hash`: whether they were new to the content store and
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
        tracing::warn!(%error, "a surface could not be written to the content content store");
    }
    arrived();
    true
}

/// Write `bytes` as `name` under `dir`, whole or not at all.
fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    tilo_files::native::atomic::write_creating_parent(&dir.join(name), bytes)
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
    collab_types::app_surface::hash_of(app)
}

/// A server running no app in its own process signs no app's changes.
#[cfg(not(feature = "agents"))]
const fn signed_surface(_: &str) -> Option<[u8; 32]> {
    None
}

/// The surfaces this build publishes for the versions of the built-in apps it
/// does not run itself: one file per version of an app's `surface.toml`, laid
/// out by the image's build beside its programs (`TILO_SURFACES` names
/// another).
const BUILT_SURFACES: &str = "/usr/local/share/tilo/surfaces";

/// Publish the surfaces this build carries into the content store `dir`, in
/// the form they are kept by
/// ([`collab_types::surface::canonical`]): a change naming one of them is
/// decided here even when no member still has that version. Files already in
/// the content store are left as they are, so a start publishes once.
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
    let (mut published, mut already) = (0u64, 0u64);
    for entry in entries.flatten() {
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok((_, surface)) = collab_types::surface::SurfaceFile::kept(&bytes) else {
            tracing::warn!(
                file = %entry.path().display(),
                "a surface this build carries does not parse; it is not published"
            );
            continue;
        };
        let hash = hex::encode(blake3::hash(surface.as_bytes()).as_bytes());
        if dir.join(&hash).exists() {
            already = already.saturating_add(1);
            continue;
        }
        match write(dir, &hash, surface.as_bytes()) {
            Ok(()) => published = published.saturating_add(1),
            Err(error) => {
                tracing::warn!(%error, %hash, "a surface this build carries was not published");
            }
        }
    }
    if published > 0 {
        tracing::info!(
            published,
            already,
            "the surfaces this build carries are in the content content store"
        );
    }
}

/// Decide every change this process receives against the surfaces of the apps
/// this host runs in its own process, or keeps under `dir`, its content store
/// for them. Call it once, on the runtime the server runs on, before the server
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
            |record: &collab_types::WrittenDocument<'_>, hash: &str| kept(record.app, hash),
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
/// be decided now. A workspace's governor replays what its server retains then
/// ([`crate::workspaces::Workspaces`]), so a held change is never let go for good.
pub(crate) fn arrivals() -> tokio::sync::watch::Receiver<u64> {
    arrival_count().subscribe()
}

/// Tell whoever follows [`arrivals`] that an app surface arrived.
pub(crate) fn arrived() {
    arrival_count().send_modify(|count| *count = count.wrapping_add(1));
}

/// Why the server refuses the change the delta `delta` carries by its author's
/// role: a Viewer's role grants nothing to write in the workspace, so no
/// document change it sends is one the workspace takes, and the server says so
/// of a change of an encrypted document too, which it cannot read.
/// `None` for any other role, for a workspace whose governor
/// projected nothing its devices may do (`device_access`), and for a delta that carries no
/// document change at all.
pub(crate) fn role_refusal(
    delta: &[u8],
    device_access: Option<&crate::workspace::DeviceAccess>,
    author_device: [u8; 32],
) -> Option<tilo_server_protocol::refusal::Refusal> {
    // What a role says about a document change it sends: an app's record or the
    // workspace's own. A content store's key material and presence frames are no
    // document change and pass.
    let (_, _, body) = crate::coedit::retention_view(delta)?;
    let message = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body).ok()?;
    if !matches!(
        message,
        neighbour_wire::NeighbourMsg::AppDocument { .. }
            | neighbour_wire::NeighbourMsg::SyncResp { .. }
    ) {
        return None;
    }
    let access = device_access?.get(&author_device)?;
    let person = access.person_id().unwrap_or_default();
    if access.policy.active_role(person) != Some(tilo_action_policy::Role::Viewer) {
        return None;
    }
    if let neighbour_wire::NeighbourMsg::AppDocument {
        app_id,
        document_id,
        ..
    } = &message
    {
        let node = collab_types::document_node(access.workspace(), app_id, document_id);
        if access.allows(
            &node,
            tilo_action_policy::ActionKind::Undoable,
            tilo_clock::wall(),
        ) {
            return None;
        }
    }
    Some(tilo_server_protocol::refusal::Refusal::NotGranted(
        collab_types::ReceiptRefusal::NotGranted {
            node: tilo_action_policy::TreePath::workspace(&access.policy.workspace).to_string(),
            kind: tilo_action_policy::ActionKind::Undoable,
        }
        .to_string(),
    ))
}

/// Refuse writes to archived member documents, including frames that omit
/// ownership already known from retained history.
pub(crate) fn own_refusal(
    store: &crate::store::WorkspaceStore,
    person: &[u8; 32],
    delta: &[u8],
) -> Option<tilo_server_protocol::refusal::Refusal> {
    let owner = store.frame_owner(delta)?;
    let reason = if owner == *person {
        collab_types::own_resource::ARCHIVE_READ_ONLY
    } else {
        "another member's private archive"
    };
    Some(tilo_server_protocol::refusal::Refusal::NotAllowed(
        reason.into(),
    ))
}

/// Why the server refuses the change the delta `delta` carries, if it does:
/// a change that names no action, a cleartext op that brings claims of its
/// own, a claim to an action or field the app's surface does not publish,
/// one writing a unit outside its action's footprint, or, where the workspace's
/// governor projected what its members' devices may do (`device_access`), an action the
/// author is not granted. Ops whose surface has not arrived here yet are
/// left to the workspace's devices, which hold them. `payloads` reads the
/// document frame.
/// The governance heads `delta`, a member's change, says it was written
/// under ([`collab_types::SignedOp::governance`]); none for a change that is
/// no document write.
pub(crate) fn governance_named(
    payloads: &dyn crate::payloads::ShellPayloads,
    delta: &[u8],
) -> Vec<[u8; 4]> {
    crate::coedit::retention_view(delta)
        .and_then(|(_, _, body)| payloads.open_document(body))
        .map(|(_, envelope)| envelope.governance)
        .unwrap_or_default()
}

pub(crate) fn refusal(
    payloads: &dyn crate::payloads::ShellPayloads,
    delta: &[u8],
    device_access: Option<&crate::workspace::DeviceAccess>,
    signed_person: Option<[u8; 32]>,
) -> Option<tilo_server_protocol::refusal::Refusal> {
    let (_, _, body) = crate::coedit::retention_view(delta)?;
    let (route, envelope) = payloads.open_document(body)?;
    let record = collab_types::WrittenDocument {
        workspace: &route.cluster_id,
        app: &route.app_id,
        document: &route.document_id,
    };
    let author = device_access.map(|device_access| device_access.get(envelope.author.as_bytes()));
    let person = signed_person.or_else(|| {
        let person = author.flatten()?.person_id()?;
        let mut key = [0; 32];
        hex::decode_to_slice(person, &mut key).ok()?;
        Some(key)
    });
    if route.own.is_some()
        || person.is_some_and(|key| {
            collab_types::OwnResource::recognise(&route.app_id, &route.document_id, key).is_some()
        })
    {
        return Some(tilo_server_protocol::refusal::Refusal::NotAllowed(
            collab_types::own_resource::ARCHIVE_READ_ONLY.into(),
        ));
    }
    let cx = tilo_action_policy::Context::at_time(tilo_clock::wall());
    if let Some(refusal) = document_group_refusal(&route, &envelope, device_access, &cx) {
        return Some(refusal);
    }
    let allows = |invoker: Option<&tilo_action_policy::Acting>,
                  node: &tilo_action_policy::TreePath,
                  kind| match author {
        // No projection: membership already decided who may write here.
        None => true,
        Some(None) => false,
        Some(Some(access)) => {
            collab_types::ChangeAuthor::answer(access, invoker, node, kind, &cx)
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
    .map(|refusal| match refusal {
        collab_types::ReceiptRefusal::NotGranted { .. } => {
            tilo_server_protocol::refusal::Refusal::NotGranted(refusal.to_string())
        }
        other => tilo_server_protocol::refusal::Refusal::NotAllowed(other.to_string()),
    })
}

fn document_group_refusal(
    route: &crate::payloads::Route,
    envelope: &collab_types::SignedOp,
    devices: Option<&crate::workspace::DeviceAccess>,
    context: &tilo_action_policy::Context<'_>,
) -> Option<tilo_server_protocol::refusal::Refusal> {
    use collab_types::{EncryptionMetadata, EncryptionScheme};
    use tilo_action_policy::{ActionKind, Answer};
    use tilo_server_protocol::refusal::Refusal;

    if let Some(EncryptionMetadata {
        scheme: EncryptionScheme::DocumentContentKey { group, .. },
        ..
    }) = envelope.encryption
    {
        let names_group =
            collab_types::WorkspaceId::parse(&route.cluster_id).is_some_and(|workspace| {
                let scope = neighbour_wire::KeyScope::Document {
                    workspace,
                    app: route.app_id.clone(),
                    document: route.document_id.clone(),
                };
                scope.group() == group
            });
        return (!names_group).then(|| {
            Refusal::NotAllowed("the encryption group belongs to another document".into())
        });
    }
    if collab_types::validator::requires_independent_document_keys(
        &route.app_id,
        &route.document_id,
    ) {
        return Some(Refusal::NotAllowed(
            "this document requires its own encryption group".into(),
        ));
    }
    let devices = devices?;
    let node = collab_types::document_node(&route.cluster_id, &route.app_id, &route.document_id);
    devices
        .values()
        .any(|device| device.answer(&node, ActionKind::Read, context) != Answer::Allow)
        .then(|| Refusal::NotAllowed("this document requires its own encryption group".into()))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::publish_from;

    #[test]
    fn the_server_refuses_new_writes_to_a_members_archive_without_a_policy_projection() {
        use crate::payloads::{NeighbourPayloads, Route, ShellPayloads as _};
        use collab_types::{OwnResource, SignedOp};

        let own = OwnResource::new("settings.root", [1; 32]).unwrap();
        let mut route = Route {
            app_id: "so.tilo.settings".into(),
            cluster_id: collab_types::WorkspaceId::from_u128(1).to_string(),
            document_id: own.document_id(),
            own: None,
            readers: None,
        };
        for (declared, signed_person) in [(Some(own.clone()), None), (None, Some(own.owner))] {
            route.own = declared;
            let body = NeighbourPayloads
                .document_body(
                    &[2; 32],
                    &route,
                    SignedOp::new_unsigned(
                        Vec::new(),
                        collab_types::document_artifact_id(
                            &route.app_id,
                            &route.cluster_id,
                            &route.document_id,
                        ),
                        collab_types::Ed25519PublicKey::from_bytes([2; 32]),
                        collab_types::document_attestation_id(
                            &route.app_id,
                            &route.cluster_id,
                            &route.document_id,
                        ),
                        collab_types::OpKind::Change,
                        tilo_clock::wall(),
                        1,
                    ),
                )
                .unwrap();
            let delta = crate::coedit::CoeditPayload::RetainedDelta {
                channel: neighbour_wire::app_document_channel(&route.app_id, &route.document_id),
                body,
            }
            .encode()
            .unwrap();
            assert_eq!(
                super::refusal(&NeighbourPayloads, &delta, None, signed_person),
                Some(tilo_server_protocol::refusal::Refusal::NotAllowed(
                    collab_types::own_resource::ARCHIVE_READ_ONLY.to_owned(),
                )),
            );
        }
    }

    #[test]
    fn a_workspace_key_cannot_admit_a_vault_or_workspace_mailbox_write() {
        use collab_types::{
            ContentRetention, Ed25519PublicKey, EncryptionMetadata, OpKind, SignedOp,
        };
        let workspace = collab_types::WorkspaceId::from_u128(1);
        let context = tilo_action_policy::Context::at(0);
        for (app, document) in [
            (
                "so.tilo.passwords",
                tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT.to_owned(),
            ),
            ("so.tilo.mail", String::new()),
            ("so.tilo.mail", neighbour_wire::document_part("", 0)),
            (
                "so.tilo.mail",
                neighbour_wire::document_part(
                    tilo_action_policy::roles::WORKSPACE_MAIL_MESSAGES_DOCUMENT,
                    0,
                ),
            ),
            (
                "so.tilo.mail",
                tilo_action_policy::roles::WORKSPACE_MAIL_TRUST_DOCUMENT.to_owned(),
            ),
            (
                "so.tilo.mail",
                tilo_action_policy::roles::WORKSPACE_MAIL_COPIES_DOCUMENT.to_owned(),
            ),
        ] {
            let route = crate::payloads::Route {
                app_id: app.into(),
                cluster_id: workspace.to_string(),
                document_id: document.clone(),
                own: None,
                readers: None,
            };
            let mut write = SignedOp::new_unsigned(
                vec![0],
                collab_types::document_artifact_id(app, &route.cluster_id, &document),
                Ed25519PublicKey::from_bytes([1; 32]),
                collab_types::document_attestation_id(app, &route.cluster_id, &document),
                OpKind::Change,
                tilo_clock::wall(),
                1,
            );
            assert!(super::document_group_refusal(&route, &write, None, &context).is_some());
            write.encryption = Some(EncryptionMetadata::content_key(1, ContentRetention::Delta));
            assert!(super::document_group_refusal(&route, &write, None, &context).is_some());
            let group = neighbour_wire::KeyScope::Document {
                workspace,
                app: app.into(),
                document,
            }
            .group();
            write.encryption = Some(EncryptionMetadata::content_group(
                workspace,
                group,
                1,
                ContentRetention::Delta,
            ));
            assert!(super::document_group_refusal(&route, &write, None, &context).is_none());
        }
    }

    /// What the image's build laid out: one file per version of the apps'
    /// `surface.toml`, named for the app and the commit, and the same version
    /// twice when two apps shared it.
    #[test]
    fn the_surfaces_a_build_carries_are_published_by_hash_once() {
        let scratch = tempfile::tempdir().unwrap();
        let built = scratch.path().join("built");
        let content_store = scratch.path().join("surfaces");
        std::fs::create_dir_all(&built).unwrap();
        let old = b"[[resources]]\nkind = \"chat.workspace\"\nlabel = \"Chat\"\nversion = 1\n";
        let older = b"[[resources]]\nkind = \"chat.workspace\"\nlabel = \"Chat\"\nversion = 2\n";
        std::fs::write(built.join("chat.9102a1.toml"), old).unwrap();
        std::fs::write(built.join("chat.4c1e77.toml"), older).unwrap();
        std::fs::write(built.join("slides.9102a1.toml"), old).unwrap();

        publish_from(&built, &content_store);

        for (version, bytes) in [("chat v1", old.as_slice()), ("chat v2", older.as_slice())] {
            let (_, surface) = collab_types::surface::SurfaceFile::kept(bytes).unwrap();
            let hash = hex::encode(blake3::hash(surface.as_bytes()).as_bytes());
            assert_eq!(
                std::fs::read(content_store.join(&hash)).unwrap(),
                surface.as_bytes(),
                "{version} is in the content store, by its hash, in the form it is kept by"
            );
        }
        assert_eq!(
            std::fs::read_dir(&content_store).unwrap().count(),
            2,
            "one file per version, not one per file"
        );

        // A start after this one publishes nothing new.
        let (_, surface) = collab_types::surface::SurfaceFile::kept(old).unwrap();
        let path = content_store.join(hex::encode(blake3::hash(surface.as_bytes()).as_bytes()));
        let kept = std::fs::metadata(&path).unwrap().modified().unwrap();
        publish_from(&built, &content_store);
        assert_eq!(std::fs::read_dir(&content_store).unwrap().count(), 2);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), kept);
    }
}

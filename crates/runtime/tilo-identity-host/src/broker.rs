//! Capability-gated identity metadata and signing service for app sandboxes.

use std::collections::HashMap;
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(test)]
use collab_types::DocumentMutation;
use collab_types::caps::Principal;
use collab_types::{Ed25519PublicKey, SignedOp, document_artifact_id, document_attestation_id};
use ed25519_dalek::Signer;
use runtime_identity::wire::{FRAMING, HANDSHAKE_FRAMING, Request, Response};
use tilo_action_policy::{ActionKind, Answer, Party, Subject, TreePath};
use tilo_clock::unix_now;
use wire_frame::FrameError;

use tilo_desktop_model::{RoomId, Workspace};
use tilo_lock::MutexRecover as _;

/// The resource an app's device-signature request names. A signature is
/// asked for one of the app's own actions, so a grant letting an app sign
/// names this resource and that action.
pub const DEVICE_SIGNATURE_RESOURCE: &str = "identity.device-signature";

/// The committed workspaces the broker authorizes each request against.
pub trait WorkspaceMembership: Send + Sync + 'static {
    /// Run `read` over the workspace whose room is `room`, when this host has it.
    fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace));
}

/// A route's access, generation and content keys, as [`ChannelKeySource`]
/// answers; the keys are empty unless newly ready.
pub type ChannelKeys = (runtime_identity::wire::ChannelAccess, u64, Vec<u8>);

/// The channel service that hands apps their routes' content keys.
pub trait ChannelKeySource: Send + Sync + 'static {
    /// The access this device has to the channel of `route`'s record in
    /// `room`, its generation, and the route's content keys when ready and
    /// newer than `known_generation`; `None` while no channel service runs.
    /// The route goes to the channel's steward with this device's access
    /// request, so it can decide Read on the record itself.
    ///
    /// # Errors
    ///
    /// Returns a message when ready keys cannot be encoded.
    fn access(
        &self,
        room: RoomId,
        route: &collab_types::ChannelRoute,
        known_generation: u64,
    ) -> Option<Result<ChannelKeys, String>>;
}

/// The records of the person's own an app keeps in, or reads from, their
/// room: the person-record kinds its surface declares. Nothing broader is
/// signed for an app in the person's room.
pub trait PersonRecordKinds: Send + Sync + 'static {
    /// The person-record kinds `app_id` declares; empty for none.
    fn person_record_kinds(&self, app_id: &str) -> Vec<String>;

    /// `app_id` attached naming `surface`, the surface file it was started
    /// with. A host that runs no package of the app may take it as the
    /// app's surface; by default it is ignored and only a package's counts.
    fn attached(&self, _app_id: &str, _surface: &[u8]) {}
}

/// A host that knows no app's person records: nothing is signed in the
/// person's room.
impl PersonRecordKinds for () {
    fn person_record_kinds(&self, _app_id: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A host without a channel service: every route is unavailable.
impl ChannelKeySource for () {
    fn access(
        &self,
        _room: RoomId,
        _route: &collab_types::ChannelRoute,
        _known_generation: u64,
    ) -> Option<Result<ChannelKeys, String>> {
        None
    }
}

/// `read` over `room`'s workspace, or `None` when this host has none.
fn read_workspace<T>(
    membership: &dyn WorkspaceMembership,
    room: &RoomId,
    read: impl FnOnce(&Workspace) -> T,
) -> Option<T> {
    let mut read = Some(read);
    let mut answer = None;
    membership.with_workspace(room, &mut |workspace| {
        if let Some(read) = read.take() {
            answer = Some(read(workspace));
        }
    });
    answer
}

pub struct IdentityBrokerHandle {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    listener: Option<thread::JoinHandle<()>>,
    clients: Arc<Mutex<HashMap<u64, UnixStream>>>,
}

impl Drop for IdentityBrokerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        close_clients(&self.clients);
        // The listener sleeps in `accept`; one connection wakes it to see the
        // flag. If nothing can connect it is not waited for.
        if let Some(listener) = self.listener.take()
            && UnixStream::connect(&self.path).is_ok()
        {
            let _ = listener.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

struct ClientConnection {
    id: u64,
    clients: Arc<Mutex<HashMap<u64, UnixStream>>>,
}
impl Drop for ClientConnection {
    fn drop(&mut self) {
        self.clients.lock_recover().remove(&self.id);
    }
}
fn close_clients(clients: &Mutex<HashMap<u64, UnixStream>>) {
    for stream in clients.lock_recover().values() {
        let _ = stream.shutdown(Shutdown::Both);
    }
}
/// Start the identity broker's unix-socket listener at `path`, serving
/// clients on their own thread until the returned handle drops.
///
/// # Errors
///
/// Returns an error if binding the unix socket at `path` fails, or if the
/// listener thread cannot be spawned.
pub fn spawn(
    path: PathBuf,
    tree: Arc<dyn WorkspaceMembership>,
    channels: Arc<dyn ChannelKeySource>,
    person: Arc<dyn PersonRecordKinds>,
) -> io::Result<IdentityBrokerHandle> {
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let clients = Arc::new(Mutex::new(HashMap::new()));
    let thread_clients = clients.clone();
    let listener_thread = thread::Builder::new()
        .name("tilo-identity-broker".into())
        .spawn(move || {
            let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();
            let mut next_client = 0_u64;
            loop {
                match listener.accept() {
                    Ok(_) if thread_stop.load(Ordering::Acquire) => break,
                    Ok((stream, _)) => {
                        let tree = Arc::clone(&tree);
                        let channels = Arc::clone(&channels);
                        let person = Arc::clone(&person);
                        let cancellation = match stream.try_clone() {
                            Ok(cancellation) => cancellation,
                            Err(error) => {
                                tracing::warn!(%error, "identity broker cannot own client cancellation");
                                continue;
                            }
                        };
                        let Some(id) = next_client.checked_add(1) else { break; };
                        next_client = id;
                        thread_clients.lock_recover().insert(id, cancellation);
                        let connection = ClientConnection { id, clients: thread_clients.clone() };
                        workers.retain(|worker| !worker.is_finished());
                        match thread::Builder::new()
                            .name("tilo-identity-client".into())
                            .spawn(move || {
                                serve(stream, &*tree, &*channels, &*person);
                                drop(connection);
                            })
                        {
                            Ok(worker) => workers.push(worker),
                            Err(error) => tracing::warn!(%error, "identity broker could not spawn client worker"),
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        tracing::warn!(%error, "identity broker listener stopped");
                        break;
                    }
                }
            }
            // Admission has stopped: also close clients accepted concurrently
            // with the handle's first cancellation sweep, then release every
            // client's membership and channel ownership before retirement.
            close_clients(&thread_clients);
            for worker in workers {
                let _ = worker.join();
            }
        })?;
    Ok(IdentityBrokerHandle {
        path,
        stop,
        listener: Some(listener_thread),
        clients,
    })
}

fn serve(
    mut stream: UnixStream,
    tree: &dyn WorkspaceMembership,
    channels: &dyn ChannelKeySource,
    person: &dyn PersonRecordKinds,
) {
    // Darwin inherits the listener's nonblocking flag on accept. This worker
    // uses blocking framed I/O; an empty receive is not the end of a session.
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "cannot configure blocking service connection");
        return;
    }
    let Ok(negotiated) = read_frame::<runtime_identity::wire::PresentationHandshake>(&mut stream)
    else {
        return;
    };
    if !matches!(
        negotiated.presentation_version,
        0 | runtime_identity::wire::PRESENTATION_VERSION
    ) {
        let _ = write_action_frame(
            &mut stream,
            &Response::Denied("unsupported identity presentation version".into()),
        );
        return;
    }
    let handshake = negotiated.handshake;
    if let Some(surface) = &handshake.surface {
        person.attached(&handshake.app_id, surface.as_bytes());
    }
    let Some(room) = RoomId::parse(&handshake.workspace_id) else {
        let _ = write_action_frame(
            &mut stream,
            &Response::Denied("invalid workspace identity".into()),
        );
        return;
    };
    while let Ok(request) = read_action_frame::<Request>(&mut stream) {
        let response = match request {
            Request::Metadata => metadata_response(&room, negotiated.presentation_version != 0),
            Request::Sign { action, message } => {
                sign(tree, &room, &handshake.app_id, &action, &message)
            }
            Request::CollaborationJoin { workspace_id } => {
                let bound = Bound {
                    room: &room,
                    app_id: &handshake.app_id,
                    person,
                };
                sign_collaboration_join(&bound, &workspace_id)
            }
            Request::CollaborationOp {
                workspace_id,
                app_id,
                record_id,
                unsigned_op,
            } => sign_collaboration_op(
                tree,
                &Bound {
                    room: &room,
                    app_id: &handshake.app_id,
                    person,
                },
                &workspace_id,
                &app_id,
                &record_id,
                &unsigned_op,
            ),
            Request::ChannelKeys {
                workspace_id,
                app_id,
                record_id,
                known_generation,
            } => channel_keys(
                tree,
                channels,
                &ChannelKeyRoute {
                    bound: Bound {
                        room: &room,
                        app_id: &handshake.app_id,
                        person,
                    },
                    workspace_id: &workspace_id,
                    app_id: &app_id,
                    record_id: &record_id,
                },
                known_generation,
            ),
        };
        if write_action_frame(&mut stream, &response).is_err() {
            return;
        }
    }
}

fn metadata_response(room: &RoomId, presentation: bool) -> Response {
    let profile = match crate::registry::selected_identity_profile(&room.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    let record = match runtime_identity::load_metadata_from_path(&profile.record_path) {
        Ok(record) => record,
        Err(error) => return Response::Error(error.to_string()),
    };
    if record.group_public_key != profile.user_public_key {
        return Response::Denied("identity record no longer matches the selected principal".into());
    }
    let (device, cert) = match require_trusted_current_device(&profile) {
        Ok(material) => material,
        Err(error) => return Response::Denied(error.to_string()),
    };
    let device_certificate = match serde_json::to_vec(&cert) {
        Ok(bytes) => bytes,
        Err(error) => return Response::Error(error.to_string()),
    };
    if presentation {
        return Response::MetadataWithPresentation {
            display_name: record.display_name,
            avatar_color: record.avatar_color,
            group_public_key: record.group_public_key,
            signing_public_key: device.verifying_key().to_bytes(),
            device_certificate: Some(device_certificate),
            threshold: record.threshold,
            total_shares: record.total_shares,
            share_index: record.share_index,
            photo: record.profile_photo,
        };
    }
    Response::Metadata {
        display_name: record.display_name,
        avatar_color: record.avatar_color,
        group_public_key: record.group_public_key,
        signing_public_key: device.verifying_key().to_bytes(),
        device_certificate: Some(device_certificate),
        threshold: record.threshold,
        total_shares: record.total_shares,
        share_index: record.share_index,
    }
}

fn sign(
    tree: &dyn WorkspaceMembership,
    room: &RoomId,
    app_id: &str,
    action: &str,
    message: &[u8],
) -> Response {
    let Some(allowed) = read_workspace(tree, room, |workspace| {
        let node = device_signature_node(&workspace.node(), app_id, action);
        // Caller-chosen bytes require an Outside Allow. The open-app
        // authorship grant covers only Read and Undoable, so it cannot
        // make the device key a signing oracle. Keep the tree's normal
        // specificity, lifetime and Ask/Deny decisions for explicit grants.
        Party::new(
            vec![Subject::App(app_id.to_owned())],
            &workspace.policy.grants,
        )
        .decide(
            &node,
            ActionKind::Outside,
            &tilo_desktop_model::context_at(unix_now()),
        )
        .answer
            == Answer::Allow
    }) else {
        return Response::Denied("workspace is not available".into());
    };
    if !allowed {
        return Response::Denied(format!(
            "{app_id} is not permitted to sign action {action} in this workspace"
        ));
    }
    let profile = match crate::registry::selected_identity_profile(&room.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    match require_trusted_current_device(&profile) {
        Ok((device, _)) => Response::Signature(device.sign(message).to_bytes().to_vec()),
        Err(error) => Response::Denied(error.to_string()),
    }
}

/// The node of `app`'s request that the device sign for its `action`: a
/// grant to the app on this node (or above it) lets it ask.
#[must_use]
pub fn device_signature_node(room: &TreePath, app: &str, action: &str) -> TreePath {
    room.clone()
        .app(app)
        .resource(DEVICE_SIGNATURE_RESOURCE)
        .action(action)
}

/// What a client's launch bound it to: its room and app, and the records
/// of the person's that app declares.
struct Bound<'a> {
    room: &'a RoomId,
    app_id: &'a str,
    person: &'a dyn PersonRecordKinds,
}

impl Bound<'_> {
    /// Whether `workspace_id` is the own room of the person the bound room's
    /// identity is, and the app keeps records of theirs there.
    fn in_person_room(
        &self,
        profile: &crate::registry::IdentityProfile,
        workspace_id: &str,
    ) -> bool {
        collab_types::person_room(&profile.user_public_key).to_string() == workspace_id
            && !self.person.person_record_kinds(self.app_id).is_empty()
    }

    /// Whether the app declares the person's record `record_id`.
    fn keeps_person_record(&self, record_id: &str) -> bool {
        collab_types::person_record_kind(record_id).is_some_and(|kind| {
            self.person
                .person_record_kinds(self.app_id)
                .iter()
                .any(|declared| declared == kind)
        })
    }
}

fn sign_collaboration_join(bound: &Bound<'_>, workspace_id: &str) -> Response {
    // The launch room, or the person's own room for the records of theirs
    // the app declares; the broker admits each of those records alone.
    let foreign = |profile: &crate::registry::IdentityProfile| {
        workspace_id != bound.room.to_string() && !bound.in_person_room(profile, workspace_id)
    };
    if workspace_id != bound.room.to_string()
        && bound.person.person_record_kinds(bound.app_id).is_empty()
    {
        return Response::Denied("collaboration join requested for a different workspace".into());
    }
    let profile = match crate::registry::selected_identity_profile(&bound.room.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    if foreign(&profile) {
        return Response::Denied("collaboration join requested for a different workspace".into());
    }
    match require_trusted_current_device(&profile) {
        Ok((device, _)) => Response::Signature(
            device
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    workspace_id,
                ))
                .to_bytes()
                .to_vec(),
        ),
        Err(error) => Response::Denied(error.to_string()),
    }
}

/// The denial for a document op the receipt does not admit, naming why: an
/// app told only "denied" cannot tell a missing grant from a change it
/// failed to name.
fn denial(
    decided: Result<collab_types::Receipt, collab_types::receipt::ReceiptRefusal>,
) -> Option<Response> {
    match decided {
        Ok(collab_types::Receipt::Admit) => None,
        Ok(collab_types::Receipt::Hold) => Some(Response::Denied(
            "the app's surface is not here to decide this document action".into(),
        )),
        Err(refusal) => Some(Response::Denied(format!(
            "workspace role or capability denies this document action: {refusal}"
        ))),
    }
}

/// Validate the entire unsigned document envelope before lending it the
/// certified device signature. The app cannot turn this into a generic
/// signing oracle: workspace/app are fixed by the launch handshake, artifact
/// and attestation ids are route-derived, and only typed `Splice` document
/// payloads pass.
fn sign_collaboration_op(
    tree: &dyn WorkspaceMembership,
    bound: &Bound<'_>,
    workspace_id: &str,
    app_id: &str,
    record_id: &str,
    unsigned_op: &[u8],
) -> Response {
    let bound_room = bound.room;
    let launch_room = workspace_id == bound_room.to_string();
    // The launch route, or a record of the person's this app keeps in their
    // own room: never another app's, and never a kind it does not declare.
    if app_id != bound.app_id || (!launch_room && !bound.keeps_person_record(record_id)) {
        return Response::Denied(
            "collaboration operation requested for a different launch route".into(),
        );
    }
    let profile = match crate::registry::selected_identity_profile(&bound_room.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    if !launch_room && !bound.in_person_room(&profile, workspace_id) {
        return Response::Denied(
            "collaboration operation requested for a different launch route".into(),
        );
    }
    let (device, _) = match require_trusted_current_device(&profile) {
        Ok(material) => material,
        Err(error) => return Response::Denied(error.to_string()),
    };
    let op: SignedOp = match postcard::from_bytes(unsigned_op) {
        Ok(op) => op,
        Err(error) => {
            return Response::Denied(format!(
                "collaboration operation is not a SignedOp: {error}"
            ));
        }
    };
    let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
    let expected_artifact = document_artifact_id(app_id, workspace_id, record_id);
    let expected_attestation = document_attestation_id(app_id, workspace_id, record_id);
    let now = std::time::SystemTime::now();
    let skew = Duration::from_secs(300);
    let earliest = now
        .checked_sub(skew)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let latest = now.checked_add(skew).unwrap_or(now);
    if op.author != author
        || op.artifact != expected_artifact
        || op.attestation_id != expected_attestation
        || collab_types::validate_document_payload_route(app_id, workspace_id, record_id, &op)
            .is_err()
        || op.timestamp < earliest
        || op.timestamp > latest
    {
        return Response::Denied(
            "collaboration operation fields do not match its certified document route".into(),
        );
    }
    // The person's own room has no roster: it is theirs, and this is their
    // device editing a record of theirs.
    if !launch_room {
        return match op.signing_bytes() {
            Ok(bytes) => Response::Signature(device.sign(&bytes).to_bytes().to_vec()),
            Err(error) => Response::Error(error.to_string()),
        };
    }
    let principal_id =
        Principal::person_from_key(Ed25519PublicKey::from_bytes(profile.user_public_key))
            .id()
            .to_string();
    let decided = read_workspace(tree, bound_room, |workspace| {
        workspace.role_of(&principal_id)?;
        let record = collab_types::Record {
            room: &workspace.policy.room,
            app: app_id,
            record: record_id,
        };
        let allows = |node: &tilo_action_policy::TreePath, kind| {
            workspace.answer_at(&principal_id, node, kind, unix_now()) == Answer::Allow
        };
        Some(collab_types::decide_op(
            &op,
            &record,
            None,
            None,
            &collab_types::Surfaces::process(),
            &allows,
        ))
    });
    let decided = match decided {
        None => return Response::Denied("workspace is not available".into()),
        Some(None) => return Response::Denied("identity is not a workspace member".into()),
        Some(Some(decided)) => decided,
    };
    if let Some(denied) = denial(decided) {
        return denied;
    }
    let signing_bytes = match op.signing_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return Response::Error(error.to_string()),
    };
    Response::Signature(device.sign(&signing_bytes).to_bytes().to_vec())
}

/// The route a [`Request::ChannelKeys`] names, with the launch binding it
/// must match.
struct ChannelKeyRoute<'a> {
    bound: Bound<'a>,
    workspace_id: &'a str,
    app_id: &'a str,
    record_id: &'a str,
}

/// Hand an app the content keys of one route of its own launch, only while
/// this device is trusted and its person may read the workspace. The keys
/// open the route's content and nothing else: no MLS secret leaves the
/// channel service, and another app's or workspace's route is refused.
fn channel_keys(
    tree: &dyn WorkspaceMembership,
    channels: &dyn ChannelKeySource,
    route: &ChannelKeyRoute<'_>,
    known_generation: u64,
) -> Response {
    let bound = &route.bound;
    let launch_room = route.workspace_id == bound.room.to_string();
    // A route of the launch, or a record of the person's the app declares in
    // their own room (its own, or another app's it reads).
    if (launch_room && route.app_id != bound.app_id)
        || (!launch_room && !bound.keeps_person_record(route.record_id))
    {
        return Response::Denied("channel keys requested for a different launch route".into());
    }
    let launch = bound.room.to_string();
    let profile = match crate::registry::selected_identity_profile(&launch) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    if !launch_room && !bound.in_person_room(&profile, route.workspace_id) {
        return Response::Denied("channel keys requested for a different launch route".into());
    }
    // The same trust check as signing, served from memory while none of the
    // files it reads changed (`join_proof`): one `stat` of each instead of
    // reading the device key and certificate. A changed, unreadable or
    // just-written file, a revocation included, is checked again from disk;
    // a refusal is never remembered. No key leaves the check.
    match crate::join_proof::trusted_current_join_proof(&launch) {
        Ok(trusted) if trusted.certificate.user_public_key == profile.user_public_key => {}
        Ok(_) => {
            return Response::Denied(
                "this device certificate does not belong to the selected identity".into(),
            );
        }
        Err(error) => return Response::Denied(error.to_string()),
    }
    let principal_id =
        Principal::person_from_key(Ed25519PublicKey::from_bytes(profile.user_public_key))
            .id()
            .to_string();
    // The person reads their own room; a workspace, as its policy says.
    let reads = !launch_room
        || read_workspace(tree, bound.room, |workspace| {
            let node = collab_types::validator::document_node(
                &workspace.policy.room,
                route.app_id,
                route.record_id,
            );
            workspace.answer_at(&principal_id, &node, ActionKind::Read, unix_now()) != Answer::Deny
        })
        .unwrap_or(false);
    if !reads {
        return Response::Denied("identity may not read this workspace".into());
    }
    // Only the room's canonical form: the channel is the hash of the route
    // in it, which a steward checks the route against.
    let Some(room) =
        RoomId::parse(route.workspace_id).filter(|room| room.to_string() == route.workspace_id)
    else {
        return Response::Denied("channel keys requested for an invalid room".into());
    };
    let record = collab_types::ChannelRoute::new(route.app_id, route.record_id);
    match channels.access(room, &record, known_generation) {
        None => Response::ChannelKeys {
            access: runtime_identity::wire::ChannelAccess::Unavailable,
            generation: 0,
            keys: Vec::new(),
        },
        Some(Err(error)) => Response::Error(error),
        Some(Ok((access, generation, keys))) => Response::ChannelKeys {
            access,
            generation,
            keys,
        },
    }
}

/// This machine's device key and its certificate for the identity selected
/// in `room`, while that certificate is valid, enrolled and not revoked. For
/// the host that governs the room with the device key; apps never get it.
///
/// # Errors
///
/// Returns an error when no identity is selected in `room` or the device is
/// not trusted for it.
pub fn trusted_current_device(
    room: &str,
) -> io::Result<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert)> {
    let profile = crate::registry::selected_identity_profile(room)?
        .ok_or_else(|| io::Error::other("no identity is selected for this workspace"))?;
    require_trusted_current_device(&profile)
}

/// Root-key operations are available only while this machine is covered by
/// a valid, non-revoked `identity -> device` certificate. Apps never receive
/// either key; this check makes revocation effective at the local broker too.
fn require_trusted_current_device(
    profile: &crate::registry::IdentityProfile,
) -> io::Result<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert)> {
    let paths = crate::provisioning::collaboration_identity_paths();
    let device = tilo_user_identity::persist::load_device_key(&paths)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let device_key = device.verifying_key().to_bytes();
    let cert_path = paths.device_cert_for_user(&profile.user_public_key);
    let cert = tilo_user_identity::persist::read_device_cert(&cert_path)
        .map_err(|error| io::Error::other(error.to_string()))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if cert.user_public_key != profile.user_public_key || cert.device_public_key != device_key {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device certificate does not belong to the selected identity and device",
        ));
    }
    // Served from memory only while the fleet files are unchanged, so a
    // revocation is seen by the very next call.
    let fleet = crate::device_fleet::DeviceFleetStore::from_env()?.load_cached(&profile.id)?;
    if fleet.is_revoked(&device_key) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device has been revoked for the selected identity",
        ));
    }
    if !fleet
        .certificates
        .iter()
        .any(|known| known.device_public_key == device_key)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device is not enrolled for the selected identity",
        ));
    }
    Ok((device, cert))
}

/// The public half of the identity and certified device this machine
/// presents in one workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalDeviceIdentity {
    pub profile_id: String,
    pub principal_id: String,
    pub user_root_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

/// The identity selected for `room` and this device's key, only while the
/// device holds a valid, enrolled, non-revoked certificate for it.
///
/// # Errors
///
/// Rejects an invalid or unminted room, a room with no selected identity, and
/// a missing, mismatched, unenrolled or revoked device certificate.
pub fn trusted_current_device_public(room: &str) -> io::Result<LocalDeviceIdentity> {
    let room = RoomId::parse(room)
        .filter(|room| !room.is_nil())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid minted room id"))?;
    let profile =
        crate::registry::selected_identity_profile(&room.to_string())?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no identity selected for this workspace",
            )
        })?;
    let (device, _cert) = require_trusted_current_device(&profile)?;
    Ok(LocalDeviceIdentity {
        profile_id: profile.id.clone(),
        principal_id: profile.principal_id(),
        user_root_public_key: profile.user_public_key,
        device_public_key: device.verifying_key().to_bytes(),
    })
}

fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let bytes = HANDSHAKE_FRAMING.read(stream)?.ok_or(FrameError::Closed)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    HANDSHAKE_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

fn read_action_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    Ok(FRAMING.read_value(stream)?.ok_or(FrameError::Closed)?)
}

fn write_action_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    FRAMING.write_value(stream, value)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_identity::wire::Handshake;

    /// What the client's launch binds it to, for an app that keeps none of
    /// the person's records.
    fn bound<'a>(room: &'a RoomId, app_id: &'a str) -> Bound<'a> {
        Bound {
            room,
            app_id,
            person: &(),
        }
    }

    /// Notes keeps the person's record of what they watched.
    struct NotesKeepsWatched;

    impl PersonRecordKinds for NotesKeepsWatched {
        fn person_record_kinds(&self, app_id: &str) -> Vec<String> {
            if app_id == "so.tilo.notes" {
                vec!["notes.watched".into()]
            } else {
                Vec::new()
            }
        }
    }
    use tilo_action_policy::{Grant, KindSet, Role};

    /// The owner's grant letting Notes ask for a signature of its
    /// `document.save`.
    fn signing_grant(workspace: &Workspace) -> Grant {
        Grant::new(
            "notes-sign-save",
            Subject::App("so.tilo.notes".into()),
            device_signature_node(&workspace.node(), "so.tilo.notes", "document.save"),
            KindSet::ALL,
            Answer::Allow,
            Subject::Person(
                workspace
                    .owner()
                    .map_or_else(String::new, ToString::to_string),
            ),
        )
    }
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use std::io::Read as _;
    use std::os::fd::IntoRawFd;
    use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
    use tilo_desktop_model::{DesktopTree, WorkspaceId};

    /// A host's committed workspaces, as the tests mutate them between requests.
    #[derive(Default)]
    struct WorkspaceTree {
        desktop: DesktopTree,
    }
    impl WorkspaceTree {
        fn shared(self) -> SharedTree {
            SharedTree(RwLock::new(self))
        }
    }
    struct SharedTree(RwLock<WorkspaceTree>);
    impl SharedTree {
        fn write(&self) -> RwLockWriteGuard<'_, WorkspaceTree> {
            self.0.write().expect("tree")
        }
        fn read(&self) -> RwLockReadGuard<'_, WorkspaceTree> {
            self.0.read().expect("tree")
        }
    }
    impl WorkspaceMembership for SharedTree {
        fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace)) {
            if let Some(workspace) = self
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| &workspace.room == room)
            {
                read(workspace);
            }
        }
    }
    fn ensure_collaboration_identity(
        identity_dir: &std::path::Path,
        paths: &tilo_user_identity::IdentityPaths,
    ) -> io::Result<crate::provisioning::ProvisionOutcome> {
        crate::provisioning::ensure_collaboration_identity(
            identity_dir,
            paths,
            crate::provisioning::IdentityStorage::Files,
        )
    }

    /// The identity service's bytes: the launcher's handshake is a
    /// little-endian `u32` length then JSON; every later frame is a
    /// little-endian `u32` length then postcard.
    fn assert_identity_service_wire_bytes_are_pinned() {
        let (mut app, mut service) = UnixStream::pair().expect("pair");
        app.write_all(b"\x21\0\0\0{\"workspace_id\":\"w\",\"app_id\":\"a\"}")
            .expect("handshake");
        let handshake = read_frame::<Handshake>(&mut service).expect("handshake");
        assert_eq!(
            (handshake.workspace_id.as_str(), handshake.app_id.as_str()),
            ("w", "a")
        );

        write_action_frame(&mut service, &Response::Denied("no".into())).expect("write");
        let mut sent = [0_u8; 8];
        app.read_exact(&mut sent).expect("response");
        assert_eq!(sent, [4, 0, 0, 0, 2, 2, b'n', b'o']);
        app.write_all(&[3, 0, 0, 0, 2, 1, b'w']).expect("request");
        assert!(matches!(
            read_action_frame::<Request>(&mut service).expect("request"),
            Request::CollaborationJoin { workspace_id } if workspace_id == "w"
        ));
    }

    #[test]
    fn identity_service_wire_is_pinned_and_signing_requires_the_exact_capability() {
        // Phase `identity_service_wire_bytes_are_pinned`.
        assert_identity_service_wire_bytes_are_pinned();
        // Phase `signing_requires_the_exact_app_action_capability`.
        {
            let temp = tempfile::tempdir().expect("temp");
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = xdg.join("tilo/identity");
            let paths = tilo_user_identity::IdentityPaths::under(&system_root);
            ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

            let room = RoomId::new();
            let mut workspace = Workspace::new(WorkspaceId(1), "Room");
            workspace.set_room(room);
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(workspace);
            let tree = tree.shared();

            assert!(matches!(
                sign(&tree, &room, "so.tilo.notes", "document.save", b"payload"),
                Response::Denied(_)
            ));
            // Use the actual open-app grant: it covers the whole room's
            // content, but not Outside effects such as arbitrary signatures.
            let grant = tilo_action_policy::roles::app_authorship(
                &room.to_string(),
                "so.tilo.notes",
                Subject::Person("owner".into()),
            );
            tree.write().desktop.workspaces[0].grant(grant);
            assert!(
                matches!(
                    sign(&tree, &room, "so.tilo.notes", "document.save", b"payload"),
                    Response::Denied(_)
                ),
                "an open-app authorship grant must not authorise a signature"
            );
            let mut grant = signing_grant(&tree.read().desktop.workspaces[0]);
            grant.answer = Answer::Ask;
            tree.write().desktop.workspaces[0].grant(grant);
            assert!(matches!(
                sign(&tree, &room, "so.tilo.notes", "document.save", b"payload"),
                Response::Denied(_)
            ));
            let grant = signing_grant(&tree.read().desktop.workspaces[0]);
            tree.write().desktop.workspaces[0].grant(grant);
            let Response::Signature(signature) =
                sign(&tree, &room, "so.tilo.notes", "document.save", b"payload")
            else {
                panic!("granted action was not signed");
            };
            let profile = crate::registry::selected_identity_profile(&room.to_string())
                .expect("registry")
                .expect("profile");
            let cert = tilo_user_identity::persist::read_device_cert(
                &paths.device_cert_for_user(&profile.user_public_key),
            )
            .expect("device certificate");
            tilo_user_identity::verify_device_cert(&cert).expect("root-signed certificate");
            assert_eq!(cert.user_public_key, profile.user_public_key);
            VerifyingKey::from_bytes(&cert.device_public_key)
                .expect("key")
                .verify(
                    b"payload",
                    &Signature::from_slice(&signature).expect("signature"),
                )
                .expect("valid device signature");
            assert!(matches!(
                sign(&tree, &room, "so.tilo.notes", "document.delete", b"payload"),
                Response::Denied(_)
            ));

            let device_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
                .expect("device key")
                .verifying_key()
                .to_bytes();
            crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
                .revoke(
                    &profile.id,
                    device_key,
                    tilo_user_identity::RevocationReason::Compromised,
                    1,
                )
                .expect("revoke current device");
            assert!(matches!(
                sign(&tree, &room, "so.tilo.notes", "document.save", b"payload"),
                Response::Denied(reason) if reason.contains("revoked")
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one signing session checked against every route, payload and role binding"
    )]
    fn collaboration_signing_binds_route_payload_and_live_action_role() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let system_root = temp.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &xdg)
            .set("TILO_IDENTITY_ROOT", &system_root)
            .set(crate::provisioning::FILE_SECRETS_ENV, "1");
        let identity_dir = xdg.join("tilo/identity");
        let paths = tilo_user_identity::IdentityPaths::under(&system_root);
        ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

        let room = RoomId::new();
        let workspace_id = room.to_string();
        let app_id = "so.tilo.notes";
        let record_id = "meeting-notes";
        let profile = crate::registry::selected_identity_profile(&workspace_id)
            .expect("registry")
            .expect("profile");
        let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("device key");
        let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());

        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.set_room(room);
        workspace.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let tree = tree.shared();

        let now = std::time::SystemTime::now();
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"automerge document".to_vec())
                .encode()
                .expect("typed mutation"),
            document_artifact_id(app_id, &workspace_id, record_id),
            author,
            document_attestation_id(app_id, &workspace_id, record_id),
            collab_types::OpKind::Change,
            now,
            1,
        );
        let unsigned = postcard::to_stdvec(&op).expect("unsigned envelope");
        let Response::Signature(signature) = sign_collaboration_op(
            &tree,
            &bound(&room, app_id),
            &workspace_id,
            app_id,
            record_id,
            &unsigned,
        ) else {
            panic!("valid certified document operation was not signed");
        };
        let signature = <[u8; 64]>::try_from(signature).expect("64-byte signature");
        device
            .verifying_key()
            .verify(
                &op.signing_bytes().expect("signing bytes"),
                &Signature::from_bytes(&signature),
            )
            .expect("device signature");

        let encrypted = SignedOp::new_unsigned_with_encryption(
            b"opaque MLS application frame".to_vec(),
            document_artifact_id(app_id, &workspace_id, record_id),
            author,
            document_attestation_id(app_id, &workspace_id, record_id),
            collab_types::OpKind::Change,
            now,
            2,
            collab_types::EncryptionMetadata::mls_application(
                3,
                collab_types::document_channel_id(app_id, &workspace_id, record_id),
            ),
        );
        let encrypted_unsigned = postcard::to_stdvec(&encrypted).expect("encrypted envelope");
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                app_id,
                record_id,
                &encrypted_unsigned,
            ),
            Response::Signature(_)
        ));
        let mut wrong_channel = encrypted;
        wrong_channel.encryption.as_mut().expect("metadata").channel =
            collab_types::ChannelId::new();
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                app_id,
                record_id,
                &postcard::to_stdvec(&wrong_channel).expect("wrong channel envelope"),
            ),
            Response::Denied(_)
        ));

        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                "so.tilo.calendar",
                record_id,
                &unsigned,
            ),
            Response::Denied(_)
        ));
        op.payload = b"untyped legacy bytes".to_vec();
        let malformed = postcard::to_stdvec(&op).expect("malformed envelope");
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                app_id,
                record_id,
                &malformed,
            ),
            Response::Denied(_)
        ));

        let mut tree_guard = tree.write();
        let workspace = &mut tree_guard.desktop.workspaces[0];
        workspace.set_owner("someone-else");
        workspace.admit(profile.principal_id(), Role::Commenter);
        drop(tree_guard);
        // Without the app's surface a change is decided as an edit of the
        // record, which a commenter is not granted.
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                app_id,
                record_id,
                &unsigned,
            ),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));
        // With the app's surface a commenter's comment is signed, their edit
        // of the page is not.
        let surface: &str = r#"
[[resources]]
kind = "notes.page"
label = "Page"
version = 1

[[resources.fields]]
owner = "page"
path = "body"
stored = "value"
title = "Body"
type = "String"

[[resources.verbs]]
id = "comment"
kind = "undoable"
label = "Comment"
field = "comments"
params = []
"#;
        let hash = *blake3::hash(surface.as_bytes()).as_bytes();
        let hex = hex::encode(hash);
        collab_types::Surfaces::set_process(collab_types::Surfaces::from_source(
            std::sync::Arc::new(collab_types::surface::ContentSurfaces::new(
                move |_, wanted| (wanted == hex).then(|| surface.as_bytes().to_vec()),
            )),
        ));
        let change = |what: tilo_actions::Did, seq: u64| {
            use automerge::transaction::Transactable as _;
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "written", seq).expect("write");
            let context = tilo_actions::ActionContext {
                what: Some(what),
                ..tilo_actions::ActionContext::for_resource(record_id, "notes.page", "Page")
            };
            doc.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(context.change_message().expect("entry")),
            );
            let mut op = op.clone();
            op.payload = DocumentMutation::Incremental(doc.save_after(&[]))
                .encode()
                .expect("typed mutation");
            op.seq = seq;
            postcard::to_stdvec(&op.with_surface(Some(hash))).expect("envelope")
        };
        let comment = change(
            tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                tilo_actions::ActionKey::new(app_id, "notes.page", "comment"),
                [],
            )),
            10,
        );
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                app_id,
                record_id,
                &comment
            ),
            Response::Signature(_)
        ));
        let edit = change(
            tilo_actions::Did::Edit(tilo_actions::FieldEdit {
                ty: "page".into(),
                field: "body".into(),
                path: vec![tilo_actions::PathStep::Field("body".into())],
                op: tilo_actions::EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            }),
            11,
        );
        assert!(matches!(
            sign_collaboration_op(&tree, &bound(&room, app_id), &workspace_id, app_id, record_id, &edit),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));
        let mut tree_guard = tree.write();
        let workspace = &mut tree_guard.desktop.workspaces[0];
        workspace.admit(profile.principal_id(), Role::Viewer);
        drop(tree_guard);
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&room, app_id),
                &workspace_id,
                app_id,
                record_id,
                &unsigned,
            ),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));
    }

    #[test]
    fn negotiated_photo_uses_actual_selected_record_and_refuses_wrong_root_and_invalid_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let xdg = temp.path().join("config");
        let system_root = temp.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &xdg)
            .set("TILO_IDENTITY_ROOT", &system_root)
            .set(crate::provisioning::FILE_SECRETS_ENV, "1");
        let identity_dir = xdg.join("tilo/identity");
        let paths = tilo_user_identity::IdentityPaths::under(system_root);
        ensure_collaboration_identity(&identity_dir, &paths).unwrap();
        let photo = runtime_identity::ProfilePhoto::new(1, 1, vec![25, 90, 210, 255]).unwrap();
        runtime_identity::update_identity_photo(&identity_dir, "primary", Some(photo.clone()))
            .unwrap();
        let room = RoomId::new();
        let profile = crate::registry::selected_identity_profile(&room.to_string())
            .unwrap()
            .unwrap();
        let original = std::fs::read(&profile.record_path).unwrap();
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.set_room(room);
        workspace.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let socket = temp.path().join("identity.sock");
        let _broker = spawn(
            socket.clone(),
            Arc::new(tree.shared()),
            Arc::new(()),
            Arc::new(()),
        )
        .unwrap();
        let connect = |version| {
            let mut client = UnixStream::connect(&socket).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            write_frame(
                &mut client,
                &runtime_identity::wire::PresentationHandshake {
                    handshake: Handshake {
                        workspace_id: room.to_string(),
                        app_id: "so.tilo.notes".into(),
                        surface: None,
                    },
                    presentation_version: version,
                },
            )
            .unwrap();
            runtime_identity::BrokerSigningIdentity::from_connected_io(client)
        };
        let client = connect(runtime_identity::wire::PRESENTATION_VERSION).unwrap();
        assert_eq!(client.metadata().profile_photo(), Some(&photo));
        assert_eq!(
            client.metadata().group_public_key(),
            profile.user_public_key
        );
        assert!(client.sign(b"photo does not grant signing").is_err());
        drop(client);
        let legacy = connect(0).unwrap();
        assert!(legacy.metadata().profile_photo().is_none());
        assert_eq!(
            legacy.metadata().group_public_key(),
            profile.user_public_key
        );
        drop(legacy);
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record["threshold_identity"]["group_public_key"] = serde_json::json!(vec![7_u8; 32]);
        tilo_files::native::atomic::write_with_mode(
            &profile.record_path,
            &serde_json::to_vec(&record).unwrap(),
            0o600,
        )
        .unwrap();
        assert!(connect(runtime_identity::wire::PRESENTATION_VERSION).is_err());
        for invalid in [
            serde_json::json!({"width":0,"height":1,"rgba":[0,0,0,255]}),
            serde_json::json!({"width":128,"height":128,"rgba":vec![0; runtime_identity::MAX_PHOTO_BYTES + 1]}),
        ] {
            let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
            record["profile_photo"] = invalid;
            tilo_files::native::atomic::write_with_mode(
                &profile.record_path,
                &serde_json::to_vec(&record).unwrap(),
                0o600,
            )
            .unwrap();
            assert!(connect(runtime_identity::wire::PRESENTATION_VERSION).is_err());
        }
        tilo_files::native::atomic::write_with_mode(&profile.record_path, &original, 0o600)
            .unwrap();
        assert_eq!(
            connect(runtime_identity::wire::PRESENTATION_VERSION)
                .unwrap()
                .metadata()
                .profile_photo(),
            Some(&photo)
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one runtime client session checked against each request it may and may not make"
    )]
    fn runtime_client_gets_metadata_and_only_granted_signing() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let service_var = "TILO_SVC_IDENTITY_FD";
        let system_root = temp.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &xdg)
            .set("TILO_IDENTITY_ROOT", &system_root)
            .set(crate::provisioning::FILE_SECRETS_ENV, "1");
        let identity_dir = xdg.join("tilo/identity");
        let paths = tilo_user_identity::IdentityPaths::under(system_root);
        ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

        let room = RoomId::new();
        let workspace_id = room.to_string();
        let profile = crate::registry::selected_identity_profile(&workspace_id)
            .expect("registry")
            .expect("profile");
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.set_room(room);
        workspace.set_owner(profile.principal_id());
        let grant = signing_grant(&workspace);
        workspace.grant(grant);
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let socket = temp.path().join("identity.sock");
        let _broker = spawn(
            socket.clone(),
            Arc::new(tree.shared()),
            Arc::new(()),
            Arc::new(NotesKeepsWatched),
        )
        .expect("broker");
        let mut client = UnixStream::connect(socket).expect("connect");
        write_frame(
            &mut client,
            &Handshake {
                workspace_id: room.to_string(),
                app_id: "so.tilo.notes".into(),
                surface: None,
            },
        )
        .expect("handshake");
        let fd = client.into_raw_fd();
        env.set(service_var, fd.to_string());

        let identity = runtime_identity::load().expect("broker metadata");
        assert!(!identity.group_public_key().iter().all(|byte| *byte == 0));
        let cert: tilo_user_identity::DeviceCert = serde_json::from_slice(
            identity
                .device_certificate()
                .expect("broker supplies identity-to-device certificate"),
        )
        .expect("certificate JSON");
        tilo_user_identity::verify_device_cert(&cert).expect("root-signed certificate");
        assert_eq!(cert.user_public_key, identity.group_public_key());
        assert_eq!(cert.device_public_key, identity.signing_public_key());
        // Leave the accepted worker waiting between frames; Darwin must not treat
        // a temporarily empty nonblocking receive as the end of this session.
        std::thread::sleep(Duration::from_millis(50));
        let signature = identity
            .sign_for("document.save", b"saved")
            .expect("granted sign");
        VerifyingKey::from_bytes(&identity.signing_public_key())
            .expect("key")
            .verify(b"saved", &Signature::from_bytes(&signature))
            .expect("signature");
        let join_proof = identity
            .sign_collaboration_join(&workspace_id)
            .expect("the launcher-bound workspace may request a join proof");
        VerifyingKey::from_bytes(&identity.signing_public_key())
            .expect("device key")
            .verify(
                &runtime_identity::collaboration_join_signing_bytes(&workspace_id),
                &Signature::from_bytes(&join_proof),
            )
            .expect("workspace-scoped device proof");
        assert!(
            identity
                .sign_collaboration_join(&RoomId::new().to_string())
                .is_err(),
            "the identity broker must not sign a join for another workspace"
        );
        let record_id = "record-1";
        let now = std::time::SystemTime::now();
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Incremental(b"automerge changes".to_vec())
                .encode()
                .expect("typed mutation"),
            document_artifact_id("so.tilo.notes", &workspace_id, record_id),
            Ed25519PublicKey::from_bytes(identity.signing_public_key()),
            document_attestation_id("so.tilo.notes", &workspace_id, record_id),
            collab_types::OpKind::Change,
            now,
            1,
        );
        let op_signature = identity
            .sign_collaboration_op(
                &workspace_id,
                "so.tilo.notes",
                record_id,
                &postcard::to_stdvec(&op).expect("unsigned envelope"),
            )
            .expect("owner may sign a typed document operation");
        op.signature = collab_types::Signature::from_bytes(op_signature);
        collab_types::verify_op(&op).expect("identity service signature verifies");
        assert!(identity.sign_for("document.delete", b"no").is_err());

        // The person's own room, for the record of theirs Notes declares.
        let own_room = collab_types::person_room(&identity.group_public_key()).to_string();
        let own_proof = identity
            .sign_collaboration_join(&own_room)
            .expect("an app keeping a record of the person's joins their room");
        VerifyingKey::from_bytes(&identity.signing_public_key())
            .expect("device key")
            .verify(
                &runtime_identity::collaboration_join_signing_bytes(&own_room),
                &Signature::from_bytes(&own_proof),
            )
            .expect("a proof for the person's room");
        let person_op = |record: &str| {
            SignedOp::new_unsigned(
                DocumentMutation::Incremental(b"automerge changes".to_vec())
                    .encode()
                    .expect("typed mutation"),
                document_artifact_id("so.tilo.notes", &own_room, record),
                Ed25519PublicKey::from_bytes(identity.signing_public_key()),
                document_attestation_id("so.tilo.notes", &own_room, record),
                collab_types::OpKind::Change,
                now,
                1,
            )
        };
        let watched = collab_types::person_record_id("notes.watched");
        let mut kept = person_op(&watched);
        kept.signature = collab_types::Signature::from_bytes(
            identity
                .sign_collaboration_op(
                    &own_room,
                    "so.tilo.notes",
                    &watched,
                    &postcard::to_stdvec(&kept).expect("unsigned envelope"),
                )
                .expect("its own record of the person's is signed"),
        );
        collab_types::verify_op(&kept).expect("the person-room change verifies");
        let undeclared = collab_types::person_record_id("notes.bookmarks");
        assert!(
            identity
                .sign_collaboration_op(
                    &own_room,
                    "so.tilo.notes",
                    &undeclared,
                    &postcard::to_stdvec(&person_op(&undeclared)).expect("envelope"),
                )
                .is_err(),
            "a kind the app does not declare is not signed"
        );
        assert!(
            identity
                .sign_collaboration_op(
                    &own_room,
                    "so.tilo.notes",
                    "record-1",
                    &postcard::to_stdvec(&person_op("record-1")).expect("envelope"),
                )
                .is_err(),
            "nothing but the person's records is signed in their room"
        );
        assert!(
            identity
                .channel_keys(&own_room, "so.tilo.notes", &watched, 0)
                .is_ok(),
            "the keys of a record of the person's the app declares are asked for"
        );
        assert!(
            identity
                .channel_keys(&own_room, "so.tilo.notes", &undeclared, 0)
                .is_err(),
            "not those of a kind it does not declare"
        );
        drop(identity);
    }

    #[test]
    fn shutdown_reaps_clients_waiting_for_requests_and_partial_handshakes() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("identity.sock");
        let membership = Arc::new(WorkspaceTree::default().shared());
        let retained = Arc::downgrade(&membership);
        let broker = spawn(socket.clone(), membership, Arc::new(()), Arc::new(())).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_frame(
            &mut client,
            &Handshake {
                workspace_id: RoomId::new().to_string(),
                app_id: "so.tilo.file-explorer".into(),
                surface: None,
            },
        )
        .unwrap();
        write_action_frame(&mut client, &Request::Metadata).unwrap();
        let _: Response = read_action_frame(&mut client).unwrap();
        let mut partial = UnixStream::connect(&socket).unwrap();
        partial
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        partial.write_all(&[0]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while broker.clients.lock().unwrap().len() != 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "both actual client workers must be accepted"
            );
            thread::yield_now();
        }
        drop(broker);
        assert!(
            retained.upgrade().is_none(),
            "accepted workers must release their committed membership before shutdown returns"
        );
        assert!(
            FRAMING
                .read_value::<_, Response>(&mut client)
                .unwrap()
                .is_none()
        );
        let mut byte = [0];
        assert_eq!(partial.read(&mut byte).unwrap(), 0);
    }

    /// A channel service holding keys for every route.
    struct ReadyChannels;
    impl ChannelKeySource for ReadyChannels {
        fn access(
            &self,
            _room: RoomId,
            _route: &collab_types::ChannelRoute,
            _known_generation: u64,
        ) -> Option<Result<ChannelKeys, String>> {
            Some(Ok((
                runtime_identity::wire::ChannelAccess::Ready,
                1,
                vec![1, 2, 3],
            )))
        }
    }

    /// Answering an app's key request checks this device's trust from the
    /// identity files while they are unchanged without reading them again,
    /// and still refuses keys on the very next request after a revocation.
    #[test]
    fn channel_keys_trust_check_reads_identity_files_once_and_sees_revocation() {
        let fixture = crate::join_proof::tests::Fixture::new();
        let room = RoomId::parse(&fixture.room).expect("room");
        let app_id = "so.tilo.notes";
        let profile = crate::registry::selected_identity_profile(&fixture.room)
            .expect("registry")
            .expect("profile");
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.room = room;
        workspace.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let tree = tree.shared();
        let ask = || {
            channel_keys(
                &tree,
                &ReadyChannels,
                &ChannelKeyRoute {
                    bound: bound(&room, app_id),
                    workspace_id: &fixture.room,
                    app_id,
                    record_id: "note",
                },
                0,
            )
        };
        let fleet_loads = crate::device_fleet::loads_on_this_thread();
        let derived = crate::join_proof::tests::mints(|| {
            for _ in 0..3 {
                assert!(matches!(ask(), Response::ChannelKeys { .. }));
            }
        });
        assert_eq!(
            derived, 1,
            "the identity files are read for the first answer only"
        );
        assert!(
            crate::device_fleet::loads_on_this_thread().saturating_sub(fleet_loads) <= 1,
            "the fleet is read at most once"
        );

        let device = trusted_current_device(&fixture.room)
            .expect("trusted device")
            .0
            .verifying_key()
            .to_bytes();
        fixture
            .store()
            .revoke(
                &fixture.identity_id,
                device,
                tilo_user_identity::RevocationReason::Lost,
                crate::join_proof::tests::proof_issued_at(&fixture).saturating_add(1),
            )
            .expect("revoke");
        assert!(
            matches!(ask(), Response::Denied(_)),
            "a revoked device gets no keys on the next request"
        );
        fixture.settle();
        assert!(matches!(ask(), Response::Denied(_)), "nor once it settles");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_idle_listener_sleeps_until_a_client_arrives() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("identity.sock");
        let (broker, listener) = tilo_test_env::idle::spawned_thread("tilo-identity-b", || {
            spawn(
                socket.clone(),
                Arc::new(WorkspaceTree::default().shared()),
                Arc::new(()),
                Arc::new(()),
            )
            .expect("broker")
        });
        tilo_test_env::idle::assert_stays_asleep(&listener, "identity broker listener");

        UnixStream::connect(&socket).expect("still accepts clients");
        let started = std::time::Instant::now();
        drop(broker);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "dropping the broker waited {:?} for its listener",
            started.elapsed()
        );
        assert!(!socket.exists(), "the socket is removed with the broker");
    }
}

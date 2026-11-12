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
#[cfg(test)]
use tilo_action_policy::Subject;
use tilo_action_policy::{ActionKind, Answer};
use tilo_clock::unix_now;
use wire_frame::FrameError;

pub use crate::sources::{ChannelKeySource, ChannelKeys, PersonRecordKinds, WorkspaceMembership};
use tilo_desktop_model::{Workspace, WorkspaceId};

use tilo_lock::MutexRecover as _;

/// The legacy resource for app device-signature grants. Ordinary app
/// connections no longer accept generic signing, regardless of grants.
pub const DEVICE_SIGNATURE_RESOURCE: &str = "identity.device-signature";

/// `read` over `workspace`, or `None` when this host has none.
fn read_workspace<T>(
    membership: &dyn WorkspaceMembership,
    workspace: &WorkspaceId,
    read: impl FnOnce(&Workspace) -> T,
) -> Option<T> {
    let mut read = Some(read);
    let mut answer = None;
    membership.with_workspace(workspace, &mut |committed| {
        if let Some(read) = read.take() {
            answer = Some(read(committed));
        }
    });
    answer
}

/// The identity this device presents in `workspace`: the person its own
/// certificate belongs to (person root plus device subkey), when the
/// workspace has them in it. A workspace without them is a membership bug,
/// answered as a fault here, never as something an app chooses its way out
/// of; the person's own cluster has no roster and is theirs.
fn identity_in(
    tree: &dyn WorkspaceMembership,
    workspace: &WorkspaceId,
) -> Result<crate::registry::IdentityProfile, String> {
    let profile = match crate::registry::identity_for_workspace(&workspace.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Err("this device has no identity to sign in with".into()),
        Err(error) => return Err(error.to_string()),
    };
    if collab_types::personal_cluster(&profile.user_public_key).to_string() == workspace.to_string()
    {
        return Ok(profile);
    }
    let principal = profile.principal_id();
    match read_workspace(tree, workspace, |workspace| {
        workspace.policy.is_member(&principal)
    }) {
        Some(true) => Ok(profile),
        Some(false) => Err(format!(
            "the person this device signs as is not a member of workspace {workspace}"
        )),
        None => Err(format!(
            "workspace {workspace} is not available on this device"
        )),
    }
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
    let Some(workspace) = WorkspaceId::parse(&handshake.workspace_id) else {
        let _ = write_action_frame(
            &mut stream,
            &Response::Denied("invalid workspace identity".into()),
        );
        return;
    };
    while let Ok(request) = read_action_frame::<Request>(&mut stream) {
        let response = match request {
            Request::Metadata => {
                metadata_response(tree, &workspace, negotiated.presentation_version != 0)
            }
            // No policy grant or caller-chosen action can turn the device
            // key into an arbitrary-message signing oracle.
            Request::Sign { .. } => Response::Denied(
                "generic signing is not available to application connections".into(),
            ),
            Request::CollaborationJoin { workspace_id } => {
                let bound = Bound {
                    workspace: &workspace,
                    app_id: &handshake.app_id,
                    person,
                };
                sign_collaboration_join(tree, &bound, &workspace_id)
            }
            Request::CollaborationOp {
                workspace_id,
                app_id,
                record_id,
                unsigned_op,
            } => sign_collaboration_op(
                tree,
                &Bound {
                    workspace: &workspace,
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
                        workspace: &workspace,
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

fn metadata_response(
    tree: &dyn WorkspaceMembership,
    workspace: &WorkspaceId,
    presentation: bool,
) -> Response {
    let profile = match identity_in(tree, workspace) {
        Ok(profile) => profile,
        Err(reason) => return Response::Error(reason),
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

/// What a client's launch bound it to: its workspace and app, and the
/// records of the person's that app declares.
struct Bound<'a> {
    workspace: &'a WorkspaceId,
    app_id: &'a str,
    person: &'a dyn PersonRecordKinds,
}

impl Bound<'_> {
    /// Whether `app_id` is the bound app's, or an id it had before whose records it keeps using.
    fn keeps_records_of(&self, app_id: &str) -> bool {
        app_id == self.app_id
            || self
                .person
                .earlier_ids(self.app_id)
                .iter()
                .any(|old| old == app_id)
    }

    /// Whether the bound app declares that it reads `app_id`'s records: another app's, which
    /// it opens and never writes.
    fn reads_records_of(&self, app_id: &str) -> bool {
        self.person.reads_records_of(self.app_id, app_id)
    }

    /// Whether `workspace_id` is the own cluster of the person the bound
    /// workspace's identity is, and the app keeps records of theirs there.
    fn in_personal_cluster(
        &self,
        profile: &crate::registry::IdentityProfile,
        workspace_id: &str,
    ) -> bool {
        collab_types::personal_cluster(&profile.user_public_key).to_string() == workspace_id
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

fn sign_collaboration_join(
    tree: &dyn WorkspaceMembership,
    bound: &Bound<'_>,
    workspace_id: &str,
) -> Response {
    // The launch workspace, or the person's own cluster for the records of
    // theirs the app declares; the broker admits each of those records alone.
    let foreign = |profile: &crate::registry::IdentityProfile| {
        workspace_id != bound.workspace.to_string()
            && !bound.in_personal_cluster(profile, workspace_id)
    };
    if workspace_id != bound.workspace.to_string()
        && bound.person.person_record_kinds(bound.app_id).is_empty()
    {
        return Response::Denied("collaboration join requested for a different workspace".into());
    }
    let profile = match identity_in(tree, bound.workspace) {
        Ok(profile) => profile,
        Err(reason) => return Response::Error(reason),
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
    let bound_workspace = bound.workspace;
    let launch_workspace = workspace_id == bound_workspace.to_string();
    // The launch route, a record every app of the launch workspace opens
    // (its grants decide the change below, as for the app's own), or a
    // record of the person's this app keeps in their own cluster: never
    // another app's, and never a kind it does not declare.
    let workspace_record =
        launch_workspace && collab_types::workspace_record(app_id, record_id).is_some();
    if (!bound.keeps_records_of(app_id) && !workspace_record)
        || (!launch_workspace && !bound.keeps_person_record(record_id))
    {
        return Response::Denied(
            "collaboration operation requested for a different launch route".into(),
        );
    }
    let profile = match identity_in(tree, bound_workspace) {
        Ok(profile) => profile,
        Err(reason) => return Response::Error(reason),
    };
    if !launch_workspace && !bound.in_personal_cluster(&profile, workspace_id) {
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
    // The person's own cluster has no roster: it is theirs, and this is
    // their device editing a record of theirs.
    if !launch_workspace {
        return match op.signing_bytes() {
            Ok(bytes) => Response::Signature(device.sign(&bytes).to_bytes().to_vec()),
            Err(error) => Response::Error(error.to_string()),
        };
    }
    let principal_id =
        Principal::person_from_key(Ed25519PublicKey::from_bytes(profile.user_public_key))
            .id()
            .to_string();
    let decided = read_workspace(tree, bound_workspace, |workspace| {
        workspace.role_of(&principal_id)?;
        let record = collab_types::Record {
            room: &workspace.policy.workspace,
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
    let launch_workspace = route.workspace_id == bound.workspace.to_string();
    // A route of the launch, a record every app of the launch workspace opens,
    // another app's record of the launch workspace it declares it reads, or a
    // record of the person's the app declares in their own store (its own, or
    // another app's it reads).
    if (launch_workspace
        && !bound.keeps_records_of(route.app_id)
        && !bound.reads_records_of(route.app_id)
        && collab_types::workspace_record(route.app_id, route.record_id).is_none())
        || (!launch_workspace && !bound.keeps_person_record(route.record_id))
    {
        return Response::Denied("channel keys requested for a different launch route".into());
    }
    let launch = bound.workspace.to_string();
    let profile = match identity_in(tree, bound.workspace) {
        Ok(profile) => profile,
        Err(reason) => return Response::Error(reason),
    };
    if !launch_workspace && !bound.in_personal_cluster(&profile, route.workspace_id) {
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
    // The person reads their own cluster, the launch workspace of an app
    // opened there included; a workspace, as its policy says.
    let own_cluster =
        collab_types::personal_cluster(&profile.user_public_key).to_string() == route.workspace_id;
    let reads = own_cluster
        || read_workspace(tree, bound.workspace, |workspace| {
            let node = collab_types::validator::document_node(
                &workspace.policy.workspace,
                route.app_id,
                route.record_id,
            );
            workspace.answer_at(&principal_id, &node, ActionKind::Read, unix_now()) != Answer::Deny
        })
        .unwrap_or(false);
    if !reads {
        return Response::Denied("identity may not read this workspace".into());
    }
    // Only the workspace's canonical form: the channel is the hash of the route
    // in it, which a participant checks the route against.
    let Some(workspace) = WorkspaceId::parse(route.workspace_id)
        .filter(|workspace| workspace.to_string() == route.workspace_id)
    else {
        return Response::Denied("channel keys requested for an invalid workspace".into());
    };
    let record = collab_types::ChannelRoute::new(route.app_id, route.record_id);
    match channels.access(workspace, &record, known_generation) {
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

/// This machine's device key and its certificate for the identity this device
/// presents in `workspace`, while that certificate is valid, enrolled and not
/// revoked.
/// For the host that governs the workspace with the device key; apps never get
/// it.
pub fn trusted_current_device(
    workspace: &str,
) -> io::Result<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert)> {
    let profile = crate::registry::identity_for_workspace(workspace)?
        .ok_or_else(|| io::Error::other("this device has no identity to sign in with"))?;
    require_trusted_current_device(&profile)
}

/// Sign an externally approved moderation operation as the workspace's
/// selected, trusted device. This is a privileged host API, not an app socket
/// request: callers must authorize the complete typed operation before calling.
/// The protocol separates moderation credentials from other device signatures.
pub fn sign_external_moderation(
    workspace: &str,
    operation: &registry_types::ExternalModerationOperation,
) -> io::Result<registry_types::SignedModerationMutation> {
    let (device, _) = trusted_current_device(workspace)?;
    registry_types::sign_external_moderation(operation, &device)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
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
    // revocation — of the device or of the stand-in that issued it — is seen by
    // the very next call.
    let fleet = crate::device_fleet::DeviceFleetStore::from_env()?.load_cached(&profile.id)?;
    if !crate::certificates::admitted(&profile.user_public_key, &fleet, &cert) {
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
    fn bound<'a>(workspace: &'a WorkspaceId, app_id: &'a str) -> Bound<'a> {
        Bound {
            workspace,
            app_id,
            person: &(),
        }
    }

    /// Every app was published as the id this names before, as an installed catalogue believes.
    struct RenamedFrom(&'static str);

    impl PersonRecordKinds for RenamedFrom {
        fn person_record_kinds(&self, _app_id: &str) -> Vec<String> {
            Vec::new()
        }

        fn earlier_ids(&self, _app_id: &str) -> Vec<String> {
            vec![self.0.to_owned()]
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

    /// A legacy signature grant that must no longer authorize signing.
    fn signing_grant(workspace: &Workspace) -> Grant {
        Grant::new(
            "notes-sign-save",
            Subject::App("so.tilo.notes".into()),
            workspace
                .node()
                .app("so.tilo.notes")
                .resource(DEVICE_SIGNATURE_RESOURCE)
                .action("document.save"),
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
    use tilo_desktop_model::{DesktopTree, WorkspaceSlot};

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
        fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace)) {
            if let Some(committed) = self
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|committed| &committed.id() == workspace)
            {
                read(committed);
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
    fn identity_service_wire_is_pinned() {
        assert_identity_service_wire_bytes_are_pinned();
    }

    #[test]
    fn privileged_moderation_signing_requires_the_selected_trusted_device() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let system_root = temp.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &xdg)
            .set("TILO_IDENTITY_ROOT", &system_root)
            .set(crate::provisioning::FILE_SECRETS_ENV, "1");
        let identity_dir = xdg.join("tilo/identity");
        let paths = tilo_user_identity::IdentityPaths::under(&system_root);
        let workspace = WorkspaceId::new().to_string();
        let operation = registry_types::ExternalModerationOperation {
            destination: registry_types::ModerationDestination {
                base_url: "https://registry.example".into(),
                owner_pubkey_hex: hex::encode([7; 32]),
            },
            request_id: "report-1".into(),
            issued_at_unix: 100,
            expires_at_unix: 200,
            mutation: registry_types::ModerationMutation::FileReport {
                package_id: "so.example.app".into(),
                category: "security".into(),
                summary: "Unexpected access to private files".into(),
                evidence_digest_hex: hex::encode([8; 32]),
            },
        };
        assert!(sign_external_moderation(&workspace, &operation).is_err());
        ensure_collaboration_identity(&identity_dir, &paths).expect("identity");
        let signed = sign_external_moderation(&workspace, &operation).expect("host signs");
        registry_types::verify_moderation_mutation(&signed).expect("valid signature");
        assert_eq!(signed.destination.as_ref(), Some(&operation.destination));
        assert_eq!(signed.mutation, operation.mutation);
        assert_eq!(signed.request_id, operation.request_id);
        assert_eq!(signed.issued_at_unix, operation.issued_at_unix);
        assert_eq!(signed.expires_at_unix, operation.expires_at_unix);
        let (device, _) = trusted_current_device(&workspace).expect("trusted device");
        assert_eq!(
            signed.actor_pubkey_hex,
            hex::encode(device.verifying_key().to_bytes())
        );
        let mut invalid = operation.clone();
        invalid.expires_at_unix = invalid.issued_at_unix;
        assert!(sign_external_moderation(&workspace, &invalid).is_err());
        let profile = crate::registry::identity_for_workspace(&workspace)
            .expect("registry")
            .expect("profile");
        crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
            .revoke(
                &profile.id,
                device.verifying_key().to_bytes(),
                tilo_user_identity::RevocationReason::Compromised,
                1,
            )
            .expect("revoke device");
        let error = sign_external_moderation(&workspace, &operation).expect_err("revoked");
        assert!(error.to_string().contains("revoked"));
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

        let workspace = WorkspaceId::new();
        let workspace_id = workspace.to_string();
        let app_id = "so.tilo.notes";
        let record_id = "meeting-notes";
        let profile = crate::registry::identity_for_workspace(&workspace_id)
            .expect("registry")
            .expect("profile");
        let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("device key");
        let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());

        let mut committed = Workspace::in_workspace(WorkspaceSlot(1), workspace, "Workspace");
        committed.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(committed);
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
            &bound(&workspace, app_id),
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
                &bound(&workspace, app_id),
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
                &bound(&workspace, app_id),
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
                &bound(&workspace, app_id),
                &workspace_id,
                "so.tilo.calendar",
                record_id,
                &unsigned,
            ),
            Response::Denied(_)
        ));
        // An app renamed from Notes, as the installed catalogue believes it, changes the record
        // Notes kept, where it is; an app claiming nothing of Notes does not.
        let renamed = RenamedFrom(app_id);
        let as_renamed = Bound {
            workspace: &workspace,
            app_id: "so.tilo.notebook",
            person: &renamed,
        };
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &as_renamed,
                &workspace_id,
                app_id,
                record_id,
                &unsigned
            ),
            Response::Signature(_)
        ));
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&workspace, "so.tilo.notebook"),
                &workspace_id,
                app_id,
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
                &bound(&workspace, app_id),
                &workspace_id,
                app_id,
                record_id,
                &malformed,
            ),
            Response::Denied(_)
        ));

        let mut tree_guard = tree.write();
        let committed = &mut tree_guard.desktop.workspaces[0];
        committed.set_owner("someone-else");
        committed.admit(profile.principal_id(), Role::Commenter);
        drop(tree_guard);
        // Without the app's surface a change is decided as an edit of the
        // record, which a commenter is not granted.
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&workspace, app_id),
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
                &bound(&workspace, app_id),
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
            sign_collaboration_op(&tree, &bound(&workspace, app_id), &workspace_id, app_id, record_id, &edit),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));
        let mut tree_guard = tree.write();
        let committed = &mut tree_guard.desktop.workspaces[0];
        committed.admit(profile.principal_id(), Role::Viewer);
        drop(tree_guard);
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &bound(&workspace, app_id),
                &workspace_id,
                app_id,
                record_id,
                &unsigned,
            ),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));
    }

    /// Any app of a workspace writes the records every app of it opens (the
    /// workspace's emoji, Chat's), decided by the workspace's grants on them,
    /// and no other record of their owner's.
    #[test]
    fn any_app_of_the_workspace_signs_its_workspace_records_and_no_other_of_their_owner() {
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

        let workspace = WorkspaceId::new();
        let workspace_id = workspace.to_string();
        let launched = "so.tilo.notes";
        let owner = collab_types::WORKSPACE_RECORDS[0];
        let emoji = collab_types::workspace_record_id(owner.kind);
        let profile = crate::registry::identity_for_workspace(&workspace_id)
            .expect("registry")
            .expect("profile");
        let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("device key");
        let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let mut committed = Workspace::in_workspace(WorkspaceSlot(1), workspace, "Workspace");
        committed.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(committed);
        let tree = tree.shared();
        let unsigned = |record: &str| {
            postcard::to_stdvec(&SignedOp::new_unsigned(
                DocumentMutation::Snapshot(b"automerge document".to_vec())
                    .encode()
                    .expect("typed mutation"),
                document_artifact_id(owner.app, &workspace_id, record),
                author,
                document_attestation_id(owner.app, &workspace_id, record),
                collab_types::OpKind::Change,
                std::time::SystemTime::now(),
                1,
            ))
            .expect("unsigned envelope")
        };
        let sign = |record: &str| {
            sign_collaboration_op(
                &tree,
                &bound(&workspace, launched),
                &workspace_id,
                owner.app,
                record,
                &unsigned(record),
            )
        };
        assert!(matches!(sign(&emoji), Response::Signature(_)));
        assert!(
            matches!(sign("tab:0000000000000000"), Response::Denied(_)),
            "the owner's other records stay its own"
        );
        let mut tree_guard = tree.write();
        let committed = &mut tree_guard.desktop.workspaces[0];
        committed.set_owner("someone-else");
        committed.admit(profile.principal_id(), Role::Viewer);
        drop(tree_guard);
        assert!(matches!(
            sign(&emoji),
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
        let workspace = WorkspaceId::new();
        let profile = crate::registry::identity_for_workspace(&workspace.to_string())
            .unwrap()
            .unwrap();
        let original = std::fs::read(&profile.record_path).unwrap();
        let mut committed = Workspace::in_workspace(WorkspaceSlot(1), workspace, "Workspace");
        committed.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(committed);
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
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            client
                .set_write_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            write_frame(
                &mut client,
                &runtime_identity::wire::PresentationHandshake {
                    handshake: Handshake {
                        workspace_id: workspace.to_string(),
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
    fn runtime_client_gets_metadata_but_never_generic_signing() {
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

        let workspace = WorkspaceId::new();
        let workspace_id = workspace.to_string();
        let profile = crate::registry::identity_for_workspace(&workspace_id)
            .expect("registry")
            .expect("profile");
        let mut committed = Workspace::in_workspace(WorkspaceSlot(1), workspace, "Workspace");
        committed.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(committed);
        let tree = Arc::new(tree.shared());
        let socket = temp.path().join("identity.sock");
        let _broker = spawn(
            socket.clone(),
            tree.clone(),
            Arc::new(()),
            Arc::new(NotesKeepsWatched),
        )
        .expect("broker");
        let mut client = UnixStream::connect(socket).expect("connect");
        write_frame(
            &mut client,
            &Handshake {
                workspace_id: workspace.to_string(),
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
        assert!(identity.sign_for("document.save", b"saved").is_err());
        let grant = tilo_action_policy::roles::app_authorship(
            &workspace_id,
            "so.tilo.notes",
            Subject::Person(profile.principal_id()),
        );
        tree.write().desktop.workspaces[0].grant(grant);
        assert!(identity.sign_for("document.save", b"saved").is_err());
        let grant = signing_grant(&tree.read().desktop.workspaces[0]);
        tree.write().desktop.workspaces[0].grant(grant);
        assert!(
            identity.sign_for("document.save", b"saved").is_err(),
            "even an exact legacy signature grant must be denied"
        );
        // The ordinary socket must deny even a broad Outside grant and
        // alternate labels, without dispatching on the app's action string.
        let node = tree.read().desktop.workspaces[0].node();
        tree.write().desktop.workspaces[0].grant(Grant::new(
            "notes-outside",
            Subject::App("so.tilo.notes".into()),
            node,
            KindSet::ALL,
            Answer::Allow,
            Subject::Person(profile.principal_id()),
        ));
        for action in [
            "document.save",
            "document.delete",
            "sign",
            "moderation.report",
            "",
        ] {
            assert!(
                identity.sign_for(action, b"caller-chosen payload").is_err(),
                "broad Outside grants must not enable generic signing for {action:?}"
            );
        }
        assert!(identity.sign(b"caller-chosen payload").is_err());
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
                .sign_collaboration_join(&WorkspaceId::new().to_string())
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

        let own_cluster = collab_types::personal_cluster(&identity.group_public_key()).to_string();
        let own_proof = identity
            .sign_collaboration_join(&own_cluster)
            .expect("an app keeping a record of the person's joins their store");
        VerifyingKey::from_bytes(&identity.signing_public_key())
            .expect("device key")
            .verify(
                &runtime_identity::collaboration_join_signing_bytes(&own_cluster),
                &Signature::from_bytes(&own_proof),
            )
            .expect("a proof for the person's store");
        let person_op = |record: &str| {
            SignedOp::new_unsigned(
                DocumentMutation::Incremental(b"automerge changes".to_vec())
                    .encode()
                    .expect("typed mutation"),
                document_artifact_id("so.tilo.notes", &own_cluster, record),
                Ed25519PublicKey::from_bytes(identity.signing_public_key()),
                document_attestation_id("so.tilo.notes", &own_cluster, record),
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
                    &own_cluster,
                    "so.tilo.notes",
                    &watched,
                    &postcard::to_stdvec(&kept).expect("unsigned envelope"),
                )
                .expect("its own record of the person's is signed"),
        );
        collab_types::verify_op(&kept).expect("the personal-cluster change verifies");
        let undeclared = collab_types::person_record_id("notes.bookmarks");
        assert!(
            identity
                .sign_collaboration_op(
                    &own_cluster,
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
                    &own_cluster,
                    "so.tilo.notes",
                    "record-1",
                    &postcard::to_stdvec(&person_op("record-1")).expect("envelope"),
                )
                .is_err(),
            "nothing but the person's records is signed in their store"
        );
        assert!(
            identity
                .channel_keys(&own_cluster, "so.tilo.notes", &watched, 0)
                .is_ok(),
            "the keys of a record of the person's the app declares are asked for"
        );
        assert!(
            identity
                .channel_keys(&own_cluster, "so.tilo.notes", &undeclared, 0)
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
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        write_frame(
            &mut client,
            &Handshake {
                workspace_id: WorkspaceId::new().to_string(),
                app_id: "so.tilo.file-explorer".into(),
                surface: None,
            },
        )
        .unwrap();
        write_action_frame(&mut client, &Request::Metadata).unwrap();
        let _: Response = read_action_frame(&mut client).unwrap();
        let mut partial = UnixStream::connect(&socket).unwrap();
        partial
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        partial.write_all(&[0]).unwrap();
        tilo_test_env::wait_until("both actual client workers to be accepted", || {
            broker.clients.lock().unwrap().len() == 2
        });
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
            _workspace: WorkspaceId,
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

    /// An app opened in the person's own store (Settings) is answered the
    /// keys of the person's records it declares there: that store is no
    /// workspace with a policy, and the person reads it.
    #[test]
    fn an_app_opened_in_the_persons_own_cluster_gets_the_keys_of_their_records() {
        let fixture = crate::join_proof::tests::Fixture::new();
        let own_cluster = WorkspaceId::parse(
            &collab_types::personal_cluster(&fixture.user_public_key).to_string(),
        )
        .expect("the person's store");
        let own = own_cluster.to_string();
        let app_id = "so.tilo.notes";
        let watched = collab_types::person_record_id("notes.watched");
        let answer = channel_keys(
            &WorkspaceTree::default().shared(),
            &ReadyChannels,
            &ChannelKeyRoute {
                bound: Bound {
                    workspace: &own_cluster,
                    app_id,
                    person: &NotesKeepsWatched,
                },
                workspace_id: &own,
                app_id,
                record_id: &watched,
            },
            0,
        );
        assert!(
            matches!(answer, Response::ChannelKeys { .. }),
            "the person reads their own cluster: {answer:?}"
        );
    }

    /// Today declares that it reads Tasks' records.
    struct TodayReadsTasks;

    impl PersonRecordKinds for TodayReadsTasks {
        fn person_record_kinds(&self, _app_id: &str) -> Vec<String> {
            Vec::new()
        }

        fn reads_records_of(&self, reader: &str, owner: &str) -> bool {
            reader == "so.tilo.lang.today" && owner == "so.tilo.lang.tasks"
        }
    }

    /// An app is handed the keys of another app's record it declares it reads, as the broker
    /// admits its read-only join, and of no other app's.
    #[test]
    fn an_app_gets_the_keys_of_the_records_it_declares_it_reads() {
        let fixture = crate::join_proof::tests::Fixture::new();
        let workspace = WorkspaceId::parse(
            &collab_types::personal_cluster(&fixture.user_public_key).to_string(),
        )
        .expect("the person's store");
        let launch = workspace.to_string();
        let keys_of = |owner: &str| {
            channel_keys(
                &WorkspaceTree::default().shared(),
                &ReadyChannels,
                &ChannelKeyRoute {
                    bound: Bound {
                        workspace: &workspace,
                        app_id: "so.tilo.lang.today",
                        person: &TodayReadsTasks,
                    },
                    workspace_id: &launch,
                    app_id: owner,
                    record_id: "",
                },
                0,
            )
        };
        let tasks = keys_of("so.tilo.lang.tasks");
        assert!(
            matches!(tasks, Response::ChannelKeys { .. }),
            "Today reads Tasks' record: {tasks:?}"
        );
        let chat = keys_of("so.tilo.lang.chat");
        assert!(
            matches!(chat, Response::Denied(_)),
            "Today declares no read of Chat: {chat:?}"
        );
    }

    /// Answering an app's key request checks this device's trust from the
    /// identity files while they are unchanged without reading them again,
    /// and still refuses keys on the very next request after a revocation.
    #[test]
    fn channel_keys_trust_check_reads_identity_files_once_and_sees_revocation() {
        let fixture = crate::join_proof::tests::Fixture::new();
        let workspace = WorkspaceId::parse(&fixture.workspace).expect("workspace");
        let app_id = "so.tilo.notes";
        let profile = crate::registry::identity_for_workspace(&fixture.workspace)
            .expect("registry")
            .expect("profile");
        let mut committed = Workspace::in_workspace(WorkspaceSlot(1), workspace, "Workspace");
        committed.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(committed);
        let tree = tree.shared();
        let ask = || {
            channel_keys(
                &tree,
                &ReadyChannels,
                &ChannelKeyRoute {
                    bound: bound(&workspace, app_id),
                    workspace_id: &fixture.workspace,
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

        let device = trusted_current_device(&fixture.workspace)
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

    #[test]
    fn the_service_signs_where_the_person_is_in_the_workspace_and_reports_where_they_are_not() {
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

        let workspace = WorkspaceId::new();
        let workspace_id = workspace.to_string();
        let app_id = "so.tilo.notes";
        let record_id = "meeting-notes";
        let profile = crate::registry::identity_for_workspace(&workspace_id)
            .expect("registry")
            .expect("profile");
        let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("device key");
        let op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"automerge document".to_vec())
                .encode()
                .expect("typed mutation"),
            document_artifact_id(app_id, &workspace_id, record_id),
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            document_attestation_id(app_id, &workspace_id, record_id),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        let unsigned = postcard::to_stdvec(&op).expect("unsigned envelope");
        let sign = |tree: &SharedTree| {
            sign_collaboration_op(
                tree,
                &bound(&workspace, app_id),
                &workspace_id,
                app_id,
                record_id,
                &unsigned,
            )
        };

        // The person this device signs as belongs to the workspace: the
        // operation is signed with the device's key, no identity picked.
        let mut committed = Workspace::in_workspace(WorkspaceSlot(1), workspace, "Workspace");
        committed.set_owner(profile.principal_id());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(committed);
        let tree = tree.shared();
        assert!(matches!(sign(&tree), Response::Signature(_)));

        // The same workspace in other people's hands: this device would sign
        // as someone its roster does not hold. That is a membership fault to
        // report, never a question to put to the app.
        let mut elsewhere = Workspace::in_workspace(WorkspaceSlot(2), workspace, "Workspace");
        let other_person = match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes([9; 32]),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            other => panic!("a person id, not {other:?}"),
        };
        elsewhere.set_owner(other_person.clone());
        tree.write().desktop.workspaces = vec![elsewhere];
        match sign(&tree) {
            Response::Error(message) => {
                assert!(message.contains("not a member"), "{message}");
            }
            other => panic!("a person the roster lacks is answered as a fault: {other:?}"),
        }

        // Two people in the workspace, the other one its owner, and this
        // device's person a member beside them: the signature is theirs,
        // again without anything chosen first.
        let mut shared = Workspace::in_workspace(WorkspaceSlot(3), workspace, "Workspace");
        shared.set_owner(other_person);
        shared
            .policy
            .members
            .insert(profile.principal_id(), collab_types::Role::Editor);
        tree.write().desktop.workspaces = vec![shared];
        assert!(matches!(sign(&tree), Response::Signature(_)));
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
        // The listener never wakes on its own (above), so only the drop can
        // end it, and dropping joins it.
        let (dropped, done) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(broker);
            let _ = dropped.send(());
        });
        tilo_test_env::recv("dropping the broker to end its listener", &done);
        assert!(!socket.exists(), "the socket is removed with the broker");
    }
}

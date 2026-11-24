//! Capability-guarded identity metadata and signing service for app sandboxes.

use std::collections::HashMap;
use std::io::{self, Write};
use std::net::Shutdown;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tilo_files::native::socket::Stream;

#[cfg(test)]
use collab_types::DocumentMutation;
use collab_types::{Ed25519PublicKey, SignedOp, document_artifact_id, document_attestation_id};
use ed25519_dalek::Signer;
use tilo_action_policy::{ActionKind, Answer};
use tilo_clock::unix_now;
use tilo_identity_client::wire::{FRAMING, HANDSHAKE_FRAMING, Request, Response};
use wire_frame::FrameError;

pub use crate::sources::{GroupKeySource, GroupKeys, PersonResourceTypes, WorkspaceMembership};
use tilo_desktop_model::{Workspace, WorkspaceId};

use tilo_lock::MutexRecover as _;

/// The resource a signature grant names. A grant admits operation kinds for
/// this device; it never makes the device key sign bytes a caller chooses.
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
/// of; the person's own workspace is theirs.
fn identity_in(
    tree: &dyn WorkspaceMembership,
    workspace: &WorkspaceId,
) -> Result<crate::store::IdentityProfile, String> {
    let profile = match crate::store::identity_for_workspace(&workspace.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Err("this device has no identity to sign in with".into()),
        Err(error) => return Err(error.to_string()),
    };
    if collab_types::person_workspace(&profile.user_public_key).to_string() == workspace.to_string()
    {
        return Ok(profile);
    }
    let actor = profile.person_id();
    match read_workspace(tree, workspace, |workspace| {
        workspace.policy.is_member(&actor)
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
    clients: Arc<Mutex<HashMap<u64, Stream>>>,
    admit: Admit,
}

impl IdentityBrokerHandle {
    /// What serves a connection another process took for this broker and handed over, as if it
    /// had connected to the broker's socket, for as long as the broker runs: the person's shell
    /// takes its apps' connections where the broker's own socket is out of their reach.
    #[must_use]
    pub fn handover(&self) -> Arc<dyn Fn(Stream) + Send + Sync> {
        let admit = self.admit.clone();
        Arc::new(move |stream| {
            admit.client(stream);
        })
    }
}

/// What a connection is served with, and the clients the broker closes when it stops.
#[derive(Clone)]
struct Admit {
    tree: Arc<dyn WorkspaceMembership>,
    groups: Arc<dyn GroupKeySource>,
    person: Arc<dyn PersonResourceTypes>,
    clients: Arc<Mutex<HashMap<u64, Stream>>>,
    next_client: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Admit {
    /// Serve `stream` on a worker of its own, which the broker's stop closes; `None` when no
    /// worker started.
    fn client(&self, stream: Stream) -> Option<thread::JoinHandle<()>> {
        if self.stop.load(Ordering::Acquire) {
            return None;
        }
        let cancellation = match stream.try_clone() {
            Ok(cancellation) => cancellation,
            Err(error) => {
                tracing::warn!(%error, "identity broker cannot own client cancellation");
                return None;
            }
        };
        let id = self.next_client.fetch_add(1, Ordering::Relaxed);
        self.clients.lock_recover().insert(id, cancellation);
        let connection = ClientConnection {
            id,
            clients: self.clients.clone(),
        };
        let (tree, groups, person) = (
            Arc::clone(&self.tree),
            Arc::clone(&self.groups),
            Arc::clone(&self.person),
        );
        match thread::Builder::new()
            .name("tilo-identity-client".into())
            .spawn(move || {
                serve(stream, &*tree, &*groups, &*person);
                drop(connection);
            }) {
            Ok(worker) => Some(worker),
            Err(error) => {
                tracing::warn!(%error, "identity broker could not spawn client worker");
                None
            }
        }
    }
}

impl Drop for IdentityBrokerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        close_clients(&self.clients);
        // The listener sleeps in `accept`; one connection wakes it to see the
        // flag. If nothing can connect it is not waited for.
        if let Some(listener) = self.listener.take()
            && tilo_files::native::socket::connect(&self.path).is_ok()
        {
            let _ = listener.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

struct ClientConnection {
    id: u64,
    clients: Arc<Mutex<HashMap<u64, Stream>>>,
}
impl Drop for ClientConnection {
    fn drop(&mut self) {
        self.clients.lock_recover().remove(&self.id);
    }
}
fn close_clients(clients: &Mutex<HashMap<u64, Stream>>) {
    for stream in clients.lock_recover().values() {
        let _ = stream.shutdown(Shutdown::Both);
    }
}
/// Start the identity broker's unix-socket listener at `path`, serving
/// clients on their own thread until the returned handle drops.
pub fn spawn(
    path: PathBuf,
    tree: Arc<dyn WorkspaceMembership>,
    groups: Arc<dyn GroupKeySource>,
    person: Arc<dyn PersonResourceTypes>,
) -> io::Result<IdentityBrokerHandle> {
    let _ = std::fs::remove_file(&path);
    let listener = tilo_files::native::socket::bind(&path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let clients = Arc::new(Mutex::new(HashMap::new()));
    let thread_clients = clients.clone();
    let admit = Admit {
        tree,
        groups,
        person,
        clients: clients.clone(),
        next_client: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        stop: Arc::clone(&stop),
    };
    let accepting = admit.clone();
    let listener_thread = thread::Builder::new()
        .name("tilo-identity-broker".into())
        .spawn(move || {
            let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();
            loop {
                match listener.accept() {
                    Ok(_) if thread_stop.load(Ordering::Acquire) => break,
                    Ok((stream, _)) => {
                        workers.retain(|worker| !worker.is_finished());
                        workers.extend(accepting.client(stream));
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
            // client's membership and group ownership before retirement.
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
        admit,
    })
}

fn serve(
    mut stream: Stream,
    tree: &dyn WorkspaceMembership,
    groups: &dyn GroupKeySource,
    person: &dyn PersonResourceTypes,
) {
    // Darwin inherits the listener's nonblocking flag on accept. This worker
    // uses blocking framed I/O; an empty receive is not the end of a session.
    // A pipe is always blocking.
    #[cfg(unix)]
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "cannot configure blocking service connection");
        return;
    }
    let Ok(negotiated) =
        read_frame::<tilo_identity_client::wire::PresentationHandshake>(&mut stream)
    else {
        return;
    };
    if !matches!(
        negotiated.presentation_version,
        0 | tilo_identity_client::wire::PRESENTATION_VERSION
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
                document_id,
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
                &document_id,
                &unsigned_op,
            ),
            Request::GroupKeys {
                workspace_id,
                app_id,
                document_id,
                known_generation,
            } => group_keys(
                tree,
                groups,
                &GroupKeyRoute {
                    bound: Bound {
                        workspace: &workspace,
                        app_id: &handshake.app_id,
                        person,
                    },
                    workspace_id: &workspace_id,
                    app_id: &app_id,
                    document_id: &document_id,
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
    let record = match tilo_identity_client::load_metadata_from_path(&profile.record_path) {
        Ok(record) => record,
        Err(error) => return Response::Error(error.to_string()),
    };
    if record.group_public_key != profile.user_public_key {
        return Response::Denied("identity record no longer matches the selected actor".into());
    }
    let (device, cert) = match crate::trusted_device::of_profile(&profile) {
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
    person: &'a dyn PersonResourceTypes,
}

impl Bound<'_> {
    /// Whether `app_id` is the bound app's.
    fn keeps_records_of(&self, app_id: &str) -> bool {
        app_id == self.app_id
    }

    /// Whether the bound app declares that it reads `app_id`'s records: another app's, which
    /// it opens and never writes.
    fn reads_documents_of(&self, app_id: &str) -> bool {
        self.person.reads_documents_of(self.app_id, app_id)
    }

    /// Whether `workspace_id` is the own workspace of the person the bound
    /// workspace's identity is, and the app keeps records of theirs there.
    fn in_person_workspace(
        &self,
        profile: &crate::store::IdentityProfile,
        workspace_id: &str,
    ) -> bool {
        collab_types::person_workspace(&profile.user_public_key).to_string() == workspace_id
            && !self.person.person_resource_types(self.app_id).is_empty()
    }

    /// Whether the app declares the person's record `document_id`.
    fn keeps_person_record(&self, document_id: &str) -> bool {
        collab_types::person_resource_type(document_id).is_some_and(|kind| {
            self.person
                .person_resource_types(self.app_id)
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
    // The launch workspace, or the person's own workspace for the records of
    // theirs the app declares; the broker admits each of those records alone.
    let foreign = |profile: &crate::store::IdentityProfile| {
        workspace_id != bound.workspace.to_string()
            && !bound.in_person_workspace(profile, workspace_id)
    };
    if workspace_id != bound.workspace.to_string()
        && bound.person.person_resource_types(bound.app_id).is_empty()
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
    match crate::trusted_device::of_profile(&profile) {
        Ok((device, _)) => match tilo_identity_client::collaboration_join_digest(workspace_id) {
            Ok(digest) => {
                match tilo_user_identity::StatementSigner::sign_digest(&device, &digest) {
                    Ok(signature) => Response::Signature(signature.to_vec()),
                    Err(error) => Response::Denied(error.to_string()),
                }
            }
            Err(error) => Response::Denied(error.to_string()),
        },
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

/// The workspace's answer for `op`, which the person `actor_key`'s device is
/// about to sign, on the document `app_id`/`document_id`: a change an agent of
/// the person's made is decided as that agent, bounded by the person, as every
/// receiver decides it.
fn decide_written(
    workspace: &Workspace,
    actor_key: &str,
    (app_id, document_id): (&str, &str),
    op: &collab_types::SignedOp,
) -> Result<collab_types::Receipt, collab_types::ReceiptRefusal> {
    let record = collab_types::WrittenDocument {
        workspace: &workspace.policy.workspace,
        app: app_id,
        document: document_id,
    };
    let access = std::cell::OnceCell::new();
    let allows = |invoker: Option<&tilo_action_policy::Acting>,
                  node: &tilo_action_policy::TreePath,
                  kind| match invoker {
        None => {
            workspace
                .policy
                .answer_at(actor_key, node, kind, unix_now())
                == Answer::Allow
        }
        Some(invoker) => {
            let access = access.get_or_init(|| {
                tilo_action_policy::MemberAccess::person(workspace.policy.clone(), actor_key)
            });
            access.invoked_by(invoker).is_some_and(|access| {
                access.answer(
                    node,
                    kind,
                    &tilo_action_policy::Context::at_secs(unix_now()),
                ) == Answer::Allow
            })
        }
    };
    collab_types::decide_op(
        op,
        &record,
        None,
        None,
        &collab_types::Surfaces::process(),
        &allows,
    )
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
    document_id: &str,
    unsigned_op: &[u8],
) -> Response {
    let bound_workspace = bound.workspace;
    let launch_workspace = workspace_id == bound_workspace.to_string();
    // The launch route, a record every app of the launch workspace opens
    // (its grants decide the change below, as for the app's own), or a
    // record of the person's this app keeps in their own workspace: never
    // another app's, and never a kind it does not declare.
    let workspace_record =
        launch_workspace && collab_types::shared_workspace_document(app_id, document_id).is_some();
    if (!bound.keeps_records_of(app_id) && !workspace_record)
        || (!launch_workspace && !bound.keeps_person_record(document_id))
    {
        return Response::Denied(
            "collaboration operation requested for a different launch route".into(),
        );
    }
    let profile = match identity_in(tree, bound_workspace) {
        Ok(profile) => profile,
        Err(reason) => return Response::Error(reason),
    };
    if !launch_workspace && !bound.in_person_workspace(&profile, workspace_id) {
        return Response::Denied(
            "collaboration operation requested for a different launch route".into(),
        );
    }
    let (device, _) = match crate::trusted_device::of_profile(&profile) {
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
    let expected_artifact = document_artifact_id(app_id, workspace_id, document_id);
    let expected_attestation = document_attestation_id(app_id, workspace_id, document_id);
    let now = std::time::SystemTime::now();
    let skew = Duration::from_secs(300);
    let earliest = now
        .checked_sub(skew)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let latest = now.checked_add(skew).unwrap_or(now);
    if op.author != author
        || op.artifact != expected_artifact
        || op.attestation_id != expected_attestation
        || collab_types::validate_document_payload_route(app_id, workspace_id, document_id, &op)
            .is_err()
        || op.timestamp < earliest
        || op.timestamp > latest
    {
        return Response::Denied(
            "collaboration operation fields do not match its certified document route".into(),
        );
    }
    // The person's own workspace is theirs, and this is
    // their device editing a record of theirs.
    if !launch_workspace {
        return match op.signing_bytes() {
            Ok(bytes) => Response::Signature(device.sign(&bytes).to_bytes().to_vec()),
            Err(error) => Response::Error(error.to_string()),
        };
    }
    let actor_key = hex::encode(profile.user_public_key);
    let decided = read_workspace(tree, bound_workspace, |workspace| {
        workspace.policy.role_of(&actor_key)?;
        Some(decide_written(
            workspace,
            &actor_key,
            (app_id, document_id),
            &op,
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

/// The route a [`Request::GroupKeys`] names, with the launch binding it
/// must match.
struct GroupKeyRoute<'a> {
    bound: Bound<'a>,
    workspace_id: &'a str,
    app_id: &'a str,
    document_id: &'a str,
}

/// Hand an app the content keys of one route of its own launch, only while
/// this device is trusted and its person may read the workspace. The keys
/// open the route's content and nothing else: no MLS secret leaves the
/// key service, and another app's or workspace's route is refused.
fn group_keys(
    tree: &dyn WorkspaceMembership,
    groups: &dyn GroupKeySource,
    route: &GroupKeyRoute<'_>,
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
        && !bound.reads_documents_of(route.app_id)
        && collab_types::shared_workspace_document(route.app_id, route.document_id).is_none())
        || (!launch_workspace && !bound.keeps_person_record(route.document_id))
    {
        return Response::Denied("group keys requested for a different launch route".into());
    }
    let launch = bound.workspace.to_string();
    let profile = match identity_in(tree, bound.workspace) {
        Ok(profile) => profile,
        Err(reason) => return Response::Error(reason),
    };
    if !launch_workspace && !bound.in_person_workspace(&profile, route.workspace_id) {
        return Response::Denied("group keys requested for a different launch route".into());
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
    let actor_key = hex::encode(profile.user_public_key);
    // The person reads their own workspace, the launch workspace of an app
    // opened there included; a workspace, as its policy says.
    let own_workspace =
        collab_types::person_workspace(&profile.user_public_key).to_string() == route.workspace_id;
    let reads = own_workspace
        || read_workspace(tree, bound.workspace, |workspace| {
            let node = collab_types::validator::document_node(
                &workspace.policy.workspace,
                route.app_id,
                route.document_id,
            );
            workspace
                .policy
                .answer_at(&actor_key, &node, ActionKind::Read, unix_now())
                == Answer::Allow
        })
        .unwrap_or(false);
    if !reads {
        return Response::Denied("identity may not read this workspace".into());
    }
    // Only the workspace's canonical form.
    let Some(workspace) = WorkspaceId::parse(route.workspace_id)
        .filter(|workspace| workspace.to_string() == route.workspace_id)
    else {
        return Response::Denied("group keys requested for an invalid workspace".into());
    };
    let keys = if own_workspace {
        groups.access(workspace, known_generation)
    } else {
        groups.document_access(workspace, route.app_id, route.document_id, known_generation)
    };
    match keys {
        None => Response::GroupKeys {
            access: tilo_identity_client::wire::KeyAccess::Unavailable,
            generation: 0,
            keys: Vec::new(),
        },
        Some(Err(error)) => Response::Error(error),
        Some(Ok((access, generation, keys))) => Response::GroupKeys {
            access,
            generation,
            keys,
        },
    }
}

fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut Stream) -> io::Result<T> {
    let bytes = HANDSHAKE_FRAMING.read(stream)?.ok_or(FrameError::Closed)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
fn write_frame<T: serde::Serialize>(stream: &mut Stream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    HANDSHAKE_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

fn read_action_frame<T: serde::de::DeserializeOwned>(stream: &mut Stream) -> io::Result<T> {
    Ok(FRAMING.read_value(stream)?.ok_or(FrameError::Closed)?)
}

fn write_action_frame<T: serde::Serialize>(stream: &mut Stream, value: &T) -> io::Result<()> {
    FRAMING.write_value(stream, value)?;
    stream.flush()
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use tilo_identity_client::wire::Handshake;

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
    /// Notes keeps the person's record of what they watched.
    struct NotesKeepsWatched;

    impl PersonResourceTypes for NotesKeepsWatched {
        fn person_resource_types(&self, app_id: &str) -> Vec<String> {
            if app_id == "so.tilo.notes" {
                vec!["notes.watched".into()]
            } else {
                Vec::new()
            }
        }
    }
    use tilo_action_policy::{Grant, Grantee, KindSet, Role};

    /// A signature grant naming this device's document-save action.
    fn signing_grant(workspace: &Workspace) -> Grant {
        Grant::new(
            "notes-sign-save",
            Grantee::person("owner"),
            workspace
                .policy
                .node()
                .app("so.tilo.notes")
                .resource(DEVICE_SIGNATURE_RESOURCE)
                .action("document.save"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person(
                workspace
                    .policy
                    .owner()
                    .map_or_else(String::new, ToString::to_string),
            ),
        )
    }

    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use std::io::Read as _;
    use std::os::fd::IntoRawFd;
    use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
    use tilo_desktop_model::{Layout, WorkspaceId};

    /// A host's committed workspaces, as the tests mutate them between requests.
    #[derive(Default)]
    struct WorkspaceTree {
        desktop: Layout,
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
        paths: &tilo_device_identity::IdentityPaths,
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
        app.write_all(&[3, 0, 0, 0, 1, 1, b'w']).expect("request");
        assert!(matches!(
            read_action_frame::<Request>(&mut service).expect("request"),
            Request::CollaborationJoin { workspace_id } if workspace_id == "w"
        ));
    }

    #[test]
    fn identity_service_wire_is_pinned() {
        assert_identity_service_wire_bytes_are_pinned();
    }

    tilo_test_env::scenario! {
        fn what_the_service_signs;

        phase collaboration_signing_binds_route_payload_and_live_action_role;
        {
            let temp = tempfile::tempdir().expect("temp");
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = crate::store::account_identity_dir(&xdg.join("tilo/identity"));
            let paths = tilo_device_identity::IdentityPaths::under(&system_root);
            ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

            let workspace = WorkspaceId::new();
            let workspace_id = workspace.to_string();
            let app_id = "so.tilo.notes";
            let document_id = "meeting-notes";
            let profile = crate::store::identity_for_workspace(&workspace_id)
                .expect("registry")
                .expect("profile");
            let device = tilo_device_identity::persist::read_device_key(&paths.device_privkey)
                .expect("device key");
            let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());

            let mut committed = Workspace::new(workspace, "Workspace");
            committed.policy.set_owner(profile.person_id());
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(committed);
            let tree = tree.shared();

            let now = std::time::SystemTime::now();
            let mut op = SignedOp::new_unsigned(
                DocumentMutation::Snapshot(b"automerge document".to_vec())
                    .encode()
                    .expect("typed mutation"),
                document_artifact_id(app_id, &workspace_id, document_id),
                author,
                document_attestation_id(app_id, &workspace_id, document_id),
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
                document_id,
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
                document_artifact_id(app_id, &workspace_id, document_id),
                author,
                document_attestation_id(app_id, &workspace_id, document_id),
                collab_types::OpKind::Change,
                now,
                2,
                collab_types::EncryptionMetadata::mls_application(3),
            );
            let encrypted_unsigned = postcard::to_stdvec(&encrypted).expect("encrypted envelope");
            assert!(matches!(
                sign_collaboration_op(
                    &tree,
                    &bound(&workspace, app_id),
                    &workspace_id,
                    app_id,
                    document_id,
                    &encrypted_unsigned,
                ),
                Response::Signature(_)
            ));
            assert!(matches!(
                sign_collaboration_op(
                    &tree,
                    &bound(&workspace, app_id),
                    &workspace_id,
                    "so.tilo.calendar",
                    document_id,
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
                    document_id,
                    &malformed,
                ),
                Response::Denied(_)
            ));

            let mut tree_guard = tree.write();
            let committed = &mut tree_guard.desktop.workspaces[0];
            committed.policy.set_owner("someone-else");
            committed.policy.add_member(profile.person_id(), Role::Commenter);
            drop(tree_guard);
            // Without the app's surface a change is decided as an edit of the
            // record, which a commenter is not granted.
            assert!(matches!(
                sign_collaboration_op(
                    &tree,
                    &bound(&workspace, app_id),
                    &workspace_id,
                    app_id,
                    document_id,
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
            let (_, kept) = collab_types::surface::SurfaceFile::kept(surface.as_bytes())
                .expect("the fixture is a surface");
            let hash = *blake3::hash(kept.as_bytes()).as_bytes();
            let hex = hex::encode(hash);
            collab_types::Surfaces::set_process(collab_types::Surfaces::from_source(
                std::sync::Arc::new(collab_types::surface::ContentSurfaces::new(
                    move |_, wanted| (wanted == hex).then(|| kept.clone().into_bytes()),
                )),
            ));
            let change = |what: tilo_actions::Did, seq: u64| {
                use automerge::transaction::Transactable as _;
                let mut doc = automerge::AutoCommit::new();
                doc.put(automerge::ROOT, "written", seq).expect("write");
                let context = tilo_actions::ActionContext {
                    what: Some(what),
                    ..tilo_actions::ActionContext::for_resource(document_id, "notes.page", "Page")
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
                    document_id,
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
                sign_collaboration_op(&tree, &bound(&workspace, app_id), &workspace_id, app_id, document_id, &edit),
                Response::Denied(reason) if reason.contains("denies this document action")
            ));
            let mut tree_guard = tree.write();
            let committed = &mut tree_guard.desktop.workspaces[0];
            committed.policy.add_member(profile.person_id(), Role::Viewer);
            drop(tree_guard);
            assert!(matches!(
                sign_collaboration_op(
                    &tree,
                    &bound(&workspace, app_id),
                    &workspace_id,
                    app_id,
                    document_id,
                    &unsigned,
                ),
                Response::Denied(reason) if reason.contains("denies this document action")
            ));
        }

        phase any_app_of_the_workspace_signs_its_workspace_records_and_no_other_of_their_owner;
        // Any app of a workspace writes the records every app of it opens (the
        // workspace's emoji, Chat's), decided by the workspace's grants on them,
        // and no other record of their owner's.
        {
            let temp = tempfile::tempdir().expect("temp");
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = crate::store::account_identity_dir(&xdg.join("tilo/identity"));
            let paths = tilo_device_identity::IdentityPaths::under(&system_root);
            ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

            let workspace = WorkspaceId::new();
            let workspace_id = workspace.to_string();
            let launched = "so.tilo.notes";
            let owner = collab_types::WORKSPACE_DOCUMENTS[0];
            let emoji = collab_types::workspace_document_id(owner.kind);
            let profile = crate::store::identity_for_workspace(&workspace_id)
                .expect("registry")
                .expect("profile");
            let device = tilo_device_identity::persist::read_device_key(&paths.device_privkey)
                .expect("device key");
            let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
            let mut committed = Workspace::new(workspace, "Workspace");
            committed.policy.set_owner(profile.person_id());
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
            committed.policy.set_owner("someone-else");
            committed.policy.add_member(profile.person_id(), Role::Viewer);
            drop(tree_guard);
            assert!(matches!(
                sign(&emoji),
                Response::Denied(reason) if reason.contains("denies this document action")
            ));
        }

        phase the_service_signs_where_the_person_is_in_the_workspace_and_reports_where_they_are_not;
        {
            let temp = tempfile::tempdir().expect("temp");
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = crate::store::account_identity_dir(&xdg.join("tilo/identity"));
            let paths = tilo_device_identity::IdentityPaths::under(&system_root);
            ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

            let workspace = WorkspaceId::new();
            let workspace_id = workspace.to_string();
            let app_id = "so.tilo.notes";
            let document_id = "meeting-notes";
            let profile = crate::store::identity_for_workspace(&workspace_id)
                .expect("registry")
                .expect("profile");
            let device = tilo_device_identity::persist::read_device_key(&paths.device_privkey)
                .expect("device key");
            let op = SignedOp::new_unsigned(
                DocumentMutation::Snapshot(b"automerge document".to_vec())
                    .encode()
                    .expect("typed mutation"),
                document_artifact_id(app_id, &workspace_id, document_id),
                Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
                document_attestation_id(app_id, &workspace_id, document_id),
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
                    document_id,
                    &unsigned,
                )
            };

            // The person this device signs as belongs to the workspace: the
            // operation is signed with the device's key, no identity picked.
            let mut committed = Workspace::new(workspace, "Workspace");
            committed.policy.set_owner(profile.person_id());
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(committed);
            let tree = tree.shared();
            assert!(matches!(sign(&tree), Response::Signature(_)));

            // The same workspace in other people's hands: this device would sign
            // as someone its roster does not hold. That is a membership fault to
            // report, never a question to put to the app.
            let mut elsewhere = Workspace::new(workspace, "Workspace");
            let other_person = hex::encode([9; 32]);
            elsewhere.policy.set_owner(other_person.clone());
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
            let mut shared = Workspace::new(workspace, "Workspace");
            shared.policy.set_owner(other_person);
            shared
                .policy
                .members
                .insert(profile.person_id(), collab_types::Role::Editor);
            tree.write().desktop.workspaces = vec![shared];
            assert!(matches!(sign(&tree), Response::Signature(_)));
        }
    }

    tilo_test_env::scenario! {
        fn broker_clients;

        phase negotiated_photo_uses_actual_selected_record_and_refuses_wrong_root_and_invalid_pixels;
        {
            let temp = tempfile::tempdir().unwrap();
            let xdg = temp.path().join("config");
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = crate::store::account_identity_dir(&xdg.join("tilo/identity"));
            let paths = tilo_device_identity::IdentityPaths::under(system_root);
            ensure_collaboration_identity(&identity_dir, &paths).unwrap();
            let photo = tilo_identity_client::ProfilePhoto::new(1, 1, vec![25, 90, 210, 255]).unwrap();
            crate::store::update_identity_photo(&identity_dir, "primary", Some(photo.clone()))
                .unwrap();
            let workspace = WorkspaceId::new();
            let profile = crate::store::identity_for_workspace(&workspace.to_string())
                .unwrap()
                .unwrap();
            let original = std::fs::read(&profile.record_path).unwrap();
            let mut committed = Workspace::new(workspace, "Workspace");
            committed.policy.set_owner(profile.person_id());
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(committed);
            let socket = temp.path().join("identity.sock");
            let broker = spawn(
                socket.clone(),
                Arc::new(tree.shared()),
                Arc::new(()),
                Arc::new(()),
            )
            .unwrap();
            let handover = broker.handover();
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
                    &tilo_identity_client::wire::PresentationHandshake {
                        handshake: Handshake {
                            workspace_id: workspace.to_string(),
                            app_id: "so.tilo.notes".into(),
                            surface: None,
                        },
                        presentation_version: version,
                    },
                )
                .unwrap();
                tilo_identity_client::BrokerSigningIdentity::from_connected_io(client)
            };
            let client = connect(tilo_identity_client::wire::PRESENTATION_VERSION).unwrap();
            assert_eq!(client.metadata().profile_photo(), Some(&photo));
            assert_eq!(
                client.metadata().group_public_key(),
                profile.user_public_key
            );
            // A connection the person's shell took for the broker and handed over is served as
            // one made on its socket: the app it came from signs in the same.
            {
                let (mut handed, served) = UnixStream::pair().unwrap();
                handover(served);
                handed
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                write_frame(
                    &mut handed,
                    &tilo_identity_client::wire::PresentationHandshake {
                        handshake: Handshake {
                            workspace_id: workspace.to_string(),
                            app_id: "so.tilo.notes".into(),
                            surface: None,
                        },
                        presentation_version: tilo_identity_client::wire::PRESENTATION_VERSION,
                    },
                )
                .unwrap();
                let handed = tilo_identity_client::BrokerSigningIdentity::from_connected_io(handed)
                    .unwrap();
                assert_eq!(handed.metadata().profile_photo(), Some(&photo));
                assert_eq!(
                    handed.metadata().group_public_key(),
                    profile.user_public_key
                );
            }
            assert!(
                client
                    .sign_collaboration_join(&WorkspaceId::new().to_string())
                    .is_err(),
                "a photo grants no signature beyond the bound workspace"
            );
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
            assert!(connect(tilo_identity_client::wire::PRESENTATION_VERSION).is_err());
            for invalid in [
                serde_json::json!({"width":0,"height":1,"rgba":[0,0,0,255]}),
                serde_json::json!({"width":128,"height":128,"rgba":vec![0; tilo_identity_client::MAX_PHOTO_BYTES + 1]}),
            ] {
                let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
                record["profile_photo"] = invalid;
                tilo_files::native::atomic::write_with_mode(
                    &profile.record_path,
                    &serde_json::to_vec(&record).unwrap(),
                    0o600,
                )
                .unwrap();
                assert!(connect(tilo_identity_client::wire::PRESENTATION_VERSION).is_err());
            }
            tilo_files::native::atomic::write_with_mode(&profile.record_path, &original, 0o600)
                .unwrap();
            assert_eq!(
                connect(tilo_identity_client::wire::PRESENTATION_VERSION)
                    .unwrap()
                    .metadata()
                    .profile_photo(),
                Some(&photo)
            );
        }

        phase runtime_client_gets_metadata_but_never_generic_signing;
        {
            let temp = tempfile::tempdir().expect("temp");
            let xdg = temp.path().join("config");
            let service_var = "TILO_SVC_IDENTITY_FD";
            let system_root = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = crate::store::account_identity_dir(&xdg.join("tilo/identity"));
            let paths = tilo_device_identity::IdentityPaths::under(system_root);
            ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

            let workspace = WorkspaceId::new();
            let workspace_id = workspace.to_string();
            let profile = crate::store::identity_for_workspace(&workspace_id)
                .expect("registry")
                .expect("profile");
            let mut committed = Workspace::new(workspace, "Workspace");
            committed.policy.set_owner(profile.person_id());
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

            let identity = tilo_identity_client::load().expect("broker metadata");
            assert!(
                !identity
                    .metadata()
                    .group_public_key()
                    .iter()
                    .all(|byte| *byte == 0)
            );
            let cert: tilo_user_identity::DeviceCert = serde_json::from_slice(
                identity
                    .metadata()
                    .device_certificate()
                    .expect("broker supplies identity-to-device certificate"),
            )
            .expect("certificate JSON");
            tilo_user_identity::verify_device_cert(&cert).expect("root-signed certificate");
            assert_eq!(
                cert.user_public_key,
                identity.metadata().group_public_key()
            );
            assert_eq!(
                cert.device_public_key,
                identity.metadata().signing_public_key()
            );
            // Leave the accepted worker waiting between frames; Darwin must not treat
            // a temporarily empty nonblocking receive as the end of this session.
            std::thread::sleep(Duration::from_millis(50));
            // Signature and Outside grants admit operation kinds for the typed
            // document operation below; no request shape turns either into a
            // signature over caller-chosen bytes.
            let grant = signing_grant(&tree.read().desktop.workspaces[0]);
            tree.write().desktop.workspaces[0].policy.grant(grant);
            let node = tree.read().desktop.workspaces[0].policy.node();
            tree.write().desktop.workspaces[0].policy.grant(Grant::new(
                "notes-outside",
                Grantee::person("owner"),
                node,
                KindSet::ALL,
                Answer::Allow,
                Grantee::person(profile.person_id()),
            ));
            let join_proof = identity
                .sign_collaboration_join(&workspace_id)
                .expect("the launcher-bound workspace may request a join proof");
            VerifyingKey::from_bytes(&identity.metadata().signing_public_key())
                .expect("device key")
                .verify(
                    &tilo_identity_client::collaboration_join_digest(&workspace_id).unwrap(),
                    &Signature::from_bytes(&join_proof),
                )
                .expect("workspace-scoped device proof");
            assert!(
                identity
                    .sign_collaboration_join(&WorkspaceId::new().to_string())
                    .is_err(),
                "the identity broker must not sign a join for another workspace"
            );
            let document_id = "record-1";
            let now = std::time::SystemTime::now();
            // Real changes: one that does not read is refused, since who made
            // it cannot be told.
            let changes = {
                use automerge::transaction::Transactable as _;
                let mut doc = automerge::AutoCommit::new();
                doc.put(automerge::ROOT, "body", "hello").expect("write");
                doc.save_after(&[])
            };
            let mut op = SignedOp::new_unsigned(
                DocumentMutation::Incremental(changes)
                    .encode()
                    .expect("typed mutation"),
                document_artifact_id("so.tilo.notes", &workspace_id, document_id),
                Ed25519PublicKey::from_bytes(identity.metadata().signing_public_key()),
                document_attestation_id("so.tilo.notes", &workspace_id, document_id),
                collab_types::OpKind::Change,
                now,
                1,
            );
            let op_signature = identity
                .sign_collaboration_op(
                    &workspace_id,
                    "so.tilo.notes",
                    document_id,
                    &postcard::to_stdvec(&op).expect("unsigned envelope"),
                )
                .expect("owner may sign a typed document operation");
            op.signature = collab_types::Signature::from_bytes(op_signature);
            collab_types::verify_op(&op).expect("identity service signature verifies");

            let own_workspace =
                collab_types::person_workspace(&identity.metadata().group_public_key()).to_string();
            let own_proof = identity
                .sign_collaboration_join(&own_workspace)
                .expect("an app keeping a record of the person's joins their store");
            VerifyingKey::from_bytes(&identity.metadata().signing_public_key())
                .expect("device key")
                .verify(
                    &tilo_identity_client::collaboration_join_digest(&own_workspace).unwrap(),
                    &Signature::from_bytes(&own_proof),
                )
                .expect("a proof for the person's store");
            let person_op = |record: &str| {
                SignedOp::new_unsigned(
                    DocumentMutation::Incremental(b"automerge changes".to_vec())
                        .encode()
                        .expect("typed mutation"),
                    document_artifact_id("so.tilo.notes", &own_workspace, record),
                    Ed25519PublicKey::from_bytes(identity.metadata().signing_public_key()),
                    document_attestation_id("so.tilo.notes", &own_workspace, record),
                    collab_types::OpKind::Change,
                    now,
                    1,
                )
            };
            let watched = collab_types::person_document_id("notes.watched");
            let mut kept = person_op(&watched);
            kept.signature = collab_types::Signature::from_bytes(
                identity
                    .sign_collaboration_op(
                        &own_workspace,
                        "so.tilo.notes",
                        &watched,
                        &postcard::to_stdvec(&kept).expect("unsigned envelope"),
                    )
                    .expect("its own record of the person's is signed"),
            );
            collab_types::verify_op(&kept).expect("the personal-cluster change verifies");
            let undeclared = collab_types::person_document_id("notes.bookmarks");
            assert!(
                identity
                    .sign_collaboration_op(
                        &own_workspace,
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
                        &own_workspace,
                        "so.tilo.notes",
                        "record-1",
                        &postcard::to_stdvec(&person_op("record-1")).expect("envelope"),
                    )
                    .is_err(),
                "nothing but the person's records is signed in their store"
            );
            assert!(
                identity
                    .group_keys(&own_workspace, "so.tilo.notes", &watched, 0)
                    .is_ok(),
                "the keys of a record of the person's the app declares are asked for"
            );
            assert!(
                identity
                    .group_keys(&own_workspace, "so.tilo.notes", &undeclared, 0)
                    .is_err(),
                "not those of a kind it does not declare"
            );
            drop(identity);
        }

        phase shutdown_reaps_clients_waiting_for_requests_and_partial_handshakes;
        {
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
    }

    /// A key service holding keys for every workspace.
    struct ReadyGroups;
    impl GroupKeySource for ReadyGroups {
        fn access(
            &self,
            _workspace: WorkspaceId,
            _known_generation: u64,
        ) -> Option<Result<GroupKeys, String>> {
            Some(Ok((
                tilo_identity_client::wire::KeyAccess::Ready,
                1,
                vec![1, 2, 3],
            )))
        }
    }

    tilo_test_env::scenario! {
        fn group_keys_for_an_app;

        phase an_app_opened_for_the_persons_records_gets_the_keys_of_them;
        // An app opened in the person's own store (Settings) is answered the
        // keys of the person's records it declares there: that store is no
        // workspace with a policy, and the person reads it.
        {
            let fixture = crate::join_proof::tests::Fixture::new();
            let own_cluster = WorkspaceId::parse(
                &collab_types::person_workspace(&fixture.user_public_key).to_string(),
            )
            .expect("the person's store");
            let own = own_cluster.to_string();
            let app_id = "so.tilo.notes";
            let watched = collab_types::person_document_id("notes.watched");
            let answer = group_keys(
                &WorkspaceTree::default().shared(),
                &ReadyGroups,
                &GroupKeyRoute {
                    bound: Bound {
                        workspace: &own_cluster,
                        app_id,
                        person: &NotesKeepsWatched,
                    },
                    workspace_id: &own,
                    app_id,
                    document_id: &watched,
                },
                0,
            );
            assert!(
                matches!(answer, Response::GroupKeys { .. }),
                "the person reads their own cluster: {answer:?}"
            );
        }

        phase an_app_gets_the_keys_of_the_records_it_declares_it_reads;
        // An app is handed the keys of another app's record it declares it reads, as the broker
        // admits its read-only join, and of no other app's.
        {
            let fixture = crate::join_proof::tests::Fixture::new();
            let workspace = WorkspaceId::parse(
                &collab_types::person_workspace(&fixture.user_public_key).to_string(),
            )
            .expect("the person's store");
            let launch = workspace.to_string();
            let keys_of = |owner: &str| {
                group_keys(
                    &WorkspaceTree::default().shared(),
                    &ReadyGroups,
                    &GroupKeyRoute {
                        bound: Bound {
                            workspace: &workspace,
                            app_id: "so.tilo.lang.today",
                            person: &TodayReadsTasks,
                        },
                        workspace_id: &launch,
                        app_id: owner,
                        document_id: "",
                    },
                    0,
                )
            };
            let tasks = keys_of("so.tilo.lang.tasks");
            assert!(
                matches!(tasks, Response::GroupKeys { .. }),
                "Today reads Tasks' record: {tasks:?}"
            );
            let chat = keys_of("so.tilo.chat");
            assert!(
                matches!(chat, Response::Denied(_)),
                "Today declares no read of Chat: {chat:?}"
            );
        }

        phase group_keys_trust_check_reads_the_devices_on_every_answer;
        // Answering an app's key request checks this device's trust from the
        // person's devices as the host that holds them answers, and refuses keys
        // on the very next request after a revocation.
        {
            let fixture = crate::join_proof::tests::Fixture::new();
            let workspace = WorkspaceId::parse(&fixture.workspace).expect("workspace");
            let app_id = "so.tilo.notes";
            let profile = crate::store::identity_for_workspace(&fixture.workspace)
                .expect("registry")
                .expect("profile");
            let mut committed = Workspace::new(workspace, "Workspace");
            committed.policy.set_owner(profile.person_id());
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(committed);
            let tree = tree.shared();
            let ask = || {
                group_keys(
                    &tree,
                    &ReadyGroups,
                    &GroupKeyRoute {
                        bound: bound(&workspace, app_id),
                        workspace_id: &fixture.workspace,
                        app_id,
                        document_id: "note",
                    },
                    0,
                )
            };
            let derived = crate::join_proof::tests::mints(|| {
                for _ in 0..3 {
                    assert!(matches!(ask(), Response::GroupKeys { .. }));
                }
            });
            assert_eq!(
                derived, 1,
                "the identity files are read for the first answer only"
            );

            let device = crate::trusted_device::current(&fixture.workspace)
                .expect("trusted device")
                .0
                .verifying_key()
                .to_bytes();
            crate::sources::test_trust::revoke(
                crate::store::identity_for_workspace(&fixture.workspace)
                    .expect("registry")
                    .expect("profile")
                    .user_public_key,
                device,
            );
            assert!(
                matches!(ask(), Response::Denied(_)),
                "a revoked device gets no keys on the next request"
            );
            fixture.settle();
            assert!(matches!(ask(), Response::Denied(_)), "nor once it settles");
        }
    }

    /// Today declares that it reads Tasks' records.
    struct TodayReadsTasks;

    impl PersonResourceTypes for TodayReadsTasks {
        fn person_resource_types(&self, _app_id: &str) -> Vec<String> {
            Vec::new()
        }

        fn reads_documents_of(&self, reader: &str, owner: &str) -> bool {
            reader == "so.tilo.lang.today" && owner == "so.tilo.lang.tasks"
        }
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

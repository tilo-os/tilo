//! Capability-gated identity metadata and signing service for app sandboxes.

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use collab_types::caps::{
    Actor, CapDecision, Principal, ResourceRef, Verb, decide_capability, verb_for_opclass,
};
use collab_types::{
    ArtifactKind, DocumentMutation, Ed25519PublicKey, SignedOp, document_artifact_id,
    document_attestation_id, role_permits,
};
use ed25519_dalek::Signer;
use runtime_identity::wire::{Handshake, Request, Response};

use crate::shell::state::{RoomId, WorkspaceTreeHandle, now_unix};

pub struct IdentityBrokerHandle {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    listener: Option<thread::JoinHandle<()>>,
}

impl Drop for IdentityBrokerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Start the identity broker's unix-socket listener at `path`, serving
/// clients on their own thread until the returned handle drops.
///
/// # Errors
///
/// Returns an error if binding the unix socket at `path` or putting it into
/// non-blocking mode fails, or if the listener thread cannot be spawned.
pub fn spawn(path: PathBuf, tree: WorkspaceTreeHandle) -> io::Result<IdentityBrokerHandle> {
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let listener_thread = thread::Builder::new()
        .name("qos-identity-broker".into())
        .spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let tree = tree.clone();
                        if let Err(error) = thread::Builder::new()
                            .name("qos-identity-client".into())
                            .spawn(move || serve(stream, &tree))
                        {
                            tracing::warn!(%error, "identity broker could not spawn client worker");
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "identity broker listener stopped");
                        break;
                    }
                }
            }
        })?;
    Ok(IdentityBrokerHandle {
        path,
        stop,
        listener: Some(listener_thread),
    })
}

fn serve(mut stream: UnixStream, tree: &WorkspaceTreeHandle) {
    let Ok(handshake) = read_frame::<Handshake>(&mut stream) else {
        return;
    };
    let Some(room) = RoomId::parse(&handshake.workspace_id) else {
        let _ = write_action_frame(
            &mut stream,
            &Response::Denied("invalid workspace identity".into()),
        );
        return;
    };
    while let Ok(request) = read_action_frame::<Request>(&mut stream) {
        let response = match request {
            Request::Metadata => metadata(&room),
            Request::Sign { action, message } => {
                sign(tree, &room, &handshake.app_id, &action, &message)
            }
            Request::CollaborationJoin { workspace_id } => {
                sign_collaboration_join(&room, &workspace_id)
            }
            Request::CollaborationOp {
                workspace_id,
                app_id,
                record_id,
                unsigned_op,
            } => sign_collaboration_op(
                tree,
                &room,
                &handshake.app_id,
                &workspace_id,
                &app_id,
                &record_id,
                &unsigned_op,
            ),
        };
        if write_action_frame(&mut stream, &response).is_err() {
            return;
        }
    }
}

fn metadata(room: &RoomId) -> Response {
    let profile = match crate::identity_registry::selected_identity_profile(&room.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    let record = match runtime_identity::load_from_path(profile.record_path.clone()) {
        Ok(record) => record,
        Err(error) => return Response::Error(error.to_string()),
    };
    let (device, cert) = match require_trusted_current_device(&profile) {
        Ok(material) => material,
        Err(error) => return Response::Denied(error.to_string()),
    };
    let device_certificate = match serde_json::to_vec(&cert) {
        Ok(bytes) => bytes,
        Err(error) => return Response::Error(error.to_string()),
    };
    Response::Metadata {
        display_name: record.display_name().to_string(),
        avatar_color: record.avatar_color(),
        group_public_key: record.group_public_key(),
        signing_public_key: device.verifying_key().to_bytes(),
        device_certificate: Some(device_certificate),
        threshold: record.threshold(),
        total_shares: record.total_shares(),
        share_index: record.share_index(),
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the read guard protects `workspace`, which the capability decision borrows \
              right up to the block's tail expression; there's no earlier point to drop it \
              without cloning the workspace's capability list out first"
)]
fn sign(
    tree: &WorkspaceTreeHandle,
    room: &RoomId,
    app_id: &str,
    action: &str,
    message: &[u8],
) -> Response {
    let allowed = {
        let tree = tree.read();
        let Some(workspace) = tree
            .workspaces
            .iter()
            .find(|workspace| &workspace.room == room)
        else {
            return Response::Denied("workspace is not available".into());
        };
        let actor = Actor::app(app_id);
        let resource = ResourceRef::Action {
            app: app_id.to_string(),
            action: action.to_string(),
        };
        decide_capability(
            &actor,
            &resource,
            Verb::Write,
            &workspace.capabilities,
            Some(now_unix()),
        ) == CapDecision::Allow
    };
    if !allowed {
        return Response::Denied(format!(
            "{app_id} is not permitted to sign action {action} in this workspace"
        ));
    }
    let profile = match crate::identity_registry::selected_identity_profile(&room.to_string()) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
    match require_trusted_current_device(&profile) {
        Ok((device, _)) => Response::Signature(device.sign(message).to_bytes().to_vec()),
        Err(error) => Response::Denied(error.to_string()),
    }
}

fn sign_collaboration_join(bound_room: &RoomId, workspace_id: &str) -> Response {
    if workspace_id != bound_room.to_string() {
        return Response::Denied("collaboration join requested for a different workspace".into());
    }
    let profile = match crate::identity_registry::selected_identity_profile(workspace_id) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
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

/// Validate the entire unsigned document envelope before lending it the
/// certified device signature. The app cannot turn this into a generic
/// signing oracle: workspace/app are fixed by the launch handshake, artifact
/// and attestation ids are route-derived, and only typed `Splice` document
/// payloads pass.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is an independently authenticated boundary value: live tree, bound workspace/app, requested route, and opaque envelope"
)]
fn sign_collaboration_op(
    tree: &WorkspaceTreeHandle,
    bound_room: &RoomId,
    bound_app_id: &str,
    workspace_id: &str,
    app_id: &str,
    record_id: &str,
    unsigned_op: &[u8],
) -> Response {
    if workspace_id != bound_room.to_string() || app_id != bound_app_id {
        return Response::Denied(
            "collaboration operation requested for a different launch route".into(),
        );
    }
    let profile = match crate::identity_registry::selected_identity_profile(workspace_id) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Response::Denied("no identity selected for this workspace".into()),
        Err(error) => return Response::Error(error.to_string()),
    };
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
        || op.encryption.is_some()
        || DocumentMutation::decode(&op.payload).is_err()
        || op.timestamp < earliest
        || op.timestamp > latest
    {
        return Response::Denied(
            "collaboration operation fields do not match its certified document route".into(),
        );
    }
    let principal_id =
        Principal::person_from_key(Ed25519PublicKey::from_bytes(profile.user_public_key))
            .id()
            .to_string();
    let allowed = {
        let tree = tree.read();
        let Some(workspace) = tree
            .workspaces
            .iter()
            .find(|workspace| &workspace.room == bound_room)
        else {
            return Response::Denied("workspace is not available".into());
        };
        let Some(role) = workspace.role_of(&principal_id) else {
            return Response::Denied("identity is not a workspace member".into());
        };
        let actor = Actor::person(Some(role), &principal_id)
            .with_principal(Principal::App(app_id.to_string()))
            .in_room(workspace_id.to_string());
        let resource = ResourceRef::Artifact {
            app: Some(app_id.to_string()),
            kind: ArtifactKind::Note,
            instance: expected_artifact,
        };
        let required = verb_for_opclass(op.op_class);
        let decision = decide_capability(
            &actor,
            &resource,
            required,
            &workspace.capabilities,
            Some(now_unix()),
        );
        let role_allows = role_permits(role, ArtifactKind::Note, op.op_class);
        drop(tree);
        decision == CapDecision::Allow && role_allows
    };
    if !allowed {
        return Response::Denied("workspace role or capability denies this document action".into());
    }
    let signing_bytes = match op.signing_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return Response::Error(error.to_string()),
    };
    Response::Signature(device.sign(&signing_bytes).to_bytes().to_vec())
}

/// Root-key operations are available only while this machine is covered by
/// a valid, non-revoked `identity -> device` certificate. Apps never receive
/// either key; this check makes revocation effective at the local broker too.
fn require_trusted_current_device(
    profile: &crate::identity_registry::IdentityProfile,
) -> io::Result<(ed25519_dalek::SigningKey, qos_user_identity::DeviceCert)> {
    let paths = crate::firstboot::collaboration_identity_paths();
    let device = qos_user_identity::persist::read_device_key(&paths.device_privkey)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let device_key = device.verifying_key().to_bytes();
    let cert_path = paths.device_cert_for_user(&profile.user_public_key);
    let cert = qos_user_identity::persist::read_device_cert(&cert_path)
        .map_err(|error| io::Error::other(error.to_string()))?;
    qos_user_identity::verify_device_cert(&cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if cert.user_public_key != profile.user_public_key || cert.device_public_key != device_key {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device certificate does not belong to the selected identity and device",
        ));
    }
    let fleet = crate::device_fleet::DeviceFleetStore::from_env()?.load(&profile.id)?;
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

fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity frame too large",
        ));
    }
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "identity frame too large"))?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

const MAX_ACTION_FRAME_BYTES: usize = 64 * 1024 * 1024;

fn read_action_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_ACTION_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity action frame exceeds 64 MiB",
        ));
    }
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes)?;
    postcard::from_bytes(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_action_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let bytes = postcard::to_allocvec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() > MAX_ACTION_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity action frame exceeds 64 MiB",
        ));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "identity frame too large"))?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{CapOverride, Scope, Subject};
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use std::os::fd::IntoRawFd;

    #[test]
    #[serial_test::serial(identity_env)]
    fn signing_requires_the_exact_app_action_capability() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let old_root = std::env::var_os("QOS_IDENTITY_ROOT");
        let system_root = temp.path().join("system");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("QOS_IDENTITY_ROOT", &system_root);
        }
        let identity_dir = xdg.join("qos/identity");
        let paths = qos_user_identity::IdentityPaths::under(&system_root);
        crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

        let room = RoomId::new();
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(1),
            "Room",
        );
        workspace.room = room;
        let mut tree = crate::shell::state::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let tree = tree.shared();

        assert!(matches!(
            sign(&tree, &room, "dev.qos.notes", "document.save", b"payload"),
            Response::Denied(_)
        ));
        tree.write().workspaces[0]
            .capabilities
            .push(CapOverride::grant(
                Subject::app("dev.qos.notes"),
                Scope::Action {
                    app: "dev.qos.notes".into(),
                    action: "document.save".into(),
                },
                Verb::Write,
            ));
        let Response::Signature(signature) =
            sign(&tree, &room, "dev.qos.notes", "document.save", b"payload")
        else {
            panic!("granted action was not signed");
        };
        let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
            .expect("registry")
            .expect("profile");
        let cert = qos_user_identity::persist::read_device_cert(
            &paths.device_cert_for_user(&profile.user_public_key),
        )
        .expect("device certificate");
        qos_user_identity::verify_device_cert(&cert).expect("root-signed certificate");
        assert_eq!(cert.user_public_key, profile.user_public_key);
        VerifyingKey::from_bytes(&cert.device_public_key)
            .expect("key")
            .verify(
                b"payload",
                &Signature::from_slice(&signature).expect("signature"),
            )
            .expect("valid device signature");
        assert!(matches!(
            sign(&tree, &room, "dev.qos.notes", "document.delete", b"payload"),
            Response::Denied(_)
        ));

        let device_key = qos_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("device key")
            .verifying_key()
            .to_bytes();
        crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
            .revoke(
                &profile.id,
                device_key,
                qos_user_identity::RevocationReason::Compromised,
                1,
            )
            .expect("revoke current device");
        assert!(matches!(
            sign(&tree, &room, "dev.qos.notes", "document.save", b"payload"),
            Response::Denied(reason) if reason.contains("revoked")
        ));

        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match old_root {
                Some(value) => std::env::set_var("QOS_IDENTITY_ROOT", value),
                None => std::env::remove_var("QOS_IDENTITY_ROOT"),
            }
        }
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn collaboration_signing_binds_route_payload_and_live_action_role() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let system_root = temp.path().join("system");
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let old_root = std::env::var_os("QOS_IDENTITY_ROOT");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("QOS_IDENTITY_ROOT", &system_root);
        }
        let identity_dir = xdg.join("qos/identity");
        let paths = qos_user_identity::IdentityPaths::under(&system_root);
        crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

        let room = RoomId::new();
        let workspace_id = room.to_string();
        let app_id = "dev.qos.notes";
        let record_id = "meeting-notes";
        let profile = crate::identity_registry::selected_identity_profile(&workspace_id)
            .expect("registry")
            .expect("profile");
        let device =
            qos_user_identity::persist::read_device_key(&paths.device_privkey).expect("device key");
        let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());

        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(1),
            "Room",
        );
        workspace.room = room;
        workspace.set_owner(profile.principal_id());
        let mut tree = crate::shell::state::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let tree = tree.shared();

        let now = std::time::SystemTime::now();
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"automerge document".to_vec())
                .encode()
                .expect("typed mutation"),
            document_artifact_id(app_id, &workspace_id, record_id),
            author,
            document_attestation_id(app_id, &workspace_id, record_id),
            collab_types::OpClass::Splice,
            now,
            1,
        );
        let unsigned = postcard::to_stdvec(&op).expect("unsigned envelope");
        let Response::Signature(signature) = sign_collaboration_op(
            &tree,
            &room,
            app_id,
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

        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &room,
                app_id,
                &workspace_id,
                "dev.qos.calendar",
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
                &room,
                app_id,
                &workspace_id,
                app_id,
                record_id,
                &malformed,
            ),
            Response::Denied(_)
        ));

        let mut tree_guard = tree.write();
        let workspace = &mut tree_guard.workspaces[0];
        workspace.owner = None;
        workspace.capabilities.clear();
        workspace.capabilities.push(CapOverride::membership(
            Subject::person(profile.principal_id()),
            workspace_id.clone(),
            Verb::Comment,
        ));
        drop(tree_guard);
        op.payload = DocumentMutation::Incremental(b"comment".to_vec())
            .encode()
            .expect("typed comment mutation");
        op.op_class = collab_types::OpClass::Comment;
        let comment = postcard::to_stdvec(&op).expect("comment envelope");
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &room,
                app_id,
                &workspace_id,
                app_id,
                record_id,
                &comment,
            ),
            Response::Signature(_)
        ));
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &room,
                app_id,
                &workspace_id,
                app_id,
                record_id,
                &unsigned,
            ),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));
        let mut tree_guard = tree.write();
        let workspace = &mut tree_guard.workspaces[0];
        workspace.capabilities.clear();
        workspace.capabilities.push(CapOverride::membership(
            Subject::person(profile.principal_id()),
            workspace_id.clone(),
            Verb::Read,
        ));
        drop(tree_guard);
        assert!(matches!(
            sign_collaboration_op(
                &tree,
                &room,
                app_id,
                &workspace_id,
                app_id,
                record_id,
                &comment,
            ),
            Response::Denied(reason) if reason.contains("denies this document action")
        ));

        unsafe {
            match old_xdg {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match old_root {
                Some(value) => std::env::set_var("QOS_IDENTITY_ROOT", value),
                None => std::env::remove_var("QOS_IDENTITY_ROOT"),
            }
        }
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn runtime_client_gets_metadata_and_only_granted_signing() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let old_root = std::env::var_os("QOS_IDENTITY_ROOT");
        let service_var = "QOS_SVC_IDENTITY_FD";
        let old_service = std::env::var_os(service_var);
        let system_root = temp.path().join("system");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("QOS_IDENTITY_ROOT", &system_root);
        }
        let identity_dir = xdg.join("qos/identity");
        let paths = qos_user_identity::IdentityPaths::under(system_root);
        crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths).expect("identity");

        let room = RoomId::new();
        let workspace_id = room.to_string();
        let profile = crate::identity_registry::selected_identity_profile(&workspace_id)
            .expect("registry")
            .expect("profile");
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(1),
            "Room",
        );
        workspace.room = room;
        workspace.set_owner(profile.principal_id());
        workspace.capabilities.push(CapOverride::grant(
            Subject::app("dev.qos.notes"),
            Scope::Action {
                app: "dev.qos.notes".into(),
                action: "document.save".into(),
            },
            Verb::Write,
        ));
        let mut tree = crate::shell::state::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let socket = temp.path().join("identity.sock");
        let _broker = spawn(socket.clone(), tree.shared()).expect("broker");
        let mut client = UnixStream::connect(socket).expect("connect");
        write_frame(
            &mut client,
            &Handshake {
                workspace_id: room.to_string(),
                app_id: "dev.qos.notes".into(),
            },
        )
        .expect("handshake");
        let fd = client.into_raw_fd();
        unsafe { std::env::set_var(service_var, fd.to_string()) };

        let identity = runtime_identity::load().expect("broker metadata");
        assert!(!identity.group_public_key().iter().all(|byte| *byte == 0));
        let cert: qos_user_identity::DeviceCert = serde_json::from_slice(
            identity
                .device_certificate()
                .expect("broker supplies identity-to-device certificate"),
        )
        .expect("certificate JSON");
        qos_user_identity::verify_device_cert(&cert).expect("root-signed certificate");
        assert_eq!(cert.user_public_key, identity.group_public_key());
        assert_eq!(cert.device_public_key, identity.signing_public_key());
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
            document_artifact_id("dev.qos.notes", &workspace_id, record_id),
            Ed25519PublicKey::from_bytes(identity.signing_public_key()),
            document_attestation_id("dev.qos.notes", &workspace_id, record_id),
            collab_types::OpClass::Splice,
            now,
            1,
        );
        let op_signature = identity
            .sign_collaboration_op(
                &workspace_id,
                "dev.qos.notes",
                record_id,
                &postcard::to_stdvec(&op).expect("unsigned envelope"),
            )
            .expect("owner may sign a typed document operation");
        op.signature = collab_types::Signature::from_bytes(op_signature);
        collab_types::verify_op(&op).expect("identity service signature verifies");
        assert!(identity.sign_for("document.delete", b"no").is_err());
        drop(identity);

        unsafe {
            match old_service {
                Some(value) => std::env::set_var(service_var, value),
                None => std::env::remove_var(service_var),
            }
            match old_xdg {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match old_root {
                Some(value) => std::env::set_var("QOS_IDENTITY_ROOT", value),
                None => std::env::remove_var("QOS_IDENTITY_ROOT"),
            }
        }
    }
}

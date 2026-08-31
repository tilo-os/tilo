//! Capability-gated identity metadata and signing service for app sandboxes.

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
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
        let _ = write_frame(
            &mut stream,
            &Response::Denied("invalid workspace identity".into()),
        );
        return;
    };
    while let Ok(request) = read_frame::<Request>(&mut stream) {
        let response = match request {
            Request::Metadata => metadata(&room),
            Request::Sign { action, message } => {
                sign(tree, &room, &handshake.app_id, &action, &message)
            }
            Request::CollaborationJoin { workspace_id } => {
                sign_collaboration_join(&room, &workspace_id)
            }
        };
        if write_frame(&mut stream, &response).is_err() {
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

fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
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
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(1),
            "Room",
        );
        workspace.room = room;
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
            .sign_collaboration_join(&room.to_string())
            .expect("the launcher-bound workspace may request a join proof");
        VerifyingKey::from_bytes(&identity.signing_public_key())
            .expect("device key")
            .verify(
                &runtime_identity::collaboration_join_signing_bytes(&room.to_string()),
                &Signature::from_bytes(&join_proof),
            )
            .expect("workspace-scoped device proof");
        assert!(
            identity
                .sign_collaboration_join(&RoomId::new().to_string())
                .is_err(),
            "the identity broker must not sign a join for another workspace"
        );
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

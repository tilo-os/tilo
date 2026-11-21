//! Local approval for a fresh native image that has no running requester.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Context, bail};
use ed25519_dalek::SigningKey;
use tilo_device_identity::{IdentityPaths, persist};
use tilo_identity::{device_fleet::DeviceFleetStore, store};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use zeroize::Zeroizing;

use super::{PersonalInstall, PersonalProjection, enrollment_commit_dir, enrollment_stage_dir};
use crate::{now_ns, sign_root_cert};

/// The setup machine's identity and the fresh, private handoff tree.
pub struct OfflineInstall<'a> {
    pub target_root: &'a Path,
    pub username: &'a str,
    pub local_identity_dir: &'a Path,
    pub local_system: &'a IdentityPaths,
    pub local_home_peers: &'a Path,
    pub local_workspaces: &'a Path,
}

/// Enroll a fresh image for the active person on this machine, without a
/// network exchange. Call only after the person chooses local enrollment.
/// The target gets a new device key and public identity data. The media
/// installer owns the subsequent transfer and sets the installed ownership.
///
/// # Errors
/// Refuses an existing destination identity, invalid account or label,
/// mismatched source identity or key, unavailable root, or failed writes.
/// The caller must discard the private handoff tree on failure.
#[allow(
    clippy::too_many_lines,
    reason = "keep the fresh target and both sides of the local enrollment together"
)]
pub fn install_from_local_identity(
    layout: &OfflineInstall<'_>,
    display_name: &str,
    device_label: &str,
) -> anyhow::Result<DeviceCert> {
    if !safe_account_name(layout.username) {
        bail!("target account name is not a single safe path component");
    }
    if device_label.is_empty() || device_label.len() > 64 {
        bail!("device label must be 1 to 64 bytes");
    }
    let identity = crate::root::load_signing_identity(Some(layout.local_identity_dir))?;
    let user = identity.group_public_key();
    let source_cert = persist::read_device_cert(&layout.local_system.device_cert)?;
    if source_cert.user_public_key != user {
        bail!("this machine's device certificate belongs to a different person");
    }
    let source_key = persist::load_device_key(layout.local_system)?;
    if source_key.verifying_key().to_bytes() != source_cert.device_public_key {
        bail!("this machine's device key does not match its certificate");
    }

    let target = fs::symlink_metadata(layout.target_root)?;
    if !target.is_dir() || !tilo_files::native::private::private_to_me(&target) {
        bail!("offline enrollment requires a private handoff directory owned by this account");
    }
    let var = layout.target_root.join("var");
    fs::create_dir(&var).context("reserve a fresh target identity tree")?;
    let identity_root = var.join("lib/tilo");
    let identity_dir = var
        .join("home")
        .join(layout.username)
        .join(".config/tilo/identity");
    tilo_identity::provisioning::prepare_identity_directory(&identity_dir)?;

    let paths = IdentityPaths::under(&identity_root);
    let mut seed = Zeroizing::new([0; 32]);
    getrandom::fill(&mut *seed)
        .map_err(|error| anyhow::anyhow!("generate the target device key: {error}"))?;
    let device = SigningKey::from_bytes(&seed);
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: device_label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now_ns()?,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_root_cert(&mut cert, &identity)?;

    let public_identity = store::public_member_record_with_presentation(
        if display_name.trim().is_empty() {
            store::UNNAMED_DISPLAY_NAME
        } else {
            display_name
        },
        identity.avatar_color(),
        user,
        identity.profile_photo().cloned(),
    )?;
    let identity_id = hex::encode(user);
    let source_store = DeviceFleetStore::at(layout.local_identity_dir, layout.local_system.clone());
    let mut fleet = source_store.load(&identity_id)?;
    if !fleet
        .certificates
        .iter()
        .any(|known| known.device_public_key == source_cert.device_public_key)
    {
        fleet.certificates.push(source_cert.clone());
    }
    fleet.certificates.push(cert.clone());
    let home_peers: Vec<_> = tilo_server_protocol::read_home_peers(layout.local_home_peers)?
        .into_iter()
        .filter(|peer| peer.user_public_key == identity_id)
        .collect();

    paths.ensure_directories()?;
    persist::write_device_key(&paths.device_privkey, &device)?;
    copy_personal_workspaces(
        layout.local_workspaces,
        &tilo_server_protocol::home_peer_registry_path(&identity_root)
            .with_file_name("personal-workspaces.json"),
    )?;
    let install = PersonalInstall {
        identity_root: &identity_root,
        identity_dir: &identity_dir,
        person_uid: None,
        image_source: Some(source_cert.device_public_key),
    };
    install.install_projection(
        &PersonalProjection {
            cert: &cert,
            identity: &public_identity,
            fleet: &serde_json::to_vec_pretty(&fleet)?,
            peers: &serde_json::to_vec_pretty(&home_peers)?,
        },
        None,
        || Ok(()),
        |_| Ok(()),
    )?;
    // No transaction paths from the setup machine travel with the image.
    fs::remove_dir(enrollment_commit_dir(&identity_root))?;
    fs::remove_dir(enrollment_stage_dir(&identity_root))?;
    source_store.remember_certificate(&identity_id, cert.clone())?;
    tilo_server_protocol::remember_personal_device(
        &layout
            .local_home_peers
            .with_file_name("personal-device-peers.json"),
        cert.device_public_key,
    )?;
    Ok(cert)
}

fn safe_account_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes
            .first()
            .is_some_and(|byte| matches!(byte, b'a'..=b'z' | b'_'))
        && bytes
            .iter()
            .skip(1)
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

fn valid_workspace_id(value: &str) -> bool {
    let mut parts = value.split('-');
    let expected = [8, 4, 4, 4, 12];
    expected.into_iter().all(|len| {
        parts.next().is_some_and(|part| {
            part.len() == len && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

fn copy_personal_workspaces(source: &Path, dest: &Path) -> anyhow::Result<()> {
    let bytes = match fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > 64 * 1024 {
        return Ok(());
    }
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return Ok(());
    };
    if ids.len() > 256 || ids.iter().any(|id| !valid_workspace_id(id)) {
        return Ok(());
    }
    let parent = dest.parent().context("the workspace list has no directory")?;
    fs::create_dir_all(parent).context("prepare the target's workspace list directory")?;
    tilo_files::native::atomic::write(dest, &serde_json::to_vec_pretty(&ids)?)
        .context("copy the setup device's workspace list")?;
    Ok(())
}

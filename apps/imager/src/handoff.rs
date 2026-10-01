//! Wi-Fi and identity written onto a target root by the setup machine.
//!
//! The collaborative install profile records the network name and whether to
//! enroll. The passphrase and the human root secret never enter that profile.
//! Enrollment certifies a new device key; the root secret stays on the machine
//! that already holds it. The privileged writer only copies a prepared tree.

use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::InstallProfile;
use crate::profile::PROFILE_BLOCK_SIZE;

/// Fixed header immediately before the secret-free profile block. The payload
/// sits directly before this header, in the slack after the image bytes.
pub const SETUP_HANDOFF_HEADER_SIZE: u64 = 4096;
pub const SETUP_HANDOFF_MAGIC: &str = "TILO_SETUP_HANDOFF_V1";

/// Where this machine's identity lives while it sets up another system.
pub struct LocalIdentity<'a> {
    pub identity_dir: &'a Path,
    pub system: &'a tilo_user_identity::IdentityPaths,
    pub home_peers: &'a Path,
}

/// Prepared etc/var tree the privileged writer copies onto an installed disk.
#[derive(Clone, Debug)]
pub struct StagedHandoff {
    pub root: PathBuf,
}

impl StagedHandoff {
    /// Temporary directory holding `etc/` and `var/` for the target.
    pub fn path(&self) -> &Path {
        &self.root
    }
}

/// Build the Wi-Fi connection and enrolled identity under a temporary tree.
///
/// Signing happens here, on the setup machine. The returned tree is secret
/// material for the target and must only be readable by the elevated writer.
///
/// # Errors
///
/// Returns a string when the network, account, or device label is unusable,
/// or when writing the staged files fails.
pub fn stage_setup_handoff(
    profile: &InstallProfile,
    wifi_psk: &str,
    local: &LocalIdentity<'_>,
) -> Result<Option<StagedHandoff>, String> {
    let needs_wifi = !profile.wifi_ssid.trim().is_empty();
    let needs_identity = profile.enroll_local_identity;
    if !needs_wifi && !needs_identity {
        return Ok(None);
    }
    if needs_wifi && wifi_psk.is_empty() {
        return Err("A Wi-Fi password is required when a network name is set.".into());
    }
    let dir = tempfile::Builder::new()
        .prefix("tilo-imager-handoff-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let root = dir.keep();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
    }
    apply_setup_handoff(&root, profile, wifi_psk, local)?;
    Ok(Some(StagedHandoff { root }))
}

/// Place the profile's Wi-Fi connection and, when requested, this machine's
/// person onto `root` as a new device.
///
/// # Errors
///
/// Returns a string when the network, account, or device label is unusable,
/// or when writing the target files fails.
pub fn apply_setup_handoff(
    root: &Path,
    profile: &InstallProfile,
    wifi_psk: &str,
    local: &LocalIdentity<'_>,
) -> Result<(), String> {
    let ssid = profile.wifi_ssid.trim();
    if !ssid.is_empty() {
        write_wifi_connection(root, ssid, wifi_psk)?;
    }
    if profile.enroll_local_identity {
        let label = profile.device_label.trim();
        if label.is_empty() {
            return Err("A device name is required to enroll this machine's identity.".into());
        }
        #[cfg(unix)]
        {
            let rooms = tilo_identity_host::provisioning::personal_rooms_export_path()
                .unwrap_or_else(|| PathBuf::from("/var/lib/tilo/personal-rooms.json"));
            tilo_identity_host::provisioning::enroll_from_local_identity(
                &tilo_identity_host::provisioning::JoinedDeviceLayout {
                    target_root: root,
                    username: profile.user.username.trim(),
                    local_identity_dir: local.identity_dir,
                    local_system: local.system,
                    local_home_peers: local.home_peers,
                    local_rooms: &rooms,
                },
                &profile.user.full_name,
                label,
                now_ns(),
            )
            .map_err(|error| error.to_string())?;
        }
        #[cfg(not(unix))]
        {
            let _ = (root, local, label);
            return Err("Identity enrollment from a Windows host is not supported yet.".into());
        }
    }
    Ok(())
}

/// Copy a staged etc/var tree onto a mounted Tilo root partition.
///
/// Looks for the deployment's `/etc` and the stateroot's `/var` the same way
/// the disk image builder does.
///
/// # Errors
///
/// Returns an I/O error when the mount is not an installed Tilo system or the
/// copy fails.
pub fn install_staged_onto_mounted_root(mnt: &Path, staged: &Path) -> io::Result<()> {
    let etc = first_existing(&[
        glob_one(mnt, "state/deploy/*/etc")?,
        glob_one(mnt, "ostree/deploy/*/deploy/*/etc")?,
    ])
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "mounted disk has no installed Tilo /etc",
        )
    })?;
    let var = first_existing(&[
        glob_one(mnt, "state/os/*/var")?,
        glob_one(mnt, "ostree/deploy/*/var")?,
    ])
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "mounted disk has no installed Tilo /var",
        )
    })?;
    let staged_etc = staged.join("etc");
    if staged_etc.is_dir() {
        copy_tree(&staged_etc, &etc)?;
    }
    let staged_var = staged.join("var");
    if staged_var.is_dir() {
        copy_tree(&staged_var, &var)?;
    }
    Ok(())
}

/// Write the staged tree into the slack between the image and the profile block.
///
/// A live installer medium has no installed root yet. The installer extracts
/// this payload and copies it onto the disk it installs.
///
/// # Errors
///
/// Returns an error when the payload would overlap the image or the profile
/// block, or when the device cannot be written.
pub fn embed_setup_handoff(
    device: &Path,
    image_len: u64,
    device_len: u64,
    staged: &Path,
) -> io::Result<()> {
    let payload = pack_setup_handoff(staged)?;
    let header_at = device_len
        .checked_sub(PROFILE_BLOCK_SIZE as u64)
        .and_then(|offset| offset.checked_sub(SETUP_HANDOFF_HEADER_SIZE))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "drive is too small for a setup handoff",
            )
        })?;
    let payload_at = header_at
        .checked_sub(u64::try_from(payload.len()).unwrap_or(u64::MAX))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "setup handoff does not fit on the drive",
            )
        })?;
    if payload_at < image_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "installer medium has no room between the image and the setup profile",
        ));
    }
    let digest = hex::encode(Sha256::digest(&payload));
    let text = format!("{SETUP_HANDOFF_MAGIC}\n{digest}\n{}\n", payload.len());
    if text.len() > SETUP_HANDOFF_HEADER_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "setup handoff header does not fit",
        ));
    }
    let mut header = vec![0_u8; SETUP_HANDOFF_HEADER_SIZE as usize];
    header[..text.len()].copy_from_slice(text.as_bytes());
    let mut file = OpenOptions::new().write(true).open(device)?;
    file.seek(SeekFrom::Start(payload_at))?;
    file.write_all(&payload)?;
    file.seek(SeekFrom::Start(header_at))?;
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(())
}

fn pack_setup_handoff(staged: &Path) -> io::Result<Vec<u8>> {
    let mut files = Vec::new();
    collect_files(staged, &mut files)?;
    files.sort();
    let mut payload = Vec::new();
    for path in files {
        let relative = path.strip_prefix(staged).unwrap_or(&path);
        let relative = relative.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "setup handoff path is not UTF-8",
            )
        })?;
        let bytes = fs::read(&path)?;
        payload.extend_from_slice(relative.as_bytes());
        payload.push(b'\n');
        payload.extend_from_slice(bytes.len().to_string().as_bytes());
        payload.push(b'\n');
        payload.extend_from_slice(&bytes);
    }
    Ok(payload)
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn nm_quoted(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        if ch == '\\' || ch == '"' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

fn write_wifi_connection(root: &Path, ssid: &str, psk: &str) -> Result<(), String> {
    if ssid.len() > 32 || ssid.bytes().any(|byte| byte < 0x20 || byte == b'"') {
        return Err(
            "Wi-Fi name must be 1–32 characters without quotes or control characters.".into(),
        );
    }
    if !(8..=63).contains(&psk.len())
        || !psk
            .bytes()
            .all(|byte| (0x20..0x7f).contains(&byte) && byte != b'"')
    {
        return Err("Wi-Fi password must be 8–63 printable characters without quotes.".into());
    }
    let dir = root.join("etc/NetworkManager/system-connections");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join("tilo-setup.nmconnection");
    let ssid = nm_quoted(ssid);
    let psk = nm_quoted(psk);
    let body = format!(
        "[connection]\nid=tilo-setup\ntype=wifi\nautoconnect=true\npermissions=\n\n[wifi]\nmode=infrastructure\nssid={ssid}\n\n[wifi-security]\nkey-mgmt=wpa-psk\npsk={psk}\n\n[ipv4]\nmethod=auto\n\n[ipv6]\nmethod=auto\n"
    );
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    file.write_all(body.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn glob_one(root: &Path, pattern: &str) -> io::Result<Option<PathBuf>> {
    let mut parts = pattern.split('/');
    let mut current = vec![root.to_path_buf()];
    for part in parts.by_ref() {
        if part == "*" {
            let mut next = Vec::new();
            for dir in &current {
                let Ok(entries) = fs::read_dir(dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        next.push(path);
                    }
                }
            }
            current = next;
        } else {
            current = current
                .into_iter()
                .map(|dir| dir.join(part))
                .filter(|path| path.exists())
                .collect();
        }
        if current.is_empty() {
            return Ok(None);
        }
    }
    Ok(current.into_iter().next())
}

fn first_existing(candidates: &[Option<PathBuf>]) -> Option<PathBuf> {
    candidates.iter().find_map(Clone::clone)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in walkdir(from)? {
        let relative = entry.strip_prefix(from).map_err(io::Error::other)?;
        let dest = to.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&entry, &dest)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let mode = fs::metadata(&entry)?.mode() & 0o777;
            fs::set_permissions(&dest, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn walkdir(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        out.push(dir.clone());
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Default locations for this machine's identity material.
#[cfg(unix)]
pub fn local_identity_from_env() -> Result<LocalIdentity<'static>, String> {
    use std::sync::OnceLock;
    static SYSTEM: OnceLock<tilo_user_identity::IdentityPaths> = OnceLock::new();
    static IDENTITY_DIR: OnceLock<PathBuf> = OnceLock::new();
    static HOME_PEERS: OnceLock<PathBuf> = OnceLock::new();
    let system = SYSTEM.get_or_init(tilo_identity_host::provisioning::collaboration_identity_paths);
    let identity_dir = IDENTITY_DIR.get_or_init(|| {
        tilo_identity_host::registry::IdentityRegistryStore::from_env().map_or_else(
            |_| dirs_fallback().join(".config/tilo/identity"),
            |store| store.identity_dir().to_path_buf(),
        )
    });
    let home_peers = HOME_PEERS.get_or_init(|| {
        system
            .user_pubkey
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("/var/lib/tilo"))
            .join("home-peers.json")
    });
    Ok(LocalIdentity {
        identity_dir,
        system,
        home_peers,
    })
}

#[cfg(not(unix))]
pub fn local_identity_from_env() -> Result<LocalIdentity<'static>, String> {
    use std::sync::OnceLock;
    static SYSTEM: OnceLock<tilo_user_identity::IdentityPaths> = OnceLock::new();
    static IDENTITY_DIR: OnceLock<PathBuf> = OnceLock::new();
    static HOME_PEERS: OnceLock<PathBuf> = OnceLock::new();
    let root = std::env::temp_dir().join("tilo-imager");
    Ok(LocalIdentity {
        identity_dir: IDENTITY_DIR.get_or_init(|| root.join("identity")),
        system: SYSTEM
            .get_or_init(|| tilo_user_identity::IdentityPaths::under(root.join("system"))),
        home_peers: HOME_PEERS.get_or_init(|| root.join("home-peers.json")),
    })
}

#[cfg(unix)]
fn dirs_fallback() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn handoff_writes_wifi_and_a_certified_device_without_the_root_secret() {
        let local_dir = tempfile::tempdir().unwrap();
        let identity_dir = local_dir.path().join("identity");
        let local_system =
            tilo_user_identity::IdentityPaths::under(local_dir.path().join("system"));
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &local_system,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let local_cert =
            tilo_user_identity::persist::read_device_cert(&local_system.device_cert).unwrap();
        let identity_id = hex::encode(local_cert.user_public_key);
        let home_peers = local_dir.path().join("home-peers.json");
        let peer = tilo_relay_protocol::HomePeerRegistration {
            schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(local_cert.device_public_key),
            relay_service_node_id: hex::encode([4_u8; 32]),
            relay_endpoint: None,
            label: "setup".into(),
            user_public_key: identity_id.clone(),
            device_cert: None,
            relay_workspace_ids: vec![7],
        };
        fs::write(
            &home_peers,
            serde_json::to_vec_pretty(&vec![peer.clone()]).unwrap(),
        )
        .unwrap();
        let local = LocalIdentity {
            identity_dir: &identity_dir,
            system: &local_system,
            home_peers: &home_peers,
        };
        let dir = tempfile::tempdir().unwrap();
        let profile = InstallProfile {
            wifi_ssid: "studio".into(),
            device_label: "orange-pi".into(),
            enroll_local_identity: true,
            ..InstallProfile::default()
        };

        apply_setup_handoff(dir.path(), &profile, "correct-horse", &local).unwrap();

        let connection = fs::read_to_string(
            dir.path()
                .join("etc/NetworkManager/system-connections/tilo-setup.nmconnection"),
        )
        .unwrap();
        assert!(connection.contains("ssid=\"studio\""));
        assert!(connection.contains("psk=\"correct-horse\""));
        assert_eq!(nm_quoted("my network"), "\"my network\"");
        assert_eq!(nm_quoted("a\\b"), "\"a\\\\b\"");
        let media = String::from_utf8(profile.to_media_profile_json().unwrap()).unwrap();
        assert!(!media.contains("correct-horse"));
        assert!(!media.contains("orange-pi"));
        let paths = tilo_user_identity::IdentityPaths::under(dir.path().join("var/lib/tilo"));
        assert!(tilo_user_identity::joined_device_is_ready(&paths, now_ns()).unwrap());
        assert!(!paths.user_privkey.exists());
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        assert_ne!(
            cert.device_public_key, local_cert.device_public_key,
            "target must get a new device key, not the setup machine's"
        );
        assert_eq!(cert.user_public_key, local_cert.user_public_key);
        assert!(cert.capabilities.can_own_workspaces);
        assert!(cert.capabilities.can_read_all_workspaces);
        let copied: Vec<tilo_relay_protocol::HomePeerRegistration> = serde_json::from_slice(
            &fs::read(dir.path().join("var/lib/tilo/home-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(copied, vec![peer]);
        let direct: Vec<String> = serde_json::from_slice(
            &fs::read(dir.path().join("var/lib/tilo/personal-device-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(direct, vec![hex::encode(local_cert.device_public_key)]);
        let target_identity = dir.path().join(format!(
            "var/home/{}/.config/tilo/identity",
            profile.user.username.trim()
        ));
        let target_fleet =
            tilo_identity_host::device_fleet::DeviceFleetStore::at(&target_identity, paths)
                .load(&identity_id)
                .unwrap();
        assert!(
            target_fleet
                .certificates
                .iter()
                .any(|item| item.device_public_key == cert.device_public_key)
        );
        assert!(
            target_fleet
                .certificates
                .iter()
                .any(|item| item.device_public_key == local_cert.device_public_key)
        );
        let local_fleet =
            tilo_identity_host::device_fleet::DeviceFleetStore::at(&identity_dir, local_system)
                .load(&identity_id)
                .unwrap();
        assert!(
            local_fleet
                .certificates
                .iter()
                .any(|item| item.device_public_key == cert.device_public_key)
        );
    }

    #[test]
    fn stage_setup_handoff_requires_a_wifi_password_when_ssid_is_set() {
        let local_dir = tempfile::tempdir().unwrap();
        let identity_dir = local_dir.path().join("identity");
        let local_system =
            tilo_user_identity::IdentityPaths::under(local_dir.path().join("system"));
        let home_peers = local_dir.path().join("home-peers.json");
        let local = LocalIdentity {
            identity_dir: &identity_dir,
            system: &local_system,
            home_peers: &home_peers,
        };
        let profile = InstallProfile {
            wifi_ssid: "studio".into(),
            ..InstallProfile::default()
        };
        let err = stage_setup_handoff(&profile, "", &local).unwrap_err();
        assert!(err.contains("Wi-Fi password"));
    }

    #[test]
    fn staged_tree_installs_onto_a_bootc_layout() {
        let staged = tempfile::tempdir().unwrap();
        write_wifi_connection(staged.path(), "studio", "correct-horse").unwrap();
        fs::create_dir_all(staged.path().join("var/lib/tilo")).unwrap();
        fs::write(staged.path().join("var/lib/tilo/marker"), b"ok").unwrap();

        let mnt = tempfile::tempdir().unwrap();
        let etc = mnt.path().join("state/deploy/abcd/etc");
        let var = mnt.path().join("state/os/fedora/var");
        fs::create_dir_all(&etc).unwrap();
        fs::create_dir_all(&var).unwrap();
        install_staged_onto_mounted_root(mnt.path(), staged.path()).unwrap();
        assert!(
            etc.join("NetworkManager/system-connections/tilo-setup.nmconnection")
                .is_file()
        );
        assert_eq!(fs::read(var.join("lib/tilo/marker")).unwrap(), b"ok");
    }

    #[test]
    fn setup_handoff_embeds_in_the_slack_before_the_profile_block() {
        let staged = tempfile::tempdir().unwrap();
        fs::write(staged.path().join("marker"), b"wifi-secret").unwrap();
        let image_len = 1000_u64;
        let device_len = image_len + PROFILE_BLOCK_SIZE as u64 + SETUP_HANDOFF_HEADER_SIZE + 8192;
        let device = tempfile::NamedTempFile::new().unwrap();
        device.as_file().set_len(device_len).unwrap();
        {
            use std::io::Write;
            let mut file = device.reopen().unwrap();
            file.write_all(&vec![0xAB; image_len as usize]).unwrap();
        }
        embed_setup_handoff(device.path(), image_len, device_len, staged.path()).unwrap();
        let bytes = fs::read(device.path()).unwrap();
        assert!(bytes[..image_len as usize].iter().all(|byte| *byte == 0xAB));
        let header_at =
            (device_len - PROFILE_BLOCK_SIZE as u64 - SETUP_HANDOFF_HEADER_SIZE) as usize;
        let header = String::from_utf8_lossy(&bytes[header_at..header_at + 80]);
        assert!(header.starts_with(SETUP_HANDOFF_MAGIC));
        assert!(bytes.windows(11).any(|window| window == b"wifi-secret"));
    }
}

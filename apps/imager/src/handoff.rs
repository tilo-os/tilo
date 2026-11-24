//! Wi-Fi, SSH login keys and identity, written onto the disk an install writes.
//!
//! The profile records the network name and whether to enroll; the passphrase, the
//! login keys and the human root secret never enter it.

use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::InstallProfile;
use imager_backend::profile::PROFILE_BLOCK_SIZE;

pub const SETUP_HANDOFF_HEADER_SIZE: u64 = 4096;
pub const SETUP_HANDOFF_MAGIC: &str = "TILO_SETUP_HANDOFF_V1";

pub struct LocalIdentity<'a> {
    pub identity_dir: &'a Path,
    pub system: &'a tilo_device_identity::IdentityPaths,
}

#[derive(Clone, Debug)]
pub struct StagedHandoff {
    pub root: PathBuf,
}

impl StagedHandoff {
    pub fn path(&self) -> &Path {
        &self.root
    }
}

pub fn stage_setup_handoff(
    profile: &InstallProfile,
    wifi_psk: &str,
    _local: &LocalIdentity<'_>,
) -> Result<Option<StagedHandoff>, String> {
    let needs_wifi = !profile.wifi_ssid.trim().is_empty();
    let needs_identity = profile.enroll_local_identity;
    if !needs_wifi && !needs_identity && profile.purpose == "computer" {
        return Ok(None);
    }
    if needs_wifi && wifi_psk.is_empty() {
        return Err("A Wi-Fi password is required when a network name is set.".into());
    }
    let root = prepare_handoff_root()?;
    if let Err(error) = apply_setup_handoff(&root, profile, wifi_psk, _local) {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    Ok(Some(StagedHandoff { root }))
}

pub fn stage_access_handoff(
    profile: &InstallProfile,
    wifi_psk: &str,
    ssh_public_keys: &str,
    name_target: bool,
) -> Result<Option<StagedHandoff>, String> {
    let ssid = profile.wifi_ssid.trim();
    let needs_wifi = !ssid.is_empty();
    let needs_key = !ssh_public_keys.trim().is_empty();
    let hostname = chosen_hostname(profile).filter(|_| name_target);
    if !needs_wifi
        && !needs_key
        && hostname.is_none()
        && !(name_target && profile.purpose == "server")
    {
        return Ok(None);
    }
    if needs_wifi && wifi_psk.is_empty() {
        return Err("A Wi-Fi password is required when a network name is set.".into());
    }
    validate_bringup_ssh_key(ssh_public_keys)?;
    let root = prepare_handoff_root()?;
    let staged = (|| {
        if name_target {
            write_install_purpose(&root, profile)?;
        }
        if needs_wifi {
            write_wifi_connection(&root, ssid, wifi_psk)?;
        }
        if let Some(hostname) = hostname {
            write_hostname(&root, hostname)?;
        }
        install_bringup_ssh_key(&root, ssh_public_keys)
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    Ok(Some(StagedHandoff { root }))
}

pub const BRINGUP_SSH_KEY: &str = "etc/tilo/bringup-ssh.pub";

pub(crate) fn chosen_hostname(profile: &InstallProfile) -> Option<&str> {
    let hostname = profile.hostname.trim();
    (!hostname.is_empty() && hostname != crate::model::DEFAULT_HOSTNAME).then_some(hostname)
}

fn write_hostname(root: &Path, hostname: &str) -> Result<(), String> {
    if !install_profile_model::valid_hostname(hostname) {
        return Err(
            "Hostname must be 1–63 letters, numbers or hyphens, without a hyphen at either end."
                .into(),
        );
    }
    let etc = root.join("etc");
    fs::create_dir_all(&etc).map_err(|error| error.to_string())?;
    let path = etc.join("hostname");
    fs::write(&path, format!("{hostname}\n")).map_err(|error| error.to_string())?;
    set_mode(&path, 0o644).map_err(|error| error.to_string())
}

pub const ACCOUNT_HOME_FILES: &str = "account-home";

/// A file name the account's home can take: one path component, not hidden,
/// and nothing a shell or an `ls` would misread.
pub(crate) fn home_file_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

pub fn stage_account_home_file(root: &Path, name: &str, contents: &[u8]) -> Result<(), String> {
    if !home_file_name_is_safe(name) {
        return Err(format!(
            "{name:?} is not a plain file name for the account's home"
        ));
    }
    let dir = root.join(ACCOUNT_HOME_FILES);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("{name}: {error}"))?;
    file.write_all(contents)
        .map_err(|error| error.to_string())?;
    set_mode(&path, 0o644).map_err(|error| error.to_string())
}

pub fn install_bringup_ssh_key(root: &Path, ssh_public_keys: &str) -> Result<(), String> {
    let Some(line) = bringup_ssh_key_lines(ssh_public_keys)? else {
        return Ok(());
    };
    let path = root.join(BRINGUP_SSH_KEY);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&path, line).map_err(|error| error.to_string())?;
    set_mode(&path, 0o644).map_err(|error| error.to_string())
}

const MAX_BRINGUP_SSH_KEYS: usize = 16;

const REFUSED_SSH_KEY: &str =
    "Each SSH key must be one public key per line: its type, the key, and an optional comment.";

/// Accept nothing, or OpenSSH public keys of a type the installer admits,
/// one per line, with no `authorized_keys` options.
pub fn validate_bringup_ssh_key(ssh_public_keys: &str) -> Result<(), String> {
    bringup_ssh_key_lines(ssh_public_keys).map(|_| ())
}

fn bringup_ssh_key_lines(text: &str) -> Result<Option<String>, String> {
    if text.contains("PRIVATE KEY") {
        return Err(REFUSED_SSH_KEY.into());
    }
    let mut keys = String::new();
    let mut count = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        count += 1;
        if count > MAX_BRINGUP_SSH_KEYS {
            return Err(format!(
                "At most {MAX_BRINGUP_SSH_KEYS} SSH keys can be added."
            ));
        }
        keys.push_str(&bringup_ssh_key_line(line)?);
    }
    Ok((count > 0).then_some(keys))
}

fn bringup_ssh_key_line(text: &str) -> Result<String, String> {
    use base64::Engine as _;
    const REFUSED: &str = REFUSED_SSH_KEY;
    let text = text.trim();
    let mut parts = text.split_whitespace();
    let kind = parts.next().unwrap_or_default();
    let blob = parts.next().unwrap_or_default();
    if !matches!(
        kind,
        "ssh-ed25519"
            | "ssh-rsa"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
    ) {
        return Err(REFUSED.into());
    }
    let wire = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map_err(|_| REFUSED.to_string())?;
    let named = wire
        .get(..4)
        .and_then(|len| usize::try_from(u32::from_be_bytes(len.try_into().ok()?)).ok())
        .and_then(|len| wire.get(4..4 + len));
    if named != Some(kind.as_bytes()) {
        return Err(REFUSED.into());
    }
    Ok(format!("{kind} {blob}\n"))
}

fn prepare_handoff_root() -> Result<PathBuf, String> {
    let dir = tempfile::Builder::new()
        .prefix("tilo-imager-handoff-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    set_mode(dir.path(), 0o700).map_err(|error| error.to_string())?;
    Ok(dir.keep())
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

pub fn apply_setup_handoff(
    root: &Path,
    profile: &InstallProfile,
    wifi_psk: &str,
    _local: &LocalIdentity<'_>,
) -> Result<(), String> {
    write_install_purpose(root, profile)?;
    let ssid = profile.wifi_ssid.trim();
    if !ssid.is_empty() {
        write_wifi_connection(root, ssid, wifi_psk)?;
    }
    if profile.enroll_local_identity {
        let label = profile.device_label.trim();
        if label.is_empty() {
            return Err("A device name is required to enroll this machine's identity.".into());
        }
        stage_identity_ticket(root, profile, label)?;
    }
    Ok(())
}

/// Give the image a ticket to join the person's devices with at its first boot, written where a
/// staged machine finds it (`/etc/tilo/ticket`, owner-only): a one-use ticket the person's own
/// device issues, which the image redeems itself. Nothing of the person's identity is written
/// onto the image: no root, no certificate.
#[cfg(unix)]
fn stage_identity_ticket(root: &Path, profile: &InstallProfile, label: &str) -> Result<(), String> {
    if profile.user.username.trim().is_empty() {
        return Err("Choose the account name before enrolling this device.".into());
    }
    let authority = tilo_api::daemon::this_computer().map_err(|error| {
        format!("This machine cannot issue the ticket the image joins with: {error}")
    })?;
    let answer = authority
        .act(
            tilo_device_identity::login::ISSUE,
            serde_json::json!({ "label": label, "wait": false }),
        )
        .map_err(|error| format!("The ticket the image joins with was not issued: {error}"))?;
    let ticket = answer
        .get("ticket")
        .and_then(serde_json::Value::as_str)
        .ok_or("The ticket the image joins with was not answered")?
        .to_owned();
    let directory = root.join("etc/tilo");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join("ticket");
    tilo_files::native::atomic::write_with_mode(&path, ticket.as_bytes(), 0o600)
        .map_err(|error| error.to_string())
}

/// See the Unix `stage_identity_ticket`: the image's accounts own their files, so only a Unix
/// machine stages one an image's first boot may read.
#[cfg(not(unix))]
fn stage_identity_ticket(
    _root: &Path,
    _profile: &InstallProfile,
    _label: &str,
) -> Result<(), String> {
    Err("This machine cannot give the image a ticket to join with.".into())
}

fn write_install_purpose(root: &Path, profile: &InstallProfile) -> Result<(), String> {
    if !install_profile_model::valid_purpose(&profile.purpose) {
        return Err("Invalid installation purpose.".into());
    }
    let directory = root.join("etc/tilo");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    fs::write(
        directory.join("install-purpose"),
        format!("{}\n", profile.purpose),
    )
    .map_err(|error| error.to_string())
}

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
    let placed = block_devices::root_customization::Placed::before_copy(staged, &etc, &var)?;
    let staged_etc = staged.join("etc");
    if staged_etc.is_dir() {
        copy_tree(&staged_etc, &etc)?;
    }
    let staged_var = staged.join("var");
    if staged_var.is_dir() {
        refuse_human_root_secret(&staged_var)?;
        copy_tree(&staged_var, &var)?;
    }
    block_devices::root_customization::finish(
        &etc,
        &var,
        staged,
        placed,
        block_devices::root_customization::Requests {
            ssh_key: BRINGUP_SSH_KEY,
            home_files: ACCOUNT_HOME_FILES,
            validate_keys: |keys| validate_bringup_ssh_key(keys).map_err(io::Error::other),
            valid_home_name: home_file_name_is_safe,
        },
    )
}

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
    refuse_human_root_secret(&staged.join("var"))?;
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

fn nm_keyfile_value(value: &str) -> String {
    let rest = value.trim_start_matches(' ');
    let mut escaped = "\\s".repeat(value.len() - rest.len());
    for ch in rest.chars() {
        if ch == '\\' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn write_wifi_connection(root: &Path, ssid: &str, psk: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.bytes().any(|byte| byte < 0x20 || byte == b'"') {
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
    let ssid = nm_keyfile_value(ssid);
    let psk = nm_keyfile_value(psk);
    let body = format!(
        "[connection]\nid=tilo-setup\nuuid=3f1c0a6e-7b2d-4e59-9c18-6a4d2b8e0f71\ntype=wifi\nautoconnect=true\nautoconnect-retries=0\n\n[wifi]\nmode=infrastructure\nssid={ssid}\n\n[wifi-security]\nkey-mgmt=wpa-psk\npsk={psk}\n\n[ipv4]\nmethod=auto\n\n[ipv6]\nmethod=auto\n"
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
                    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                        next.push(path);
                    }
                }
            }
            current = next;
        } else {
            current = current
                .into_iter()
                .map(|dir| dir.join(part))
                .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
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

/// The person's root lives in their identity store, whose file provider keeps
/// it in `secrets/`. A joined device gets only the public side of its
/// identity, so a staged home whose store holds secrets is refused whole.
fn refuse_human_root_secret(staged_var: &Path) -> io::Result<()> {
    let Ok(homes) = fs::read_dir(staged_var.join("home")) else {
        return Ok(());
    };
    for home in homes {
        let secrets = home?.path().join(".config/tilo/identity/secrets");
        if fs::symlink_metadata(&secrets).is_ok() {
            return Err(io::Error::other(
                "refusing to copy the human root secret onto the target",
            ));
        }
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(from)?.is_dir() {
        return Err(io::Error::other("handoff source is not a real directory"));
    }
    ensure_copy_directory(to, Path::new(""))?;
    for entry in walkdir(from)? {
        let relative = entry.strip_prefix(from).map_err(io::Error::other)?;
        let dest = to.join(relative);
        let meta = fs::symlink_metadata(&entry)?;
        if meta.is_dir() {
            ensure_copy_directory(to, relative)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if let Some(parent) = relative.parent() {
            ensure_copy_directory(to, parent)?;
        }
        if fs::symlink_metadata(&dest).is_ok_and(|meta| meta.file_type().is_symlink()) {
            fs::remove_file(&dest)?;
        }
        fs::copy(&entry, &dest)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            set_mode(&dest, meta.mode() & 0o777)?;
        }
    }
    Ok(())
}

fn ensure_copy_directory(root: &Path, relative: &Path) -> io::Result<()> {
    let mut current = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            let std::path::Component::Normal(name) = component else {
                return Err(io::Error::other(
                    "handoff directory escapes its destination",
                ));
            };
            current.push(name);
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(
                    "handoff destination is not a real directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error),
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
            if fs::symlink_metadata(&path)?.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

pub fn local_identity_from_env() -> Result<LocalIdentity<'static>, String> {
    use std::sync::OnceLock;
    static SYSTEM: OnceLock<tilo_device_identity::IdentityPaths> = OnceLock::new();
    static IDENTITY_DIR: OnceLock<PathBuf> = OnceLock::new();
    let system = SYSTEM.get_or_init(tilo_identity::provisioning::collaboration_identity_paths);
    let identity_dir = IDENTITY_DIR.get_or_init(|| {
        tilo_identity::store::IdentityRegistryStore::from_env().map_or_else(
            |_| dirs_fallback().join(".config/tilo/identity"),
            |store| store.identity_dir().to_path_buf(),
        )
    });
    Ok(LocalIdentity {
        identity_dir,
        system,
    })
}

fn dirs_fallback() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image written with no enrolment carries no identity to join with.
    #[test]
    fn an_image_written_without_adding_a_device_carries_no_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("image");
        fs::create_dir(&root).unwrap();
        let profile = InstallProfile {
            wifi_ssid: "Home".into(),
            ..InstallProfile::default()
        };
        let local = LocalIdentity {
            identity_dir: Path::new("/nonexistent"),
            system: &tilo_device_identity::IdentityPaths::default_system(),
        };
        assert!(
            apply_setup_handoff(&root, &profile, "hunter2", &local).is_err()
                || !root.join("etc/tilo/ticket").exists()
        );
        assert!(
            !root.join("etc/tilo/ticket").exists(),
            "the image carries no ticket unless the person asks for one"
        );
    }

    /// A machine that asks for the image to come up joined gets a ticket, never a certificate or
    /// a root: with no daemon to issue one here, it is told so and the image carries nothing.
    #[cfg(unix)]
    #[test]
    fn an_image_that_joins_carries_a_ticket_and_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("image");
        fs::create_dir(&root).unwrap();
        let profile = InstallProfile {
            enroll_local_identity: true,
            device_label: "Box".into(),
            user: install_profile_model::media::UserAccount {
                username: "ada".into(),
                ..install_profile_model::media::UserAccount::default()
            },
            ..InstallProfile::default()
        };
        let local = LocalIdentity {
            identity_dir: Path::new("/nonexistent"),
            system: &tilo_device_identity::IdentityPaths::default_system(),
        };
        // No daemon serves this test: nothing is written, and the person is told why.
        let refused = apply_setup_handoff(&root, &profile, "", &local).unwrap_err();
        assert!(refused.contains("ticket"), "{refused}");
        assert!(!root.join("etc/tilo/ticket").exists());
        assert!(
            !root.join("var/lib/tilo").exists(),
            "no identity is written"
        );
    }
}

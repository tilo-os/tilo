//! Wi-Fi, SSH login keys, and identity written onto a target root by the
//! setup machine.
//!
//! The collaborative install profile records the network name and whether to
//! enroll. The passphrase, the login keys, and the human root secret never
//! enter that profile.
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
pub fn stage_setup_handoff(
    profile: &InstallProfile,
    wifi_psk: &str,
    local: &LocalIdentity<'_>,
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
    if let Err(error) = apply_setup_handoff(&root, profile, wifi_psk, local) {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    Ok(Some(StagedHandoff { root }))
}

/// Stage Wi-Fi, owner SSH keys and the chosen hostname without enrolling an
/// identity.
///
/// Any piece may be absent. Only enrollment needs this machine's identity,
/// so a write that joins a network, names the machine or carries the
/// owner's keys works on a setup machine that has none. `name_target` is
/// false for installer media, whose installed system takes its name from
/// the embedded profile instead.
///
/// # Errors
///
/// Returns a string when the network, a key or the hostname is unusable, or
/// when the staged files cannot be written.
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

/// Path of the owner SSH keys inside a handoff tree, one key per line.
///
/// The installer applies this request only when it installs a development
/// image, as key-only owner login for the account it creates. A production
/// install refuses it instead of opening SSH. A disk that already carries
/// its account gets the same login from the write
/// ([`block_devices::installed_root`]).
pub const BRINGUP_SSH_KEY: &str = "etc/tilo/bringup-ssh.pub";

/// The hostname the operator chose, or `None` while the profile still has
/// the default, so a written disk keeps the name its image gives it.
pub(crate) fn chosen_hostname(profile: &InstallProfile) -> Option<&str> {
    let hostname = profile.hostname.trim();
    (!hostname.is_empty() && hostname != crate::model::DEFAULT_HOSTNAME).then_some(hostname)
}

/// Name the target. Tilo images announce their hostname over mDNS (Avahi),
/// so this is also the `<hostname>.local` a headless machine is reached at.
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

/// Files for the home of the account a written disk already has, staged
/// under this directory of the handoff tree by name.
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

/// Stage `contents` to land as `name` in the home of the disk's account.
///
/// # Errors
///
/// Returns a string when `name` is not one plain file name or the file
/// cannot be written.
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

/// Add the owner SSH keys to a staged tree. No keys add nothing.
///
/// # Errors
///
/// Returns a string when a line is not one OpenSSH public key, or the file
/// cannot be written.
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

/// The most owner keys one request carries.
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
    // The wire encoding starts with the type name again. A blob whose name
    // differs, or that is not base64 at all, is not this key.
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

/// Place the profile's Wi-Fi connection and, when requested, this machine's
/// person onto `root` as a new device.
pub fn apply_setup_handoff(
    root: &Path,
    profile: &InstallProfile,
    wifi_psk: &str,
    local: &LocalIdentity<'_>,
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
        if profile.user.username.trim().is_empty() {
            return Err("Choose the account name before enrolling this device.".into());
        }
        let rooms = tilo_identity_host::provisioning::personal_rooms_export_path()
            .unwrap_or_else(|| PathBuf::from("/var/lib/tilo/peers/personal-rooms.json"));
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
            ui::time::unix_now_ns(),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
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

/// Copy a staged etc/var tree onto a mounted Tilo root partition.
///
/// Looks for the deployment's `/etc` and the stateroot's `/var` the same way
/// the disk image builder does.
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

/// Escape a value for a NetworkManager keyfile.
///
/// NetworkManager keeps surrounding quotes as part of the SSID and the
/// password, so a quoted `studio` looks for a network named `"studio"` and
/// never associates. GLib only needs a backslash escaped, and a leading
/// space written as `\s`.
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
    // No `permissions` key: an empty value can read as a user restriction
    // that never autoconnects, while a missing key is system-wide. The fixed
    // uuid is the one profile this handoff ever writes. NetworkManager's
    // default retry limit blocks the profile after four failures, which
    // strands a machine whose access point came up late; 0 keeps trying.
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

fn refuse_human_root_secret(staged_var: &Path) -> io::Result<()> {
    let secret = staged_var.join("lib/tilo/user-identity/privkey.bin");
    match fs::symlink_metadata(&secret) {
        Ok(meta) if meta.is_file() => Err(io::Error::other(
            "refusing to copy the human root secret onto the target",
        )),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Copy regular files and directories. Symlinks are skipped on the source
/// side and replaced on the target side, so the copy never writes through a
/// link planted on either disk.
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

/// Verify every destination component before copying, including pre-existing
/// directories; create_dir_all alone follows an image's planted parent link.
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

/// Default locations for this machine's identity material.
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
        tilo_relay_protocol::home_peer_registry_path(
            system
                .user_pubkey
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| Path::new("/var/lib/tilo")),
        )
    });
    Ok(LocalIdentity {
        identity_dir,
        system,
        home_peers,
    })
}

fn dirs_fallback() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
            environments: tilo_relay_protocol::RelayEnvironments::default(),
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
        assert!(connection.contains("\nssid=studio\n"));
        assert!(connection.contains("\npsk=correct-horse\n"));
        assert!(connection.contains("\nuuid="));
        assert!(!connection.contains("permissions="));
        assert_eq!(nm_keyfile_value("my network"), "my network");
        assert_eq!(nm_keyfile_value("a\\b"), "a\\\\b");
        assert_eq!(nm_keyfile_value("  lead"), "\\s\\slead");
        let media = String::from_utf8(profile.to_media_profile_json().unwrap()).unwrap();
        assert!(!media.contains("correct-horse"));
        // The device's name is a preference the media carries; the
        // passphrase never is.
        assert!(media.contains("\"device_label\":\"orange-pi\""));
        let paths = tilo_user_identity::IdentityPaths::under(dir.path().join("var/lib/tilo"));
        assert!(
            tilo_user_identity::joined_device_is_ready(&paths, ui::time::unix_now_ns()).unwrap()
        );
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
            &fs::read(tilo_relay_protocol::home_peer_registry_path(
                &dir.path().join("var/lib/tilo"),
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(copied, vec![peer]);
        let direct: Vec<String> = serde_json::from_slice(
            &fs::read(
                tilo_relay_protocol::home_peer_registry_path(&dir.path().join("var/lib/tilo"))
                    .with_file_name("personal-device-peers.json"),
            )
            .unwrap(),
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

    /// An obviously synthetic key: a well-formed ed25519 encoding of zeros.
    const TEST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test@example";

    #[test]
    fn access_handoff_writes_wifi_and_the_owner_key_without_an_identity() {
        let profile = InstallProfile {
            wifi_ssid: "studio".into(),
            ..InstallProfile::default()
        };
        let stage =
            stage_access_handoff(&profile, "correct-horse", &format!("  {TEST_KEY}\n"), true)
                .unwrap()
                .expect("Wi-Fi and a key stage a tree");
        let root = stage.path();
        assert!(
            root.join("etc/NetworkManager/system-connections/tilo-setup.nmconnection")
                .is_file()
        );
        assert_eq!(
            fs::read_to_string(root.join(BRINGUP_SSH_KEY)).unwrap(),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n"
        );
        assert!(!root.join("var").exists(), "no identity was asked for");
        fs::remove_dir_all(root).unwrap();

        let none = stage_access_handoff(&InstallProfile::default(), "", "", true).unwrap();
        assert!(none.is_none());
        let err = stage_access_handoff(&profile, "", "", true).unwrap_err();
        assert!(err.contains("Wi-Fi password"));
    }

    #[test]
    fn owner_keys_are_public_keys_one_per_line() {
        validate_bringup_ssh_key("").unwrap();
        validate_bringup_ssh_key(TEST_KEY).unwrap();
        let root = tempfile::tempdir().unwrap();
        install_bringup_ssh_key(root.path(), &format!("{TEST_KEY}\n\n  {TEST_KEY} other\n"))
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join(BRINGUP_SSH_KEY))
                .unwrap()
                .lines()
                .count(),
            2
        );
        let too_many = format!("{TEST_KEY}\n").repeat(MAX_BRINGUP_SSH_KEYS + 1);
        assert!(validate_bringup_ssh_key(&too_many).is_err());
        for bad in [
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----", // placeholder
            &format!("{TEST_KEY}\nnot a key"),
            "no-pty ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "ssh-ed25519 not-base64!",
            // An RSA type name over an ed25519 encoding.
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(validate_bringup_ssh_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn access_handoff_names_the_target_only_when_a_hostname_was_chosen() {
        let unnamed = InstallProfile {
            wifi_ssid: "studio".into(),
            ..InstallProfile::default()
        };
        let staged = stage_access_handoff(&unnamed, "correct-horse", "", true)
            .unwrap()
            .unwrap();
        assert!(!staged.path().join("etc/hostname").exists());
        fs::remove_dir_all(staged.path()).unwrap();

        let named = InstallProfile {
            hostname: "tilo-board".into(),
            ..InstallProfile::default()
        };
        let staged = stage_access_handoff(&named, "", "", true).unwrap().unwrap();
        let hostname = staged.path().join("etc/hostname");
        assert_eq!(fs::read_to_string(&hostname).unwrap(), "tilo-board\n");
        // Windows has no mode bits; the staged tree is written to the target
        // as these bytes, so the Unix check covers what Windows stages too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&hostname).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        assert!(
            !staged
                .path()
                .join("etc/NetworkManager/system-connections")
                .exists()
        );
        fs::remove_dir_all(staged.path()).unwrap();

        assert!(
            stage_access_handoff(&named, "", "", false)
                .unwrap()
                .is_none(),
            "installer media name the machine from their profile"
        );

        let invalid = InstallProfile {
            hostname: "bad host".into(),
            ..InstallProfile::default()
        };
        let error = stage_access_handoff(&invalid, "", "", true).unwrap_err();
        assert!(error.contains("Hostname"), "{error}");
    }

    #[test]
    fn account_home_staging_refuses_non_plain_names() {
        let staged = tempfile::tempdir().unwrap();
        for bad in ["", ".bashrc", "../escape", "a/b", "sp ace"] {
            assert!(
                stage_account_home_file(staged.path(), bad, b"x").is_err(),
                "{bad:?}"
            );
        }
        assert!(!staged.path().join(ACCOUNT_HOME_FILES).exists());
    }

    #[cfg(unix)]
    #[test]
    fn handoff_copy_refuses_a_planted_destination_parent() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("NetworkManager/system-connections")).unwrap();
        fs::write(
            source
                .path()
                .join("NetworkManager/system-connections/tilo-setup.nmconnection"),
            b"wifi-secret",
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), target.path().join("NetworkManager")).unwrap();
        let error = copy_tree(source.path(), target.path())
            .expect_err("a planted parent must refuse the copy");
        assert!(
            error.to_string().contains("not a real directory"),
            "{error}"
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_human_root_secret_never_leaves_the_setup_machine() {
        let staged = tempfile::tempdir().unwrap();
        let secret = staged.path().join("var/lib/tilo/user-identity/privkey.bin");
        fs::create_dir_all(secret.parent().unwrap()).unwrap();
        fs::write(&secret, b"human-root-secret").unwrap();

        let mnt = tempfile::tempdir().unwrap();
        let var = mnt.path().join("state/os/fedora/var");
        fs::create_dir_all(mnt.path().join("state/deploy/abcd/etc")).unwrap();
        fs::create_dir_all(&var).unwrap();
        assert!(install_staged_onto_mounted_root(mnt.path(), staged.path()).is_err());
        assert!(!var.join("lib/tilo/user-identity/privkey.bin").exists());

        let device = tempfile::NamedTempFile::new().unwrap();
        let device_len = 1000 + PROFILE_BLOCK_SIZE as u64 + SETUP_HANDOFF_HEADER_SIZE + 8192;
        device.as_file().set_len(device_len).unwrap();
        assert!(embed_setup_handoff(device.path(), 1000, device_len, staged.path()).is_err());
        assert!(
            !fs::read(device.path())
                .unwrap()
                .windows(17)
                .any(|window| window == b"human-root-secret")
        );
    }

    #[cfg(unix)]
    #[test]
    fn handoff_copy_does_not_follow_symlinks_on_either_side() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret");
        fs::write(&secret, b"keep").unwrap();

        let staged = tempfile::tempdir().unwrap();
        fs::create_dir_all(staged.path().join("etc")).unwrap();
        std::os::unix::fs::symlink(&secret, staged.path().join("etc/leak")).unwrap();
        fs::write(staged.path().join("etc/marker"), b"new").unwrap();

        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&secret, target.path().join("marker")).unwrap();
        copy_tree(&staged.path().join("etc"), target.path()).unwrap();

        assert!(!target.path().join("leak").exists());
        assert_eq!(fs::read(&secret).unwrap(), b"keep");
        let marker = target.path().join("marker");
        assert!(
            !fs::symlink_metadata(&marker)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&marker).unwrap(), b"new");
    }
}

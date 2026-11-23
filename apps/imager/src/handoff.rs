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
        enroll_local_identity(root, profile, local, label)?;
    }
    Ok(())
}

/// Make the image one of the person's devices before it ever runs: this
/// machine holds their root, so it certifies the image's new device key on
/// the spot, with no network and nothing for the image to redeem. The key and
/// certificate go where a device that logged in with a ticket keeps them
/// (`var/lib/tilo`), so the image's first boot takes them up the same way,
/// and the image's account names the person as this machine knows them.
#[cfg(unix)]
fn enroll_local_identity(
    root: &Path,
    profile: &InstallProfile,
    local: &LocalIdentity<'_>,
    label: &str,
) -> Result<(), String> {
    use tilo_device_identity::{IdentityPaths, persist};

    let username = profile.user.username.trim();
    if !safe_account_name(username) {
        return Err("The account name must be a lowercase Linux user name.".into());
    }
    if label.len() > 64 {
        return Err("The device name must be at most 64 bytes.".into());
    }
    let person = tilo_identity::store::load_person_root(local.identity_dir)
        .map_err(|error| format!("This machine cannot certify another device: {error}"))?;
    let user = person.group_public_key();
    let source_cert = persist::read_device_cert(&local.system.device_cert)
        .map_err(|error| format!("This machine's device certificate: {error}"))?;
    if source_cert.user_public_key != user {
        return Err("This machine's device certificate belongs to a different person.".into());
    }

    let fresh = |error: io::Error| format!("reserve a fresh target identity tree: {error}");
    let target = fs::symlink_metadata(root).map_err(fresh)?;
    if !target.is_dir() || !tilo_files::native::private::private_to_me(&target) {
        return Err("The handoff directory must be private to this account.".into());
    }
    let var = root.join("var");
    fs::create_dir(&var).map_err(fresh)?;
    let paths = IdentityPaths::under(var.join("lib/tilo"));
    let identity_dir = var
        .join("home")
        .join(username)
        .join(".config/tilo/identity");

    let failed = |error: &dyn std::fmt::Display| format!("Could not enroll the device: {error}");
    let (device, _) = persist::load_or_create_device_key(&paths).map_err(|e| failed(&e))?;
    let cert = signed_at_once(tilo_server_client::certification::issue_device_cert(
        &person,
        device.verifying_key().to_bytes(),
        label,
        tilo_server_client::certification::IssuedRole::Device,
    ))
    .map_err(|e| failed(&e))?;
    let now = ui::time::unix_now_ns();
    persist::install_device_cert(&paths, &cert, now).map_err(|e| failed(&e))?;
    tilo_identity::provisioning::complete_enrolled_device_named(
        &identity_dir,
        &paths,
        now,
        &profile.user.full_name,
        person.avatar_color(),
        person.profile_photo().cloned(),
    )
    .map_err(|e| failed(&e))?;
    // Both machines know each other as the person's devices from the start.
    let identity_id = hex::encode(user);
    tilo_identity::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
        .remember_certificate(&identity_id, source_cert)
        .map_err(|e| failed(&e))?;
    tilo_identity::device_fleet::DeviceFleetStore::at(local.identity_dir, local.system.clone())
        .remember_certificate(&identity_id, cert)
        .map_err(|e| failed(&e))?;
    Ok(())
}

/// See the Unix `enroll_local_identity`: the image's accounts own their
/// files, so only a Unix machine prepares an image its identity enrolls.
#[cfg(not(unix))]
fn enroll_local_identity(
    _root: &Path,
    _profile: &InstallProfile,
    _local: &LocalIdentity<'_>,
    _label: &str,
) -> Result<(), String> {
    Err("This machine cannot enroll the image with its identity.".into())
}

/// The root this machine holds signs as it is asked, so its certificate is
/// ready the first time it is polled.
#[cfg(unix)]
fn signed_at_once<T>(signing: impl std::future::Future<Output = T>) -> T {
    let mut signing = std::pin::pin!(signing);
    match signing
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(signed) => signed,
        std::task::Poll::Pending => unreachable!("a held root signs without waiting"),
    }
}

/// A Linux account name the image's home directory is made under.
#[cfg(unix)]
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

    /// An image this machine writes, with no network and no daemon running
    /// anywhere, boots later as one of the person's devices: its first boot
    /// takes up the key and certificate the way a computer that logged in
    /// with a ticket does, its daemon finds itself certified by the person,
    /// and this machine verifies the image's certificate against their root.
    #[cfg(unix)]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one image, from writing it to both ends of its first boot"
    )]
    fn an_image_written_offline_boots_as_the_persons_device() {
        use tilo_device_identity::{IdentityPaths, login, persist};
        use tilo_identity::{device_fleet::DeviceFleetStore, provisioning, store};

        let local_dir = tempfile::tempdir().unwrap();
        let identity_dir = local_dir.path().join("identity");
        let local_system = IdentityPaths::under(local_dir.path().join("system"));
        provisioning::ensure_collaboration_identity(
            &identity_dir,
            &local_system,
            provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let local_cert = persist::read_device_cert(&local_system.device_cert).unwrap();
        let photo = tilo_identity_client::ProfilePhoto::new(1, 1, vec![15, 80, 210, 255]).unwrap();
        store::update_identity_presentation(
            &identity_dir,
            "primary",
            "Source name",
            [3, 4, 5],
            Some(photo.clone()),
        )
        .unwrap();
        let person = store::load_person_root(&identity_dir)
            .unwrap()
            .group_public_key();
        assert_eq!(local_cert.user_public_key, person);
        let identity_id = hex::encode(person);
        let local = LocalIdentity {
            identity_dir: &identity_dir,
            system: &local_system,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TMPDIR", dir.path());
        let mut profile = InstallProfile {
            wifi_ssid: "studio".into(),
            device_label: "orange-pi".into(),
            ..InstallProfile::default()
        };
        profile.user.full_name = "Ada".into();
        let access = stage_setup_handoff(&profile, "correct-horse", &local)
            .unwrap()
            .unwrap();
        assert!(!access.path().join("var").exists());
        fs::remove_dir_all(access.path()).unwrap();

        // Writing the image.
        profile.enroll_local_identity = true;
        let staged = stage_setup_handoff(&profile, "correct-horse", &local)
            .unwrap()
            .unwrap();
        let written = IdentityPaths::under(staged.path().join("var/lib/tilo"));
        let first_key = fs::read(&written.device_privkey).unwrap();
        let retry = InstallProfile {
            wifi_ssid: String::new(),
            ..profile.clone()
        };
        let error = apply_setup_handoff(staged.path(), &retry, "", &local).unwrap_err();
        assert!(
            error.contains("reserve a fresh target identity tree"),
            "{error}"
        );
        assert_eq!(fs::read(&written.device_privkey).unwrap(), first_key);

        // The image is read from another path once it is on the machine.
        let installed = dir.path().join("installed");
        fs::rename(staged.path(), &installed).unwrap();
        let connection = fs::read_to_string(
            installed.join("etc/NetworkManager/system-connections/tilo-setup.nmconnection"),
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
        assert!(media.contains("\"device_label\":\"orange-pi\""));
        let home = installed.join("var/home");
        assert!(!home.read_dir().unwrap().any(|account| {
            account
                .unwrap()
                .path()
                .join(".config/tilo/identity/secrets")
                .exists()
        }));

        // The image's key and certificate are where a ticket's login keeps them.
        let paths = IdentityPaths::under(installed.join("var/lib/tilo"));
        let cert = persist::read_device_cert(&paths.device_cert).unwrap();
        assert_eq!(
            persist::read_device_cert(&paths.device_cert_for_user(&person)).unwrap(),
            cert
        );
        let device = persist::load_device_key(&paths).unwrap();
        assert_eq!(cert.device_public_key, device.verifying_key().to_bytes());
        assert_ne!(
            cert.device_public_key, local_cert.device_public_key,
            "the image gets its own device key, not the setup machine's"
        );
        assert_eq!(cert.user_public_key, person);
        assert!(cert.capabilities.can_own_workspaces);
        assert!(!cert.capabilities.can_admit_devices);
        assert_eq!(cert.device_label, "orange-pi");

        // First boot, a month later: the session takes up the enrolled
        // device and provisioning finds it ready, as for a ticket's login.
        let boot = cert.issued_at_ns + 30 * 24 * 60 * 60 * 1_000_000_000;
        assert!(tilo_device_identity::joined_device_is_ready(&paths, boot).unwrap());
        let target_identity = home.join(format!(
            "{}/.config/tilo/identity",
            profile.user.username.trim()
        ));
        assert!(provisioning::complete_enrolled_device(&target_identity, &paths, boot).unwrap());
        assert!(matches!(
            provisioning::ensure_collaboration_identity(
                &target_identity,
                &paths,
                provisioning::IdentityStorage::Files,
            )
            .unwrap(),
            provisioning::ProvisionOutcome::Existing
        ));
        let target_record = store::resolve_active_identity(&target_identity).unwrap();
        let presentation = tilo_identity_client::load_metadata_from_path(&target_record).unwrap();
        assert_eq!(presentation.group_public_key, person);
        assert_eq!(presentation.display_name, "Ada");
        assert_eq!(presentation.avatar_color, [3, 4, 5]);
        assert_eq!(presentation.profile_photo, Some(photo));
        let registry = store::IdentityRegistryStore::at(&target_identity)
            .load_reconciled()
            .unwrap();
        assert_eq!(registry.identities[&identity_id].record_path, target_record);
        // Its daemon serves it as the person's device.
        let place = login::Place::at(dir.path().join("device-state"), paths.clone());
        assert_eq!(
            login::certified_by(&place, &cert.device_public_key),
            vec![person]
        );
        assert!(login::claimed(&place, &cert.device_public_key));
        let knows = |fleet: &tilo_identity::device_fleet::DeviceFleet, key: [u8; 32]| {
            fleet
                .certificates
                .iter()
                .any(|known| known.device_public_key == key)
        };
        let target_fleet = DeviceFleetStore::at(&target_identity, paths)
            .load(&identity_id)
            .unwrap();
        assert!(knows(&target_fleet, cert.device_public_key));
        assert!(knows(&target_fleet, local_cert.device_public_key));

        // The setup machine holds the image's certificate, and it verifies
        // against the person's root.
        let local_fleet = DeviceFleetStore::at(&identity_dir, local_system.clone())
            .load(&identity_id)
            .unwrap();
        let held = local_fleet
            .certificates
            .iter()
            .find(|known| known.device_public_key == cert.device_public_key)
            .expect("the setup machine keeps the image's certificate");
        assert_eq!(held.user_public_key, person);
        assert!(held.issuer.is_none(), "the person's root signed it itself");
        tilo_user_identity::verify_device_cert_at(held, boot).unwrap();

        let other_identity = local_dir.path().join("other-identity");
        let other_paths = IdentityPaths::under(local_dir.path().join("other-system"));
        provisioning::ensure_collaboration_identity(
            &other_identity,
            &other_paths,
            provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let mismatched = LocalIdentity {
            identity_dir: &other_identity,
            ..local
        };
        let error = stage_setup_handoff(&profile, "correct-horse", &mismatched).unwrap_err();
        assert!(error.contains("belongs to a different person"), "{error}");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "failed handoff was removed"
        );
    }

    tilo_test_env::scenario! {
        fn a_setup_handoff_stages_what_was_chosen_and_keeps_the_root_secret;

        phase stage_setup_handoff_requires_a_wifi_password_when_ssid_is_set;
        {
            let local_dir = tempfile::tempdir().unwrap();
            let identity_dir = local_dir.path().join("identity");
            let local_system =
                tilo_device_identity::IdentityPaths::under(local_dir.path().join("system"));
            let local = LocalIdentity {
                identity_dir: &identity_dir,
                system: &local_system,
            };
            let profile = InstallProfile {
                wifi_ssid: "studio".into(),
                ..InstallProfile::default()
            };
            let err = stage_setup_handoff(&profile, "", &local).unwrap_err();
            assert!(err.contains("Wi-Fi password"));
        }

        phase staged_tree_installs_onto_a_bootc_layout;
        {
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

        phase setup_handoff_embeds_in_the_slack_before_the_profile_block;
        {
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

        phase the_human_root_secret_never_leaves_the_setup_machine;
        {
            let staged = tempfile::tempdir().unwrap();
            let secret = staged
                .path()
                .join("var/home/person/.config/tilo/identity/secrets")
                .join(format!("{}.key", "ab".repeat(32)));
            fs::create_dir_all(secret.parent().unwrap()).unwrap();
            fs::write(&secret, b"human-root-secret").unwrap();

            let mnt = tempfile::tempdir().unwrap();
            let var = mnt.path().join("state/os/fedora/var");
            fs::create_dir_all(mnt.path().join("state/deploy/abcd/etc")).unwrap();
            fs::create_dir_all(&var).unwrap();
            assert!(install_staged_onto_mounted_root(mnt.path(), staged.path()).is_err());
            assert!(!var.join("home/person").exists());

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
    }

    const TEST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test@example";

    tilo_test_env::scenario! {
        fn an_access_handoff_carries_wifi_and_owner_keys_without_an_identity;

        phase access_handoff_writes_wifi_and_the_owner_key_without_an_identity;
        {
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

        phase owner_keys_are_public_keys_one_per_line;
        {
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
                "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ] {
                assert!(validate_bringup_ssh_key(bad).is_err(), "{bad}");
            }
        }

        phase access_handoff_names_the_target_only_when_a_hostname_was_chosen;
        {
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

        phase account_home_staging_refuses_non_plain_names;
        {
            let staged = tempfile::tempdir().unwrap();
            for bad in ["", ".bashrc", "../escape", "a/b", "sp ace"] {
                assert!(
                    stage_account_home_file(staged.path(), bad, b"x").is_err(),
                    "{bad:?}"
                );
            }
            assert!(!staged.path().join(ACCOUNT_HOME_FILES).exists());
        }
    }

    tilo_test_env::scenario! {
        #[cfg(unix)]
        fn a_handoff_copy_stays_inside_the_tree_it_was_given;

        phase handoff_copy_refuses_a_planted_destination_parent;
        {
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

        phase handoff_copy_does_not_follow_symlinks_on_either_side;
        {
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
}

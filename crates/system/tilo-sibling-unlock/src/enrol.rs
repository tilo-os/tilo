//! Enrolling the sibling factor on a volume (`tilo-disk-encrypt enrol
//! --sibling`), and removing it.
//!
//! 1. Draw the device half, the sibling half and a boot-only key pair.
//! 2. Add a key slot opened by the key the two halves derive, authorised by an
//!    existing secret of the volume.
//! 3. Hand the sibling half and the boot key's public half to the sibling
//!    service on this device ([`Registrar`]). It certifies the boot key with
//!    the person's identity, seals the sibling half to the person's
//!    secret-sync key and replicates it to their other devices, and answers
//!    with the certificate and the devices that hold the half.
//! 4. Store the token: the certificate, those devices, and the device half,
//!    boot key and chosen unlock networks, sealed to the TPM where there is
//!    one and otherwise in the clear.
//!
//! The sibling half is wiped here once it is handed over. A failure after the
//! slot was added removes the slot again, so a half-enrolled volume never
//! remains.
//!
//! Enrolling again with `--replace-networks` keeps the slot, the certificate
//! and the siblings, and only chooses the unlock networks anew: the material
//! is unsealed, its networks replaced, and it is sealed the same way again.
//!
//! What is enrolled is described for Settings under [`STATE_DIR`], without
//! secrets but readable only by root: it names the networks the owner trusts
//! to unlock this machine. Settings reads it through polkit
//! (`tilo-sibling-unlock volumes`).

use crate::luks;
use crate::material::{self, Material};
use crate::seal;
use crate::token::{Sealing, Sibling, TOKEN_TYPE, TOKEN_VERSION, Token};
use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_unlock_protocol::{
    ENROL_SOCKET, EnrolReply, EnrolRequest, EnrolResponse, Half, SlotId, decode_frame,
    derive_slot_key, encode_frame, frame_len,
};

/// Where the enrolment is described for Settings, without secrets.
pub const STATE_DIR: &str = "/var/lib/tilo/sibling-unlock";

/// `tilo-sibling-unlock enrol`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The LUKS2 partition.
    pub device: PathBuf,
    /// A key file holding an existing passphrase of the volume; without one
    /// cryptsetup asks for it (the recovery key will do).
    #[arg(long)]
    pub key_file: Option<PathBuf>,
    /// A `NetworkManager` connection early boot may join to reach your other
    /// devices (repeatable). Wired links need none. Its saved password goes
    /// with it: sealed to the TPM where there is one, otherwise readable by
    /// anyone holding the disk.
    #[arg(long = "network")]
    pub networks: Vec<String>,
    /// Keep the volume's sibling unlock and only choose its unlock networks
    /// again: the `--network` list replaces the earlier one, and none leaves
    /// wired links only. Needs no passphrase and no signed-in session.
    #[arg(long)]
    pub replace_networks: bool,
    /// The signed-in person whose session registers the enrolment with
    /// their devices (its `tilo-session/device-unlock.sock`). Without it the
    /// one session that serves the socket is used.
    #[arg(long)]
    pub session_uid: Option<u32>,
    /// Refuse enrollment unless the device factor can be sealed to the TPM.
    #[arg(long)]
    pub require_tpm: bool,
}

/// Hands an [`EnrolRequest`] to the sibling service.
pub trait Registrar {
    /// Registers the request and returns the certificate and siblings.
    ///
    /// # Errors
    /// When the service refuses or cannot be reached.
    fn register(&self, request: &EnrolRequest) -> Result<EnrolResponse>;
}

/// The sibling service in the person's session, over its unix socket
/// ([`ENROL_SOCKET`] under their runtime directory): one framed
/// `EnrolRequest` in, one framed `EnrolReply` out.
pub struct SessionRegistrar(pub PathBuf);

impl SessionRegistrar {
    /// The socket of `uid`'s session, or of the one session serving one.
    ///
    /// # Errors
    /// When no session, or more than one, serves the socket.
    pub fn find(uid: Option<u32>) -> Result<Self> {
        if let Some(uid) = uid {
            return Ok(Self(
                Path::new("/run/user")
                    .join(uid.to_string())
                    .join(ENROL_SOCKET),
            ));
        }
        let mut found: Vec<PathBuf> = std::fs::read_dir("/run/user")
            .context("no signed-in session")?
            .filter_map(|entry| entry.ok().map(|entry| entry.path().join(ENROL_SOCKET)))
            .filter(|socket| socket.exists())
            .collect();
        match (found.pop(), found.is_empty()) {
            (Some(socket), true) => Ok(Self(socket)),
            (None, _) => bail!("no signed-in session can register the unlock; sign in first"),
            _ => bail!("several sessions are signed in; name one with --session-uid"),
        }
    }
}

impl Registrar for SessionRegistrar {
    fn register(&self, request: &EnrolRequest) -> Result<EnrolResponse> {
        use std::io::{Read as _, Write as _};
        let mut stream = std::os::unix::net::UnixStream::connect(&self.0)
            .with_context(|| format!("could not reach your session at {}", self.0.display()))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(120)))?;
        let frame =
            zeroize::Zeroizing::new(encode_frame(request).map_err(|error| anyhow!("{error}"))?);
        stream
            .write_all(&frame)
            .context("your session stopped reading")?;
        let mut prefix = [0_u8; 4];
        stream
            .read_exact(&mut prefix)
            .context("your session did not answer")?;
        let mut body = vec![0_u8; frame_len(prefix).map_err(|error| anyhow!("{error}"))?];
        stream
            .read_exact(&mut body)
            .context("your session's answer was cut short")?;
        match decode_frame(&body).map_err(|error| anyhow!("{error}"))? {
            EnrolReply::Enrolled(response) => Ok(*response),
            EnrolReply::Refused(reason) => bail!("your devices refused the enrolment: {reason}"),
        }
    }
}

/// What Settings shows about an enrolment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub keyslot: u32,
    /// `tpm2`, or `cleartext`: the device half and the unlock networks'
    /// passwords are readable by anyone holding the disk.
    pub sealing: Sealing,
    pub networks: Vec<String>,
    /// The devices asked at boot, by label.
    pub siblings: Vec<String>,
}

/// Reads a `NetworkManager` connection's keyfile by name, secrets included.
fn network(name: &str) -> Result<material::Network> {
    if !material::valid_network_id(name) {
        bail!("{name:?} is not a usable connection name");
    }
    let listing = crate::exec::run(
        "nmcli",
        ["-t", "-f", "NAME,FILENAME", "connection", "show"],
        None,
    )?;
    let listing = String::from_utf8_lossy(&listing);
    let file = listing
        .lines()
        .find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(':'))
        })
        .ok_or_else(|| anyhow!("no saved connection is named {name:?}"))?
        .replace("\\:", ":");
    let keyfile = std::fs::read_to_string(&file)
        .with_context(|| format!("could not read the saved connection {file}"))?;
    let keyfile = for_early_boot(&keyfile, |interface| {
        std::fs::read_to_string(format!("/sys/class/net/{interface}/address"))
            .ok()
            .map(|mac| mac.trim().to_owned())
    });
    let secret_elsewhere = keyfile.lines().any(|line| {
        let line = line.trim();
        (line.starts_with("psk-flags=") || line.starts_with("password-flags="))
            && !line.ends_with("=0")
    });
    if secret_elsewhere {
        bail!(
            "the password of {name:?} is kept in your session, not with the connection; save it for all users first"
        );
    }
    Ok(material::Network {
        id: name.to_owned(),
        keyfile,
    })
}

/// A saved connection as early boot can use it. A profile limited to one
/// user's session cannot come up before login, so its `permissions=` go.
/// Interface names may differ in the initramfs, so a profile tied to one
/// interface is tied to that interface's MAC address instead.
fn for_early_boot(keyfile: &str, mac_of: impl Fn(&str) -> Option<String>) -> String {
    let mut mac = None;
    let mut kept: Vec<String> = Vec::new();
    for line in keyfile.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("permissions=") {
            continue;
        }
        if let Some(interface) = trimmed.strip_prefix("interface-name=") {
            mac = mac_of(interface.trim());
            continue;
        }
        kept.push(line.to_owned());
    }
    let has_mac = kept
        .iter()
        .any(|line| line.trim_start().starts_with("mac-address="));
    if let (Some(mac), false) = (mac, has_mac) {
        let kind = kept
            .iter()
            .find_map(|line| line.trim().strip_prefix("type="))
            .map_or("ethernet", |kind| match kind {
                "802-11-wireless" => "wifi",
                "802-3-ethernet" => "ethernet",
                other => other,
            })
            .to_owned();
        let header = format!("[{kind}]");
        if let Some(at) = kept.iter().position(|line| line.trim() == header) {
            kept.insert(at.saturating_add(1), format!("mac-address={mac}"));
        } else {
            kept.push(header);
            kept.push(format!("mac-address={mac}"));
        }
    }
    kept.iter().fold(String::new(), |mut out, line| {
        out.push_str(line);
        out.push('\n');
        out
    })
}

/// Enrols the sibling factor.
///
/// # Errors
/// When any step fails; the slot is removed again.
pub fn run(args: &Args) -> Result<()> {
    let sealing = enrollment_sealing(seal::available(), args.require_tpm)?;
    let enrolled = luks::token(&args.device)?.is_some();
    match (enrolled, args.replace_networks) {
        (true, false) => bail!(
            "{} already has a sibling unlock; pass --replace-networks to choose its networks again, or remove it first",
            args.device.display()
        ),
        (false, true) => bail!(
            "{} has no sibling unlock whose networks could be replaced",
            args.device.display()
        ),
        _ => {}
    }
    let networks = args
        .networks
        .iter()
        .map(|name| network(name))
        .collect::<Result<Vec<_>>>()?;
    if args.replace_networks {
        return replace_networks(&args.device, networks);
    }
    let registrar = SessionRegistrar::find(args.session_uid)?;
    enrol_with_sealing(
        &args.device,
        args.key_file.as_deref(),
        networks,
        &registrar,
        sealing,
    )
}

fn enrollment_sealing(available: Sealing, require_tpm: bool) -> Result<Sealing> {
    if require_tpm && available != Sealing::Tpm2 {
        bail!("TPM sealing is required for this enrollment");
    }
    Ok(available)
}

/// The enrolment itself, with the registrar as a seam.
///
/// # Errors
/// As [`run`].
pub fn enrol(
    device: &Path,
    key_file: Option<&Path>,
    networks: Vec<material::Network>,
    registrar: &impl Registrar,
) -> Result<()> {
    enrol_with_sealing(device, key_file, networks, registrar, seal::available())
}

fn enrol_with_sealing(
    device: &Path,
    key_file: Option<&Path>,
    networks: Vec<material::Network>,
    registrar: &impl Registrar,
    sealing: Sealing,
) -> Result<()> {
    let volume_uuid = luks::uuid_bytes(device)?;
    let device_half = Half::generate().map_err(|error| anyhow!("{error}"))?;
    let sibling_half = Half::generate().map_err(|error| anyhow!("{error}"))?;
    let boot_key = SigningKey::from_bytes(
        Half::generate()
            .map_err(|error| anyhow!("{error}"))?
            .expose(),
    );
    let passphrase = derive_slot_key(&device_half, &sibling_half, &volume_uuid);
    let keyslot = luks::add_slot(device, key_file, &passphrase)?;
    drop(passphrase);
    let finish = || -> Result<Token> {
        let slot = SlotId {
            volume_uuid,
            keyslot: u16::try_from(keyslot).context("key slot out of range")?,
        };
        let request = EnrolRequest::new(&boot_key, slot, None, sibling_half)
            .map_err(|error| anyhow!("{error}"))?;
        let response = registrar.register(&request)?;
        drop(request);
        let cert = &response.cert;
        cert.verify()
            .map_err(|error| anyhow!("the issued certificate: {error}"))?;
        if cert.boot_public_key != boot_key.verifying_key().to_bytes() || cert.slot != slot {
            bail!("the issued certificate is for another boot key or slot");
        }
        if response.siblings.is_empty() {
            bail!("none of your other devices holds the sibling half");
        }
        let material = Material::new(&device_half, &boot_key, networks);
        let token = Token {
            kind: TOKEN_TYPE.into(),
            keyslots: vec![keyslot.to_string()],
            tilo_version: TOKEN_VERSION,
            cert: Token::encode_cert(cert)?,
            siblings: response.siblings.iter().map(Sibling::from).collect(),
            sealing,
            material: seal::seal(sealing, &material.to_json()?)?,
        };
        token.validate()?;
        luks::import_token(device, &token)?;
        Ok(token)
    };
    match finish() {
        Ok(token) => {
            write_status(&status_of(&token)?, &volume_uuid);
            Ok(())
        }
        Err(error) => {
            let _ = luks::kill_slot(device, keyslot, key_file);
            Err(error)
        }
    }
}

/// Chooses the unlock networks of an enrolled volume again, leaving its slot,
/// certificate and siblings as they are. The material is sealed the way it
/// was: a volume enrolled in the clear stays in the clear.
///
/// # Errors
/// When the volume has no sibling unlock, its material cannot be unsealed,
/// or cryptsetup refuses the new token.
pub fn replace_networks(device: &Path, networks: Vec<material::Network>) -> Result<()> {
    let mut token = luks::token(device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    let mut material = Material::from_json(&seal::unseal(token.sealing, &token.material)?)?;
    material.networks = networks;
    token.material = seal::seal(token.sealing, &material.to_json()?)?;
    token.validate()?;
    luks::import_token(device, &token)?;
    write_status(&status_of(&token)?, &luks::uuid_bytes(device)?);
    Ok(())
}

fn status_of(token: &Token) -> Result<Status> {
    let material = Material::from_json(&seal::unseal(token.sealing, &token.material)?)?;
    Ok(Status {
        keyslot: token.keyslot()?,
        sealing: token.sealing,
        networks: material
            .networks
            .iter()
            .map(|network| network.id.clone())
            .collect(),
        siblings: token
            .siblings
            .iter()
            .map(|sibling| {
                if sibling.label.is_empty() {
                    sibling.device.clone()
                } else {
                    sibling.label.clone()
                }
            })
            .collect(),
    })
}

fn write_status(status: &Status, volume_uuid: &[u8; 16]) {
    let _ = write_status_in(Path::new(STATE_DIR), status, volume_uuid);
}

/// Writes the description root-only, replacing an earlier one whole.
fn write_status_in(dir: &Path, status: &Status, volume_uuid: &[u8; 16]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile_in(dir)?;
    file.write_all(&serde_json::to_vec_pretty(status)?)?;
    file.as_file().sync_all()?;
    file.persist(dir.join(format!("{}.json", hex::encode(volume_uuid))))?;
    Ok(())
}

/// One volume enrolled on this machine, as `tilo-sibling-unlock volumes`
/// prints it for Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrolled {
    /// The LUKS UUID, as `/dev/disk/by-uuid` names the volume.
    pub volume: String,
    /// Where `enrol --replace-networks` finds it.
    pub device: PathBuf,
    #[serde(flatten)]
    pub status: Status,
}

/// Every enrolment described under [`STATE_DIR`].
///
/// # Errors
/// When the directory exists but cannot be read (it is root's alone).
pub fn enrolled() -> Result<Vec<Enrolled>> {
    enrolled_in(Path::new(STATE_DIR))
}

fn enrolled_in(dir: &Path) -> Result<Vec<Enrolled>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", dir.display()));
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(hex_uuid) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        let Some(volume) = dashed_uuid(hex_uuid) else {
            continue;
        };
        let status: Status = serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("{} is malformed", path.display()))?;
        out.push(Enrolled {
            device: Path::new("/dev/disk/by-uuid").join(&volume),
            volume,
            status,
        });
    }
    out.sort_by(|a, b| a.volume.cmp(&b.volume));
    Ok(out)
}

/// `0123…` (32 hex digits) as `01234567-89ab-cdef-0123-456789abcdef`.
fn dashed_uuid(hex_uuid: &str) -> Option<String> {
    let bytes = hex::decode(hex_uuid)
        .ok()
        .filter(|bytes| bytes.len() == 16)?;
    let part = |range: std::ops::Range<usize>| bytes.get(range).map(hex::encode);
    Some(format!(
        "{}-{}-{}-{}-{}",
        part(0..4)?,
        part(4..6)?,
        part(6..8)?,
        part(8..10)?,
        part(10..16)?
    ))
}

/// What is enrolled on `device`.
///
/// # Errors
/// When the volume has no sibling unlock or its token is unreadable.
pub fn status(device: &Path) -> Result<Status> {
    let token = luks::token(device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    status_of(&token)
}

/// Removes the sibling slot and its token.
///
/// # Errors
/// When cryptsetup refuses.
pub fn remove(device: &Path, key_file: Option<&Path>) -> Result<()> {
    let token = luks::token(device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    luks::remove_token(device)?;
    luks::kill_slot(device, token.keyslot()?, key_file)?;
    let _ = std::fs::remove_file(
        Path::new(STATE_DIR).join(format!("{}.json", hex::encode(luks::uuid_bytes(device)?))),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_is_root_only_and_lists_back() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sibling-unlock");
        let status = Status {
            keyslot: 2,
            sealing: Sealing::Cleartext,
            networks: vec!["home".into()],
            siblings: vec!["laptop".into()],
        };
        let uuid = [0xab_u8; 16];
        write_status_in(&dir, &status, &uuid).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        let file = dir.join(format!("{}.json", hex::encode(uuid)));
        assert_eq!(mode(&file), 0o600);

        let replaced = Status {
            networks: Vec::new(),
            ..status
        };
        write_status_in(&dir, &replaced, &uuid).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let listed = enrolled_in(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].volume, "abababab-abab-abab-abab-abababababab");
        assert_eq!(
            listed[0].device,
            Path::new("/dev/disk/by-uuid/abababab-abab-abab-abab-abababababab")
        );
        assert_eq!(listed[0].status, replaced);

        let json = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(json["sealing"], "cleartext");
        assert_eq!(json["networks"], serde_json::json!([]));
        assert!(enrolled_in(&root.path().join("absent")).unwrap().is_empty());
    }

    #[test]
    fn enrollment_pins_required_tpm_before_any_slot_is_added() {
        assert!(enrollment_sealing(Sealing::Cleartext, true).is_err());
        assert_eq!(
            enrollment_sealing(Sealing::Tpm2, true).unwrap(),
            Sealing::Tpm2
        );
        assert_eq!(
            enrollment_sealing(Sealing::Cleartext, false).unwrap(),
            Sealing::Cleartext
        );
    }

    #[test]
    fn a_saved_connection_is_tied_to_its_mac_for_early_boot() {
        let saved = "[connection]\nid=lan\ntype=ethernet\ninterface-name=enp0s4\npermissions=user:tester;\n\n[ethernet]\n\n[ipv4]\naddress1=192.168.231.11/24\nmethod=manual\n";
        let adapted = for_early_boot(saved, |interface| {
            (interface == "enp0s4").then(|| "52:54:00:23:83:0b".to_owned())
        });
        assert!(
            !adapted.contains("interface-name") && !adapted.contains("permissions"),
            "{adapted}"
        );
        assert!(
            adapted.contains("[ethernet]\nmac-address=52:54:00:23:83:0b\n"),
            "{adapted}"
        );
        assert!(adapted.contains("address1=192.168.231.11/24"));

        let wifi =
            "[connection]\nid=home\ntype=wifi\ninterface-name=wlan0\n[wifi-security]\npsk=x\n";
        let adapted = for_early_boot(wifi, |_| Some("aa:bb:cc:dd:ee:ff".to_owned()));
        assert!(
            adapted.ends_with("[wifi]\nmac-address=aa:bb:cc:dd:ee:ff\n"),
            "{adapted}"
        );
    }
}

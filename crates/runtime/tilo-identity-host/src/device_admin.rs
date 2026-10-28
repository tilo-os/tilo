//! Pairing and administering the person's devices, whichever door asks: the
//! shell's `identity.device.pair.*` and `identity.*` actions, or the
//! `tilo-identity` command on a headless device. Each door decides who may
//! take a step; what the step runs, and with which arguments, is decided
//! here once.
//!
//! Pairing runs `tilo-enroll`: `discover[-device]` listens on the LAN for an
//! unprovisioned device announcing itself, and the privileged helper
//! (`tilo-home-peer-control approve-…`) admits the device discovery found.
//! The helper is started through polkit at a desktop, or sudo over SSH.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tilo_actions::remote::identity_admin::FleetDevice;
use tilo_user_identity::{IdentityPaths, RevocationReason};

use crate::device_fleet::DeviceFleetStore;

/// The enrollment program the image installs.
pub const ENROLL: &str = "/usr/libexec/tilo/tilo-enroll";
/// The privileged helper that admits a discovered device.
pub const CONTROL_HELPER: &str = "/usr/libexec/tilo-home-peer-control";

/// The kind of device a pairing admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairRole {
    /// Another of the person's own devices.
    PersonalDevice,
    /// An always-on home server that keeps the person's workspaces.
    HomePeer,
}

impl PairRole {
    /// The role `text` names: `personal-device` or `home-peer`.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "personal-device" => Ok(Self::PersonalDevice),
            "home-peer" => Ok(Self::HomePeer),
            _ => Err("role must be personal-device or home-peer".into()),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalDevice => "personal-device",
            Self::HomePeer => "home-peer",
        }
    }

    /// The `tilo-enroll` command that listens for a device of this role.
    #[must_use]
    pub const fn discover_command(self) -> &'static str {
        match self {
            Self::PersonalDevice => "discover-device",
            Self::HomePeer => "discover",
        }
    }

    const fn approve_command(self) -> &'static str {
        match self {
            Self::PersonalDevice => "approve-device",
            Self::HomePeer => "approve-enrollment",
        }
    }
}

/// A device found nearby, as `tilo-enroll discover` reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Candidate {
    pub device_node_id: String,
    /// The relay service node id it announced, lowercase hex.
    pub relay_service_node_id: String,
    pub label: String,
    /// The code both screens show.
    pub verification_code: String,
}

impl Candidate {
    #[must_use]
    pub fn answer(&self, role: PairRole) -> serde_json::Value {
        serde_json::json!({
            "role": role.as_str(),
            "device": self.device_node_id,
            "service": self.relay_service_node_id,
            "label": self.label,
            "verification_code": self.verification_code,
        })
    }
}

/// The device a finished `tilo-enroll discover[-device]` reports.
pub fn discovered(success: bool, stdout: &[u8], stderr: &[u8]) -> Result<Candidate, String> {
    if !success {
        return Err(command_error(stderr, "no new device was found nearby"));
    }
    let candidate: Candidate = serde_json::from_slice(stdout)
        .map_err(|_| "nearby discovery returned an invalid identity".to_string())?;
    Ok(Candidate {
        device_node_id: node_id(&candidate.device_node_id, "the discovered device")?,
        relay_service_node_id: node_id(
            &candidate.relay_service_node_id,
            "the discovered relay service",
        )?,
        ..candidate
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub role: PairRole,
    pub device: String,
    /// The relay service node id it announced, in lowercase hex.
    pub service: String,
    /// For a home peer, the workspaces it keeps; `None` takes the relay
    /// configuration's.
    pub workspace_ids: Option<Vec<u64>>,
}

/// Where the identity an approved device joins lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonPaths {
    /// The device root (`/var/lib/tilo` on an installed system).
    pub root: PathBuf,
    /// The person's identity directory.
    pub dir: PathBuf,
}

impl PersonPaths {
    /// The identity this account runs as: `$TILO_IDENTITY_ROOT`, else the
    /// system device root when it exists, else the person's data directory;
    /// and the person's identity directory.
    ///
    /// # Errors
    ///
    /// Why the person's identity directory cannot be found.
    pub fn from_env() -> Result<Self, String> {
        let root = std::env::var_os("TILO_IDENTITY_ROOT").map_or_else(
            || {
                if Path::new("/var/lib/tilo/device").is_dir() {
                    PathBuf::from("/var/lib/tilo")
                } else {
                    tilo_paths::data_dir()
                        .unwrap_or_else(|| PathBuf::from(".local/share"))
                        .join("tilo")
                }
            },
            PathBuf::from,
        );
        let dir = runtime_identity::identity_dir_from_env()
            .map_err(|error| format!("could not find your identity: {error}"))?;
        Ok(Self { root, dir })
    }
}

/// The helper and its arguments that admit `approval`'s device into the
/// identity at `paths`, for the launcher (polkit or sudo) to run.
#[must_use]
pub fn approval_args(
    approval: &Approval,
    paths: &PersonPaths,
    workspace_ids: &[u64],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = [CONTROL_HELPER, approval.role.approve_command()]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push("--identity-root".into());
    args.push(paths.root.clone().into_os_string());
    args.push("--identity-dir".into());
    args.push(paths.dir.clone().into_os_string());
    args.push("--expected-device".into());
    args.push(approval.device.clone().into());
    args.push("--expected-service".into());
    args.push(approval.service.clone().into());
    if approval.role == PairRole::HomePeer {
        for id in workspace_ids {
            args.push("--workspace-id".into());
            args.push(id.to_string().into());
        }
    }
    args
}

/// What of the identity's key this device holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyHolding {
    /// All of it: this device certifies the devices it pairs itself.
    Whole,
    /// None, or one share: the devices it pairs are certified by the
    /// person's devices that hold the key ([`crate::certification_ceremony`]),
    /// `threshold` of the key's `total` devices together.
    Part {
        person: [u8; 32],
        threshold: u16,
        total: u16,
    },
}

/// What of the identity at `paths` this device holds, without opening its
/// secret.
///
/// # Errors
///
/// When this account has no identity, or its record cannot be read.
pub fn key_holding(paths: &PersonPaths) -> Result<KeyHolding, String> {
    if IdentityPaths::under(&paths.root).user_privkey.exists() {
        return Ok(KeyHolding::Whole);
    }
    let record = runtime_identity::resolve_active_identity(&paths.dir)
        .ok_or("this account has no identity yet")?;
    let metadata = runtime_identity::load_metadata_from_path(&record)
        .map_err(|error| format!("could not read your identity: {error}"))?;
    Ok(if metadata.single_device {
        KeyHolding::Whole
    } else {
        KeyHolding::Part {
            person: metadata.group_public_key,
            threshold: metadata.threshold,
            total: metadata.total_shares,
        }
    })
}

/// The certificate to propose for `candidate`, a personal device of
/// `person`, valid from about `now_ns`; and the relay service it announced.
///
/// # Errors
///
/// When the candidate's keys are malformed.
pub fn proposed_certificate(
    person: [u8; 32],
    candidate: &Candidate,
    now_ns: u64,
) -> Result<(tilo_user_identity::DeviceCert, [u8; 32]), String> {
    let key = |text: &str| -> Result<[u8; 32], String> {
        hex::decode(text)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| "the discovered device has a malformed key".to_string())
    };
    Ok((
        tilo_user_identity::DeviceCert {
            user_public_key: person,
            device_public_key: key(&candidate.device_node_id)?,
            device_label: candidate.label.clone(),
            capabilities: crate::enrollment::EnrollmentRole::PersonalDevice.preset(),
            // As a certificate issued here: a minute back, for a new device
            // whose clock trails.
            issued_at_ns: now_ns.saturating_sub(60 * 1_000_000_000),
            expires_at_ns: None,
            signature: [0; 64],
        },
        key(&candidate.relay_service_node_id)?,
    ))
}

/// `tilo-enroll`'s arguments that pair `approval`'s personal device with the
/// certificate at `approved_cert`, which the person's key holders signed.
/// No administrator is involved: this device signs nothing as the identity.
#[must_use]
pub fn approved_args(
    approval: &Approval,
    paths: &PersonPaths,
    approved_cert: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![PairRole::PersonalDevice.approve_command().into()];
    for (flag, value) in [
        ("--identity-root", paths.root.as_os_str()),
        ("--identity-dir", paths.dir.as_os_str()),
        ("--expected-device", std::ffi::OsStr::new(&approval.device)),
        (
            "--expected-service",
            std::ffi::OsStr::new(&approval.service),
        ),
        ("--approved-cert", approved_cert.as_os_str()),
    ] {
        args.push(flag.into());
        args.push(value.to_owned());
    }
    args
}

/// A device key or relay service node id: 32 bytes as hex, lowercased.
///
/// # Errors
///
/// When `text` is not 64 hexadecimal characters.
pub fn node_id(text: &str, what: &str) -> Result<String, String> {
    let text = text.trim();
    if text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(format!("{what} must be 64 hexadecimal characters"))
    }
}

/// A child's diagnostic, safe to show a person, or `fallback` when it said
/// nothing.
#[must_use]
pub fn command_error(stderr: &[u8], fallback: &str) -> String {
    let detail = String::from_utf8_lossy(stderr);
    let detail: String = detail
        .trim()
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(512)
        .collect();
    if detail.is_empty() {
        fallback.to_owned()
    } else {
        detail
    }
}

/// The devices trusted under the identity `identity_id` (whose root key is
/// `user_public_key`), the one `device` holds a certificate for marked
/// current.
///
/// # Errors
///
/// Why the fleet cannot be read, or that it holds another identity's
/// records.
pub fn trusted_devices(
    fleets: &DeviceFleetStore,
    device: &IdentityPaths,
    identity_id: &str,
    user_public_key: [u8; 32],
) -> Result<Vec<FleetDevice>, String> {
    let fleet = fleets
        .load(identity_id)
        .map_err(|error| error.to_string())?;
    if fleet
        .certificates
        .iter()
        .any(|cert| cert.user_public_key != user_public_key)
        || fleet
            .revocations
            .iter()
            .any(|entry| entry.user_public_key != user_public_key)
    {
        return Err("the device fleet holds another identity's records".into());
    }
    let current = tilo_user_identity::persist::read_device_cert(
        &device.device_cert_for_user(&user_public_key),
    )
    .ok()
    .map(|cert| cert.device_public_key);
    Ok(fleet
        .certificates
        .iter()
        .map(|cert| FleetDevice {
            label: cert.device_label.clone(),
            key_hex: hex::encode(cert.device_public_key),
            fingerprint: fingerprint(&cert.device_public_key),
            current: current == Some(cert.device_public_key),
            revoked: fleet.is_revoked(&cert.device_public_key),
            can_host: cert.capabilities.can_act_as_host,
        })
        .collect())
}

/// A key's short form people compare between screens: its first six bytes.
fn fingerprint(key: &[u8; 32]) -> String {
    key.iter()
        .take(6)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The revocation reason `text` names.
pub fn revocation_reason(text: &str) -> Result<RevocationReason, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "lost" => Ok(RevocationReason::Lost),
        "stolen" => Ok(RevocationReason::Stolen),
        "retired" => Ok(RevocationReason::Retired),
        "compromised" => Ok(RevocationReason::Compromised),
        _ => Err("reason must be lost, stolen, retired, or compromised".into()),
    }
}

/// Root-sign the permanent revocation of `device` (64 hex characters) under
/// the identity `identity_id`, for `reason`.
///
/// # Errors
///
/// Why the device was not revoked.
pub fn revoke(
    fleets: &DeviceFleetStore,
    identity_id: &str,
    device: &str,
    reason: &str,
) -> Result<String, String> {
    let device = node_id(device, "device")?;
    let mut key = [0; 32];
    hex::decode_to_slice(&device, &mut key).map_err(|error| error.to_string())?;
    let reason = revocation_reason(reason)?;
    fleets
        .revoke(identity_id, key, reason, tilo_clock::unix_now_ns())
        .map_err(|error| error.to_string())?;
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const SERVICE: &str = "bb00000000000000000000000000000000000000000000000000000000000002";

    #[test]
    fn keys_roles_and_discoveries_are_checked_before_anything_runs() {
        assert_eq!(PairRole::parse("home-peer"), Ok(PairRole::HomePeer));
        assert_eq!(
            PairRole::parse(" personal-device "),
            Ok(PairRole::PersonalDevice)
        );
        assert!(PairRole::parse("server").is_err());
        assert_eq!(
            node_id(&DEVICE.to_ascii_uppercase(), "device"),
            Ok(DEVICE.into())
        );
        assert!(node_id("abc", "device").is_err());
        assert!(node_id(&"g".repeat(64), "device").is_err());
        assert_eq!(
            discovered(false, b"", b"\x1b[31mno matching device\n"),
            Err("[31mno matching device".into())
        );
        assert!(discovered(true, b"{\"device_node_id\":\"x\"}", b"").is_err());
        let found = format!(
            "{{\"device_node_id\":\"{}\",\"relay_service_node_id\":\"{SERVICE}\",\
             \"label\":\"Phone\",\"verification_code\":\"123-456\",\"kind\":\"PersonalDevice\"}}\n",
            DEVICE.to_ascii_uppercase()
        );
        let candidate = discovered(true, found.as_bytes(), b"").unwrap();
        assert_eq!(candidate.device_node_id, DEVICE);
        assert_eq!(candidate.verification_code, "123-456");
        assert_eq!(revocation_reason(" Lost "), Ok(RevocationReason::Lost));
        assert!(revocation_reason("misplaced").is_err());
    }

    #[test]
    fn every_door_runs_the_helper_with_the_same_arguments() {
        let paths = PersonPaths {
            root: PathBuf::from("/var/lib/tilo"),
            dir: PathBuf::from("/home/person/.config/tilo/identity"),
        };
        let approval = Approval {
            role: PairRole::HomePeer,
            device: DEVICE.into(),
            service: SERVICE.into(),
            workspace_ids: None,
        };
        assert_eq!(
            approval_args(&approval, &paths, &[7, 9]),
            [
                CONTROL_HELPER,
                "approve-enrollment",
                "--identity-root",
                "/var/lib/tilo",
                "--identity-dir",
                "/home/person/.config/tilo/identity",
                "--expected-device",
                DEVICE,
                "--expected-service",
                SERVICE,
                "--workspace-id",
                "7",
                "--workspace-id",
                "9",
            ]
            .map(OsString::from)
            .to_vec()
        );
        let personal = approval_args(
            &Approval {
                role: PairRole::PersonalDevice,
                ..approval
            },
            &paths,
            &[7],
        );
        assert_eq!(personal.get(1), Some(&OsString::from("approve-device")));
        assert!(
            !personal.contains(&OsString::from("--workspace-id")),
            "a personal device keeps no workspaces of its own"
        );
    }
}

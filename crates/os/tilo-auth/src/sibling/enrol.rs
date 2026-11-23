//! Enrolling the sibling factor on a volume (`tilo-disk-encrypt enrol
//! --sibling`), and removing it.
//!
//! 1. Draw the device half, the sibling half and a boot-only key pair.
//! 2. Add a key slot opened by the key the two halves derive, authorised by an
//!    existing secret of the volume.
//! 3. Hand the sibling half and the boot key's public half to a device of the
//!    person that holds their root ([`Registrar`]): the session on this
//!    machine, or, on a machine that holds none of it, one of their devices
//!    over the peer network ([`PeerRegistrar`]), which asks them first. It
//!    certifies the boot key with the person's identity, seals the sibling
//!    half to the person's secret-sync key and replicates it to their other
//!    devices, and answers with the certificate and the devices that hold the
//!    half.
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
//! (`tilo-auth sibling volumes`).

use super::material::{self, Material};
use super::token::{Sibling, TOKEN_TYPE, TOKEN_VERSION, Token};
use crate::luks::{self, Authority};
use crate::tpm::{self, Sealing};
use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use iroh::{RelayMap, RelayMode};
use iroh_base::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_device_identity::IdentityPaths;
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_rollback::exec::Runner;
use tilo_rollback::policy::Workspace;
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, ENROL_ALPN, ENROL_SOCKET, EnrolReply, EnrolRequest, EnrolResponse, Half,
    SlotId, decode_frame, derive_slot_key, encode_frame, frame_len,
};
use tilo_user_identity::{DeviceCapabilities, verify_device_cert};

/// Where the enrolment is described for Settings, without secrets.
pub const STATE_DIR: &str = "/var/lib/tilo/sibling-unlock";

/// `tilo-auth sibling enrol`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The LUKS2 partition.
    pub device: PathBuf,
    /// A key file holding an existing passphrase of the volume; without one
    /// it is asked for on the terminal (the recovery key will do).
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
    /// The person whose session registers the enrolment with their devices
    /// (its `tilo-session/device-unlock.sock`), and whose identity directory
    /// names the devices this one asks when it holds no root of theirs.
    /// Without it the one session that serves the socket is used, and this
    /// process's own account when none does.
    #[arg(long)]
    pub session_uid: Option<u32>,
    /// Refuse enrollment unless the device factor can be sealed to the TPM.
    #[arg(long)]
    pub require_tpm: bool,
    /// Accept this machine's current boot chain, so the owner's other devices
    /// release its sibling half without asking them: an unattended unlock,
    /// for a server that reboots with nobody there. Needs a TPM, and covers
    /// the chain measured now, so it is chosen again after an OS update that
    /// changes PCR 4.
    #[arg(long)]
    pub unattended: bool,
    /// The packet relay that reaches the person's devices when this machine
    /// has no session of theirs to ask: a deployment on its own relay names
    /// it here, where the public one would not reach them. Without it the
    /// relay this machine's `/etc/tilo/server.toml` names is used, and
    /// without that the peer network's own.
    #[arg(long)]
    pub relay: Option<String>,
}

/// Hands an [`EnrolRequest`] to the sibling service.
pub trait Registrar {
    /// Registers the request and returns the certificate and siblings.
    fn register(&self, request: &EnrolRequest) -> Result<EnrolResponse, NotEnrolled>;
}

/// Why a registrar did not enrol this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotEnrolled {
    /// This device does not hold the person's root: another of their devices
    /// must sign the enrolment.
    NotKeyHolder,
    /// The person refused the enrolment, or did not answer in time.
    Denied,
    /// Not enrolled, with a reason for the person.
    Failed(String),
}

impl std::fmt::Display for NotEnrolled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotKeyHolder => formatter.write_str(
                "this device does not hold the person's root, and none of their other devices \
                 could enrol it",
            ),
            Self::Denied => formatter.write_str("your device refused the enrolment"),
            Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for NotEnrolled {}

impl From<EnrolReply> for NotEnrolled {
    fn from(reply: EnrolReply) -> Self {
        match reply {
            EnrolReply::Enrolled(response) => Self::Failed(format!(
                "the enrolment answered with a certificate for boot key {} instead of replying",
                hex::encode(response.cert.device_public_key)
            )),
            EnrolReply::NotKeyHolder => Self::NotKeyHolder,
            EnrolReply::Denied => Self::Denied,
            EnrolReply::Refused(reason) => Self::Failed(reason),
        }
    }
}

/// The sibling service in the person's session, over its unix socket
/// ([`ENROL_SOCKET`] under their runtime directory): one framed
/// `EnrolRequest` in, one framed `EnrolReply` out.
pub struct SessionRegistrar(pub PathBuf);

impl SessionRegistrar {
    /// The socket of `uid`'s session, or of the one session serving one.
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
    fn register(&self, request: &EnrolRequest) -> Result<EnrolResponse, NotEnrolled> {
        use std::io::{Read as _, Write as _};
        // No session is running here to ask: another of the person's devices
        // is, over the peer network.
        if !self.0.exists() {
            return Err(NotEnrolled::NotKeyHolder);
        }
        let mut stream = tilo_files::native::socket::connect(&self.0)
            .with_context(|| format!("could not reach your session at {}", self.0.display()))
            .map_err(|error| NotEnrolled::Failed(format!("{error:#}")))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(120)))
            .map_err(|error| NotEnrolled::Failed(error.to_string()))?;
        let frame = zeroize::Zeroizing::new(
            encode_frame(request).map_err(|error| NotEnrolled::Failed(error.to_string()))?,
        );
        stream
            .write_all(&frame)
            .context("your session stopped reading")
            .map_err(|error| NotEnrolled::Failed(format!("{error:#}")))?;
        let mut prefix = [0_u8; 4];
        stream
            .read_exact(&mut prefix)
            .context("your session did not answer")
            .map_err(|error| NotEnrolled::Failed(format!("{error:#}")))?;
        let mut body =
            vec![0_u8; frame_len(prefix).map_err(|error| NotEnrolled::Failed(error.to_string()))?];
        stream
            .read_exact(&mut body)
            .context("your session's answer was cut short")
            .map_err(|error| NotEnrolled::Failed(format!("{error:#}")))?;
        let reply = decode_frame(&body).map_err(|error| NotEnrolled::Failed(error.to_string()))?;
        match reply {
            EnrolReply::Enrolled(response) => Ok(*response),
            reply => Err(reply.into()),
        }
    }
}

/// Asks the session on this device first, and the person's other devices over
/// the peer network when it holds no root: one enrolment flow, whichever side
/// can sign it.
struct Choosing(Vec<Box<dyn Registrar>>);

impl Registrar for Choosing {
    fn register(&self, request: &EnrolRequest) -> Result<EnrolResponse, NotEnrolled> {
        let mut unable = None;
        for registrar in &self.0 {
            match registrar.register(request) {
                Ok(response) => return Ok(response),
                // This device cannot sign it; another might.
                Err(NotEnrolled::NotKeyHolder) => unable = Some(NotEnrolled::NotKeyHolder),
                // The person answered, or something went wrong: asking again elsewhere would ask
                // them the same question twice or hide the reason.
                Err(error) => return Err(error),
            }
        }
        Err(unable.unwrap_or_else(|| {
            NotEnrolled::Failed("no device of yours could enrol the unlock".into())
        }))
    }
}

/// The person's devices, asked over the peer network: for a machine that holds
/// no root of its own, such as a desktop that joined the identity or a server
/// with no session.
///
/// Each is dialled at [`ENROL_ALPN`] as this device's own key; the one that
/// holds the root asks its person, then certifies the boot key. A device that
/// does not hold the root says so and the next is asked.
pub struct PeerRegistrar {
    /// This device's fleet key: the endpoint dials as it, and the certificate
    /// it asks for names it.
    device: SigningKey,
    /// The person's other personal devices, in the fleet's order.
    devices: Vec<[u8; 32]>,
    /// The relays that reach them, when the caller knows them; `None` uses the
    /// peer network's default.
    relays: Option<RelayMap>,
}

impl PeerRegistrar {
    /// This device as a peer-network client, asking `devices` in turn.
    #[must_use]
    pub fn new(device: SigningKey, devices: Vec<[u8; 32]>, relays: Option<RelayMap>) -> Self {
        Self {
            device,
            devices,
            relays,
        }
    }

    /// This device's own key and the person's other devices, read from the
    /// identity this machine was bound with and the fleet `session_uid`'s
    /// identity directory holds. `relays` reaches them when the caller knows
    /// the network; `None` uses the peer network's default.
    pub fn find(session_uid: Option<u32>, relays: Option<RelayMap>) -> Result<Self> {
        let paths = identity_paths();
        let device = tilo_device_identity::persist::load_device_key(&paths)
            .map_err(|error| anyhow!("this device's key: {error}"))?;
        let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
            .map_err(|error| anyhow!("this device's certificate: {error}"))?;
        if cert.device_public_key != device.verifying_key().to_bytes() {
            bail!("this device's key is not the one its certificate names");
        }
        let identity_id = hex::encode(cert.user_public_key);
        // The person's devices as this machine's own store lists them: on a
        // personal device their account's identity directory holds the fleet,
        // and on a server the identity root its enrollment wrote it into. No
        // session has to be running for this machine to know who to ask.
        let store = identity_dir(session_uid)
            .ok()
            .map(|directory| DeviceFleetStore::at(directory, paths.clone()))
            .filter(|store| store.fleet_file(&identity_id).exists())
            .unwrap_or_else(|| DeviceFleetStore::of_device(paths, &identity_id));
        let fleet = store
            .load(&identity_id)
            .map_err(|error| anyhow!("your devices: {error}"))?;
        let devices: Vec<[u8; 32]> = fleet
            .personal_devices(&cert.user_public_key, tilo_clock::unix_now_ns())
            .into_iter()
            .filter(|key| key != &cert.device_public_key)
            .collect();
        if devices.is_empty() {
            bail!("none of your other devices can enrol this one");
        }
        Ok(Self::new(device, devices, relays))
    }

    /// Dial, ask, and read the answer, on this device's endpoint.
    async fn ask(&self, request: &EnrolRequest) -> Result<EnrolResponse, NotEnrolled> {
        let frame = encode_frame(request)
            .map_err(|error| NotEnrolled::Failed(format!("the request: {error}")))?;
        let endpoint = self
            .endpoint()
            .await
            .map_err(|error| NotEnrolled::Failed(format!("no peer-network endpoint: {error:#}")))?;
        let mut unable = None;
        for key in &self.devices {
            match self.ask_one(&endpoint, key, &frame).await {
                Ok(response) => {
                    endpoint.close().await;
                    return Ok(response);
                }
                Err(NotEnrolled::NotKeyHolder) => unable = Some(NotEnrolled::NotKeyHolder),
                Err(error) => unable = Some(error),
            }
        }
        endpoint.close().await;
        Err(unable.unwrap_or(NotEnrolled::NotKeyHolder))
    }

    async fn ask_one(
        &self,
        endpoint: &Endpoint,
        key: &[u8; 32],
        frame: &[u8],
    ) -> Result<EnrolResponse, NotEnrolled> {
        let addr = self.address(key)?;
        let connection = tokio::time::timeout(DIAL_FOR, endpoint.connect(addr, ENROL_ALPN))
            .await
            .map_err(|_| NotEnrolled::Failed("no answer".into()))?
            .map_err(|error| NotEnrolled::Failed(error.to_string()))?;
        let asked = async {
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|error| NotEnrolled::Failed(error.to_string()))?;
            send.write_all(frame)
                .await
                .map_err(|error| NotEnrolled::Failed(error.to_string()))?;
            let mut prefix = [0_u8; 4];
            recv.read_exact(&mut prefix)
                .await
                .map_err(|error| NotEnrolled::Failed(error.to_string()))?;
            let mut body = vec![
                0_u8;
                frame_len(prefix)
                    .map_err(|error| NotEnrolled::Failed(error.to_string()))?
            ];
            recv.read_exact(&mut body)
                .await
                .map_err(|error| NotEnrolled::Failed(error.to_string()))?;
            decode_frame::<EnrolReply>(&body)
                .map_err(|error| NotEnrolled::Failed(error.to_string()))
        };
        // The person is asked on their device, and may take their time: the
        // sibling side's own answering window bounds it.
        let reply = tokio::time::timeout(DECIDE_FOR, asked)
            .await
            .map_err(|_| NotEnrolled::Failed("the device took too long to answer".into()))??;
        connection.close(0u32.into(), b"done");
        match reply {
            EnrolReply::Enrolled(response) => Ok(*response),
            reply => Err(reply.into()),
        }
    }

    async fn endpoint(&self) -> Result<Endpoint> {
        let builder = match &self.relays {
            Some(relays) => {
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Custom(relays.clone()))
            }
            // The peer network's own relays and names, as early boot reaches a
            // sibling it was not given an address for.
            None => Endpoint::builder(presets::N0),
        };
        builder
            .secret_key(iroh::SecretKey::from_bytes(&self.device.to_bytes()))
            .bind_addr((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .context("this machine's endpoint address")?
            .bind()
            .await
            .context("bind this machine's endpoint")
    }

    fn address(&self, key: &[u8; 32]) -> Result<EndpointAddr, NotEnrolled> {
        let id = EndpointId::from_bytes(key)
            .map_err(|error| NotEnrolled::Failed(format!("a device key: {error}")))?;
        let mut addr = EndpointAddr::new(id);
        if let Some(url) = self
            .relays
            .as_ref()
            .and_then(|relays| relays.urls::<Vec<_>>().into_iter().next())
        {
            addr = addr.with_relay_url(url);
        }
        Ok(addr)
    }
}

impl Registrar for PeerRegistrar {
    fn register(&self, request: &EnrolRequest) -> Result<EnrolResponse, NotEnrolled> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| NotEnrolled::Failed(format!("no async runtime: {error}")))?;
        runtime.block_on(self.ask(request))
    }
}

/// How long one device is given to accept the connection.
const DIAL_FOR: std::time::Duration = std::time::Duration::from_secs(20);
/// How long a device may take to put the request to its person.
const DECIDE_FOR: std::time::Duration =
    std::time::Duration::from_secs(APPROVAL_TIMEOUT_NS / 1_000_000_000 + 60);

/// This device's identity paths, with the test and development root override
/// the session's own reads ([`tilo_identity::provisioning::collaboration_identity_paths`]).
fn identity_paths() -> IdentityPaths {
    std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|value| !value.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under)
}

/// The identity directory whose device fleet names the person's devices: the
/// named account's, or this process's own.
fn identity_dir(session_uid: Option<u32>) -> Result<PathBuf> {
    if let Some(uid) = session_uid {
        return Ok(home_of(uid)?.join(tilo_identity::local_person::IDENTITY_DIR_IN_HOME));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(xdg).join("tilo/identity"));
    }
    let home = std::env::var_os("HOME").context("this account has no home directory")?;
    Ok(PathBuf::from(home).join(tilo_identity::local_person::IDENTITY_DIR_IN_HOME))
}

/// The home directory of account `uid`.
fn home_of(uid: u32) -> Result<PathBuf> {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // The largest home directory Linux accepts; getpwuid_r reports what it needs.
    let mut buffer = vec![0_u8; 4096];
    // SAFETY: the buffer and the entry pointers are valid for the call, and the
    // returned entry points into `buffer`, which outlives the reads below.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() {
        bail!("this device has no account {uid}");
    }
    // SAFETY: a non-null result points at the initialized `entry`.
    let entry = unsafe { entry.assume_init() };
    // SAFETY: the home pointer is a NUL-terminated string inside `buffer`.
    let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    let home = home.to_str().context("this account's home is not UTF-8")?;
    Ok(PathBuf::from(home))
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

/// The file `NetworkManager` keeps the saved connection called `name` in.
fn keyfile_of(name: &str) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("no async runtime")?;
    let profiles = runtime
        .block_on(async { tilo_nm::NmClient::system().await?.profiles().await })
        .context("could not ask NetworkManager for its saved connections")?;
    profiles
        .into_iter()
        .find(|profile| profile.settings.id() == name)
        .map(|profile| profile.filename)
        .filter(|file| !file.is_empty())
        .ok_or_else(|| anyhow!("no saved connection is named {name:?}"))
}

/// Reads a `NetworkManager` connection's keyfile by name, secrets included.
fn network(name: &str) -> Result<material::Network> {
    if !material::valid_network_id(name) {
        bail!("{name:?} is not a usable connection name");
    }
    let file = keyfile_of(name)?;
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

/// This machine as an enrolment uses it: the runner that reaches its
/// cryptsetup and TPM, and whether it has a TPM at all.
#[derive(Clone, Copy)]
pub struct Machine<'a> {
    pub runner: &'a dyn Runner,
    pub has_tpm: bool,
}

impl Machine<'_> {
    /// Enrolment needs the device to be able to attest, so a request for
    /// unattended unlock on a machine with no TPM fails instead of silently
    /// asking the owner for every boot.
    fn require(&self, unattended: bool) -> Result<()> {
        if unattended && !self.has_tpm {
            bail!("unattended unlock needs a TPM to attest this machine's boot chain");
        }
        Ok(())
    }

    fn sealing(&self) -> Sealing {
        if self.has_tpm {
            Sealing::Tpm2
        } else {
            Sealing::Cleartext
        }
    }
}

/// An existing passphrase of the volume: the key file's, or one asked for on
/// the terminal.
pub fn existing_passphrase(
    device: &Path,
    key_file: Option<&Path>,
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    if let Some(file) = key_file {
        return std::fs::read(file)
            .map(zeroize::Zeroizing::new)
            .with_context(|| format!("could not read {}", file.display()));
    }
    let output = std::process::Command::new("systemd-ask-password")
        .arg("--timeout=0")
        .arg(format!("An existing passphrase of {}:", device.display()))
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("could not ask for an existing passphrase")?;
    let mut answer = zeroize::Zeroizing::new(output.stdout);
    if !output.status.success() {
        bail!("no passphrase was given");
    }
    if answer.last() == Some(&b'\n') {
        answer.pop();
    }
    Ok(answer)
}

/// Enrols the sibling factor.
///
/// # Errors
/// When any step fails; the slot is removed again.
pub fn run(args: &Args, machine: Machine<'_>) -> Result<()> {
    let sealing = enrollment_sealing(machine.sealing(), args.require_tpm)?;
    machine.require(args.unattended)?;
    let enrolled = luks::sibling(machine.runner, &args.device)?.is_some();
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
        return replace_networks(machine.runner, &args.device, networks);
    }
    let registrar = registrars(args.session_uid, args.relay.as_deref())?;
    let existing = existing_passphrase(&args.device, args.key_file.as_deref())?;
    enrol(
        machine,
        &args.device,
        &existing,
        networks,
        &registrar,
        sealing,
        args.unattended,
    )
}

/// This machine's registrars in the order they are asked: its own session,
/// then the person's devices over the peer network. A machine that holds no
/// root reaches the same devices either way.
fn registrars(session_uid: Option<u32>, relay: Option<&str>) -> Result<Choosing> {
    let mut candidates: Vec<Box<dyn Registrar>> = Vec::new();
    match SessionRegistrar::find(session_uid) {
        Ok(session) => candidates.push(Box::new(session)),
        Err(error) if session_uid.is_some() => return Err(error),
        Err(_) => {}
    }
    match PeerRegistrar::find(session_uid, relays(relay)?) {
        Ok(peer) => candidates.push(Box::new(peer)),
        Err(error) => {
            if candidates.is_empty() {
                return Err(error);
            }
        }
    }
    Ok(Choosing(candidates))
}

/// Where this machine's configuration keeps the relay it reaches the person's
/// devices through (the server image writes it, and its enrollment uses it).
const SERVER_CONFIG: &str = "/etc/tilo/server.toml";

/// The relays the question is asked over: the one named, else the one this
/// machine's configuration names, else `None` for the peer network's own.
/// A machine with a session on it asks that session and never dials.
fn relays(named: Option<&str>) -> Result<Option<RelayMap>> {
    let Some(url) = named
        .map(str::to_owned)
        .or_else(|| configured_relay(Path::new(SERVER_CONFIG)))
    else {
        return Ok(None);
    };
    let url = url
        .parse::<iroh::RelayUrl>()
        .with_context(|| format!("{url:?} is not a packet relay"))?;
    Ok(Some(RelayMap::from(url)))
}

/// The `relay` a `server.toml` at `path` names, if it names one it can read.
/// The file is the server image's, and this reads the one setting the
/// enrolment needs rather than depending on the server's schema.
fn configured_relay(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    relay_in(&text)
}

/// The value of the top-level `relay` key in `text`, as TOML spells it.
fn relay_in(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("relay")?.trim_start();
        let value = rest.strip_prefix('=')?.trim();
        value
            .strip_prefix('"')
            .and_then(|value| value.split_once('"'))
            .map(|(url, _)| url.to_owned())
            .filter(|url| !url.is_empty())
    })
}

fn enrollment_sealing(available: Sealing, require_tpm: bool) -> Result<Sealing> {
    if require_tpm && available != Sealing::Tpm2 {
        bail!("TPM sealing is required for this enrollment");
    }
    Ok(available)
}

/// The enrolment itself, with the registrar as a boundary. `existing` is a
/// passphrase the volume opens with, which authorises the new slot.
///
/// # Errors
/// As [`run`].
pub fn enrol(
    machine: Machine<'_>,
    device: &Path,
    existing: &[u8],
    networks: Vec<material::Network>,
    registrar: &impl Registrar,
    sealing: Sealing,
    unattended: bool,
) -> Result<()> {
    machine.require(unattended)?;
    let runner = machine.runner;
    let dir = crate::scratch()?;
    let ws = Workspace::new(dir.path());
    let volume_uuid = luks::uuid(runner, device)?;
    let device_half = Half::generate().map_err(|error| anyhow!("{error}"))?;
    let sibling_half = Half::generate().map_err(|error| anyhow!("{error}"))?;
    let boot_key = SigningKey::from_bytes(
        Half::generate()
            .map_err(|error| anyhow!("{error}"))?
            .expose(),
    );
    let passphrase = derive_slot_key(&device_half, &sibling_half, &volume_uuid);
    let keyslot = luks::add_slot(
        runner,
        &ws,
        device,
        &Authority::Passphrase(existing),
        passphrase.as_bytes(),
    )?;
    drop(passphrase);
    // Everything after the slot exists can still fail, and the slot is
    // removed again when it does. That includes the TPM: an unattended
    // enrolment whose attestation key cannot be made fails rather than
    // leaving a machine that silently falls back to a prompt.
    let finish = || -> Result<Token> {
        let (attestation, attestation_key, unattended_chain) = if machine.has_tpm {
            let (attestation, point) = tpm::create_attestation_key(runner)?;
            let chain = if unattended {
                Some(tpm::current_chain(runner)?)
            } else {
                None
            };
            (Some(attestation), Some(point), chain)
        } else {
            (None, None, None)
        };
        let slot = SlotId {
            volume_uuid,
            keyslot: u16::try_from(keyslot).context("key slot out of range")?,
        };
        let request = EnrolRequest::new(
            &boot_key,
            slot,
            attestation_key,
            unattended_chain,
            sibling_half,
        )
        .map_err(|error| anyhow!("{error}"))?;
        let response = registrar.register(&request)?;
        drop(request);
        let cert = &response.cert;
        verify_device_cert(cert).map_err(|error| anyhow!("the issued certificate: {error}"))?;
        if cert.device_public_key != boot_key.verifying_key().to_bytes() {
            bail!("the issued certificate is for another boot key");
        }
        if cert.capabilities != DeviceCapabilities::nothing() {
            bail!("the issued certificate grants the boot key more than nothing");
        }
        if response.siblings.is_empty() {
            bail!("none of your other devices holds the sibling half");
        }
        let material = Material::new(&device_half, &boot_key, networks, attestation);
        let token = Token {
            kind: TOKEN_TYPE.into(),
            keyslots: vec![keyslot.to_string()],
            tilo_version: TOKEN_VERSION,
            cert: Token::encode_cert(cert)?,
            siblings: response.siblings.iter().map(Sibling::from).collect(),
            sealing,
            material: tpm::seal_material(runner, sealing, &material.to_json()?)?,
        };
        token.validate()?;
        luks::write_sibling(runner, &ws, device, &token)?;
        Ok(token)
    };
    match finish() {
        Ok(token) => {
            write_status(&status_of(runner, &token)?, &volume_uuid);
            Ok(())
        }
        Err(error) => {
            let _ = luks::kill_slot(runner, &ws, device, keyslot, Some(existing));
            Err(error)
        }
    }
}

/// Chooses the unlock networks of an enrolled volume again, leaving its slot,
/// certificate and siblings as they are. The material is sealed the way it
/// was: a volume enrolled in the clear stays in the clear.
pub fn replace_networks(
    runner: &dyn Runner,
    device: &Path,
    networks: Vec<material::Network>,
) -> Result<()> {
    let mut token = luks::sibling(runner, device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    token.validate()?;
    let mut material = Material::from_json(&tpm::unseal_material(
        runner,
        token.sealing,
        &token.material,
    )?)?;
    material.networks = networks;
    token.material = tpm::seal_material(runner, token.sealing, &material.to_json()?)?;
    token.validate()?;
    let dir = crate::scratch()?;
    luks::write_sibling(runner, &Workspace::new(dir.path()), device, &token)?;
    write_status(&status_of(runner, &token)?, &luks::uuid(runner, device)?);
    Ok(())
}

fn status_of(runner: &dyn Runner, token: &Token) -> Result<Status> {
    let material = Material::from_json(&tpm::unseal_material(
        runner,
        token.sealing,
        &token.material,
    )?)?;
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
    let path = dir.join(format!("{}.json", hex::encode(volume_uuid)));
    tilo_files::native::atomic::write_private(&path, &serde_json::to_vec_pretty(status)?)?;
    Ok(())
}

/// One volume enrolled on this machine, as `tilo-auth sibling volumes`
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
pub fn status(runner: &dyn Runner, device: &Path) -> Result<Status> {
    let token = luks::sibling(runner, device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    token.validate()?;
    status_of(runner, &token)
}

/// Removes the sibling slot and its token, with a passphrase of the volume
/// where the caller has one.
pub fn remove(runner: &dyn Runner, device: &Path, existing: Option<&[u8]>) -> Result<()> {
    let token = luks::sibling(runner, device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    luks::remove_sibling(runner, device)?;
    let dir = crate::scratch()?;
    luks::kill_slot(
        runner,
        &Workspace::new(dir.path()),
        device,
        token.keyslot()?,
        existing,
    )?;
    let _ = std::fs::remove_file(
        Path::new(STATE_DIR).join(format!("{}.json", hex::encode(luks::uuid(runner, device)?))),
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
    fn a_machines_configured_relay_is_read_from_its_server_config() {
        // The server image's own file: the key beside others, its value quoted.
        let config = "# where this machine enrolls\nidentity_root = \"/var/lib/tilo\"\n\
                      iroh_listen = \"0.0.0.0:0\"\n\
                      relay = \"https://relay.example.test.\"\n";
        assert_eq!(
            relay_in(config).as_deref(),
            Some("https://relay.example.test.")
        );
        let absent = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(configured_relay(absent.path()), None);
        // A commented-out key is not the machine's setting, and neither is a
        // neighbouring key whose name merely starts with it.
        assert_eq!(relay_in("# relay = \"https://old.example.test.\"\n"), None);
        assert_eq!(
            relay_in("relay_url = \"https://page.example.test.\"\n"),
            None
        );
        assert_eq!(relay_in("relay = \"\"\n"), None);
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), config).unwrap();
        assert_eq!(
            configured_relay(file.path()).as_deref(),
            Some("https://relay.example.test.")
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

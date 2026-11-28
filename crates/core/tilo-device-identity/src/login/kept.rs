//! This computer as one of the person's devices, as it keeps it: its [`Place`], and the
//! certificates the person's root signed for its device key ([`file_certificate`]).

use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use tilo_user_identity::DeviceCert;

use super::ServerAddress;
use crate::IdentityPaths;
use crate::persist::{read_device_cert, taken_device_key, write_device_cert};

/// What a device certificate this computer cannot read says.
pub const LOGIN_DAMAGED: &str = "This computer's Tilo certificate is damaged: log in again with a \
     ticket `tilo device add` prints on one of your devices.";

/// Where a login is kept below a state directory.
pub(crate) const KEPT_IN: &str = "tilo/device";

/// This account's login directory, as [`Place::here`] keeps it, without making it: a program
/// that only asks this computer's daemon something needs the daemon's socket, not a login of its
/// own, which its sandbox may not even allow it to make.
pub(crate) fn this_account_dir() -> Result<PathBuf> {
    Ok(tilo_paths::state_dir()
        .context("this account has no state directory (no HOME)")?
        .join(KEPT_IN))
}

/// Where this account keeps its login and what the login keeps on this computer (its folders,
/// its background process's state): `$XDG_STATE_HOME/tilo/device`. The device key and its certificates are
/// this computer's, kept where its session keeps them.
#[derive(Clone, Debug)]
pub struct Place {
    dir: PathBuf,
    keys: IdentityPaths,
}

impl Place {
    /// This account's place: its state directory's, with `keys`, the device key and
    /// certificates its session keeps
    /// (`tilo_identity::provisioning::collaboration_identity_paths`).
    ///
    /// The directory is made, private to this account, when the computer has never had a login:
    /// a computer with the person's root issues tickets from it, and one logging in keeps its
    /// login there.
    ///
    /// # Errors
    /// This account has no state directory, or the directory cannot be made.
    pub fn here(keys: IdentityPaths) -> Result<Self> {
        let dir = this_account_dir()?;
        let mut making = std::fs::DirBuilder::new();
        making.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut making, 0o700);
        making
            .create(&dir)
            .with_context(|| format!("making {}", dir.display()))?;
        Ok(Self { dir, keys })
    }

    /// A login kept in `dir`, with the device key and certificates `keys` names.
    #[must_use]
    pub fn at(dir: PathBuf, keys: IdentityPaths) -> Self {
        Self { dir, keys }
    }

    /// The directory the login is kept in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This computer's device key and certificates.
    #[must_use]
    pub fn keys(&self) -> &IdentityPaths {
        &self.keys
    }
}

/// Whether a person's certificate names this computer's device key `device`, among the device's
/// certificates in `place`. A computer no certificate names yet is served with its key alone, and
/// a ticket logs it in through its daemon.
#[must_use]
pub fn claimed(place: &Place, device: &[u8; 32]) -> bool {
    !certified_by(place, device).is_empty()
}

/// The people, by their root keys, whose certificates among the device's certificates in `place`
/// name `device`: none before anyone's does.
#[must_use]
pub fn certified_by(place: &Place, device: &[u8; 32]) -> Vec<[u8; 32]> {
    let person_of = |path: &Path| {
        read_device_cert(path)
            .ok()
            .filter(|cert| cert.device_public_key == *device)
            .map(|cert| cert.user_public_key)
    };
    let mut people: Vec<[u8; 32]> = Vec::new();
    let filed = place.keys.device_cert_for_user(&[0; 32]);
    let entries = filed
        .parent()
        .and_then(|certs| std::fs::read_dir(certs).ok())
        .into_iter()
        .flatten()
        .flatten();
    for entry in entries {
        if let Some(person) = person_of(&entry.path())
            && !people.contains(&person)
        {
            people.push(person);
        }
    }
    people
}

/// Where this computer goes, and as which device of the person: what a ticket's answer names,
/// and what the daemon finds in its copy of the person's own workspace afterwards. A ticket's
/// login writes it down ([`file_entry`]), so a daemon that starts again enters through it
/// before it holds a copy of the person's own workspace; a certificate kept from an older login
/// is not read as this computer's here unless it names the same device key.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Device {
    pub site: String,
    pub host_origin: String,
    pub workspace: String,
    pub workspace_name: Option<String>,
    pub agent: String,
    /// The certificate the person's root signed for this computer's device key.
    pub cert: DeviceCert,
    /// The certificate of the server this computer enters through.
    pub server_cert: DeviceCert,
    /// Where the server is dialed over iroh, as its signed invitation named it: a server reached
    /// that way only (behind a NAT, through a packet relay) has no web origin to ask.
    pub server_address: Option<ServerAddress>,
}

impl Device {
    /// How the person's devices name this computer.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.cert.device_label
    }

    /// The person's user public key, hex.
    #[must_use]
    pub fn user(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }

    /// Whether it names a server to enter through.
    #[must_use]
    pub const fn reaches_a_server(&self) -> bool {
        self.server_address.is_some()
    }
}

/// The file a login's entry is kept in, under the login's place: where this computer enters its
/// person's workspaces, and as which of their devices.
const ENTRY: &str = "entry.json";

/// Write down where this computer enters, as the login that made it one of the person's devices
/// answered ([`Device`]): a daemon that starts again, in a process of its own or in the app's,
/// enters through it before it holds a copy of the person's own workspace, which names the same
/// server once it arrives.
///
/// # Errors
/// The entry cannot be written.
pub fn file_entry(place: &Place, device: &Device) -> Result<()> {
    let bytes = serde_json::to_vec(device).context("writing this computer's login entry")?;
    tilo_files::native::atomic::write_private(&place.dir.join(ENTRY), &bytes)
        .map_err(anyhow::Error::from)
}

/// The entry the last login wrote in `place`, once its certificate for this computer's device
/// key is the one the entry names: the device as a daemon that starts again enters through it.
/// `None` on a computer that never logged in, or whose entry is unreadable.
#[must_use]
pub fn filed_entry(place: &Place) -> Option<Device> {
    let bytes = std::fs::read(place.dir.join(ENTRY)).ok()?;
    let device: Device = serde_json::from_slice(&bytes).ok()?;
    if device.cert.device_public_key != this_device(&place.keys).ok()? {
        return None;
    }
    let filed = read_device_cert(
        &place
            .keys
            .device_cert_for_user(&device.cert.user_public_key),
    )
    .ok()?;
    (filed == device.cert).then_some(device)
}

/// File `cert`, the certificate a person's root signed for this computer's device key, among the
/// device's certificates in `place`: it makes the device theirs ([`claimed`]).
///
/// # Errors
/// The certificate is for another key than this computer's, or it cannot be written.
pub fn file_certificate(place: &Place, cert: &DeviceCert) -> Result<()> {
    if cert.device_public_key != this_device(&place.keys)? {
        bail!("the certificate is for another device key than this computer's");
    }
    write_device_cert(
        &place.keys.device_cert_for_user(&cert.user_public_key),
        cert,
    )?;
    Ok(())
}

/// This computer's device key, as its daemon has it: the key this process took, in the daemon,
/// else the one the daemon says it signs with. The key file is never read here.
fn this_device(keys: &IdentityPaths) -> Result<[u8; 32]> {
    if let Ok(key) = taken_device_key(keys) {
        return Ok(key.verifying_key().to_bytes());
    }
    let actions = super::daemon::DeviceActions::here(keys.clone()).map_err(anyhow::Error::msg)?;
    Ok(actions.device_key()?)
}

/// The label a login gives a computer that runs `agent` on the host `host`:
/// `"Reviewer on build-box"`, cut to what a certificate's label carries. The agent is read back
/// from it ([`agent_of`]).
#[must_use]
pub fn device_label(agent: &str, host: &str) -> String {
    let mut fitted = format!("{agent}{ON}{host}");
    let room = tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
    fitted.truncate(
        (0..=room.min(fitted.len()))
            .rev()
            .find(|end| fitted.is_char_boundary(*end))
            .unwrap_or(0),
    );
    fitted
}

/// What a login's label puts between the agent and the host.
const ON: &str = " on ";

/// The agent a device labelled `label` runs, as [`device_label`] names it first; the whole label
/// for one it did not make.
#[must_use]
pub fn agent_of(label: &str) -> &str {
    label
        .split_once(ON)
        .map_or(label, |(agent, _)| agent)
        .trim()
}

/// The certificate a ticket's login filed for the device key `device` among the certificates in
/// `keys` ([`file_certificate`]): one naming it for another person than the identity founded
/// there, if any was, the newest when several do. `None` where no ticket logged it in.
#[must_use]
pub fn ticket_cert(keys: &IdentityPaths, device: &[u8; 32]) -> Option<DeviceCert> {
    let founded = crate::persist::read_public_key(&keys.user_pubkey).ok();
    let persons = keys.device_cert_for_user(&[0; 32]);
    std::fs::read_dir(persons.parent()?)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let cert = read_device_cert(&entry.path()).ok()?;
            Some((entry.metadata().ok()?.modified().ok()?, cert))
        })
        .filter(|(_, cert)| {
            cert.device_public_key == *device && Some(cert.user_public_key) != founded
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, cert)| cert)
}

/// The certificate this computer's device key is served as, read without the key: the device's
/// own, else the newest of those it holds for its persons. `None` before anything certifies it.
#[must_use]
pub fn served_cert(keys: &IdentityPaths) -> Option<DeviceCert> {
    if let Ok(Some(cert)) = crate::persist::read_owner_cert(keys) {
        return Some(cert);
    }
    let persons = keys.device_cert_for_user(&[0; 32]);
    std::fs::read_dir(persons.parent()?)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let cert = read_device_cert(&entry.path()).ok()?;
            Some((entry.metadata().ok()?.modified().ok()?, cert))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, cert)| cert)
}

/// The device key this computer's daemon serves as, from a certificate of it in `keys`
/// ([`served_cert`]), so the secret is not read. A computer whose device certificate went (one in
/// an encoding no build reads) has its key certified for its person beside it, and every one of
/// those names the same key. `None` before anything certifies it.
#[must_use]
pub fn served_device_key(keys: &IdentityPaths) -> Option<[u8; 32]> {
    served_cert(keys).map(|cert| cert.device_public_key)
}

/// The name the device daemon serves the workspaces' trees to the files daemon at, among its
/// sockets.
pub const FILES_TREE_SOCKET: &str = "files-tree.sock";

/// Where the device daemon the account `uid` reaches serves the workspaces' trees, and the files
/// daemon that mounts them dials: beside the daemon's other sockets
/// ([`tilo_machine::daemon_sockets`]), which the daemon makes. Neither made nor checked
/// here: a machine's files daemon is root's and dials the person's daemon, whose directory is
/// not its own.
///
/// # Errors
/// As [`tilo_machine::daemon_sockets`].
#[cfg(unix)]
pub fn files_tree_socket(uid: u32) -> Result<PathBuf, String> {
    Ok(tilo_machine::daemon_sockets(uid)?.join(FILES_TREE_SOCKET))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    /// A computer that never had a login has no device directory yet; finding its place makes
    /// it, so `tilo device add` and `tilo login` both start there.
    #[test]
    fn a_computer_that_never_had_a_login_gets_its_device_directory() {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_STATE_HOME", home.path().join("state"));
        let runtime = home.path().join("run");
        std::fs::create_dir(&runtime).unwrap();
        env.set("XDG_RUNTIME_DIR", &runtime);
        let place = Place::here(IdentityPaths::under(home.path().join("keys"))).unwrap();
        assert!(place.dir().is_dir(), "{}", place.dir().display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(place.dir()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only this account enters it: {mode:o}");
        }
    }

    /// Whether a daemon serves is read from the certificate, and reading it never makes the process
    /// undumpable: the daemon refuses a connection from a process it cannot read the namespaces
    /// of, and a program acting as this computer's device only needs to know where its daemon is.
    #[test]
    fn whether_a_daemon_serves_is_read_from_the_certificate_and_the_process_stays_dumpable() {
        tilo_test_env::in_own_process(
            module_path!(),
            "whether_a_daemon_serves_is_read_from_the_certificate_and_the_process_stays_dumpable",
            || {
                // SAFETY: PR_GET_DUMPABLE takes no pointers and only reads a flag.
                let dumpable = || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
                assert_eq!(dumpable(), 1, "a test process starts dumpable");

                let root =
                    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
                let keys = IdentityPaths::under(root.path().join("identity"));
                keys.ensure_directories().unwrap();

                let device_key = SigningKey::from_bytes(&[7; 32]);
                let root_key = SigningKey::from_bytes(&[9; 32]);
                let user = root_key.verifying_key().to_bytes();
                let mut cert = DeviceCert {
                    user_public_key: user,
                    device_public_key: device_key.verifying_key().to_bytes(),
                    device_label: "laptop".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::member(),
                    issued_at_ns: 1_000,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                };
                tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
                write_device_cert(&keys.device_cert_for_user(&user), &cert).unwrap();
                assert!(
                    !keys.device_privkey.exists(),
                    "this computer keeps no device key here, so naming its daemon must not need one"
                );

                assert!(
                    served_device_key(&keys).is_some(),
                    "a certified device is one its daemon serves"
                );
                assert_eq!(
                    dumpable(),
                    1,
                    "finding this computer's daemon reads no device key"
                );
            },
        );
    }

    /// A computer that made its own identity and never logged in is found served from its own
    /// device certificate.
    #[test]
    fn a_device_that_never_logged_in_is_served_from_its_own_certificate() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let keys = IdentityPaths::under(root.path().join("identity"));
        keys.ensure_directories().unwrap();
        let root_key = SigningKey::from_bytes(&[9; 32]);
        let mut cert = DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
            device_label: "server".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1_000,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
        write_device_cert(&keys.device_cert_for_user(&cert.user_public_key), &cert).unwrap();
        assert!(
            served_device_key(&keys).is_some(),
            "a device of its own is served by its daemon"
        );
    }
}

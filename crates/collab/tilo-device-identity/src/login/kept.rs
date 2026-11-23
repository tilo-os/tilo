//! A login as this computer keeps it: [`Place`], [`load`] and [`store`].

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_user_identity::DeviceCert;

use super::ServerAddress;
use crate::IdentityPaths;
use crate::persist::{load_device_key, read_device_cert, write_device_cert};

/// What a login this computer cannot read says.
pub const LOGIN_DAMAGED: &str = "This computer's Tilo login is damaged: log in again with a ticket \
     `tilo device add` prints on one of your devices.";

/// What a login says whose certificate is for a device key this computer no longer has.
const LOGIN_STALE: &str = "This computer's Tilo login is for a device key it no longer has: \
     log in again with a ticket `tilo device add` prints on one of your devices.";

/// The file in [`Place::dir`] a login is kept in.
const LOGIN_FILE: &str = "login.json";

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
        let dir = tilo_paths::state_dir()
            .context("this account has no state directory (no HOME)")?
            .join("tilo/device");
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

    /// Whether this computer logged in, without reading the device key: a process that reads it
    /// makes itself undumpable.
    #[must_use]
    pub fn logged_in(&self) -> bool {
        self.dir.join(LOGIN_FILE).exists()
    }
}

/// Whether a person's certificate names this computer's device key `device`: a login kept in
/// `place`, or a certificate for the key among the device's certificates. A computer no
/// certificate names yet is served with its key alone, and a ticket logs it in without its daemon.
#[must_use]
pub fn claimed(place: &Place, device: &[u8; 32]) -> bool {
    place.logged_in() || !certified_by(place, device).is_empty()
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
    let mut people: Vec<[u8; 32]> = person_of(&place.keys.device_cert).into_iter().collect();
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

/// A login: where this computer goes, and as which device of the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub site: String,
    pub host_origin: String,
    pub workspace: String,
    pub workspace_name: Option<String>,
    pub agent: String,
    /// The certificate the person's root signed for this computer's device key.
    pub cert: DeviceCert,
    /// The host's own certificate, as the login was answered it.
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

    /// Whether the login names a server to enter through. One that names none, as logins
    /// older builds kept do, has ended: nothing this computer runs reaches its workspace again.
    #[must_use]
    pub const fn reaches_a_server(&self) -> bool {
        self.server_address.is_some()
    }
}

/// A login as [`LOGIN_FILE`] keeps it: the certificate is kept with the device key, and found by
/// the person it names.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept {
    site: String,
    host_origin: String,
    workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_name: Option<String>,
    user: String,
    agent: String,
    /// The host's certificate, its canonical bytes in hex.
    server_cert: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_address: Option<ServerAddress>,
}

/// Keep `device` in `place`: its certificate beside this computer's device key, which it must
/// certify, and the rest in [`LOGIN_FILE`].
///
/// # Errors
/// The certificate is for another key than this computer's, or the files cannot be written.
pub fn store(place: &Place, device: &Device) -> Result<()> {
    let key = load_device_key(&place.keys).context("reading this computer's device key")?;
    if device.cert.device_public_key != key.verifying_key().to_bytes() {
        bail!("the login's certificate is for another device key than this computer's");
    }
    write_device_cert(
        &place
            .keys
            .device_cert_for_user(&device.cert.user_public_key),
        &device.cert,
    )?;
    let kept = Kept {
        site: device.site.clone(),
        host_origin: device.host_origin.clone(),
        workspace: device.workspace.clone(),
        workspace_name: device.workspace_name.clone(),
        user: device.user(),
        agent: device.agent.clone(),
        server_cert: hex::encode(tilo_user_identity::canonical::canonical_bytes(
            &device.server_cert,
        )?),
        server_address: device.server_address.clone(),
    };
    tilo_files::native::atomic::write_private(
        &place.dir.join(LOGIN_FILE),
        &serde_json::to_vec_pretty(&kept)?,
    )?;
    Ok(())
}

/// This computer's device key and the login kept in `place`; `None` when this computer did not
/// log in.
///
/// Reading the device key makes this process undumpable ([`load_device_key`]), so a program that
/// only needs to know which device this computer is takes [`load_public`] instead.
///
/// # Errors
/// The login is damaged, or its certificate is for a device key this computer no longer has.
pub fn load(place: &Place) -> Result<Option<(SigningKey, Device)>> {
    let Some(device) = load_public(place)? else {
        return Ok(None);
    };
    let key = load_device_key(&place.keys).map_err(|error| damaged("device key", &error))?;
    if device.cert.device_public_key != key.verifying_key().to_bytes() {
        bail!(LOGIN_STALE);
    }
    Ok(Some((key, device)))
}

/// What a login this computer cannot read says, with why kept to this process's log.
fn damaged(what: &str, why: &dyn std::fmt::Display) -> anyhow::Error {
    tracing::debug!(%why, "reading this computer's Tilo {what}");
    anyhow!(LOGIN_DAMAGED)
}

/// The login kept in `place`, as its certificate and what the login names: read without this
/// computer's device key, so the process stays dumpable. The certificate names the device, which
/// is all a program that finds this device's daemon ([`serves_a_device`]) needs.
///
/// # Errors
/// The login is damaged, or its certificate is for another person than the one it names.
pub fn load_public(place: &Place) -> Result<Option<Device>> {
    let bytes = match std::fs::read(place.dir.join(LOGIN_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(damaged("login", &error)),
    };
    let kept: Kept = serde_json::from_slice(&bytes).map_err(|error| damaged("login", &error))?;
    let user: [u8; 32] = hex::decode(&kept.user)
        .ok()
        .and_then(|user| user.try_into().ok())
        .ok_or_else(|| anyhow!(LOGIN_DAMAGED))?;
    let cert = read_device_cert(&place.keys.device_cert_for_user(&user))
        .map_err(|error| damaged("device certificate", &error))?;
    if cert.user_public_key != user {
        bail!(LOGIN_STALE);
    }
    let server_cert = hex::decode(&kept.server_cert)
        .ok()
        .and_then(|bytes| tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok())
        .ok_or_else(|| anyhow!(LOGIN_DAMAGED))?;
    Ok(Some(Device {
        site: kept.site,
        host_origin: kept.host_origin,
        workspace: kept.workspace,
        workspace_name: kept.workspace_name,
        agent: kept.agent,
        cert,
        server_cert,
        server_address: kept.server_address,
    }))
}

/// The workspace this computer's login entered: its id, and the name the
/// device keeps for it, which is how a command that acts in it says which
/// workspace it used, with this computer's device key and certificates at `keys`. `None` when
/// this computer is not logged in.
///
/// # Errors
/// The login is damaged.
pub fn entered_workspace(keys: IdentityPaths) -> Result<Option<(String, String)>> {
    let Some(device) = load_public(&Place::here(keys)?)? else {
        return Ok(None);
    };
    let name = device
        .workspace_name
        .clone()
        .unwrap_or_else(|| device.workspace.clone());
    Ok(Some((device.workspace, name)))
}

/// Whether this computer's daemon serves a device here, so its API answers every program of
/// the account as that device: a login, or, with none, a certificate of the device it made for
/// itself. `keys` are this computer's device key and certificates.
///
/// The device is read from a certificate, never from the device key: a program that reads the
/// key makes itself undumpable, and the daemon admits a caller by reading its namespaces, which
/// a non-dumpable process hides from it.
///
/// # Errors
/// The login is damaged.
#[cfg(unix)]
pub fn serves_a_device(keys: IdentityPaths) -> Result<bool> {
    serves_a_device_in(&Place::here(keys)?)
}

/// [`serves_a_device`] for the login `place` keeps. A computer whose device certificate went
/// (one in an encoding no build reads) has its key certified for its person beside it.
#[cfg(unix)]
fn serves_a_device_in(place: &Place) -> Result<bool> {
    Ok(load_public(place)?.is_some() || served_device_key(place.keys()).is_some())
}

/// The device key a daemon with no login serves as, from a certificate of it in `keys`: the
/// device's own, else the newest of those it holds for its persons.
#[cfg(unix)]
fn served_device_key(keys: &IdentityPaths) -> Option<[u8; 32]> {
    if let Ok(cert) = read_device_cert(&keys.device_cert) {
        return Some(cert.device_public_key);
    }
    let persons = keys.device_cert_for_user(&[0; 32]);
    std::fs::read_dir(persons.parent()?)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let cert = read_device_cert(&entry.path()).ok()?;
            Some((
                entry.metadata().ok()?.modified().ok()?,
                cert.device_public_key,
            ))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, key)| key)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

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
                let login_dir = root.path().join("login");
                std::fs::create_dir(&login_dir).unwrap();
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
                let mut server = cert.clone();
                server.device_public_key =
                    SigningKey::from_bytes(&[11; 32]).verifying_key().to_bytes();
                tilo_user_identity::sign_device_cert(&mut server, &root_key).unwrap();
                let server_cert =
                    hex::encode(tilo_user_identity::canonical::canonical_bytes(&server).unwrap());
                let kept = serde_json::json!({
                    "site": "https://tilo.sh",
                    "host_origin": "https://eu1.tilo.sh",
                    "workspace": "6f1e1c3a-0000-4000-8000-000000000001",
                    "user": hex::encode(user),
                    "agent": "tilo-test",
                    "server_cert": server_cert,
                });
                std::fs::write(
                    login_dir.join(LOGIN_FILE),
                    serde_json::to_vec(&kept).unwrap(),
                )
                .unwrap();
                assert!(
                    !keys.device_privkey.exists(),
                    "this login keeps no device key, so naming its daemon must not need one"
                );

                let place = Place::at(login_dir, keys);
                assert!(
                    serves_a_device_in(&place).unwrap(),
                    "a login is a device its daemon serves"
                );
                assert_eq!(
                    dumpable(),
                    1,
                    "finding this computer's daemon reads no device key"
                );
            },
        );
    }

    /// A computer that made its own identity and never logged in keeps no login, so its state
    /// directory may not even exist: its daemon is still found, from the device's certificate.
    #[test]
    fn a_device_that_never_logged_in_finds_its_daemon_without_a_login_directory() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let login_dir = root.path().join("state/tilo/device");
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
        write_device_cert(&keys.device_cert, &cert).unwrap();
        assert!(!login_dir.exists(), "this computer never logged in");

        assert!(
            serves_a_device_in(&Place::at(login_dir, keys)).unwrap(),
            "a device of its own is served by its daemon"
        );
    }
}

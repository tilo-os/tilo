//! This mount's identity on the network and in the document: the device certificate the
//! person's root signed for this computer's device key. The mount keeps no key — the device's
//! daemon is the key's one writer — so it asks that daemon which device this computer is
//! ([`device_public_key`]) and checks the certificate names it. Nothing here reads a key file.

use std::path::PathBuf;

use thiserror::Error;
use tilo_device_identity::login::daemon::{DeviceActions, DeviceRefusal};
use tilo_device_identity::{IdentityPaths, PersistError};
use tilo_user_identity::{DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at};

/// Overrides the identity root (`/var/lib/tilo`); tests point it at a tempdir.
pub const IDENTITY_ROOT_ENV: &str = "TILO_IDENTITY_ROOT";

#[must_use]
pub fn revocation_list_path(paths: &IdentityPaths) -> PathBuf {
    tilo_device_identity::revocations_path(paths)
}

/// Load the signed revocations this device verified, for peer admission.
///
/// A missing list means no revocation has been learned yet; any other read, decode
/// or signature failure aborts collaborative networking rather than treating corrupt
/// trust state as empty.
pub fn load_revocation_list(
    paths: &IdentityPaths,
) -> Result<RevocationList, tilo_device_identity::RevocationsError> {
    tilo_device_identity::read_revocations(paths)
}

/// Where this computer's device certificate lives, [`IdentityPaths::default_system`] unless
/// [`IDENTITY_ROOT_ENV`] overrides it.
#[must_use]
pub fn identity_paths() -> IdentityPaths {
    match std::env::var(IDENTITY_ROOT_ENV) {
        Ok(root) if !root.is_empty() => IdentityPaths::under(PathBuf::from(root)),
        _ => IdentityPaths::default_system(),
    }
}

/// The device's Ed25519 public key, as this computer's daemon answers it ([`DeviceActions`]).
///
/// The daemon is the device key's one writer, so this is how the mount learns which key its
/// certificate must name. A daemon that is not running is refused in its own words.
///
/// # Errors
/// The daemon is not running or refused, or its answer names no 32-byte device key.
pub fn device_public_key(paths: &IdentityPaths) -> Result<[u8; 32], CertifiedIdentityError> {
    let actions = DeviceActions::here(paths.clone())
        .map_err(|message| CertifiedIdentityError::DeviceService(DeviceRefusal::said(message)))?;
    device_public_key_of(&actions)
}

/// [`device_public_key`] from the daemon at `actions`.
fn device_public_key_of(actions: &DeviceActions) -> Result<[u8; 32], CertifiedIdentityError> {
    let info = actions.ask("info", serde_json::json!({}))?;
    let named = info
        .get("device")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| no_device_key("the device service answered no device key"))?;
    let key = hex::decode(named)
        .map_err(|_| no_device_key("the device service answered a device key that is not hex"))?;
    key.as_slice().try_into().map_err(|_| {
        no_device_key(format!(
            "the device service answered a device key of {} bytes, not 32",
            key.len()
        ))
    })
}

fn no_device_key(message: impl Into<String>) -> CertifiedIdentityError {
    CertifiedIdentityError::DeviceService(DeviceRefusal::said(message.into()))
}

/// The device certificate presented during collaborative-FS peer auth. The constructor is
/// private: every instance has proved that its certificate is valid and names the device's
/// public key.
#[derive(Clone)]
pub struct PeerCredentials {
    cert: DeviceCert,
}

impl PeerCredentials {
    /// The certificate `cert` of the device whose public key the daemon answers as
    /// `device_public_key`: it must verify at `now_ns` and name that key.
    ///
    /// # Errors
    /// The certificate is invalid or not yet valid at `now_ns`
    /// ([`CertifiedIdentityError::Certificate`]), or names another device
    /// ([`CertifiedIdentityError::DeviceKeyMismatch`]).
    pub fn certified(
        cert: DeviceCert,
        device_public_key: [u8; 32],
        now_ns: u64,
    ) -> Result<Self, CertifiedIdentityError> {
        verify_device_cert_at(&cert, now_ns)?;
        if cert.device_public_key != device_public_key {
            return Err(CertifiedIdentityError::DeviceKeyMismatch);
        }
        Ok(Self { cert })
    }

    pub fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    pub fn user_public_key(&self) -> [u8; 32] {
        self.cert.user_public_key
    }

    pub fn device_public_key(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

#[derive(Debug, Error)]
pub enum CertifiedIdentityError {
    #[error("device certificate could not be loaded: {0}")]
    CertificateFile(#[from] PersistError),
    #[error("device certificate is invalid: {0}")]
    Certificate(#[from] DeviceCertError),
    #[error("device certificate does not name this device's public key")]
    DeviceKeyMismatch,
    /// The device's daemon did not answer, or refused: said in its own words.
    #[error("{0}")]
    DeviceService(#[from] DeviceRefusal),
}

/// The device certificate at `paths`, proved to name `device_public_key` and be valid at
/// `now_ns`.
///
/// Fails rather than falling back to another identity: a mount that cannot prove which device
/// it is has no business recording authorship.
///
/// # Errors
/// The certificate is missing or cannot be decoded ([`CertifiedIdentityError::CertificateFile`]),
/// is invalid or not yet valid ([`CertifiedIdentityError::Certificate`]), or names another
/// device ([`CertifiedIdentityError::DeviceKeyMismatch`]).
pub fn load_certified_mount_identity(
    paths: &IdentityPaths,
    device_public_key: [u8; 32],
    now_ns: u64,
) -> Result<PeerCredentials, CertifiedIdentityError> {
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)?;
    PeerCredentials::certified(cert, device_public_key, now_ns)
}

/// [`load_certified_mount_identity`] at the wall clock, waiting while the certificate is
/// refused only because the clock reads a time before its issuance.
///
/// A machine whose clock came up behind refuses its own intact certificate until
/// network time steps it. The daemon waits for that step on a timerfd the kernel ends
/// when the clock changes, and reads the certificate again after each wake.
pub async fn load_certified_mount_identity_once_valid(
    paths: &IdentityPaths,
    device_public_key: [u8; 32],
) -> Result<PeerCredentials, CertifiedIdentityError> {
    loop {
        let now_ns = crate::clock::unix_now_ns();
        let refused = match load_certified_mount_identity(paths, device_public_key, now_ns) {
            Err(CertifiedIdentityError::Certificate(DeviceCertError::NotYetValid)) => {
                CertifiedIdentityError::Certificate(DeviceCertError::NotYetValid)
            }
            loaded => return loaded,
        };
        let valid_from_ns =
            tilo_device_identity::persist::read_device_cert(&paths.device_cert)?.issued_at_ns;
        if valid_from_ns <= now_ns {
            // Written anew between the two reads: check it again.
            continue;
        }
        eprintln!(
            "device certificate is valid from {valid_from_ns} ns and the clock reads {now_ns} ns; \
             waiting for the clock"
        );
        let waited = tokio::task::spawn_blocking(move || {
            tilo_device_identity::wait_for_wall_clock(valid_from_ns)
        })
        .await;
        match waited {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                eprintln!("cannot wait for the clock: {error}");
                return Err(refused);
            }
            Err(error) => {
                eprintln!("the wait for the clock ended without an answer: {error}");
                return Err(refused);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_device_identity::persist::write_device_cert;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    const NOW_NS: u64 = 5_000;
    const DEVICE_SEED: [u8; 32] = [41; 32];
    const ROOT_SEED: [u8; 32] = [73; 32];

    /// The public key of the device whose key is `DEVICE_SEED`, as the daemon would answer it.
    fn device_public() -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&DEVICE_SEED)
            .verifying_key()
            .to_bytes()
    }

    fn signed_cert(
        device_public_key: [u8; 32],
        issued_at_ns: u64,
        expires_at_ns: Option<u64>,
    ) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&ROOT_SEED);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key,
            device_label: "Deterministic test laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    fn certified_error(paths: &IdentityPaths) -> CertifiedIdentityError {
        match load_certified_mount_identity(paths, device_public(), NOW_NS) {
            Ok(_) => panic!("certified identity unexpectedly loaded"),
            Err(err) => err,
        }
    }

    /// A daemon stand-in at `dir/native.sock` that answers every `info` call with `device`.
    #[cfg(unix)]
    struct StandInDaemon {
        socket: PathBuf,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl StandInDaemon {
        fn start(dir: &std::path::Path, device: [u8; 32]) -> Self {
            use std::io::{BufRead as _, Write as _};
            use std::sync::atomic::{AtomicBool, Ordering};
            std::fs::create_dir_all(dir).unwrap();
            let socket = dir.join(tilo_paths::machine::DAEMON_NATIVE);
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let stopping = std::sync::Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    if stopping.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut line = String::new();
                    let _ =
                        std::io::BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
                    let answer = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "content": [{"type": "text", "text": ""}],
                            "structuredContent": {
                                "status": "ok",
                                "result": {"device": hex::encode(device)},
                            },
                        },
                    });
                    let _ = writeln!(stream, "{answer}");
                }
            });
            Self {
                socket,
                stop,
                thread: Some(thread),
            }
        }
    }

    #[cfg(unix)]
    impl Drop for StandInDaemon {
        fn drop(&mut self) {
            use std::sync::atomic::Ordering;
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::os::unix::net::UnixStream::connect(&self.socket);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_device_public_key_is_what_the_daemon_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = StandInDaemon::start(tmp.path(), device_public());
        let key = device_public_key_of(&DeviceActions::at(daemon.socket.clone())).unwrap();
        assert_eq!(key, device_public());
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_that_is_not_running_is_refused_in_its_own_words() {
        let tmp = tempfile::tempdir().unwrap();
        let actions = DeviceActions::at(tmp.path().join(tilo_paths::machine::DAEMON_NATIVE));
        let refused = device_public_key_of(&actions).expect_err("nothing listens");
        let CertifiedIdentityError::DeviceService(refusal) = refused else {
            panic!("a daemon that is not running is refused as such, not {refused:?}");
        };
        assert!(refusal.not_running(), "{refusal:?}");
        assert!(
            refusal
                .message
                .starts_with("the device service isn't running on this computer"),
            "{refusal}"
        );
    }

    #[tokio::test]
    async fn a_certificate_issued_after_the_clocks_reading_loads_once_the_clock_reaches_it() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        let read_at_ns = crate::clock::unix_now_ns();
        let issued_at_ns = read_at_ns + 200_000_000;
        let cert = signed_cert(device_public(), issued_at_ns, None);
        write_device_cert(&paths.device_cert, &cert).unwrap();
        assert!(matches!(
            load_certified_mount_identity(&paths, device_public(), read_at_ns),
            Err(CertifiedIdentityError::Certificate(
                DeviceCertError::NotYetValid
            ))
        ));

        let loaded = load_certified_mount_identity_once_valid(&paths, device_public())
            .await
            .expect("the daemon waits for the clock instead of giving up");
        assert_eq!(loaded.cert(), &cert);
        assert!(crate::clock::unix_now_ns() >= issued_at_ns);

        let other = ed25519_dalek::SigningKey::from_bytes(&[99; 32]);
        let foreign = signed_cert(other.verifying_key().to_bytes(), 1, None);
        write_device_cert(&paths.device_cert, &foreign).unwrap();
        assert!(matches!(
            load_certified_mount_identity_once_valid(&paths, device_public()).await,
            Err(CertifiedIdentityError::DeviceKeyMismatch)
        ));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn certified_loading_requires_a_valid_current_certificate_for_this_device() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let cert = signed_cert(device_public(), NOW_NS - 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();

            let certified = load_certified_mount_identity(&paths, device_public(), NOW_NS).unwrap();
            assert_eq!(certified.device_public_key(), cert.device_public_key);
            assert_eq!(certified.user_public_key(), cert.user_public_key);
            assert_eq!(certified.cert(), &cert);
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let err = certified_error(&paths);
            assert!(matches!(
                err,
                CertifiedIdentityError::CertificateFile(PersistError::Io(ref io_err))
                    if io_err.kind() == std::io::ErrorKind::NotFound
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            paths.ensure_directories().unwrap();
            std::fs::write(&paths.device_cert, b"not a canonical certificate").unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::CertificateFile(PersistError::Canonical(_))
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let mut cert = signed_cert(device_public(), NOW_NS - 1, None);
            cert.device_label.push_str(" (tampered)");
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::Certificate(DeviceCertError::BadSignature)
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let cert = signed_cert(device_public(), NOW_NS - 100, Some(NOW_NS));
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::Certificate(DeviceCertError::Expired)
            ));

            let cert = signed_cert(device_public(), NOW_NS + 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::Certificate(DeviceCertError::NotYetValid)
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let other_device = ed25519_dalek::SigningKey::from_bytes(&[99; 32]);
            let cert = signed_cert(other_device.verifying_key().to_bytes(), NOW_NS - 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::DeviceKeyMismatch
            ));
        }
    }

    tilo_test_env::scenario! {
        fn an_identity_root_override_selects_that_roots_certificate;

        phase an_identity_root_override_is_the_identity_that_is_read;
        {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("isolated");
            let paths = IdentityPaths::under(&root);
            let cert = signed_cert(device_public(), NOW_NS - 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(IDENTITY_ROOT_ENV, &root);

            let selected = identity_paths();
            assert_eq!(selected.device_cert, paths.device_cert);
            let certified = load_certified_mount_identity(&selected, device_public(), NOW_NS)
                .expect("the selected root's certificate");
            assert_eq!(certified.cert(), &cert);
        }

        phase no_override_is_the_systems_identity;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove(IDENTITY_ROOT_ENV);
            assert_eq!(
                identity_paths().device_cert,
                IdentityPaths::default_system().device_cert
            );
        }
    }
}

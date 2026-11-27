//! Host entry point for the primary Files workspace: start the daemon only for
//! saved, authorized state, never by creating an empty namespace.
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use tilo_device_identity::{IdentityPaths, persist};

use crate::workspace_startup;

/// systemd's `RestartPreventExitStatus=` for state that retrying cannot fix.
const NOT_READY_EXIT: i32 = 78;

#[derive(clap::Args, Debug)]
pub(super) struct PrimaryArgs {
    /// Print the daemon command line as JSON instead of starting it.
    #[arg(long)]
    dry_run: bool,
}

/// The person the existing identity at `paths` is certified for: the device's public key,
/// answered by the device's daemon, its root-signed certificate and the person's public key
/// agree, the certificate is current and its root has not revoked the device. Nothing is
/// created here: a machine that is not a certified device starts no mount.
fn existing_person(paths: &IdentityPaths) -> Result<[u8; 32]> {
    let device = tilo_fs::device_public_key(paths)?;
    let person = persist::read_public_key(&paths.user_pubkey)?;
    let certified =
        tilo_fs::load_certified_mount_identity(paths, device, tilo_clock::unix_now_ns())?;
    ensure!(
        certified.user_public_key() == person,
        "existing identity projection is inconsistent"
    );
    let revocations = tilo_fs::load_revocation_list(paths)?;
    ensure!(
        !revocations.refuses_at(certified.cert(), tilo_clock::unix_now_ns()),
        "existing device is revoked"
    );
    Ok(person)
}

struct Paths {
    state: PathBuf,
    identity: PathBuf,
    config: PathBuf,
    runtime: PathBuf,
    identity_socket: PathBuf,
    /// The storage socket alone, so Settings can be handed this directory
    /// without the blob socket in `runtime`.
    storage: PathBuf,
}

fn paths(env: &dyn Fn(&str) -> Option<OsString>) -> Result<Paths> {
    let home = PathBuf::from(env("HOME").context("HOME is not set")?);
    let runtime_dir = env("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    // An empty variable counts as unset, as in the XDG base directory spec.
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    let base =
        |name: &str, fallback: &str| var(name).map_or_else(|| home.join(fallback), PathBuf::from);
    let data_home = base("XDG_DATA_HOME", ".local/share");
    let runtime_dir = PathBuf::from(runtime_dir);
    let paths = Paths {
        state: base("XDG_STATE_HOME", ".local/state").join("tilo/files"),
        identity: var("TILO_IDENTITY_ROOT").map_or_else(|| data_home.join("tilo"), PathBuf::from),
        config: base("XDG_CONFIG_HOME", ".config").join("tilo"),
        runtime: runtime_dir.join("tilo/files-workspace"),
        identity_socket: runtime_dir.join("tilo/services/identity.sock"),
        storage: runtime_dir.join("tilo/files-storage"),
    };
    for path in [&paths.state, &paths.identity, &paths.config, &paths.runtime] {
        ensure!(path.is_absolute(), "Files paths must be absolute");
    }
    Ok(paths)
}

fn private_dir(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("create {}", path.display()))
}

fn command(_args: &PrimaryArgs, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Command> {
    let paths = paths(env)?;
    // Read-only: with no saved tree and no workspace service to sync one
    // from, nothing starts, so the desktop never shows an empty workspace.
    // This production check validates the existing certificate and key; it
    // never creates credentials.
    workspace_startup::inspect(&paths.state, &super::tree_socket(None)?)?.require_existing()?;
    let person = existing_person(&IdentityPaths::under(&paths.identity))?;
    eprintln!(
        "Files mount starts as this device's identity, certified for person {}",
        hex::encode(person)
    );
    let mount = paths.state.join("mount");
    private_dir(&mount)?;
    private_dir(&paths.runtime)?;
    private_dir(&paths.storage)?;
    let fs_binary = std::env::current_exe().context("locate the fs executable")?;
    let mut serve = Command::new(fs_binary);
    serve
        .arg("serve")
        .arg("--require-existing-workspace")
        .arg("--cache-dir")
        .arg(&paths.state)
        .arg("--mount-point")
        .arg(&mount)
        .arg("--acl")
        .arg(paths.config.join("fs-capabilities.json"))
        .args(["--backend", "fuse", "--storage-sock"])
        .arg(paths.storage.join("storage.sock"))
        .arg("--blob-sock")
        .arg(paths.runtime.join("fs-blobs.sock"))
        .arg("--workspace-key-socket")
        .arg(&paths.identity_socket)
        .env("TILO_IDENTITY_ROOT", &paths.identity);
    Ok(serve)
}

/// The argv as one JSON line in the list layout the service tooling reads.
fn argv_json(command: &Command) -> String {
    let words: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word: &OsStr| {
            serde_json::Value::String(word.to_string_lossy().into_owned()).to_string()
        })
        .collect();
    format!("[{}]", words.join(", "))
}

/// Runs before the async runtime exists, so the exec replaces a single thread.
pub(super) fn run(args: &PrimaryArgs) -> i32 {
    let mut serve = match command(args, &|name| std::env::var_os(name)) {
        Ok(serve) => serve,
        Err(error) => {
            eprintln!("Files service is not ready: {error:#}");
            return NOT_READY_EXIT;
        }
    };
    if args.dry_run {
        println!("{}", argv_json(&serve));
        return 0;
    }
    let error = serve.exec();
    eprintln!("starting the Files daemon failed: {error}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certified device of a person, written under `root`, as a machine that
    /// joined leaves it.
    #[cfg(unix)]
    fn certified_identity(root: &Path) {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let paths = IdentityPaths::under(root);
        paths.ensure_directories().expect("identity directories");
        let person = root_key.verifying_key().to_bytes();
        persist::write_public_key(&paths.user_pubkey, &person).expect("write person");
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "fixture".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root_key).expect("sign the device");
        persist::write_device_cert(&paths.device_cert_for_user(&person), &cert)
            .expect("write the certificate");
    }

    /// The device's daemon stand-in at its native endpoint `socket`, answering `info` with the
    /// fixture device's public key, stopped when dropped.
    #[cfg(unix)]
    struct StandInDaemon {
        socket: PathBuf,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl StandInDaemon {
        fn start(socket: PathBuf) -> Self {
            use std::io::{BufRead as _, Write as _};
            let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32])
                .verifying_key()
                .to_bytes();
            let listener = std::os::unix::net::UnixListener::bind(&socket).expect("daemon socket");
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopping = std::sync::Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                use std::sync::atomic::Ordering;
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    if stopping.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut line = String::new();
                    let _ = std::io::BufReader::new(stream.try_clone().expect("daemon stream"))
                        .read_line(&mut line);
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
    fn daemon_environment_names_the_checked_identity_and_private_runtime() {
        let dir = tempfile::tempdir().expect("isolated fixture");
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".local/state/tilo/files")).expect("create cache");
        std::fs::create_dir_all(home.join(".local/state/tilo/files/trees/one"))
            .expect("create kept tree");
        std::fs::write(
            home.join(".local/state/tilo/files/trees/one/0.automerge"),
            b"saved",
        )
        .expect("write a kept document");
        certified_identity(&home.join("certified"));
        let runtime = home.join("runtime");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime)
            .expect("the runtime directory");
        let mut scoped = tilo_test_env::ScopedEnv::new();
        scoped
            .set("HOME", &home)
            .set("XDG_STATE_HOME", home.join("state"))
            .set("XDG_RUNTIME_DIR", &runtime)
            .set("TILO_RUNTIME_PROFILE", home.join("daemon"));
        // The daemon listens where the identity's clients dial it: its profile's own sockets.
        let daemon = StandInDaemon::start(
            tilo_device_identity::login::daemon::DeviceActions::here(IdentityPaths::under(
                home.join("certified"),
            ))
            .expect("the daemon's sockets")
            .native()
            .to_path_buf(),
        );
        let env = |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "XDG_RUNTIME_DIR" => Some(runtime.clone().into_os_string()),
            "TILO_IDENTITY_ROOT" => Some(home.join("certified").into_os_string()),
            _ => None,
        };
        let args = PrimaryArgs { dry_run: true };
        let serve = command(&args, &env).expect("ready fixture");
        let mut flags = serve.get_args();
        assert!(flags.any(|arg| arg == "--workspace-key-socket"));
        assert_eq!(
            flags.next(),
            Some(runtime.join("tilo/services/identity.sock").as_os_str())
        );
        let envs: std::collections::HashMap<_, _> = serve.get_envs().collect();
        assert_eq!(
            envs[OsStr::new("TILO_IDENTITY_ROOT")],
            Some(home.join("certified").as_os_str())
        );
        assert!(
            std::fs::read_dir(home.join("certified/device/certs"))
                .unwrap()
                .next()
                .is_some()
        );
        drop(daemon);
    }
}

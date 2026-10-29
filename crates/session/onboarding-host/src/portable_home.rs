//! A declared portable home stays pending independently of identity activation.
//! The canonical headless helper authenticates and adopts; this controller only
//! retains explicit intent, bounded public progress and completion/defer receipts.
use crate::continuation::{self as continuation, Context, Continuation, Kind, Settled};
use crate::prepared::{Pending as Prepared, PortableHome};
use std::{
    fs::OpenOptions,
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub type Pending = Continuation<Home>;

/// A portable home carried over by a prepared setup.
pub enum Home {}

fn failure() -> io::Error {
    continuation::failure::<Home>()
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub enum HelperEvent {
    #[serde(rename = "ready-to-adopt")]
    Ready { preview: HelperPreview },
    #[serde(rename = "complete")]
    Complete { result: HelperResult },
    #[serde(rename = "incomplete")]
    Incomplete { result: HelperResult },
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperPreview {
    install_items: usize,
    collisions: usize,
    inactive_custody: usize,
    runtime_sockets: u64,
    recovered: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperResult {
    complete: bool,
    cancelled: bool,
    installed: usize,
    collisions: usize,
    inactive_custody: usize,
    failures: usize,
    verified: bool,
}
#[cfg(test)]
fn root(home: &Path) -> PathBuf {
    continuation::root::<Home>(home)
}
pub fn helper_executable() -> io::Result<&'static Path> {
    let executable = match std::fs::symlink_metadata("/etc/tilo/prepared-setup") {
        Ok(_) => Path::new("/etc/tilo/prepared-setup"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Path::new("/usr/libexec/tilo-prepared-setup")
        }
        Err(_) => return Err(failure()),
    };
    let helper = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(executable)?;
    let metadata = helper.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(failure());
    }
    Ok(executable)
}

impl Kind for Home {
    type Declaration = PortableHome;
    type Event = HelperEvent;
    const FAILURE: &'static str = "Portable home migration is pending or unavailable";
    const ROOT: &'static str = "prepared-home";
    const RECEIPT_BOUND: u64 = 2048;
    const LINE_BOUND: usize = 16 * 1024;
    const HELPER_FLAG: &'static str = "--portable-home";
    const WORKSPACE: &'static str = "portable-home";
    const WORKSPACE_NAMESPACE: bool = false;
    const OUTPUT_BOUND: usize = 16 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(3600);
    const READY: &'static str = "ready-to-adopt";
    const PROCEED: &'static str = "adopt";
    const PROCEED_ACTION: &'static str = "adopt";
    const PROCEEDING: &'static str = "adopting";
    const CONTINUES_WHEN_COMPLETE: bool = false;
    const FORGETS_PASSWORD_ON_FAILURE: bool = true;
    const FAILURE_CLEARS_RESULT: bool = false;
    const MONOTONIC_RECEIPTS: bool = true;
    const COMPLETION_NEEDS_PROOF: bool = false;

    fn declared(prepared: &Prepared, identity: &Path) -> io::Result<Option<PortableHome>> {
        prepared.portable_home(identity)
    }
    fn valid(declaration: &PortableHome) -> bool {
        declaration.valid()
    }
    fn operation(declaration: &PortableHome) -> &str {
        &declaration.setup_binding
    }
    fn mark(declaration: &PortableHome, status: &mut serde_json::Map<String, serde_json::Value>) {
        status.insert("portable_home".to_owned(), true.into());
        status.insert(
            "setup_binding".to_owned(),
            declaration.setup_binding.clone().into(),
        );
    }
    fn package(cx: &Context<Self>, directory: &Path) -> io::Result<(PathBuf, String)> {
        let receipt =
            continuation::private_file::<Self>(&directory.join("expectation.json"), 16 * 1024)?;
        let receipt: serde_json::Value = serde_json::from_slice(&receipt).map_err(|_| failure())?;
        let operation = receipt
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(failure)?;
        let filename = receipt
            .get("filename")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(failure)?;
        if operation.is_empty()
            || operation.len() > 96
            || !operation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || filename != format!("home-{operation}.sealed")
            || receipt
                .get("setup_binding")
                .and_then(serde_json::Value::as_str)
                != Some(cx.declaration.setup_binding.as_str())
        {
            return Err(failure());
        }
        // Refuse missing/redirected files before launching. The helper validates
        // exact ciphertext, limits and authenticated footer.
        let pin = crate::migration::directory(directory)?;
        crate::migration::private(&pin)?;
        let package = directory.join(filename);
        continuation::private_package::<Self>(&package, None)?;
        Ok((package, operation.to_owned()))
    }
    fn settle(_: &Context<Self>, event: HelperEvent) -> Settled {
        match event {
            HelperEvent::Ready { preview } => Settled::Ready {
                proceed: preview.collisions == 0,
                result: serde_json::to_value(preview).unwrap_or_default(),
            },
            HelperEvent::Complete { result }
                if result.complete
                    && result.verified
                    && !result.cancelled
                    && result.failures == 0
                    && result.collisions == 0 =>
            {
                Settled::Complete {
                    result: serde_json::to_value(result).unwrap_or_default(),
                    projection: None,
                }
            }
            HelperEvent::Incomplete { result } => Settled::Incomplete {
                result: serde_json::to_value(result).unwrap_or_default(),
            },
            HelperEvent::Complete { .. } => Settled::Failed,
        }
    }
    fn prepare_root(home: &Path) -> io::Result<()> {
        let mut pin = crate::migration::directory(home)?;
        for segment in [".local", "state", "tilo", "prepared-home"] {
            pin = crate::migration::create(&pin, segment)?;
            crate::migration::private(&pin)?;
        }
        Ok(())
    }
}

/// Fixtures for the tests of this module and of the session that hosts setup.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "test support: a fixture that cannot be written fails the calling test"
)]
pub mod fixtures {
    use super::{PortableHome, Prepared};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    pub fn fixture() -> (tempfile::TempDir, PathBuf, PortableHome) {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join(".local/state/tilo/prepared-setup");
        std::fs::create_dir_all(&state).unwrap();
        for part in [
            ".local",
            ".local/state",
            ".local/state/tilo",
            ".local/state/tilo/prepared-setup",
        ] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let bytes = b"private transport boundary fixture ciphertext";
        for (name, value) in [("prepared-setup.sealed", bytes.to_vec()),("receipt.json", serde_json::to_vec(&serde_json::json!({"schema":1,"sealed_bytes":bytes.len(),"ciphertext_sha256":hex::encode(Sha256::digest(bytes))})).unwrap())] {
            let path=state.join(name);std::fs::write(&path,value).unwrap();std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let identity = home.path().join(".config/tilo/identity");
        std::fs::create_dir_all(&identity).unwrap();
        for part in [".config", ".config/tilo", ".config/tilo/identity"] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        tilo_identity_host::provisioning::create_primary_identity(
            &identity,
            "Already imported",
            [10, 40, 60],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        runtime_identity::set_active_identity(&identity, "primary").unwrap();
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        let declaration = PortableHome {
            schema: 1,
            setup_binding: "installed-operation".into(),
            selection: "full_home".into(),
            applications: false,
        };
        Prepared::open(home.path())
            .unwrap()
            .acknowledge(
                signer.group_public_key(),
                Some(declaration.clone()),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        (home, identity, declaration)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use zeroize::Zeroizing;
    #[test]
    fn authenticated_password_handoff_is_bound_to_exact_setup_and_defer_clears_it() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, identity, declaration) = fixture();
        let digest = Prepared::open(home.path()).unwrap().digest();
        let mut pending = Pending::open(home.path().to_path_buf(), identity, declaration).unwrap();
        let secret = b"previously authenticated setup password";
        assert!(
            pending
                .receive_prepared_password([0; 32], Zeroizing::new(secret.to_vec()))
                .is_err()
        );
        assert!(pending.needs_password());
        pending
            .receive_prepared_password(digest, Zeroizing::new(secret.to_vec()))
            .unwrap();
        assert!(!pending.needs_password());
        assert_eq!(pending.state(), "pending");
        assert!(!pending.busy());
        assert!(
            !pending
                .status()
                .to_string()
                .contains(std::str::from_utf8(secret).unwrap())
        );
        pending.invoke("later", &[]).unwrap();
        assert!(!pending.holds_password());
        let mut reopened = Pending::open(
            pending.context().home.clone(),
            pending.context().identity.clone(),
            pending.context().declaration.clone(),
        )
        .unwrap();
        reopened.invoke("resume", &[]).unwrap();
        assert!(reopened.needs_password());
    }

    #[test]
    fn declared_missing_home_stays_pending_and_explicit_defer_is_not_completion() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, identity, declaration) = fixture();
        let before = std::fs::read(identity.join("primary.json")).unwrap();
        let setup = std::fs::read(
            home.path()
                .join(".local/state/tilo/prepared-setup/prepared-setup.sealed"),
        )
        .unwrap();
        let mut pending =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        assert!(pending.pending());
        assert!(pending.invoke("continue", b"").is_err());
        assert!(pending.invoke("review", b"private setup password").is_err());
        assert_eq!(pending.status()["state"], "unavailable");
        assert!(pending.pending());
        assert!(!pending.busy());
        assert!(
            !pending
                .status()
                .to_string()
                .contains("private setup password")
        );
        assert!(pending.invoke("review", b"private setup password").is_err());
        pending.invoke("later", b"").unwrap();
        assert_eq!(pending.status()["state"], "deferred");
        assert!(!pending.pending());
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root(home.path()).join("choice.json")).unwrap())
                .unwrap();
        assert_eq!(receipt["outcome"], "deferred");
        assert_eq!(
            receipt["declaration"]["setup_binding"],
            declaration.setup_binding
        );
        assert_eq!(
            std::fs::read(identity.join("primary.json")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(
                home.path()
                    .join(".local/state/tilo/prepared-setup/prepared-setup.sealed")
            )
            .unwrap(),
            setup
        );
        let mut resumed = Pending::open(home.path().into(), identity, declaration).unwrap();
        assert!(!resumed.pending());
        resumed.invoke("resume", b"").unwrap();
        assert!(resumed.pending());
        assert!(resumed.invoke("review", b"private setup password").is_err());
        assert!(resumed.pending());
        resumed.invoke("later", b"").unwrap();
        assert_eq!(resumed.status()["state"], "deferred");
    }
    #[test]
    fn declaration_receipt_refuses_wrong_binding_and_unsafe_defer_destination() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, identity, declaration) = fixture();
        let mut wrong = declaration.clone();
        wrong.setup_binding = "unrelated-operation".into();
        assert!(Pending::open(home.path().into(), identity.clone(), wrong).is_err());
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root(home.path())).unwrap();
        assert!(Pending::open(home.path().into(), identity, declaration).is_err());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }
}

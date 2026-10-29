//! First-login continuation for separately selected browser domains.
//! The canonical helper authenticates the carrier and independently verifies the
//! projection. A saved public choice never substitutes for those proofs.
use crate::onboarding_continuation::{self as continuation, Context, Continuation, Kind, Settled};
use crate::onboarding_prepared::{BrowserDeclaration, Pending as Prepared};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) type Pending = Continuation<Browser>;

/// Browser data carried over by a prepared setup.
pub(crate) enum Browser {}

fn failure() -> io::Error {
    continuation::failure::<Browser>()
}
fn root(home: &Path) -> PathBuf {
    continuation::root::<Browser>(home)
}
fn private_file(path: &Path, bound: u64) -> io::Result<Vec<u8>> {
    continuation::private_file::<Browser>(path, bound)
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub(crate) enum Event {
    #[serde(rename = "ready-to-activate")]
    Ready { profiles: usize },
    #[serde(rename = "complete")]
    Complete {
        verified: bool,
        projection: PathBuf,
        identity_public_key: [u8; 32],
        operation: String,
        profiles: usize,
    },
}

/// Authorize the certificate the proof carries against the current person's
/// enrolled device. Returns the person and device public keys; never a key read
/// from the proof itself.
fn authorized_device(identity: &Path, certificate: &[u8]) -> io::Result<([u8; 32], [u8; 32])> {
    let paths = crate::firstboot::collaboration_identity_paths();
    if !tilo_identity_host::provisioning::active_identity_owns_device(identity, &paths)? {
        return Err(failure());
    }
    let active = runtime_identity::resolve_active_identity(identity).ok_or_else(failure)?;
    let metadata = runtime_identity::load_metadata_from_path(&active).map_err(|_| failure())?;
    let cert =
        tilo_user_identity::persist::read_device_cert(&paths.device_cert).map_err(|_| failure())?;
    let proof_cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(certificate).map_err(|_| failure())?;
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| failure())?
            .as_nanos(),
    )
    .map_err(|_| failure())?;
    tilo_user_identity::verify_device_cert_at(&proof_cert, now).map_err(|_| failure())?;
    // The proof is the group's own certificate for this very device.
    let group_signed = proof_cert.user_public_key == metadata.group_public_key;
    let this_device = proof_cert.device_public_key == cert.device_public_key;
    if !group_signed || !this_device {
        return Err(failure());
    }
    let fleet = crate::device_fleet::DeviceFleetStore::at(identity, paths)
        .load(&hex::encode(metadata.group_public_key))?;
    if fleet.is_revoked(&cert.device_public_key) {
        return Err(failure());
    }
    Ok((metadata.group_public_key, cert.device_public_key))
}

fn completion_projection(
    home: &Path,
    identity: &Path,
    declaration: &BrowserDeclaration,
    digest: [u8; 32],
) -> io::Result<Option<PathBuf>> {
    let receipt = root(home).join("completion.json");
    let proof = match browser_continuity::completion::Completion::read(&receipt) {
        Ok(proof) => proof,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let (person, device) = authorized_device(identity, proof.certificate())?;
    if person != declaration.plan.identity_public_key {
        return Err(failure());
    }
    proof.verify_binding(
        &declaration.plan,
        digest,
        declaration.payload.ciphertext_sha256,
        &declaration.payload.operation,
    )?;
    let projection = proof.verify_projection(device)?;
    if !valid_projection(home, &declaration.payload.operation, &projection) {
        return Err(failure());
    }
    Ok(Some(projection))
}

/// The deferred same-computer route has no installation ciphertext. Its proof
/// binds the reviewed migration operation and the exact projection the Browser
/// app will import, and the certificate still comes from the active person's
/// enrolled device.
pub(crate) fn deferred_projection(home: &Path, identity: &Path) -> io::Result<Option<PathBuf>> {
    let base = home.join(".local/state/tilo/browser-continuity");
    let pin = match crate::onboarding_migration::directory(&base) {
        Ok(pin) => pin,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    crate::onboarding_migration::private(&pin)?;
    let mut found = None;
    for entry in std::fs::read_dir(&base)?.take(64) {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if uuid::Uuid::parse_str(&name).is_err() {
            continue;
        }
        let operation_root = base.join(&name);
        let receipt = operation_root.join(browser_continuity::completion::FILE_NAME);
        let proof = match browser_continuity::completion::Completion::read(&receipt) {
            Ok(proof) => proof,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let operation = proof.operation();
        // The receipt belongs to the canonical directory derived from its
        // reviewed migration operation; a receipt copied elsewhere is refused.
        if browser_continuity::completion::deferred_root(home, operation) != operation_root {
            // A completion in this root belongs to the installation-media
            // route, not the deferred one; leave it to that consumer.
            continue;
        }
        let plan_file = home
            .join(".local/state/tilo/migration-handoff")
            .join(format!("{operation}.migration.json"));
        match private_file(&plan_file, 9 * 1024 * 1024) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        let (person, device) = authorized_device(identity, proof.certificate())?;
        if person != proof.plan().identity_public_key {
            continue;
        }
        let plan_bytes = proof.plan().encode().map_err(|_| failure())?;
        let browser_digest: [u8; 32] = Sha256::digest(&plan_bytes).into();
        proof.verify_binding(
            proof.plan(),
            browser_continuity::completion::deferred_setup_digest(operation),
            browser_digest,
            operation,
        )?;
        let projection = proof.verify_projection(device)?;
        if projection
            != browser_continuity::completion::deferred_projection(
                &browser_continuity::completion::deferred_root(home, operation),
            )
            || !valid_deferred_projection(home, &projection)
        {
            return Err(failure());
        }
        if found.replace(projection).is_some() {
            return Err(failure());
        }
    }
    Ok(found)
}

/// Recheck the signed deferred proof and attach it to the Browser launch.
pub(crate) fn attach_deferred_continuity() {
    let Some(home) = crate::onboarding::session_home() else {
        return;
    };
    let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env() else {
        return;
    };
    if let Ok(Some(path)) = deferred_projection(&home, store.identity_dir()) {
        crate::shell::launcher::set_browser_continuity(Some(path));
    }
}

fn valid_deferred_projection(home: &Path, path: &Path) -> bool {
    let base = home.join(".local/state/tilo/browser-continuity");
    let Ok(relative) = path.strip_prefix(&base) else {
        return false;
    };
    let parts = relative.components().collect::<Vec<_>>();
    let [
        std::path::Component::Normal(operation),
        std::path::Component::Normal(container),
        std::path::Component::Normal(import),
        std::path::Component::Normal(data),
    ] = parts.as_slice()
    else {
        return false;
    };
    operation
        .to_str()
        .is_some_and(|operation| uuid::Uuid::parse_str(operation).is_ok())
        && container.to_str() == Some(browser_continuity::completion::DEFERRED_CONTAINER)
        && import.to_str() == Some("import")
        && data.to_str() == Some("home")
        && crate::onboarding_migration::directory(path)
            .is_ok_and(|pin| crate::onboarding_migration::private(&pin).is_ok())
}
fn valid_projection(home: &Path, operation: &str, path: &Path) -> bool {
    let workspace = home
        .join(".local/state/tilo/browser-continuity")
        .join(operation);
    path.file_name().is_some_and(|name| name == "home")
        && path.parent().is_some_and(|parent| {
            parent.file_name().is_some_and(|name| name == "import")
                && parent.parent().is_some_and(|container| {
                    container.parent() == Some(workspace.as_path())
                        && container
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("browser-import-"))
                })
        })
        && crate::onboarding_migration::directory(path)
            .is_ok_and(|pin| crate::onboarding_migration::private(&pin).is_ok())
}
/// Recheck current-person/device authority and the signed exact projection on every launch.
pub(crate) fn validate_published_projection(path: &Path) -> bool {
    let Some(home) = crate::onboarding::session_home() else {
        return false;
    };
    let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env() else {
        return false;
    };
    let identity = store.identity_dir();
    if deferred_projection(&home, identity).is_ok_and(|found| found.as_deref() == Some(path)) {
        return true;
    }
    let Ok(prepared) = Prepared::open(&home) else {
        return false;
    };
    let Ok(Some(declaration)) = prepared.browser(identity) else {
        return false;
    };
    completion_projection(&home, identity, &declaration, prepared.digest())
        .is_ok_and(|verified| verified.as_deref() == Some(path))
}
impl Kind for Browser {
    type Declaration = BrowserDeclaration;
    type Event = Event;
    const FAILURE: &'static str = "Browser data are pending or unavailable";
    const ROOT: &'static str = "prepared-browser";
    const RECEIPT_BOUND: u64 = 24576;
    const LINE_BOUND: usize = 24576;
    const HELPER_FLAG: &'static str = "--browser-transfer";
    const WORKSPACE: &'static str = "browser-continuity";
    const WORKSPACE_NAMESPACE: bool = true;
    const OUTPUT_BOUND: usize = 64 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(1800);
    const READY: &'static str = "reviewed";
    const PROCEED: &'static str = "start";
    const PROCEED_ACTION: &'static str = "activate";
    const PROCEEDING: &'static str = "activating-browser";
    const CONTINUES_WHEN_COMPLETE: bool = true;
    const FORGETS_PASSWORD_ON_FAILURE: bool = false;
    const FAILURE_CLEARS_RESULT: bool = true;
    const MONOTONIC_RECEIPTS: bool = false;
    const COMPLETION_NEEDS_PROOF: bool = true;

    fn declared(prepared: &Prepared, identity: &Path) -> io::Result<Option<BrowserDeclaration>> {
        prepared.browser(identity)
    }
    fn valid(declaration: &BrowserDeclaration) -> bool {
        declaration.valid()
    }
    fn operation(declaration: &BrowserDeclaration) -> &str {
        &declaration.payload.operation
    }
    fn mark(_: &BrowserDeclaration, status: &mut serde_json::Map<String, serde_json::Value>) {
        status.insert("browser".to_owned(), true.into());
    }
    fn package(cx: &Context<Self>, directory: &Path) -> io::Result<(PathBuf, String)> {
        let expected: crate::onboarding_prepared::BrowserPayload =
            serde_json::from_slice(&private_file(&directory.join("expectation.json"), 24576)?)
                .map_err(|_| failure())?;
        if expected != cx.declaration.payload {
            return Err(failure());
        }
        let package = directory.join(&cx.declaration.payload.filename);
        continuation::private_package::<Self>(&package, Some(cx.declaration.payload.sealed_bytes))?;
        Ok((package, cx.declaration.payload.operation.clone()))
    }
    fn settle(cx: &Context<Self>, event: Event) -> Settled {
        let declared_profiles = cx.declaration.plan.profiles.len();
        match event {
            Event::Ready { profiles } if profiles == declared_profiles => Settled::Ready {
                result: serde_json::json!({"profiles": profiles}),
                proceed: true,
            },
            Event::Complete {
                verified: true,
                projection,
                identity_public_key,
                operation,
                profiles,
            } if identity_public_key == cx.declaration.plan.identity_public_key
                && operation == cx.declaration.payload.operation
                && profiles == declared_profiles
                && valid_projection(&cx.home, &operation, &projection)
                && completion_projection(&cx.home, &cx.identity, &cx.declaration, cx.digest)
                    .is_ok_and(|verified| verified.as_deref() == Some(projection.as_path())) =>
            {
                Settled::Complete {
                    result: serde_json::json!({"profiles":profiles,"verified":true}),
                    projection: Some(projection),
                }
            }
            _ => Settled::Failed,
        }
    }
    fn verified_completion(cx: &Context<Self>) -> io::Result<Option<PathBuf>> {
        completion_projection(&cx.home, &cx.identity, &cx.declaration, cx.digest)
    }
}

impl Pending {
    pub(crate) fn verified_projection(&self) -> Option<&Path> {
        self.projection().filter(|path| {
            self.state() == "complete"
                && valid_projection(&self.context().home, self.operation(), path)
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    use zeroize::Zeroizing;

    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf, BrowserDeclaration) {
        let (home, identity, _) = crate::onboarding_portable_home::tests::fixture();
        let prepared = Prepared::open(home.path()).unwrap();
        std::fs::remove_file(
            home.path()
                .join(".local/state/tilo/prepared-setup/activated.json"),
        )
        .unwrap();
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        let plan = browser_continuity::Plan::new(
            "browser-binding".into(),
            signer.group_public_key(),
            vec![
                browser_continuity::Profile::new(
                    "a".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let declaration = BrowserDeclaration {
            payload: crate::onboarding_prepared::BrowserPayload {
                schema: 1,
                filename: format!("browser-{operation}.sealed"),
                operation,
                purpose: "browser_profiles".into(),
                setup_binding: plan.setup_binding.clone(),
                sealed_bytes: 18,
                plaintext_bytes: 16,
                entries: 2,
                ciphertext_sha256: [2; 32],
                plan_sha256: Sha256::digest(plan.encode().unwrap()).into(),
                limits: crate::onboarding_prepared::ApplicationLimits {
                    plaintext_bytes: 1024,
                    entries: 64,
                    metadata_bytes: 1024,
                },
            },
            plan,
        };
        prepared
            .acknowledge(
                signer.group_public_key(),
                None,
                None,
                Some(declaration.clone()),
                None,
                None,
            )
            .unwrap();
        let directory = root(home.path());
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (name, bytes) in [
            (
                "expectation.json".to_owned(),
                serde_json::to_vec(&declaration.payload).unwrap(),
            ),
            (
                declaration.payload.filename.clone(),
                b"fixture ciphertext".to_vec(),
            ),
        ] {
            let path = directory.join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        (home, identity, declaration)
    }
    pub(crate) fn certified_projection(
        home: &Path,
        identity: &Path,
        declaration: &BrowserDeclaration,
    ) -> PathBuf {
        let paths = crate::firstboot::collaboration_identity_paths();
        crate::firstboot::ensure_collaboration_identity(identity, &paths).unwrap();
        let projection = home
            .join(".local/state/tilo/browser-continuity")
            .join(&declaration.payload.operation)
            .join("browser-import-fixture/import/home");
        let profile = projection
            .join("profiles")
            .join(&declaration.plan.profiles.first().unwrap().id);
        std::fs::create_dir_all(&profile).unwrap();
        let mut current = profile.as_path();
        while current != home {
            std::fs::set_permissions(current, std::fs::Permissions::from_mode(0o700)).unwrap();
            current = current.parent().unwrap();
        }
        for (path, bytes) in [
            (
                projection.join("plan.json"),
                declaration.plan.encode().unwrap(),
            ),
            (
                profile.join("Bookmarks"),
                br#"{"roots":{"bookmark_bar":{"type":"folder","name":"Bookmarks","children":[]}}}"#
                    .to_vec(),
            ),
        ] {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let digest = Prepared::open(home).unwrap().digest();
        let unsigned = browser_continuity::completion::capture(
            declaration.plan.clone(),
            digest,
            declaration.payload.ciphertext_sha256,
            declaration.payload.operation.clone(),
            projection.clone(),
        )
        .unwrap();
        unsigned.verify_unchanged().unwrap();
        let key = tilo_user_identity::persist::load_device_key(&paths).unwrap();
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        let proof = unsigned
            .sign(&key, serde_json::to_vec(&cert).unwrap())
            .unwrap();
        let receipt = root(home).join("completion.json");
        std::fs::write(&receipt, proof.encode().unwrap()).unwrap();
        std::fs::set_permissions(receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
        projection
    }
    pub(crate) fn deferred_projection_fixture(home: &Path, identity: &Path) -> (PathBuf, PathBuf) {
        let paths = crate::firstboot::collaboration_identity_paths();
        crate::firstboot::ensure_collaboration_identity(identity, &paths).unwrap();
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let plans = home.join(".local/state/tilo/migration-handoff");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::set_permissions(&plans, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan_file = plans.join(format!("{operation}.migration.json"));
        std::fs::write(
            &plan_file,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "operation_id": operation,
                "categories": {"home": true, "flatpak": false, "accounts": false},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&plan_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let plan = browser_continuity::Plan::new(
            format!("deferred:{operation}"),
            signer.group_public_key(),
            vec![
                browser_continuity::Profile::new(
                    "b".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let root = browser_continuity::completion::deferred_root(home, &operation);
        let projection = browser_continuity::completion::deferred_projection(&root);
        let profile = projection
            .join("profiles")
            .join(&plan.profiles.first().unwrap().id);
        std::fs::create_dir_all(&profile).unwrap();
        let mut current = profile.as_path();
        while current != home {
            std::fs::set_permissions(current, std::fs::Permissions::from_mode(0o700)).unwrap();
            current = current.parent().unwrap();
        }
        for (path, bytes) in [
            (projection.join("plan.json"), plan.encode().unwrap()),
            (
                profile.join("Bookmarks"),
                br#"{"roots":{"bookmark_bar":{"type":"folder","name":"Bookmarks","children":[]}}}"#
                    .to_vec(),
            ),
        ] {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let setup_digest = browser_continuity::completion::deferred_setup_digest(&operation);
        let browser_digest: [u8; 32] = Sha256::digest(plan.encode().unwrap()).into();
        let unsigned = browser_continuity::completion::capture(
            plan,
            setup_digest,
            browser_digest,
            operation,
            projection.clone(),
        )
        .unwrap();
        let key = tilo_user_identity::persist::load_device_key(&paths).unwrap();
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        let proof = unsigned
            .sign(&key, serde_json::to_vec(&cert).unwrap())
            .unwrap();
        let receipt = root.join(browser_continuity::completion::FILE_NAME);
        std::fs::write(&receipt, proof.encode().unwrap()).unwrap();
        std::fs::set_permissions(receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
        (root, projection)
    }

    #[test]
    fn deferred_first_launch_projection_is_attached_only_with_its_signed_proof() {
        let (home, identity, _declaration) = fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        env.set("TILO_IDENTITY_ROOT", home.path().join("system"));
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (root, projection) = deferred_projection_fixture(home.path(), &identity);
        assert_eq!(
            deferred_projection(home.path(), &identity).unwrap(),
            Some(projection.clone())
        );
        assert!(validate_published_projection(&projection));
        assert!(
            !std::fs::read(root.join(browser_continuity::completion::FILE_NAME))
                .unwrap()
                .is_empty()
        );
        let bookmarks = projection
            .join("profiles")
            .join(
                &browser_continuity::Plan::decode(
                    &std::fs::read(projection.join("plan.json")).unwrap(),
                )
                .unwrap()
                .profiles
                .first()
                .unwrap()
                .id,
            )
            .join("Bookmarks");
        std::fs::write(&bookmarks, b"changed after completion").unwrap();
        assert!(
            deferred_projection(home.path(), &identity).is_err(),
            "a changed projection must not be attached"
        );
        assert!(!validate_published_projection(&projection));
    }

    fn wait(pending: &mut Pending) {
        tilo_test_env::wait_until("the worker to finish", || {
            if !pending.busy() {
                return true;
            }
            pending.poll();
            false
        });
    }
    #[test]
    fn authenticated_browser_protocol_requires_exact_verified_projection_and_rechecks_reopen() {
        let (home, identity, declaration) = fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        env.set("TILO_IDENTITY_ROOT", home.path().join("system"));
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let mut pending =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        let prepared = Prepared::open(home.path()).unwrap();
        pending
            .receive_prepared_password(
                prepared.digest(),
                Zeroizing::new(b"private password".to_vec()),
            )
            .unwrap();
        pending.manual();
        let helper = home.path().join("inert-browser-helper");
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"ready-to-activate\",\"profiles\":1}'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        pending.run_with("review", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "reviewed");
        assert!(pending.verified_projection().is_none());
        let projection = home
            .path()
            .join(".local/state/tilo/browser-continuity")
            .join(&declaration.payload.operation)
            .join("browser-import-fixture/import/home");
        std::fs::create_dir_all(&projection).unwrap();
        for path in [
            projection.clone(),
            projection.parent().unwrap().to_path_buf(),
            projection.parent().unwrap().parent().unwrap().to_path_buf(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut result = serde_json::json!({"event":"complete","verified":true,"projection":projection,"identity_public_key":vec![0u8;32],"operation":declaration.payload.operation,"profiles":1});
        let emit = |result: &serde_json::Value| {
            std::fs::write(
                &helper,
                format!(
                    "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{}'\n",
                    serde_json::to_string(result).unwrap()
                ),
            )
            .unwrap();
        };
        emit(&result);
        pending.run_with("activate", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "failed");
        assert!(pending.verified_projection().is_none());
        assert!(!root(home.path()).join("choice.json").exists());
        result["identity_public_key"] = serde_json::json!(declaration.plan.identity_public_key);
        emit(&result);
        pending.run_with("activate", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(
            pending.status()["state"],
            "failed",
            "verified stdout is not a signed completion proof"
        );
        assert!(pending.verified_projection().is_none());
        assert_eq!(
            certified_projection(home.path(), &identity, &declaration),
            projection
        );
        emit(&result);
        pending.run_with("activate", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.verified_projection(), Some(projection.as_path()));
        assert!(!pending.holds_password());
        let reopened = Pending::open(home.path().into(), identity, declaration).unwrap();
        assert!(!reopened.pending());
        assert_eq!(reopened.verified_projection(), Some(projection.as_path()));
        assert_eq!(
            std::fs::read(root(home.path()).join(&pending.context().declaration.payload.filename))
                .unwrap(),
            b"fixture ciphertext"
        );
    }
}

//! The plans for original-file custody a migration leaves behind: what the
//! person chose to keep from the system Tilo replaced, read back only from
//! private files bound to the active identity, and the receipts that
//! acknowledge exactly one plan.
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
pub const BOUND: u64 = 64 * 1024;
pub fn failure() -> io::Error {
    io::Error::other("Original files are unavailable")
}
pub fn plans(home: &Path) -> PathBuf {
    home.join(".local/state/tilo/migration-handoff")
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
pub fn read_private(path: &Path) -> io::Result<Vec<u8>> {
    let pin = crate::migration::directory(path.parent().ok_or_else(failure)?)?;
    crate::migration::private(&pin)?;
    let file =
        crate::migration::child(&pin, path.file_name().ok_or_else(failure)?, libc::O_RDONLY)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.len() > BOUND
    {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take(BOUND + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > BOUND {
        return Err(failure());
    }
    Ok(bytes)
}
fn active_key() -> io::Result<[u8; 32]> {
    let directory = runtime_identity::identity_dir_from_env()?;
    let active = runtime_identity::resolve_active_identity(&directory).ok_or_else(failure)?;
    runtime_identity::load_from_path(active)
        .map(|signer| signer.group_public_key())
        .map_err(|_| failure())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub operation: String,
    pub digest: String,
    pub applications: bool,
    pub identity_public_key: [u8; 32],
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Copy,
    Shared,
    Applications,
}
pub fn intent(home: &Path, operation: &str) -> io::Result<Intent> {
    if !valid_id(operation) {
        return Err(failure());
    }
    let plan: serde_json::Value = serde_json::from_slice(&read_private(
        &plans(home).join(format!("{operation}.migration.json")),
    )?)?;
    match plan
        .get("home_policy")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("copy")
    {
        "access-read-write" => Ok(Intent::Shared),
        "copy"
            if plan
                .get("categories")
                .and_then(|value| value.get("home"))
                .and_then(serde_json::Value::as_bool)
                == Some(false)
                && plan
                    .get("categories")
                    .and_then(|value| value.get("flatpak"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true) =>
        {
            Ok(Intent::Applications)
        }
        "copy" => Ok(Intent::Copy),
        _ => Err(failure()),
    }
}
pub fn request(home: &Path, operation: &str) -> io::Result<Option<Request>> {
    if !valid_id(operation) {
        return Err(failure());
    }
    let bytes = read_private(&plans(home).join(format!("{operation}.migration.json")))?;
    let plan: serde_json::Value = serde_json::from_slice(&bytes)?;
    let policy = plan
        .get("home_policy")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("copy");
    if policy == "copy" {
        return Ok(None);
    }
    if policy != "access-read-write"
        || plan.get("operation_id").and_then(serde_json::Value::as_str) != Some(operation)
        || plan
            .get("categories")
            .and_then(|v| v.get("home"))
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err(failure());
    }
    let pending_scope = plan.get("pending_computer_applications_scope").is_some();
    let legacy_applications = plan
        .get("categories")
        .and_then(|value| value.get("flatpak"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if legacy_applications && !pending_scope {
        // The old shared-app plan has no registered independent copy source;
        // its former rootless importer cannot establish that missing authority.
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Installed application transfer declaration is unavailable",
        ));
    }
    let expected = Request {
        operation: operation.into(),
        digest: hex::encode(Sha256::digest(&bytes)),
        identity_public_key: active_key()?,
        // A composite keeps Flatpaks out of the shared Home review; its
        // independently registered apps declaration is a separate transfer.
        applications: pending_scope
            || plan
                .get("categories")
                .and_then(|value| value.get("flatpak"))
                .and_then(serde_json::Value::as_bool)
                == Some(true),
    };
    if pending_scope {
        // Presence alone cannot bypass migration: malformed or substituted
        // pending metadata refuses instead of falling through to RW access.
        pending_applications_operation(home, &expected)?;
    }
    Ok(Some(expected))
}
/// Descriptive installed-plan routing only; the root independently admits the
/// registered apps-only declaration and actual source/caller/setup bindings.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingComputerApplicationsScope {
    schema: u32,
    source_operation: String,
    applications_operation: String,
    staging_operation: String,
    staging_plan_sha256: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
}
pub fn pending_applications_operation(home: &Path, expected: &Request) -> io::Result<String> {
    let bytes = read_private(&plans(home).join(format!("{}.migration.json", expected.operation)))?;
    if !expected.applications || hex::encode(Sha256::digest(&bytes)) != expected.digest {
        return Err(failure());
    }
    let plan: serde_json::Value = serde_json::from_slice(&bytes)?;
    let scope: PendingComputerApplicationsScope = serde_json::from_value(
        plan.get("pending_computer_applications_scope")
            .cloned()
            .ok_or_else(failure)?,
    )?;
    let canonical = |value: &str| {
        uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
    };
    if scope.schema != 1
        || scope.source_operation != expected.operation
        || scope.identity_public_key != expected.identity_public_key
        || !canonical(&scope.source_operation)
        || !canonical(&scope.applications_operation)
        || !canonical(&scope.staging_operation)
        || scope.staging_operation == scope.source_operation
        || scope.staging_operation == scope.applications_operation
        || scope.source_operation == scope.applications_operation
        || scope.staging_plan_sha256.len() != 64
        || !scope
            .staging_plan_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || scope.setup_binding.is_empty()
        || scope.setup_binding.len() > 256
        || scope.setup_binding.chars().any(char::is_control)
    {
        return Err(failure());
    }
    Ok(scope.staging_operation)
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    operation: String,
    plan_sha256: String,
    identity_public_key: [u8; 32],
    outcome: String,
}
pub fn acknowledged(home: &Path, operation: &str) -> io::Result<bool> {
    let path = plans(home).join(format!("{operation}.access.json"));
    let bytes = match read_private(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    let expected = request(home, operation)?.ok_or_else(failure)?;
    if receipt.schema != 1
        || receipt.operation != operation
        || receipt.outcome != "access-read-write"
        || receipt.plan_sha256 != expected.digest
        || receipt.identity_public_key != expected.identity_public_key
    {
        return Err(failure());
    }
    Ok(true)
}
pub fn acknowledge(home: &Path, expected: &Request) -> io::Result<()> {
    if request(home, &expected.operation)?.as_ref() != Some(expected) {
        return Err(failure());
    }
    if acknowledged(home, &expected.operation)? {
        return Ok(());
    }
    let directory = plans(home);
    let pin = crate::migration::directory(&directory)?;
    crate::migration::private(&pin)?;
    let receipt = Receipt {
        schema: 1,
        operation: expected.operation.clone(),
        plan_sha256: expected.digest.clone(),
        identity_public_key: expected.identity_public_key,
        outcome: "access-read-write".into(),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(&serde_json::to_vec(&receipt)?)?;
    temporary.as_file().sync_all()?;
    if request(home, &expected.operation)?.as_ref() != Some(expected) {
        return Err(failure());
    }
    temporary
        .persist_noclobber(directory.join(format!("{}.access.json", expected.operation)))
        .map_err(|_| failure())?;
    pin.sync_all()
}
#[cfg(test)]
mod tests {
    #[test]
    fn pending_composite_scope_routes_staging_and_refuses_mutated_binding() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(plans(home.path())).unwrap();
        std::fs::set_permissions(plans(home.path()), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let source = uuid::Uuid::new_v4().to_string();
        let apps = uuid::Uuid::new_v4().to_string();
        let stage = uuid::Uuid::new_v4().to_string();
        let mut plan = serde_json::json!({"pending_computer_applications_scope":{
            "schema":1,"source_operation":source,"applications_operation":apps,
            "staging_operation":stage,"staging_plan_sha256":"a".repeat(64),
            "setup_binding":"owned setup fixture","identity_public_key":vec![7u8;32]
        }});
        let path = plans(home.path()).join(format!("{source}.migration.json"));
        let write = |plan: &serde_json::Value| {
            let bytes = serde_json::to_vec(plan).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            Request {
                operation: source.clone(),
                digest: hex::encode(Sha256::digest(bytes)),
                applications: true,
                identity_public_key: [7; 32],
            }
        };
        let expected = write(&plan);
        assert_eq!(
            pending_applications_operation(home.path(), &expected).unwrap(),
            stage
        );
        for (field, value) in [
            (
                "source_operation",
                serde_json::json!(uuid::Uuid::new_v4().to_string()),
            ),
            ("staging_operation", serde_json::json!(source)),
            ("identity_public_key", serde_json::json!(vec![8u8; 32])),
            ("staging_plan_sha256", serde_json::json!("a".repeat(63))),
            ("extra", serde_json::json!(true)),
        ] {
            let mut altered = plan.clone();
            altered["pending_computer_applications_scope"][field] = value;
            let current = write(&altered);
            assert!(pending_applications_operation(home.path(), &current).is_err());
        }
        plan["pending_computer_applications_scope"]["setup_binding"] =
            serde_json::json!("changed setup");
        write(&plan);
        assert!(pending_applications_operation(home.path(), &expected).is_err());
    }

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture(
        policy: Option<&str>,
        apps: bool,
        env: &mut tilo_test_env::ScopedEnv,
    ) -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
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
            "Source owner",
            [20, 30, 40],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        runtime_identity::set_active_identity(&identity, "primary").unwrap();
        let directory = plans(home.path());
        std::fs::create_dir_all(&directory).unwrap();
        for part in [
            ".local",
            ".local/state",
            ".local/state/tilo",
            ".local/state/tilo/migration-handoff",
        ] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let mut value = serde_json::json!({"schema":1,"operation_id":"test-operation","categories":{"home":true,"flatpak":apps,"accounts":false},"home":{}});
        if let Some(policy) = policy {
            value["home_policy"] = policy.into();
        }
        let path = directory.join("test-operation.migration.json");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (home, path)
    }
    #[test]
    fn composite_review_keeps_rw_queued_until_same_staging_cleanup_finishes() {
        use launcher::retained_view::installed_copy::CopyOwnerEvent;
        let mut env = tilo_test_env::ScopedEnv::new();
        let (home, old_path) = fixture(Some("access-read-write"), false, &mut env);
        let source = uuid::Uuid::new_v4().to_string();
        let stage = uuid::Uuid::new_v4().to_string();
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&old_path).unwrap()).unwrap();
        value["operation_id"] = serde_json::json!(source);
        value["pending_computer_applications_scope"] = serde_json::json!({
            "schema":1,"source_operation":source,
            "applications_operation":uuid::Uuid::new_v4().to_string(),
            "staging_operation":stage,"staging_plan_sha256":"a".repeat(64),
            "setup_binding":"owned setup fixture","identity_public_key":active_key().unwrap(),
        });
        let path = plans(home.path()).join(format!("{source}.migration.json"));
        std::fs::rename(&old_path, &path).unwrap();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert_eq!(value["categories"]["home"], true);
        assert_eq!(value["categories"]["flatpak"], false);
        let pending = request(home.path(), &source).unwrap().unwrap();
        assert!(
            pending.applications,
            "separate pending apps scope is independent of Home categories"
        );
        let mut migration = crate::migration::Migration::open(home.path().into());
        migration.invoke("review", Some(&source)).unwrap();
        let copy = migration.take_original_copy_request().unwrap();
        assert_eq!(copy.operation, stage);
        assert!(migration.take_access_request().is_none());
        assert!(migration.invoke("continue", None).is_err());
        copy.events
            .send(CopyOwnerEvent::Complete {
                operation: stage.clone(),
                home_cipher_sha256: None,
                applications_cipher_sha256: Some("b".repeat(64)),
            })
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["state"], "releasing-applications");
        assert!(migration.take_access_request().is_none());
        copy.events
            .send(CopyOwnerEvent::CleanupPending {
                operation: stage.clone(),
                message: "actual source retirement pending".into(),
                resumable: true,
            })
            .unwrap();
        assert!(migration.poll());
        assert!(migration.take_access_request().is_none());
        assert_eq!(migration.next_action(), "cleanup-resume");
        // This tests the real onboarding controller's receipt consumption,
        // not source authority: the driver separately verifies actual cleanup.
        copy.events
            .send(CopyOwnerEvent::CleanupFinished {
                operation: stage,
                metadata_quarantined: false,
            })
            .unwrap();
        assert!(migration.poll());
        let access = migration.take_access_request().unwrap();
        assert_eq!(access.operation, source);
        assert!(access.applications);
        assert_eq!(access.digest, hex::encode(Sha256::digest(&original)));
        assert_eq!(migration.status()["state"], "reviewing");
        assert_eq!(migration.status()["cleanup_state"], "finished");
        assert!(migration.invoke("continue", None).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert!(!home.path().join(".local/state/tilo/migration").exists());
        value["pending_computer_applications_scope"]["source_operation"] =
            serde_json::json!(uuid::Uuid::new_v4().to_string());
        let path = plans(home.path()).join(format!("{source}.migration.json"));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            request(home.path(), &source).is_err(),
            "malformed pending scope cannot become direct RW access"
        );
        let mut changed = crate::migration::Migration::open(home.path().into());
        assert!(changed.invoke("review", Some(&source)).is_err());
        assert!(changed.take_access_request().is_none());
    }

    #[test]
    fn legacy_shared_flatpak_without_registered_scope_refuses_before_rw() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let (home, path) = fixture(Some("access-read-write"), true, &mut env);
        let original = std::fs::read(&path).unwrap();
        let error = request(home.path(), "test-operation").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "Installed application transfer declaration is unavailable"
        );
        let mut migration = crate::migration::Migration::open(home.path().into());
        let error = migration
            .invoke("review", Some("test-operation"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(migration.take_access_request().is_none());
        assert!(migration.take_original_copy_request().is_none());
        assert!(!acknowledged(home.path(), "test-operation").unwrap());
        assert!(!home.path().join(".local/state/tilo/migration").exists());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn legacy_copy_does_not_request_original_access_and_access_never_claims_copy_completion() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let (legacy, _) = fixture(None, false, &mut env);
        assert!(request(legacy.path(), "test-operation").unwrap().is_none());
        let (home, path) = fixture(Some("access-read-write"), false, &mut env);
        let original = std::fs::read(&path).unwrap();
        let mut migration = crate::migration::Migration::open(home.path().into());
        migration.invoke("review", Some("test-operation")).unwrap();
        let queued = migration.take_access_request().unwrap();
        assert_eq!(queued.operation, "test-operation");
        assert_eq!(queued.digest, hex::encode(Sha256::digest(&original)));
        assert!(!queued.applications);
        assert_eq!(migration.status()["state"], "reviewing");
        assert!(migration.invoke("start", None).is_err());
        assert!(migration.invoke("continue", None).is_err());
        assert!(!acknowledged(home.path(), "test-operation").unwrap());
        migration.invoke("cancel", None).unwrap();
        assert_eq!(
            migration.take_access_release().as_deref(),
            Some("test-operation")
        );
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert!(
            !home.path().join(".local/state/tilo/migration").exists(),
            "queueing access must not launch copying or adopt state"
        );
    }
    #[test]
    fn access_receipt_is_bound_to_exact_private_plan_and_refuses_redirected_source() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let (home, path) = fixture(Some("access-read-write"), false, &mut env);
        let expected = request(home.path(), "test-operation").unwrap().unwrap();
        // Exercise receipt custody only; this does not fabricate a live actor or authentication.
        acknowledge(home.path(), &expected).unwrap();
        assert!(acknowledged(home.path(), "test-operation").unwrap());
        let (_other, _) = fixture(None, false, &mut env);
        assert!(
            acknowledged(home.path(), "test-operation").is_err(),
            "another active identity must not inherit original-file consent"
        );
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        let original = std::fs::read(&path).unwrap();
        let mut changed = original.clone();
        changed.push(b' ');
        std::fs::write(&path, &changed).unwrap();
        assert!(acknowledged(home.path(), "test-operation").is_err());
        assert!(acknowledge(home.path(), &expected).is_err());
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(external.path(), &original).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(external.path(), &path).unwrap();
        assert!(request(home.path(), "test-operation").is_err());
        assert_eq!(std::fs::read(external.path()).unwrap(), original);
    }
}

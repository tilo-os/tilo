use super::*;
use tilo_user_identity::DeviceCert as PinnedCert;
use tilo_user_identity::persist::IdentityPaths;

struct Fixture {
    joined: Joined,
    directory: tempfile::TempDir,
    /// This thread's profile, pinned for as long as the fixture lives: some
    /// tests leave the thread scoped to a profile whose registry names no
    /// identity, and reading ownership from disk needs one.
    _profile: tilo_paths::ProfileScope,
    workspace: WorkspaceId,
    ended: Arc<SessionAdmission>,
    other: Arc<SessionAdmission>,
}

impl Fixture {
    async fn new() -> Self {
        // A profile of this test's own that names the fixture's identity:
        // reading the ownership from disk selects it, and nothing here can
        // reach the machine's own identity store.
        let profile = tilo_paths::scope_profile(std::env::temp_dir());

        let workspace = WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let (joined, directory) = Joined::test_hosting(workspace).await;
        let joined =
            Arc::try_unwrap(joined).unwrap_or_else(|_| panic!("fixture holds no sessions"));
        let actor = tilo_action_broker::collab_app_broker::person_id_for_root(
            joined.identity.device_cert.user_public_key,
        );
        let admit = || {
            joined
                .admit_mcp_execution(&workspace.to_string(), &actor)
                .unwrap()
        };
        let (ended, other) = (admit(), admit());
        Self {
            joined,
            directory,
            _profile: profile,
            workspace,
            ended,
            other,
        }
    }

    /// The registry's live admissions, then one of them ends normally before
    /// they are rechecked.
    fn snapshot_then_end_one(&self) -> Vec<Arc<SessionAdmission>> {
        let admissions = self.joined.executions.admissions(self.workspace).unwrap();
        assert_eq!(admissions.len(), 2);
        self.ended.caller().retire_execution();
        admissions
    }

    /// Relabel the device certificate without signing it again: the same
    /// pinned material, a signature that no longer verifies.
    fn install_device_cert_label(&self, label: &str) {
        let pinned = unsafe { &*Arc::as_ptr(&self.other) };
        let identity = std::ptr::from_ref::<PinnedCert>(pinned.certificate());
        // SAFETY: the fixture holds the only admissions, and this runs
        // before any of them is checked.
        unsafe { (*(identity.cast_mut())).device_label = label.into() };
    }

    /// Name the fixture's identity in this profile, so an ownership read from
    /// disk selects it (and the fleet, which holds the certificate).
    fn install_identity(&self, paths: &IdentityPaths) {
        let cert = &self.joined.identity.device_cert;
        let id = hex::encode(cert.user_public_key);
        // Both copies a provisioned device holds: the identity's own and the
        // device's, which reading the ownership looks through.
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&cert.user_public_key),
            cert,
        )
        .unwrap();
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, cert).unwrap();
        let store = tilo_identity_host::registry::IdentityRegistryStore::from_env().unwrap();
        let mut registry = store.load_reconciled().unwrap();
        registry.default_identity = Some(id.clone());
        registry.identities.insert(
            id.clone(),
            tilo_identity_host::registry::IdentityProfile {
                id: id.clone(),
                display_name: "Synthetic runtime".into(),
                record_path: paths.user_privkey.clone(),
                user_public_key: cert.user_public_key,
            },
        );
        store.save(&registry).unwrap();
        // The identity's fleet names this device: the ownership read takes the
        // revocations from it (none).
        let fleet = store
            .identity_dir()
            .join("fleets")
            .join(format!("{id}.json"));
        std::fs::create_dir_all(fleet.parent().unwrap()).unwrap();
        std::fs::write(
            fleet,
            serde_json::json!({
                "schema_version": 1,
                "identity_id": id,
                "certificates": [cert],
                "revocations": [],
            })
            .to_string(),
        )
        .unwrap();
    }

    /// The same device identity, this time read from disk, so the next read
    /// of it can fail without any ownership changing. An admission pins it
    /// first; the key at the canonical path is what a later check reads.
    fn read_from_disk(&self) -> Arc<SessionAdmission> {
        let paths = identity_paths(self.directory.path());
        self.install_identity(&paths);
        tilo_user_identity::persist::write_device_key(
            &paths.device_privkey,
            &super::tests::native_key(&self.joined.identity),
        )
        .unwrap();
        self.joined.install_identity_paths(paths);
        self.joined
            .admit_mcp_execution(&self.workspace.to_string(), &actor(&self.joined))
            .unwrap()
    }
}

fn actor(joined: &Joined) -> String {
    tilo_action_broker::collab_app_broker::person_id_for_root(
        joined.identity.device_cert.user_public_key,
    )
}

fn identity_paths(root: &std::path::Path) -> IdentityPaths {
    // The accepted device key at its canonical path, written by the test.
    let key = root.join("device-private").join("keypair.bin");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    IdentityPaths {
        user_pubkey: root.join("user-identity").join("pubkey.bin"),
        user_privkey: root.join("user-identity").join("privkey.bin"),
        device_privkey: key,
        device_cert: root.join("device").join("cert.bin"),
        device_key_ownership: tilo_user_identity::persist::DeviceKeyOwnership::File,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_execution_ending_after_the_snapshot_does_not_refuse_the_workspace() {
    let fixture = Fixture::new().await;
    let admissions = fixture.snapshot_then_end_one();
    fixture
        .joined
        .recheck_admissions(&admissions)
        .expect("another execution ending normally cannot refuse current workspace ownership");
    assert!(fixture.other.caller().execution_is_live());
    fixture.other.check(&fixture.joined).unwrap();
    assert_eq!(
        fixture.ended.check(&fixture.joined),
        Err(Refusal::Ended),
        "the ended execution stays refused, and says so"
    );
    drop(fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn an_execution_ending_after_the_snapshot_cannot_hide_changed_ownership() {
    let fixture = Fixture::new().await;
    let admissions = fixture.snapshot_then_end_one();
    // The device certificate the runs were admitted with no longer verifies:
    // a confirmed change of the pinned material, while the workspace and the
    // other run's certificate stay as they were.
    fixture.install_device_cert_label("unsigned label");
    assert_eq!(
        fixture.joined.recheck_admissions(&admissions).unwrap_err(),
        "execution session ended"
    );
    assert!(!fixture.other.caller().execution_is_live());
    assert!(
        !fixture.ended.caller().execution_is_live(),
        "restoring the selection does not revive the ended run"
    );
    drop(fixture);
}

/// A disk that fails once (the night's full disk, a busy identity store) is
/// not an ownership change: the call is refused as retryable, the run keeps its
/// lease, and the next read that works lets it act again.
#[tokio::test(flavor = "current_thread")]
async fn ownership_that_cannot_be_read_keeps_the_run() {
    let fixture = Fixture::new().await;
    let execution = fixture.read_from_disk();
    let paths = identity_paths(fixture.directory.path());
    std::fs::remove_file(&paths.device_privkey).unwrap();
    let refusal = execution
        .check(&fixture.joined)
        .expect_err("unreadable ownership cannot be compared");
    let Refusal::Unavailable { reason } = &refusal else {
        panic!("unreadable ownership is retryable, not {refusal:?}");
    };
    assert!(!reason.is_empty(), "the refusal says what failed");
    assert!(
        execution.caller().execution_is_live(),
        "a failed read must not end the person's agent session"
    );
    let seen = refusal.message();
    assert!(
        seen.contains("retry") && seen.contains("not run"),
        "the client is told it may retry: {seen}"
    );
    assert_eq!(
        tilo_actions::remote::ActionCallError::from(refusal.clone()),
        tilo_actions::remote::ActionCallError::Unavailable {
            reason: reason.clone()
        }
    );

    // The identity material is back: the same run acts again.
    tilo_user_identity::persist::write_device_key(
        &paths.device_privkey,
        &super::tests::native_key(&fixture.joined.identity),
    )
    .unwrap();
    execution
        .check(&fixture.joined)
        .expect("ownership that reads again admits the same run");
    let snapshot = fixture
        .joined
        .executions
        .admissions(fixture.workspace)
        .unwrap();
    fixture
        .joined
        .recheck_admissions(&snapshot)
        .expect("a transient read failure is not an ownership change");
    assert!(execution.caller().execution_is_live());
    drop(fixture);
}

/// A real change the person made ends the run, and the client is told that
/// rather than to retry.
#[tokio::test(flavor = "current_thread")]
async fn ownership_that_really_changed_ends_the_run() {
    let fixture = Fixture::new().await;
    fixture.install_device_cert_label("device renamed without a new signature");
    assert_eq!(fixture.other.check(&fixture.joined), Err(Refusal::Ended));
    assert!(
        !fixture.other.caller().execution_is_live(),
        "a confirmed ownership change ends the run"
    );
    assert_eq!(Refusal::Ended.message(), "execution session ended");
    assert_eq!(
        tilo_actions::remote::ActionCallError::from(Refusal::Ended),
        tilo_actions::remote::ActionCallError::NotPermitted
    );
    drop(fixture);
}

use super::*;
use tilo_user_identity::DeviceCert as PinnedCert;
use tilo_user_identity::persist::IdentityPaths;

struct Fixture {
    joined: Joined,
    directory: tempfile::TempDir,
    /// This thread's profile, pinned for as long as the fixture lives: some
    /// tests leave the thread scoped to a profile whose registry names no
    /// identity, and reading custody from disk needs one.
    _profile: tilo_paths::ProfileScope,
    room: RoomId,
    ended: Arc<SessionAdmission>,
    other: Arc<SessionAdmission>,
}

impl Fixture {
    async fn new() -> Self {
        // A profile of this test's own that names the fixture's identity:
        // reading the custody from disk selects it, and nothing here can
        // reach the machine's own identity store.
        let profile = tilo_paths::scope_profile(std::env::temp_dir());

        let (joined, directory) = Joined::test_empty().await;
        let joined = Arc::try_unwrap(joined).unwrap_or_else(|_| panic!("fixture has no rooms"));
        let room = RoomId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let actor = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
            joined.custody.device_cert.user_public_key,
        );
        let admit = || {
            joined
                .admit_mcp_execution(&room.to_string(), &room.to_string(), &actor)
                .unwrap()
        };
        let (ended, other) = (admit(), admit());
        Self {
            joined,
            directory,
            _profile: profile,
            room,
            ended,
            other,
        }
    }

    /// The registry's live admissions, then one of them ends normally before
    /// they are rechecked.
    fn snapshot_then_end_one(&self) -> Vec<Arc<SessionAdmission>> {
        let admissions = self.joined.executions.admissions(self.room).unwrap();
        assert_eq!(admissions.len(), 2);
        self.ended.caller().retire_execution();
        admissions
    }

    /// Relabel the device certificate without signing it again: the same
    /// pinned material, a signature that no longer verifies.
    fn install_device_cert_label(&self, label: &str) {
        let pinned = unsafe { &*Arc::as_ptr(&self.other) };
        let custody = pinned.certificate() as *const PinnedCert;
        // SAFETY: the fixture holds the only admissions, and this runs
        // before any of them is checked.
        unsafe { (*(custody.cast_mut())).device_label = label.into() };
    }

    /// Name the fixture's identity in this profile, so a custody read from
    /// disk selects it (and the fleet, which holds the certificate).
    fn install_identity(&self, paths: &IdentityPaths) {
        let cert = &self.joined.custody.device_cert;
        let id = hex::encode(cert.user_public_key);
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&cert.user_public_key),
            cert,
        )
        .unwrap();
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
        // The identity's fleet names this device: the custody read takes the
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

    /// The same device custody, this time read from disk, so the next read
    /// of it can fail without any custody changing. An admission pins it
    /// first; the key at the canonical path is what a later check reads.
    fn read_from_disk(&self) -> Arc<SessionAdmission> {
        let paths = identity_paths(self.directory.path());
        self.install_identity(&paths);
        tilo_user_identity::persist::write_device_key(
            &paths.device_privkey,
            &self.joined.custody.device_signing_key,
        )
        .unwrap();
        self.joined.install_custody_paths(paths);
        self.joined
            .admit_mcp_execution(
                &self.room.to_string(),
                &self.room.to_string(),
                &actor(&self.joined),
            )
            .unwrap()
    }
}

fn actor(joined: &Joined) -> String {
    tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        joined.custody.device_cert.user_public_key,
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
        device_key_custody: tilo_user_identity::persist::DeviceKeyCustody::File,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_execution_ending_after_the_snapshot_does_not_refuse_the_room() {
    let fixture = Fixture::new().await;
    let admissions = fixture.snapshot_then_end_one();
    fixture
        .joined
        .recheck_admissions(&admissions)
        .expect("another execution ending normally cannot refuse current room custody");
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
async fn an_execution_ending_after_the_snapshot_cannot_hide_changed_custody() {
    let fixture = Fixture::new().await;
    let admissions = fixture.snapshot_then_end_one();
    // The device certificate the runs were admitted with no longer verifies:
    // a confirmed change of the pinned material, while the room and the
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
/// not a custody change: the call is refused as retryable, the run keeps its
/// lease, and the next read that works lets it act again.
#[tokio::test(flavor = "current_thread")]
async fn custody_that_cannot_be_read_keeps_the_run() {
    let fixture = Fixture::new().await;
    let execution = fixture.read_from_disk();
    let paths = fixture
        .joined
        .custody
        .identity_paths
        .clone()
        .expect("the fixture reads custody from disk");
    std::fs::remove_file(&paths.device_privkey).unwrap();
    let refusal = execution
        .check(&fixture.joined)
        .expect_err("unreadable custody cannot be compared");
    let Refusal::Unavailable { reason } = &refusal else {
        panic!("unreadable custody is retryable, not {refusal:?}");
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
        tilo_actions::remote::ActionHostError::from(refusal.clone()),
        tilo_actions::remote::ActionHostError::Unavailable {
            reason: reason.clone()
        }
    );

    // The identity material is back: the same run acts again.
    tilo_user_identity::persist::write_device_key(
        &paths.device_privkey,
        &fixture.joined.custody.device_signing_key,
    )
    .unwrap();
    execution
        .check(&fixture.joined)
        .expect("custody that reads again admits the same run");
    let snapshot = fixture.joined.executions.admissions(fixture.room).unwrap();
    fixture
        .joined
        .recheck_admissions(&snapshot)
        .expect("a transient read failure is not a custody change");
    assert!(execution.caller().execution_is_live());
    drop(fixture);
}

/// A real change the person made ends the run, and the client is told that
/// rather than to retry.
#[tokio::test(flavor = "current_thread")]
async fn custody_that_really_changed_ends_the_run() {
    let fixture = Fixture::new().await;
    fixture.install_device_cert_label("device renamed without a new signature");
    assert_eq!(fixture.other.check(&fixture.joined), Err(Refusal::Ended));
    assert!(
        !fixture.other.caller().execution_is_live(),
        "a confirmed custody change ends the run"
    );
    assert_eq!(Refusal::Ended.message(), "execution session ended");
    assert_eq!(
        tilo_actions::remote::ActionHostError::from(Refusal::Ended),
        tilo_actions::remote::ActionHostError::NotPermitted
    );
    drop(fixture);
}

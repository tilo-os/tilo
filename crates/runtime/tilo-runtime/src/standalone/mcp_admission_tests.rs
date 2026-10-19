use super::*;

struct Fixture {
    joined: Joined,
    _directory: tempfile::TempDir,
    room: RoomId,
    ended: Arc<SessionAdmission>,
    other: Arc<SessionAdmission>,
}

impl Fixture {
    async fn new() -> Self {
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
            _directory: directory,
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
    assert!(
        fixture.ended.check(&fixture.joined).is_err(),
        "the ended execution stays refused"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_execution_ending_after_the_snapshot_cannot_hide_changed_custody() {
    let mut fixture = Fixture::new().await;
    let admissions = fixture.snapshot_then_end_one();
    let original = fixture.joined.custody.device_cert.clone();
    fixture.joined.custody.device_cert.device_label = "changed without a new signature".into();
    assert_eq!(
        fixture.joined.recheck_admissions(&admissions).unwrap_err(),
        "device-cert signature did not verify"
    );
    assert!(!fixture.other.caller().execution_is_live());
    fixture.joined.custody.device_cert = original;
    fixture.joined.recheck_mcp_executions(fixture.room).unwrap();
    assert!(
        !fixture.ended.caller().execution_is_live(),
        "restoring the certificate does not revive the ended run"
    );
}

use super::*;
use std::cell::RefCell;

thread_local! {
    static AFTER_SNAPSHOT: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(super) fn after_snapshot() {
    if let Some(callback) = AFTER_SNAPSHOT.with_borrow_mut(Option::take) {
        callback();
    }
}

struct Fixture {
    joined: Joined,
    _directory: tempfile::TempDir,
    room: RoomId,
    admission: Arc<SessionAdmission>,
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
        let admission = joined
            .admit_mcp_execution(&room.to_string(), &room.to_string(), &actor)
            .unwrap();
        let other = joined
            .admit_mcp_execution(&room.to_string(), &room.to_string(), &actor)
            .unwrap();
        Self {
            joined,
            _directory: directory,
            room,
            admission,
            other,
        }
    }

    fn end_after_snapshot(&self) {
        let caller = self.admission.caller().clone();
        AFTER_SNAPSHOT.with_borrow_mut(|callback| {
            assert!(callback.is_none());
            *callback = Some(Box::new(move || caller.retire_execution()));
        });
    }
}

#[tokio::test(flavor = "current_thread")]
async fn normal_eof_after_snapshot_does_not_poison_current_room_custody() {
    let fixture = Fixture::new().await;
    fixture.end_after_snapshot();
    assert!(
        fixture.joined.recheck_mcp_executions(fixture.room).is_ok(),
        "another connection ending normally cannot refuse current room custody"
    );
    assert!(!fixture.admission.caller().execution_is_live());
    assert!(fixture.other.caller().execution_is_live());
    fixture.other.check(&fixture.joined).unwrap();
    assert!(
        fixture.admission.check(&fixture.joined).is_err(),
        "the retired original execution must remain refused"
    );
    drop(fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn normal_eof_after_snapshot_cannot_hide_changed_certificate_custody() {
    let mut fixture = Fixture::new().await;
    let original = fixture.joined.custody.device_cert.clone();
    fixture.joined.custody.device_cert.device_label = "changed without original signature".into();
    fixture.end_after_snapshot();
    assert_eq!(
        fixture
            .joined
            .recheck_mcp_executions(fixture.room)
            .unwrap_err(),
        "device-cert signature did not verify",
        "selected original material must still be verified after normal EOF"
    );
    assert!(!fixture.other.caller().execution_is_live());
    fixture.joined.custody.device_cert = original;
    fixture.joined.recheck_mcp_executions(fixture.room).unwrap();
    assert!(
        !fixture.admission.caller().execution_is_live(),
        "restoring the certificate must not revive the original run"
    );
    drop(fixture);
}

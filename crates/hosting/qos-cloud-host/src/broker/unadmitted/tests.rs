//! Status of never-admitted invocations becomes definitive only after its
//! deadline, only for invocations the host holds no record of, and fences
//! the identity so the original can never be admitted afterwards.
use super::*;
use qos_relay::peer_actions::{ActionMessage, DirectedAction};

type Owner = Arc<crate::agent_runtime_adapter::RuntimeOwner>;
struct Fixture {
    _directory: tempfile::TempDir,
    root: std::path::PathBuf,
    owner: Owner,
    caller: DeviceCert,
    room: uuid::Uuid,
    host: [u8; 32],
}
fn fixture() -> Fixture {
    let (directory, owner) = crate::agent_workers::tests::selected();
    let store = owner.store().unwrap();
    let caller = store
        .peer()
        .unwrap()
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key != store.key().verifying_key().to_bytes())
        .unwrap();
    let room = store.state.room.parse().unwrap();
    let host = store.peer().unwrap().authority().device_public_key;
    drop(store);
    Fixture {
        root: directory.path().to_owned(),
        _directory: directory,
        owner,
        caller,
        room,
        host,
    }
}
impl Fixture {
    async fn send(&self, message: ActionMessage) -> ActionMessage {
        let action = DirectedAction {
            room: self.room,
            request_id: uuid::Uuid::new_v4(),
            source: self.caller.device_public_key,
            target: self.host,
            message,
        };
        handle_with_policy_time(
            &self.root,
            "agent-admission",
            &self.caller,
            &action,
            &self.owner.policy_time().unwrap(),
            self.owner.policy_context(),
        )
        .await
        .unwrap()
    }
    async fn status(&self, invocation: InvocationId) -> (Option<InvocationUpdate>, bool) {
        match self.send(ActionMessage::Status { invocation }).await {
            ActionMessage::Observation {
                update, uncertain, ..
            } => (update, uncertain),
            other => panic!("{other:?}"),
        }
    }
    async fn open_notes(&self, invocation: InvocationId) -> Option<InvocationUpdate> {
        match self
            .send(ActionMessage::InvokeBound {
                host: self.room,
                invocation,
                app_peer: String::new(),
                app_id: "dev.qos.desktop".into(),
                record_id: String::new(),
                resource: "desktop:app:so.tilo.notes".into(),
                action: "open".into(),
                argument: None,
                confirmation: None,
                revision: None,
            })
            .await
        {
            ActionMessage::Observation { update, .. } => update,
            other => panic!("{other:?}"),
        }
    }
    fn scope(&self, invocation: InvocationId) -> String {
        format!(
            "{}:{}",
            hex::encode(self.caller.device_public_key),
            invocation.0
        )
    }
    fn backdate(&self, invocation: InvocationId) {
        backdate(
            &self.room.to_string(),
            self.caller.device_public_key,
            invocation.0,
            UNADMITTED_STATUS_DEADLINE + Duration::from_secs(1),
        );
    }
}

fn is_not_found(update: &Option<InvocationUpdate>) -> bool {
    matches!(update, Some(InvocationUpdate::Failed { error, .. })
        if error.code == UNKNOWN_INVOCATION && !error.retryable)
}

#[tokio::test(flavor = "current_thread")]
async fn before_its_deadline_a_never_admitted_invocation_stays_no_record() {
    let f = fixture();
    let invocation = InvocationId::new();
    let generation = f.owner.store().unwrap().state.generation;
    assert_eq!(f.status(invocation).await, (None, false));
    assert_eq!(f.status(invocation).await, (None, false), "still no record");
    let store = f.owner.store().unwrap();
    assert!(!store.state.receipts.contains_key(&f.scope(invocation)));
    assert_eq!(
        store.state.generation, generation,
        "a status writes nothing"
    );
    drop(store);
    // The issuer may still send it: it is admitted normally.
    assert!(matches!(
        f.open_notes(invocation).await,
        Some(InvocationUpdate::Succeeded { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn after_its_deadline_a_never_admitted_invocation_is_definitive_and_fenced() {
    let f = fixture();
    let invocation = InvocationId::new();
    assert_eq!(f.status(invocation).await, (None, false));
    f.backdate(invocation);
    let (update, uncertain) = f.status(invocation).await;
    assert!(is_not_found(&update), "{update:?}");
    assert!(!uncertain);
    // Durable: the same answer again, from the fence.
    let (again, _) = f.status(invocation).await;
    assert!(is_not_found(&again), "{again:?}");
    let store = f.owner.store().unwrap();
    // The fence sits in the caller's device:invocation scope, which the
    // private agent owner's cross-journal check refuses to admit.
    assert_eq!(
        store.state.receipts[&f.scope(invocation)].1.status,
        NOT_FOUND
    );
    drop(store);
    // The original arriving late is refused, not admitted.
    let late = f.open_notes(invocation).await;
    assert!(
        matches!(&late, Some(InvocationUpdate::Failed { .. })),
        "{late:?}"
    );
    let store = f.owner.store().unwrap();
    assert!(
        !store
            .peer()
            .unwrap()
            .rows()
            .iter()
            .any(|row| row.app_id == "so.tilo.notes"),
        "no window was opened"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_admitted_invocation_keeps_its_normal_observation_after_the_deadline() {
    let f = fixture();
    let invocation = InvocationId::new();
    assert!(matches!(
        f.open_notes(invocation).await,
        Some(InvocationUpdate::Succeeded { .. })
    ));
    f.backdate(invocation);
    let (update, uncertain) = f.status(invocation).await;
    assert!(
        matches!(&update, Some(InvocationUpdate::Succeeded { .. })),
        "{update:?}"
    );
    assert!(!uncertain);
    // A collaboration intent retained without its receipt yet is admitted:
    // never fenced, whatever the deadline.
    let mut store = f.owner.store().unwrap();
    let pending = InvocationId::new();
    let mut state = store.state.clone();
    state.canonical_intents.insert(
        f.scope(pending),
        StoredCanonicalIntent {
            digest: [7; 32],
            operation: NativeOperation::CollaborationListInvitations,
        },
    );
    store.commit(state).unwrap();
    drop(store);
    f.backdate(pending);
    assert_eq!(f.status(pending).await, (None, false));
    let mut store = f.owner.store().unwrap();
    assert!(!store.state.receipts.contains_key(&f.scope(pending)));
    // And the fence itself rechecks under the lock.
    assert!(!fence(&mut store, f.caller.device_public_key, pending).unwrap());
    assert!(!store.state.receipts.contains_key(&f.scope(pending)));
}

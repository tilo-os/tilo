//! An agent's call on this device that waits for its person's yes, through the device's own
//! dispatch of it: the asked store a person answers in, and the device's record of the
//! invocation its agent follows. The workspace here is a provider that asks for the person's
//! yes as the broker does, and notes every call it runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::asked::{
    self, AskedFacts, AskedKeeping, AskedRecords as _, AskedStore, KeptAsk, Offer, RecordRef,
    Resolve,
};
use tilo_actions::remote::invocation_record::{Invocations, Route};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

use super::{Admits, DeviceAsked, Run};

const ADA: [u8; 32] = [1; 32];
const WORKSPACE: &str = "acme";
const WAIT: Duration = Duration::from_secs(5);

/// The workspace as the agent's calls reach it: an action the agent's grants leave to its
/// person is refused for want of their yes unless the call carries their approve; once the
/// agent's access is taken back, nothing of it runs.
#[derive(Default)]
struct Workspace {
    ran: Mutex<Vec<InvokeRequest>>,
    revoked: Mutex<bool>,
}

impl ActionProvider for Workspace {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if *self.revoked.lock().unwrap() {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        if request.approval().is_none() {
            return Pending::ready(Err(ActionCallError::asks(
                AskedFacts {
                    verb: "send".into(),
                    ..AskedFacts::default()
                }
                .with_fact("To", "bo@example.com"),
            )));
        }
        let invocation = request.invocation;
        self.ran.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        }))
    }

    fn observe(&self, _: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.ran
            .lock()
            .unwrap()
            .iter()
            .any(|ran| ran.invocation == *id)
            .then(|| {
                InvocationState::new(
                    *id,
                    InvocationUpdate::Succeeded {
                        invocation: *id,
                        output: ActionOutput::default(),
                    },
                )
            })
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

impl Workspace {
    fn ran(&self) -> Vec<InvokeRequest> {
        self.ran.lock().unwrap().clone()
    }
}

/// The device's own record of its asked calls, as its workspaces keep it across restarts.
#[derive(Default)]
struct Kept(Mutex<Vec<KeptAsk>>);

impl AskedKeeping for Kept {
    fn keep(&self, ask: &KeptAsk) {
        let mut kept = self.0.lock().unwrap();
        kept.retain(|kept| kept.at != ask.at);
        kept.push(ask.clone());
    }

    fn forget(&self, at: &RecordRef) {
        self.0.lock().unwrap().retain(|kept| kept.at != *at);
    }
}

/// Ada's agent Scout, under the key this device keeps for it.
fn scout() -> Caller {
    Caller::delegated_agent("scout", ADA, [9; 32]).in_workspace(WORKSPACE)
}

/// Ada on one of her devices.
fn ada() -> Caller {
    Caller::authenticated(ADA, [3; 32]).in_workspace(WORKSPACE)
}

fn run() -> Run {
    Run::Agent {
        workspace: WORKSPACE.into(),
        agent: "scout".into(),
        name: "Scout".into(),
    }
}

fn send_mail() -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("to".into(), "bo@example.com".into());
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "record:mail:drafts".into(),
            address: ActionAddress::new("mail.drafts:d1", "send"),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The device: its store, kept in `kept`, its record of invocations, and its dispatch, which
/// sends to `workspace` and admits a run again there.
struct Device {
    store: Arc<AskedStore>,
    invocations: Arc<Invocations>,
    asked: Arc<DeviceAsked>,
    workspace: Arc<Workspace>,
}

impl Device {
    fn start(kept: &Arc<Kept>, workspace: &Arc<Workspace>) -> Self {
        let store = Arc::new(AskedStore::default());
        store
            .keep_through(
                Arc::clone(kept) as Arc<dyn AskedKeeping>,
                kept.0.lock().unwrap().clone(),
            )
            .unwrap();
        let invocations: Arc<Invocations> = Arc::default();
        let again = Arc::clone(workspace);
        let admits: Admits = Box::new(move |run| {
            assert_eq!(*run, self::run(), "the run is admitted again as it was");
            let provider: Arc<dyn ActionProvider + Send + Sync> = again.clone();
            Ok((provider, scout()))
        });
        let asked = DeviceAsked::new(Arc::clone(&store), Arc::clone(&invocations), admits);
        Self {
            store,
            invocations,
            asked,
            workspace: Arc::clone(workspace),
        }
    }

    fn route(&self) -> Route {
        let provider: Arc<dyn ActionProvider + Send + Sync> = self.workspace.clone();
        (provider, scout())
    }

    /// Scout sends `call` through the device's dispatch.
    fn send(&self, call: &InvokeRequest) -> Result<InvocationReceipt, ActionCallError> {
        self.asked
            .send(run(), &scout(), self.route(), call.clone())
            .wait_timeout(WAIT)
            .ok()
            .flatten()
            .expect("the dispatch answers at once")
    }

    fn resolve(
        &self,
        call: &InvokeRequest,
        by: &Caller,
        resolution: Offer,
    ) -> Result<(), ActionCallError> {
        let at = self
            .store
            .record(&call.invocation.0.to_string())
            .expect("the call is kept as its record");
        self.store
            .resolve(
                &at,
                by,
                &Resolve {
                    resolution,
                    ..Resolve::default()
                },
            )
            .wait_timeout(WAIT)
            .ok()
            .flatten()
            .expect("the answer is admitted or refused at once")
    }

    /// How Scout reads its call now, through the device's record of it.
    fn standing(&self, call: &InvokeRequest) -> Option<InvocationUpdate> {
        self.invocations
            .observe(&scout(), &call.invocation)
            .map(|state| state.update)
    }
}

fn refusal(result: Result<(), ActionCallError>) -> String {
    match result {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Scout's call that needs Ada's yes is kept: it waits under way, listed as waiting for her,
/// and asks her, named as Scout's. Her approve sends it once more, byte for byte and under its
/// id, carrying her approve of that record; a second approve finds nothing to answer.
#[test]
fn an_agents_call_waits_and_runs_once_on_the_persons_approve() {
    let device = Device::start(&Arc::default(), &Arc::default());
    let call = send_mail();
    let receipt = device.send(&call).expect("kept, not refused");
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "it waits under way: {receipt:?}"
    );
    let recorded = device.invocations.get(&scout(), &call.invocation).unwrap();
    assert!(
        recorded.waiting,
        "listed as waiting for its person, at once"
    );
    assert!(
        matches!(
            device.standing(&call),
            Some(InvocationUpdate::Progress { phase, .. }) if phase == asked::ASKED_PHASE
        ),
        "Scout reads that it is asked"
    );
    let questions = device.store.asked_of(&ADA);
    assert_eq!(questions.len(), 1, "Ada is asked");
    assert!(
        matches!(&questions[0].facts.by, asked::AskedBy::Agent(agent) if agent.id == "scout" && agent.name == "Scout"),
        "named as the agent's: {:?}",
        questions[0].facts.by
    );
    assert!(
        device.workspace.ran().is_empty(),
        "nothing ran before she answered"
    );

    device.resolve(&call, &ada(), Offer::Approve).unwrap();
    let ran = device.workspace.ran();
    assert_eq!(ran.len(), 1, "it ran once");
    assert_eq!(ran[0].invocation, call.invocation, "under its id");
    assert!(
        asked::same_call(&ran[0], &call),
        "byte for byte the call Scout made"
    );
    assert!(
        matches!(&ran[0].confirmation, Some(Confirmation::Asked(approval)) if approval.record.record == call.invocation.0.to_string()),
        "carrying her approve of its record"
    );
    assert!(
        matches!(
            device.standing(&call),
            Some(InvocationUpdate::Succeeded { .. })
        ),
        "Scout reads it ran, where it ran"
    );
    assert_eq!(
        refusal(device.resolve(&call, &ada(), Offer::Approve)),
        asked::ANSWERED
    );
    assert_eq!(
        device.workspace.ran().len(),
        1,
        "a second approve runs nothing"
    );
}

/// Ada declines: the call ends declined and nothing of it runs.
#[test]
fn a_decline_ends_the_call_with_nothing_run() {
    let device = Device::start(&Arc::default(), &Arc::default());
    let call = send_mail();
    device.send(&call).unwrap();
    device.resolve(&call, &ada(), Offer::Decline).unwrap();
    assert!(device.workspace.ran().is_empty(), "nothing ran");
    assert!(
        matches!(
            device.standing(&call),
            Some(InvocationUpdate::Cancelled { message: Some(message), .. }) if message == asked::DECLINED
        ),
        "Scout reads it was declined"
    );
    assert!(
        device.store.asked_of(&ADA).is_empty(),
        "Ada is asked no more"
    );
}

/// The agent that asked never answers its own question.
#[test]
fn the_agent_that_asked_cannot_answer() {
    let device = Device::start(&Arc::default(), &Arc::default());
    let call = send_mail();
    device.send(&call).unwrap();
    assert_eq!(
        refusal(device.resolve(&call, &scout(), Offer::Approve)),
        asked::NOT_YOURS
    );
    assert!(device.workspace.ran().is_empty(), "nothing ran");
    assert_eq!(device.store.asked_of(&ADA).len(), 1, "it still asks Ada");
}

/// A call carrying an approve the device never handed back is refused whole at its frame, and
/// one carrying the approve of another call is too.
#[test]
fn an_approve_the_agent_writes_itself_is_refused() {
    let device = Device::start(&Arc::default(), &Arc::default());
    let waiting = send_mail();
    device.send(&waiting).unwrap();
    let at = device
        .store
        .record(&waiting.invocation.0.to_string())
        .unwrap();
    let mut forged = send_mail();
    forged.confirmation = Some(Confirmation::Asked(asked::Approval::new(at, None)));
    assert_eq!(
        refusal(asked::admit_frame(Some(device.asked.store()), &forged)),
        asked::FORGED
    );
    assert!(device.workspace.ran().is_empty(), "nothing ran");
}

/// Ada approves after Scout's access was taken back: her approve is checked against the grants
/// as they stand then, nothing runs, and Scout reads why.
#[test]
fn an_approve_after_the_agent_lost_its_access_runs_nothing() {
    let device = Device::start(&Arc::default(), &Arc::default());
    let call = send_mail();
    device.send(&call).unwrap();
    *device.workspace.revoked.lock().unwrap() = true;
    device.resolve(&call, &ada(), Offer::Approve).unwrap();
    assert!(device.workspace.ran().is_empty(), "nothing ran");
    assert!(
        matches!(
            device.standing(&call),
            Some(InvocationUpdate::Failed { error, .. }) if error.code == ActionCallError::NotPermitted.code()
        ),
        "Scout reads it was not permitted"
    );
}

/// Stopping a call that waits withdraws its question: Ada is asked no more, and an answer to it
/// finds nothing.
#[test]
fn stopping_a_waiting_call_withdraws_its_question() {
    let device = Device::start(&Arc::default(), &Arc::default());
    let call = send_mail();
    device.send(&call).unwrap();
    device
        .asked
        .cancel(&scout(), &call.invocation)
        .wait_timeout(WAIT)
        .ok()
        .flatten()
        .unwrap()
        .unwrap();
    assert!(
        device.store.asked_of(&ADA).is_empty(),
        "Ada is asked no more"
    );
    assert!(matches!(
        device.standing(&call),
        Some(InvocationUpdate::Cancelled { .. })
    ));
    assert_eq!(
        refusal(device.resolve(&call, &ada(), Offer::Approve)),
        asked::ANSWERED
    );
    assert!(device.workspace.ran().is_empty(), "nothing ran");
}

/// The device restarts while Scout's call waits: Ada is asked again, and her approve runs it in a
/// run admitted afresh for Scout, byte for byte and under its id.
#[test]
fn a_waiting_call_survives_the_devices_restart() {
    let (kept, workspace) = (Arc::<Kept>::default(), Arc::<Workspace>::default());
    let call = send_mail();
    {
        let before = Device::start(&kept, &workspace);
        before.send(&call).unwrap();
    }
    let after = Device::start(&kept, &workspace);
    assert_eq!(after.store.asked_of(&ADA).len(), 1, "Ada is asked again");
    after.resolve(&call, &ada(), Offer::Approve).unwrap();
    let ran = workspace.ran();
    assert_eq!(ran.len(), 1, "it ran once");
    assert_eq!(ran[0].invocation, call.invocation);
    assert!(asked::same_call(&ran[0], &call));
    assert!(kept.0.lock().unwrap().is_empty(), "nothing waits any more");
}

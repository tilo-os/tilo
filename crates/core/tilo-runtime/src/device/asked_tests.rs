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
    /// What it asks, when not the agent's default: a program's question that takes a secret.
    asks: Mutex<Option<AskedFacts>>,
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
            if let Some(asks) = self.asks.lock().unwrap().clone() {
                return Pending::ready(Err(ActionCallError::asks(asks)));
            }
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

/// Scout's question reads in the workspace's invocations, where Ada's devices draw it: on the
/// device's own record of the call, and as a row of its own where the device keeps no record of
/// it (it waited across a restart). It names her as the one asked and carries what she reads.
#[test]
fn a_waiting_question_reads_in_the_workspaces_invocations() {
    use tilo_actions::remote::workspace_invocations::{Invocation, InvocationStatus};

    let device = Device::start(&Arc::default(), &Arc::default());
    let call = send_mail();
    device.send(&call).unwrap();
    let waiting = device.store.waiting_in(WORKSPACE);
    assert_eq!(waiting.len(), 1);
    assert!(device.store.waiting_in("elsewhere").is_empty());
    let id = call.invocation.0.to_string();

    let mut recorded = vec![Invocation {
        id: Some(id.clone()),
        status: InvocationStatus::Running,
        ..Invocation::default()
    }];
    super::super::services::with_questions(&mut recorded, waiting.clone(), None);
    assert_eq!(recorded.len(), 1, "the record it has is the one read");
    let item = &recorded[0];
    assert_eq!(item.status, InvocationStatus::Waiting);
    assert_eq!(item.person.as_deref(), Some(hex::encode(ADA).as_str()));
    let asked = item.asked.as_ref().expect("what she is asked");
    assert_eq!(asked.record.record, id);
    assert!(asked.facts.facts.iter().any(|fact| fact.label == "To"));

    let mut after_restart = Vec::new();
    super::super::services::with_questions(&mut after_restart, waiting.clone(), None);
    assert_eq!(
        after_restart.len(),
        1,
        "listed though the device kept no record"
    );
    assert_eq!(after_restart[0].by.kind, "agent");
    assert_eq!(after_restart[0].by.name, "Scout");
    assert_eq!(after_restart[0].status, InvocationStatus::Waiting);

    let mut another = Vec::new();
    super::super::services::with_questions(&mut another, waiting, Some("another"));
    assert!(
        another.is_empty(),
        "a reading of one invocation reads only that one"
    );
}

/// A program outside Tilo asks Ada, through her shell, for her password: the question is the
/// program's, answered only on this device. What she types there reaches the program's provider
/// with her approve, and is never written to the record, the device's keeping of it or its
/// reading; the same answer from her other device is refused.
#[test]
fn a_secret_is_answered_only_where_it_was_asked_and_never_kept() {
    use tilo_actions::remote::asked::{AskedBy, Take};

    const HERE: [u8; 32] = [7; 32];
    const SECRET: &str = "correct horse battery staple";
    let (kept, workspace) = (Arc::<Kept>::default(), Arc::<Workspace>::default());
    *workspace.asks.lock().unwrap() = Some(AskedFacts {
        verb: "unlock".into(),
        takes: vec![Take {
            label: "Password".into(),
            secret: true,
            shown: false,
            initial: None,
            chord: false,
        }],
        ..AskedFacts::by(AskedBy::Program {
            name: "polkit".into(),
            app_id: Some("org.freedesktop.policykit".into()),
        })
    });
    let device = Device::start(&kept, &workspace);
    device.store.asks_on(&HERE);
    let ada_here = Caller::authenticated(ADA, HERE).in_workspace(WORKSPACE);
    let call = send_mail();
    let route: Route = (workspace.clone(), ada_here.clone());
    let run = Run::Own {
        workspace: WORKSPACE.into(),
        pressed: None,
    };
    device
        .asked
        .send(run, &ada_here, route, call.clone())
        .wait_timeout(WAIT)
        .ok()
        .flatten()
        .unwrap()
        .expect("kept for her to answer");
    let questions = device.store.asked_of(&ADA);
    assert_eq!(questions.len(), 1);
    assert!(
        matches!(&questions[0].facts.by, AskedBy::Program { name, .. } if name == "polkit"),
        "the program's question, as its provider named it: {:?}",
        questions[0].facts.by
    );
    assert!(
        !questions[0].offers.contains(&Offer::ApproveAndGrant),
        "a program's question grants nothing"
    );
    assert_eq!(
        questions[0].record.device.as_deref(),
        Some(hex::encode(HERE).as_str()),
        "answered only on this device"
    );

    let at = device.store.record(&call.invocation.0.to_string()).unwrap();
    let typed = |by: &Caller| {
        device
            .store
            .resolve(
                &at,
                by,
                &Resolve {
                    typed: vec![SECRET.into()],
                    ..Resolve::default()
                },
            )
            .wait_timeout(WAIT)
            .ok()
            .flatten()
            .unwrap()
    };
    assert_eq!(refusal(typed(&ada())), asked::ANSWERED_THERE);
    assert!(
        workspace.ran().is_empty(),
        "nothing ran from her other device"
    );
    let written = |what: &str| {
        let kept = serde_json::to_string(&*kept.0.lock().unwrap()).unwrap();
        let reading = format!("{:?}", device.store.waiting_in(WORKSPACE));
        assert!(
            !kept.contains(SECRET) && !reading.contains(SECRET),
            "the secret is in no record {what}"
        );
    };
    written("while it waits");

    typed(&ada_here).expect("answered here");
    let ran = workspace.ran();
    assert_eq!(ran.len(), 1, "it ran once");
    let approval = ran[0].approval().expect("with her approve");
    assert_eq!(
        approval
            .typed
            .iter()
            .map(|typed| typed.as_str())
            .collect::<Vec<_>>(),
        [SECRET],
        "what she typed reaches the provider"
    );
    assert!(
        !serde_json::to_string(&ran[0]).unwrap().contains(SECRET),
        "the call as it travels carries no secret"
    );
    written("once answered");
    let recorded = device
        .invocations
        .get(&ada_here, &call.invocation)
        .expect("the device's record of it");
    assert!(
        !serde_json::to_string(&recorded.request)
            .unwrap()
            .contains(SECRET),
        "the device's record of the call carries no secret"
    );
}

/// Ada's saved shortcut, as the workspace asks for a step of it that needs her yes.
fn shortcut_asks() -> AskedFacts {
    AskedFacts {
        verb: "run".into(),
        ..AskedFacts::by(asked::AskedBy::App {
            app: "so.tilo.shortcuts".into(),
            title: "Send the weekly mail".into(),
            author: None,
        })
    }
    .with_fact("Send", "bo@example.com")
}

/// Ada presses the key of her shortcut: its run reaches the dispatch as her own call.
fn press(device: &Device, call: &InvokeRequest) -> InvocationReceipt {
    let route: Route = (device.workspace.clone(), ada());
    let run = Run::Own {
        workspace: WORKSPACE.into(),
        pressed: None,
    };
    device
        .asked
        .send(run, &ada(), route, call.clone())
        .wait_timeout(WAIT)
        .ok()
        .flatten()
        .expect("the dispatch answers at once")
        .expect("kept, not refused")
}

/// Ada presses the key of a shortcut with a step that needs her yes: it waits as the
/// shortcut's asked record with nothing of it run, and runs once on her approve, under its id.
#[test]
fn a_pressed_shortcut_asks_and_runs_on_the_persons_approve() {
    let workspace = Arc::<Workspace>::default();
    *workspace.asks.lock().unwrap() = Some(shortcut_asks());
    let device = Device::start(&Arc::default(), &workspace);
    let call = send_mail();
    let receipt = press(&device, &call);
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "it waits under way: {receipt:?}"
    );
    let questions = device.store.asked_of(&ADA);
    assert_eq!(questions.len(), 1, "Ada is asked");
    assert_eq!(
        questions[0].facts.by,
        shortcut_asks().by,
        "named as the shortcut that asks"
    );
    assert!(device.workspace.ran().is_empty(), "nothing ran yet");

    device.resolve(&call, &ada(), Offer::Approve).unwrap();
    let ran = device.workspace.ran();
    assert_eq!(ran.len(), 1, "it ran once");
    assert_eq!(ran[0].invocation, call.invocation, "under its id");
    assert!(asked::same_call(&ran[0], &call));
}

/// Ada declines what her pressed shortcut asks: nothing of it runs, and she is asked no more.
#[test]
fn a_declined_pressed_shortcut_runs_nothing() {
    let workspace = Arc::<Workspace>::default();
    *workspace.asks.lock().unwrap() = Some(shortcut_asks());
    let device = Device::start(&Arc::default(), &workspace);
    let call = send_mail();
    press(&device, &call);
    device.resolve(&call, &ada(), Offer::Decline).unwrap();
    assert!(device.workspace.ran().is_empty(), "nothing ran");
    assert!(
        device.store.asked_of(&ADA).is_empty(),
        "Ada is asked no more"
    );
}

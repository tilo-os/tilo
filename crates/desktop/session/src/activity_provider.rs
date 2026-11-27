//! A workspace's activity, served to the apps this session runs.
//! The shell serves the activity model to its own windows in process
//! ([`tilo_session_core::desktop::session::activity`]); an app runs in a process of its own and reaches
//! the same model through the action broker, under [`wire::PROVIDER`]. Both ask the same API
//! ([`ActivityRequests`]): this is the broker's end of it. Its reads are live views, re-read
//! whenever the shell's model has served something new; its writes run on the shell's thread when the
//! shell takes them, so a write and a read of the same change cannot cross.
//! What this device has seen is the person's own: an agent asking for it is refused here, before
//! anything reaches the model.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page serves its own activity over this API next"
    )
)]

use std::sync::Arc;

use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{Admitted, Caller, InvokeRequest, LiveRead, LiveView, Outcome, Pending};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ExecutionLocus,
    IRREVERSIBLE_TAG, InputSpec, InvocationId, OperationKind, RiskLevel,
};
use tilo_broker_core::BrokerActor;
use tilo_session_core::desktop::activity::vocabulary as wire;
use tilo_session_core::desktop::activity::{is_read, refused_to_agent};
use tilo_session_core::desktop::i18n::t;
use tilo_session_core::desktop::session::ActivityRequests;

/// The broker's API to the activity the shell's model serves.
pub(crate) struct ActivityWire {
    requests: ActivityRequests,
    /// Moves whenever what an app follows may read differently.
    changes: tokio::sync::watch::Sender<u64>,
}

impl ActivityWire {
    /// The API `requests` serves, and the changes to what it serves.
    pub(crate) fn new(requests: ActivityRequests) -> Self {
        Self {
            requests,
            changes: tokio::sync::watch::channel(0).0,
        }
    }

    /// The shell's model served its reads again: what an app follows may read differently now.
    pub(crate) fn changed(&self) {
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

#[allow(
    clippy::unnecessary_box_returns,
    reason = "the broker carries a step's failure boxed"
)]
fn failure(code: &str, message: impl Into<String>) -> Box<ActionFailure> {
    Box::new(ActionFailure {
        code: code.into(),
        message: message.into(),
        details: None,
        retryable: false,
    })
}

/// The typed arguments an argument slot carries; none for none.
fn arguments_of(
    argument: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(argument) = argument else {
        return Ok(serde_json::Map::new());
    };
    let json = argument
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or("workspace activity takes typed arguments")?;
    serde_json::from_str(json).map_err(|error| error.to_string())
}

/// `caller`'s member, as the API knows it: an app reaches the activity as the person using it,
/// confined to the workspace it runs in.
fn caller_of(caller: &Caller, workspace: &str) -> Caller {
    use tilo_member_types::{Actor, MemberId};

    let member = BrokerActor::for_caller(caller);
    // A bot's installation id may carry a `:` of its own: it is software, so
    // it reaches the activity as itself, never through a person's id.
    let caller = match member.actor() {
        Actor::Member(MemberId::Agent { id: agent, person }) => {
            Caller::local_agent(agent.as_str(), person.clone())
        }
        Actor::Member(MemberId::Bot(bot)) => Caller::local_bot(bot.as_str()),
        Actor::Member(MemberId::Person(person)) => Caller::local_person(person.clone()),
        // An app acting by itself reaches the activity as itself, as its daemon attested it.
        Actor::App(_) | Actor::Automation(_) | Actor::Visitor(_) => caller.clone(),
    };
    caller.in_workspace(workspace)
}

/// Whether `action` overwrites what documents hold with an earlier state, so Undo cannot take it
/// back: the changes it drops are in everyone's documents and its own write is not a change the
/// undo service reverts.
fn rewrites_history(action: &str) -> bool {
    [
        wire::UNDO,
        wire::RESTORE,
        wire::UNDO_SESSION,
        wire::UNDO_RECORD,
    ]
    .contains(&action)
}

fn descriptor(decl: &tilo_resource::EffectDecl) -> ActionDescriptor {
    let action = decl.id;
    let kind = decl.kind;
    let contract = if is_read(action) {
        ActionContract::live_view(serde_json::json!({ "type": "object" }))
    } else if kind == OperationKind::View {
        ActionContract::view()
    } else if rewrites_history(action) {
        // Undoing a change, restoring a version and undoing an agent's task overwrite documents
        // with what they held before: the changes they drop stay in the record, but Undo does not
        // bring them back, and they are everybody's. So the declaration says Undo cannot take it
        // back, and every API asks the person first. Narrating a work group (naming, splitting,
        // merging) writes the activity record alone and stays undoable, so it runs at once.
        let mut contract = ActionContract {
            kind: ActionKind::Undoable,
            risk: RiskLevel::Destructive,
            tags: vec![IRREVERSIBLE_TAG.to_owned()],
            ..ActionContract::default()
        };
        contract.settle_confirmation();
        contract
    } else {
        ActionContract {
            kind: ActionKind::Undoable,
            ..ActionContract::default()
        }
    };
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(wire::RESOURCE, action),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("activity-title-explore"),
        action_label: decl.label.read(),
        keywords: decl
            .keywords
            .iter()
            .map(|word| (*word).to_owned())
            .collect(),
        icon: decl.icon.map(str::to_owned),
        description: (!decl.description.is_empty()).then(|| decl.description.to_owned()),
        locus: ExecutionLocus::default(),
        kind,
        contract,
        input: InputSpec::None,
        params: decl.params.clone(),
        view: None,
    }
}

impl tilo_actions::remote::Admits for ActivityWire {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = wire::APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(Self::descriptors())
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let action = request.target.address.action.clone();
        let slot = request.argument.to_slot();
        let arguments = arguments_of(slot.as_deref())?;
        if BrokerActor::for_caller(caller).is_agent()
            && let Some(refusal) = refused_to_agent(&action, &arguments)
        {
            return Err(refusal);
        }
        let ran = self.requests.act(&action, arguments);
        Ok(ran.map(|ran| {
            ran.map(|()| tilo_actions::ActionOutput::default())
                .map_err(|message| failure("activity_refused", message))
        }))
    }

    fn live(&self, caller: &Caller, request: &InvokeRequest) -> Result<LiveView, String> {
        let action = request.target.address.action.clone();
        if !is_read(&action) {
            return Err("this step has no live result".into());
        }
        let slot = request.argument.to_slot();
        let arguments = arguments_of(slot.as_deref())?;
        if BrokerActor::for_caller(caller).is_agent()
            && let Some(refusal) = refused_to_agent(&action, &arguments)
        {
            return Err(refusal);
        }
        let workspace = caller
            .workspace()
            .ok_or("the activity is followed in no workspace")?;
        let caller = caller_of(caller, workspace);
        let invocation = InvocationId::new();
        self.requests
            .follow(&caller, invocation, &action, arguments)
            .map_err(|error| format!("the activity is not followed: {error:?}"))?;
        // The follow ends when the view is dropped, which is when the app stopped following it.
        let followed = Followed {
            requests: self.requests.clone(),
            caller,
            invocation,
        };
        let changes = self.changes.subscribe();
        Ok(LiveView {
            read: Box::new(move || {
                let _followed = &followed;
                let read = followed.requests.served(&followed.invocation);
                let read: LiveRead = match read {
                    // Not served yet: the shell reads it on its next pass.
                    None => Ok(serde_json::json!({ "pending": true })),
                    Some(Ok(value)) => Ok(value),
                    Some(Err(message)) => Err(failure("activity_unreadable", message)),
                };
                Pending::ready(read)
            }),
            changes: Box::new(move || {
                let mut receiver = changes.clone();
                receiver.borrow_and_update();
                let (woken, awaited) = tilo_actions::remote::pending();
                drop(tilo_rt::app().spawn(async move {
                    if receiver.changed().await.is_ok() {
                        woken.complete(());
                    }
                }));
                awaited
            }),
        })
    }
}

impl ActivityWire {
    fn descriptors() -> Vec<ActionDescriptor> {
        use tilo_resource::Effect as _;
        wire::ActivityAction::declarations()
            .iter()
            .map(descriptor)
            .collect()
    }
}

/// A read an app follows: once it stops following it, the model is told, so what it showed stops
/// counting as what the person is reading.
struct Followed {
    requests: ActivityRequests,
    caller: Caller,
    invocation: InvocationId,
}

impl Drop for Followed {
    fn drop(&mut self) {
        let _ = self.requests.cancel(&self.caller, &self.invocation);
    }
}

/// The broker's end of the activity the shell's model serves.
pub(crate) fn wire(requests: ActivityRequests) -> Arc<ActivityWire> {
    Arc::new(ActivityWire::new(requests))
}

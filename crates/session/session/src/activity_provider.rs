//! A workspace's activity, served to the apps this session runs.
//!
//! The desk serves the activity model to its own windows in process
//! ([`tilo_desktop_surface::session::activity`]); an app runs in a process of
//! its own and reaches the same model through the action broker, under
//! [`wire::PROVIDER`]. Both ask the same door
//! ([`ActivityRequests`]): this is the broker's end of it. Its reads are live
//! views, re-read whenever the desk's model has served something new; its
//! writes run on the desk's thread when the desk takes them, so a write and
//! a read of the same change cannot cross.
//!
//! What this device has seen is the person's own: an agent asking for it is
//! refused here, before anything reaches the model.

use std::sync::Arc;

use tilo_action_broker::services::{HostStepOutcome, HostStepRequest, HostStepServices, LiveView};
use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::remote::{Caller, Pending};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ExecutionLocus,
    IRREVERSIBLE_TAG, InputSpec, InvocationId, OperationKind, RiskLevel,
};
use tilo_desktop_surface::activity::{is_read, refused_to_agent};
use tilo_desktop_surface::i18n::t;
use tilo_desktop_surface::session::ActivityRequests;

/// The broker's door to the activity the desk's model serves.
pub(crate) struct ActivityWire {
    requests: ActivityRequests,
    /// Moves whenever what an app follows may read differently.
    changes: tokio::sync::watch::Sender<u64>,
}

impl ActivityWire {
    /// The door `requests` serves, and the changes to what it serves.
    pub(crate) fn new(requests: ActivityRequests) -> Self {
        Self {
            requests,
            changes: tokio::sync::watch::channel(0).0,
        }
    }

    /// The desk's model served its reads again: what an app follows may read
    /// differently now.
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

/// `request`'s caller, as the door knows it: an app reaches the activity as
/// the person using it, confined to the workspace it runs in.
fn caller_of(request: &HostStepRequest<'_>) -> Caller {
    let caller = if request.actor.is_agent() {
        Caller::local_agent(request.actor.id())
    } else {
        Caller::local_person(request.actor.id())
    };
    caller.in_room(request.room)
}

/// Whether `action` overwrites what documents hold with an earlier state, so
/// Undo cannot take it back: the changes it drops are in everyone's documents
/// and its own write is not a change the undo service reverts.
fn rewrites_history(action: &str) -> bool {
    [wire::UNDO, wire::RESTORE, wire::UNDO_RUN, wire::UNDO_RECORD].contains(&action)
}

fn descriptor(action: &str, label: &str, kind: OperationKind) -> ActionDescriptor {
    let contract = if is_read(action) {
        ActionContract::live_view(serde_json::json!({ "type": "object" }))
    } else if kind == OperationKind::View {
        ActionContract::view()
    } else if rewrites_history(action) {
        // Undoing a change, restoring a version and undoing an agent's task
        // overwrite documents with what they held before: the changes they
        // drop stay in the record, but Undo does not bring them back, and
        // they are everybody's. So the declaration says Undo cannot take it
        // back, and every door asks the person first. Narrating a work group
        // (naming, splitting, merging) writes the activity record alone and
        // stays undoable, so it runs at once.
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
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(wire::RESOURCE, action),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("activity-title-explore"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::default(),
        kind,
        contract,
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

impl HostStepServices for ActivityWire {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        wire::APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        use OperationKind::{Document, View};
        [
            (wire::FEED, "activity-title-explore", View),
            (wire::SUMMARY, "activity-title-whats-new", View),
            (wire::REPLAY, "activity-title-replay", View),
            (wire::DETAIL, "activity-pane-details", View),
            (wire::FOCUS, "activity-detail-open", View),
            (wire::MARK_VIEWED, "activity-action-mark-viewed", View),
            (wire::FOCUSED, "activity-action-focused", View),
            (wire::NAME, "activity-action-name", Document),
            (wire::SPLIT, "activity-detail-split", Document),
            (wire::MERGE, "activity-detail-merge", Document),
            (
                wire::MOVE_BOUNDARY,
                "activity-action-move-boundary",
                Document,
            ),
            (wire::UNDO, "activity-history-undo", Document),
            (wire::RESTORE, "activity-history-restore", Document),
            (wire::UNDO_RUN, "activity-detail-undo-task", Document),
            (wire::UNDO_RECORD, "activity-action-undo-record", Document),
            (wire::OPEN, "activity-detail-open-app", View),
            (wire::CHOOSE_SOURCE, "activity-action-choose-source", View),
            (wire::SOURCE, "activity-action-source", View),
        ]
        .into_iter()
        .map(|(action, label, kind)| descriptor(action, label, kind))
        .collect()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let action = request.address.action.clone();
        let arguments = arguments_of(request.argument)?;
        if request.actor.is_agent()
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

    fn watch(&self, request: &HostStepRequest<'_>) -> Result<LiveView, String> {
        let action = request.address.action.clone();
        if !is_read(&action) {
            return Err("this step has no live result".into());
        }
        let arguments = arguments_of(request.argument)?;
        if request.actor.is_agent()
            && let Some(refusal) = refused_to_agent(&action, &arguments)
        {
            return Err(refusal);
        }
        let caller = caller_of(request);
        let invocation = InvocationId::new();
        self.requests
            .follow(&caller, invocation, &action, arguments)
            .map_err(|error| format!("the activity is not followed: {error:?}"))?;
        // The follow ends when the broker drops the view, which is when the
        // app stopped following it.
        let followed = Followed {
            requests: self.requests.clone(),
            caller,
            invocation,
        };
        Ok(LiveView {
            read: Box::new(move || {
                let _followed = &followed;
                let read = followed.requests.served(&followed.invocation);
                Pending::ready(match read {
                    // Not served yet: the desk reads it on its next pass.
                    None => Ok(serde_json::json!({ "pending": true })),
                    Some(Ok(value)) => Ok(value),
                    Some(Err(message)) => Err(failure("activity_unreadable", message)),
                })
            }),
            changes: self.changes.subscribe(),
        })
    }
}

/// A read an app follows: once it stops following it, the model is told, so
/// what it showed stops counting as what the person is reading.
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

/// The broker's end of the activity the desk's model serves.
pub(crate) fn wire(requests: ActivityRequests) -> Arc<ActivityWire> {
    Arc::new(ActivityWire::new(requests))
}

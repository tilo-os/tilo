//! What the workspace answers a driven app beyond its own calls, as the driver's peer plays it
//! ([`Driver::connect_peer`]): another provider's action is answered by what serves it
//! ([`Driver::serve_provider`]), and the actions a window shows are decided by what stands for the
//! device's daemon ([`Driver::decide_shown_actions`]), again whenever permissions change.

use std::collections::HashMap;

use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
};
use tilo_actions::{ActionOutput, Actor, ApiError};

use super::Driver;
use crate::shown_actions::{ActionDecision, ShownAction};
use tilo_ui_base::{collaboration::CollabEvent, collaboration::CollabRequest};

/// One invocation of another provider's action ([`Driver::serve_provider`]).
pub struct ProvidedInvocation<'a> {
    /// The action as the provider publishes it.
    pub address: &'a tilo_actions::ActionAddress,
    pub argument: &'a InvokeArgument,
    /// Who invoked it.
    pub actor: &'a Actor,
    /// The person's yes, when the provider asked them and this is the call coming back.
    pub approval: Option<&'a tilo_actions::remote::asked::Approval>,
}

/// What answers another provider's action in place of the workspace.
pub type ServedProvider = Box<dyn FnMut(&ProvidedInvocation<'_>) -> Result<ActionOutput, ApiError>>;

/// What decides, in place of the device's daemon, the actions a window shows.
pub type ShownHost = Box<dyn FnMut(&[ShownAction]) -> Vec<ActionDecision>>;

/// The providers the caller serves and what decides the window's shown actions.
#[derive(Default)]
pub(super) struct Served {
    providers: HashMap<String, ServedProvider>,
    /// What decides the shown actions, the actions the window last asked about, and the
    /// permissions revision they were decided at.
    shown: Option<(ShownHost, Vec<ShownAction>, Option<u64>)>,
}

impl Served {
    /// The receipt of `request` from the provider the caller serves in its name; `None` when
    /// the caller serves no such provider.
    pub(super) fn invoke(
        &mut self,
        request: &InvokeRequest,
        actor: &Actor,
    ) -> Option<RemoteActionMessage> {
        let serve = self.providers.get_mut(&request.target.provider)?;
        let status = match serve(&ProvidedInvocation {
            address: &request.target.address,
            argument: &request.argument,
            actor,
            approval: request.approval(),
        }) {
            Ok(output) => ReceiptStatus::Completed { output },
            Err(error) => ReceiptStatus::Failed {
                error: tilo_actions::ActionFailure {
                    code: error.code().into(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                },
            },
        };
        Some(RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    /// Take the window's question about the actions it shows; the verdicts, when something
    /// decides them.
    pub(super) fn asked(&mut self, request: &CollabRequest) -> Option<CollabEvent> {
        let CollabRequest::DecideShownActions { actions } = request else {
            return None;
        };
        let (_, shown, decided) = self.shown.as_mut()?;
        shown.clone_from(actions);
        *decided = None;
        self.decide_shown()
    }

    /// Whether the shown actions are owed a decision: permissions changed since they were
    /// decided, or a host was given after the window asked.
    pub(super) fn owes_decision(&self) -> bool {
        self.shown.as_ref().is_some_and(|(_, _, decided)| {
            *decided != Some(tilo_ui_base::permissions::shared_revision())
        })
    }

    /// The verdicts a decision owed now gives.
    pub(super) fn unasked(&mut self) -> Option<CollabEvent> {
        if self.owes_decision() {
            self.decide_shown()
        } else {
            None
        }
    }

    fn decide_shown(&mut self) -> Option<CollabEvent> {
        let (host, shown, decided) = self.shown.as_mut()?;
        *decided = Some(tilo_ui_base::permissions::shared_revision());
        if shown.is_empty() {
            return None;
        }
        Some(CollabEvent::ActionVerdicts {
            verdicts: host(shown),
        })
    }
}

impl Driver {
    /// Answer `provider`'s actions with `serve`, as the workspace delivers them to it: what the
    /// app's controls bound to another provider's actions ([`tilo_actions::ActionSpec::run_by`])
    /// invoke. Connects the driver's peer when it is not.
    pub fn serve_provider(&mut self, provider: impl Into<String>, serve: ServedProvider) {
        self.served_mut().providers.insert(provider.into(), serve);
    }

    /// Stop answering `provider`'s actions: what the app invokes there goes to the window's
    /// session, or is refused as unavailable, as the workspace refuses a provider nobody serves.
    pub fn stop_serving_provider(&mut self, provider: &str) {
        self.served_mut().providers.remove(provider);
    }

    /// Decide with `host`, as the device's daemon does, the actions the window shows: what the
    /// window asks of them is answered as it asks, and again whenever permissions change.
    /// Connects the driver's peer when it is not.
    pub fn decide_shown_actions(&mut self, host: ShownHost) {
        let asked = self
            .peer_requests()
            .into_iter()
            .rev()
            .find_map(|request| match request {
                CollabRequest::DecideShownActions { actions } => Some(actions),
                _ => None,
            });
        self.served_mut().shown = Some((host, asked.unwrap_or_default(), None));
        self.run_frame();
    }
}

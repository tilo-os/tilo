//! What the workspace answers a driven app beyond its own calls, as the driver's peer plays it
//! ([`Driver::connect_peer`]): another provider's action is answered by what serves it
//! ([`Driver::serve_provider`]), a record of another workspace is read from what serves that
//! workspace ([`Driver::serve_records`]), and the actions a window shows are decided by what stands for the
//! device's daemon ([`Driver::decide_shown_actions`]), again whenever permissions change.

use std::collections::HashMap;

use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    ResourceReadRequest,
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

/// What answers the reads of one workspace's records in place of the workspace: the readings
/// (`{"revision", "records": [...]}`) as the record API answers the read, or why it cannot.
pub type ServedReads = Box<dyn Fn(&ResourceReadRequest) -> Result<serde_json::Value, String>>;

/// What decides, in place of the device's daemon, the actions a window shows.
pub type ShownHost = Box<dyn FnMut(&[ShownAction]) -> Vec<ActionDecision>>;

/// The providers the caller serves and what decides the window's shown actions.
#[derive(Default)]
pub(super) struct Served {
    providers: HashMap<String, ServedProvider>,
    /// What answers the reads of each workspace's records, by the workspace.
    reads: HashMap<String, ServedReads>,
    /// Reads waiting for a write past the revision they name, by their correlation: answered once
    /// the record moved, as the workspace holds a followed read.
    held: Vec<(u64, ResourceReadRequest)>,
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
        // A record of an app in a workspace is the app's there: what serves the app in that
        // workspace answers each of its records, as the workspace delivers them to it.
        let provider = &request.target.provider;
        let token = tilo_actions::remote::application_provider(provider).map(|(workspace, app)| {
            tilo_actions::remote::application_provider_token(workspace, app)
        });
        let serve = match token {
            Some(token) if !self.providers.contains_key(provider) => {
                self.providers.get_mut(&token)?
            }
            _ => self.providers.get_mut(provider)?,
        };
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

    /// Answer the read `read`, asked under `correlation`, from what serves its workspace: at once,
    /// or, as `None`, held until the record moves past the revision a followed read names or
    /// until something serves that workspace.
    pub(super) fn read(
        &mut self,
        correlation: u64,
        read: &ResourceReadRequest,
    ) -> Option<RemoteActionMessage> {
        let Some(serve) = self.reads.get(&read.workspace) else {
            // Nothing serves that workspace yet: the host may still be told of it, as a test
            // host's is once its window is built, so the read waits for it rather than being
            // refused.
            self.held.push((correlation, read.clone()));
            return None;
        };
        let result = serve(read);
        if let (Ok(readings), Some(after)) = (&result, &read.after)
            && readings["revision"].as_str() == Some(after.as_str())
        {
            self.held.push((correlation, read.clone()));
            return None;
        }
        Some(reading(correlation, result))
    }

    /// The followed reads whose record moved since they were held, answered.
    pub(super) fn moved(&mut self) -> Vec<RemoteActionMessage> {
        let mut answers = Vec::new();
        let reads = &self.reads;
        self.held.retain(|(correlation, read)| {
            let Some(serve) = reads.get(&read.workspace) else {
                // Still nothing serves it: the read waits.
                return true;
            };
            let result = serve(read);
            let held = matches!(&result, Ok(readings)
                if readings["revision"].as_str() == read.after.as_deref());
            if !held {
                answers.push(reading(*correlation, result));
            }
            held
        });
        answers
    }

    /// Whether a held read's record moved: an answer a frame is owed to bring to the app.
    pub(super) fn owes_reading(&self) -> bool {
        self.held.iter().any(|(_, read)| {
            self.reads.get(&read.workspace).is_some_and(|serve| {
                !matches!(serve(read), Ok(readings)
                    if readings["revision"].as_str() == read.after.as_deref())
            })
        })
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

/// The answer to the read asked under `correlation`.
fn reading(correlation: u64, result: Result<serde_json::Value, String>) -> RemoteActionMessage {
    RemoteActionMessage::ResourceReading {
        correlation,
        result: result
            .map(|readings| readings.to_string())
            .map_err(|reason| tilo_actions::remote::ActionCallError::Unavailable { reason }),
    }
}

impl Driver {
    /// Answer `provider`'s actions with `serve`, as the workspace delivers them to it: what the
    /// app's controls bound to another provider's actions ([`tilo_actions::ActionSpec::run_by`])
    /// invoke. Connects the driver's peer when it is not.
    pub fn serve_provider(&mut self, provider: impl Into<String>, serve: ServedProvider) {
        self.served_mut().providers.insert(provider.into(), serve);
    }

    /// Answer the reads of `workspace`'s records with `reads`, as the workspace answers a window
    /// reading a record of another workspace than its own (the person's own records): a followed
    /// read is held until the record moves past the revision it names. Reads asked before this
    /// workspace was served were held for it, and are answered now. Connects the driver's peer
    /// when it is not.
    pub fn serve_records(&mut self, workspace: impl Into<String>, reads: ServedReads) {
        self.served_mut().reads.insert(workspace.into(), reads);
        self.run_frame();
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

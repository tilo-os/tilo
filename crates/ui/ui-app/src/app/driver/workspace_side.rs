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
use tilo_actions::{Acting, ActionOutput, ApiError};

use super::Driver;
use crate::shown_actions::{ActionDecision, ActionVerdict, ShownAction};
use tilo_ui_base::{collaboration::CollabEvent, collaboration::CollabRequest};

/// One invocation of another provider's action ([`Driver::serve_provider`]).
pub struct ProvidedInvocation<'a> {
    /// The action as the provider publishes it.
    pub address: &'a tilo_actions::ActionAddress,
    pub argument: &'a InvokeArgument,
    /// Who invoked it: the caller of the call the app made it in when it continues one (an
    /// agent acting for its person, with what that person handed it), else the window's person.
    pub acting: &'a Acting,
    /// The person's yes, when the provider asked them and this is the call coming back.
    pub approval: Option<&'a tilo_actions::remote::asked::Approval>,
    /// What decides, in place of the workspace holding a record, the actions of it for an actor
    /// ([`Driver::decide_shown_actions`]); `None` where nothing does.
    decide: Option<&'a std::cell::RefCell<&'a mut ShownHost>>,
}

impl ProvidedInvocation<'_> {
    /// Admit this call, of `action` of a record its workspace holds, as that workspace admits
    /// it: by the actor's own grants there, an agent's never past its person's. Admitted where
    /// nothing decides it.
    ///
    /// # Errors
    /// The workspace's refusal, or that it asks its person first.
    pub fn admit(&self, action: &ShownAction) -> Result<(), ApiError> {
        let Some(decide) = self.decide else {
            return Ok(());
        };
        let verdict = (decide.borrow_mut())(self.acting, std::slice::from_ref(action))
            .pop()
            .map(|decision| decision.verdict);
        let refused = |reason: String| ApiError::Unavailable {
            code: "permission_denied".into(),
            reason,
        };
        match verdict {
            Some(ActionVerdict::Allowed) => Ok(()),
            Some(ActionVerdict::AsksFirst) => Err(ApiError::ConfirmationRequired),
            Some(ActionVerdict::Refused(why)) => Err(refused(why)),
            None => Err(refused(format!(
                "nothing decided `{}` of {}",
                action.action, action.resource
            ))),
        }
    }
}

/// What answers another provider's action in place of the workspace.
pub type ServedProvider = Box<dyn FnMut(&ProvidedInvocation<'_>) -> Result<ActionOutput, ApiError>>;

/// What answers the reads of one workspace's records in place of the workspace: the readings
/// (`{"revision", "records": [...]}`) as the record API answers the read, or why it cannot.
pub type ServedReads = Box<dyn Fn(&ResourceReadRequest) -> Result<serde_json::Value, String>>;

/// What decides, in place of the device's daemon, the actions of records another workspace
/// holds for the actor asking: those a window shows its person, and each call of one a served
/// provider is asked for, for its caller ([`ProvidedInvocation::admit`]).
pub type ShownHost = Box<dyn FnMut(&Acting, &[ShownAction]) -> Vec<ActionDecision>>;

/// The providers the caller serves and what decides the window's shown actions.
#[derive(Default)]
pub(super) struct Served {
    providers: HashMap<String, ServedProvider>,
    /// What answers the reads of each workspace's records, by the workspace.
    reads: HashMap<String, ServedReads>,
    /// Reads waiting for a write past the revision they name, by their correlation: answered once
    /// the record moved, as the workspace holds a followed read.
    held: Vec<(u64, ResourceReadRequest)>,
    /// What decides the shown actions, the window's person they are shown to, the actions the
    /// window last asked about, and the permissions revision they were decided at.
    shown: Option<(ShownHost, Acting, Vec<ShownAction>, Option<u64>)>,
}

impl Served {
    /// The key `request`'s provider is served under, when the caller serves it. A record of an
    /// app in a workspace is the app's there: what serves the app in that workspace answers each
    /// of its records, as the workspace delivers them to it.
    fn served_as(&self, request: &InvokeRequest) -> Option<String> {
        let provider = &request.target.provider;
        if self.providers.contains_key(provider) {
            return Some(provider.clone());
        }
        let (workspace, app) = tilo_actions::remote::application_provider(provider)?;
        let token = tilo_actions::remote::application_provider_token(workspace, app);
        self.providers.contains_key(&token).then_some(token)
    }

    /// Whether the caller serves `request`'s provider.
    pub(super) fn serves(&self, request: &InvokeRequest) -> bool {
        self.served_as(request).is_some()
    }

    /// The receipt of `request`, made for `acting`, from the provider the caller serves in its
    /// name; `None` when the caller serves no such provider.
    pub(super) fn invoke(
        &mut self,
        request: &InvokeRequest,
        acting: &Acting,
    ) -> Option<RemoteActionMessage> {
        let key = self.served_as(request)?;
        let Self {
            providers, shown, ..
        } = self;
        let serve = providers.get_mut(&key)?;
        let decide = shown
            .as_mut()
            .map(|(host, ..)| std::cell::RefCell::new(host));
        let status = match serve(&ProvidedInvocation {
            address: &request.target.address,
            argument: &request.argument,
            acting,
            approval: request.approval(),
            decide: decide.as_ref(),
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
        let (_, _, shown, decided) = self.shown.as_mut()?;
        shown.clone_from(actions);
        *decided = None;
        self.decide_shown()
    }

    /// Whether the shown actions are owed a decision: permissions changed since they were
    /// decided, or a host was given after the window asked.
    pub(super) fn owes_decision(&self) -> bool {
        self.shown.as_ref().is_some_and(|(_, _, _, decided)| {
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
        let (host, person, shown, decided) = self.shown.as_mut()?;
        *decided = Some(tilo_ui_base::permissions::shared_revision());
        if shown.is_empty() {
            return None;
        }
        Some(CollabEvent::ActionVerdicts {
            verdicts: host(person, shown),
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
    /// window asks of them is answered as it asks, and again whenever permissions change. The
    /// same host decides each call of such an action a served provider is asked for, as the
    /// workspace holding the record admits it for its caller. Connects the driver's peer when it
    /// is not.
    pub fn decide_shown_actions(&mut self, host: ShownHost) {
        let asked = self
            .peer_requests()
            .into_iter()
            .rev()
            .find_map(|request| match request {
                CollabRequest::DecideShownActions { actions } => Some(actions),
                _ => None,
            });
        let person = Acting::member(self.app.local_author.clone());
        self.served_mut().shown = Some((host, person, asked.unwrap_or_default(), None));
        self.run_frame();
    }
}

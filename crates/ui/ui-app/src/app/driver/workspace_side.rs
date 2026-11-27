//! What the workspace answers a driven app beyond its own calls, as the driver's peer plays it
//! ([`Driver::connect_peer`]): another provider's action is answered by what serves it
//! ([`Driver::serve_provider`]), a record of another workspace is read from what serves that
//! workspace ([`Driver::serve_records`]), and the actions a window shows are decided by what stands
//! for the device's daemon ([`Driver::decide_shown_actions`]), again whenever permissions change.

use std::collections::HashMap;

use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    ResourceReadRequest,
};
use tilo_actions::shown::{
    ActionDecision, ActionVerdict, ShownAction, ShownAsk, decisions_revision,
};
use tilo_actions::{Acting, ActionOutput, ApiError};

use super::Driver;

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
    decide: Option<&'a ShownHost>,
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
        let verdict = decide(self.acting, std::slice::from_ref(action))
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
            None => Err(refused(format!("nothing decided {:?}", action.node))),
        }
    }
}

/// What answers another provider's action in place of the workspace.
pub type ServedProvider = Box<dyn FnMut(&ProvidedInvocation<'_>) -> Result<ActionOutput, ApiError>>;

/// What answers the reads of one workspace's records in place of the workspace: the readings
/// (`{"revision", "records": [...]}`) as the record API answers the read, or why it cannot.
pub type ServedReads = Box<dyn Fn(&ResourceReadRequest) -> Result<serde_json::Value, String>>;

/// What decides, in place of the device's daemon, the actions of records a workspace keeps for
/// the actor asking: those a window shows its person, and each call of one a served provider is
/// asked for, for its caller ([`ProvidedInvocation::admit`]).
pub type ShownHost = Box<dyn Fn(&Acting, &[ShownAction]) -> Vec<ActionDecision>>;

pub use tilo_ui::application::ShownDevice;

/// An ask of the window's shown actions the driver keeps until they change.
struct WaitingAsk {
    correlation: u64,
    ask: ShownAsk,
}

/// The providers the caller serves and what decides the window's shown actions.
#[derive(Default)]
pub(super) struct Served {
    providers: HashMap<String, ServedProvider>,
    /// What answers the reads of each workspace's records, by the workspace.
    reads: HashMap<String, ServedReads>,
    /// Reads waiting for a write past the revision they name, by their correlation: answered once
    /// the record moved, as the workspace holds a followed read.
    held: Vec<(u64, ResourceReadRequest)>,
    /// What decides the shown actions, the window's person they are shown to, and what the window
    /// asked for that the driver keeps.
    shown: Option<(ShownHost, Acting, Option<WaitingAsk>)>,
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
        let decide = shown.as_ref().map(|(host, ..)| host);
        let status = match serve(&ProvidedInvocation {
            address: &request.target.address,
            argument: &request.argument,
            acting,
            approval: request.approval(),
            decide,
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

    /// Answer what the window asks of its device that is the shown actions: decided now, or
    /// waiting for the grants to change when it names the decisions it shows and a wait.
    pub(super) fn asked(&mut self, message: &RemoteActionMessage) -> Option<RemoteActionMessage> {
        let RemoteActionMessage::ShownActionsRequest {
            correlation,
            actions,
            after,
            wait_ms,
        } = message
        else {
            return None;
        };
        let (host, person, waiting) = self.shown.as_mut()?;
        let ask = ShownAsk {
            actions: actions.clone(),
            after: after.clone(),
            wait_ms: *wait_ms,
        };
        let decisions = host(person, &ask.actions);
        let revision = decisions_revision(&decisions);
        let waits = wait_ms.is_some()
            && !ask.actions.is_empty()
            && after.as_deref() == Some(revision.as_str());
        if waits {
            *waiting = Some(WaitingAsk {
                correlation: *correlation,
                ask,
            });
            return None;
        }
        *waiting = None;
        Some(RemoteActionMessage::ShownActionsResponse {
            correlation: *correlation,
            result: Ok(decisions),
        })
    }

    /// What the device standing in says of `action` on `resource`, which `app` publishes: the
    /// verdicts a call of it is admitted again by where it lands, as the device admits every
    /// call that reaches it. `None` when nothing stands in for the device.
    pub(super) fn verdicts_of(
        &self,
        app: &str,
        (resource, action): (&tilo_actions::ResourceSpec, &tilo_actions::ActionSpec),
        argument: &tilo_actions::remote::InvokeArgument,
    ) -> Option<tilo_ui_base::permissions::Verdicts> {
        let (host, person, _) = self.shown.as_ref()?;
        let mut verdicts = tilo_ui_base::permissions::Verdicts::default();
        verdicts.ask([ShownAction::published(app, resource, action)]);
        // What admitting the call reads (the value a generic edit carries) is asked with it.
        let _ = verdicts.admit(app, resource, action, carried(argument));
        verdicts.take_missed();
        verdicts.decide(&host(person, &verdicts.asked()));
        Some(verdicts)
    }

    /// What decides the actions the window shows, and the person it shows them to, when anything
    /// stands in for its device.
    fn host(&self) -> Option<(&ShownHost, &Acting)> {
        self.shown.as_ref().map(|(host, person, _)| (host, person))
    }

    /// Whether the shown actions kept are owed a decision: what the device says of them now
    /// differs from what the window shows.
    pub(super) fn owes_decision(&self) -> bool {
        self.shown.as_ref().is_some_and(|(host, person, waiting)| {
            waiting.as_ref().is_some_and(|waiting| {
                decisions_revision(&host(person, &waiting.ask.actions))
                    != waiting.ask.after.clone().unwrap_or_default()
            })
        })
    }

    /// The decisions a kept ask is owed now.
    pub(super) fn unasked(&mut self) -> Option<RemoteActionMessage> {
        if !self.owes_decision() {
            return None;
        }
        self.answer_held()
    }

    /// Answer a kept ask, as the device does when what it says of the actions changes.
    fn answer_held(&mut self) -> Option<RemoteActionMessage> {
        let (host, person, waiting) = self.shown.as_mut()?;
        let waiting = waiting.take()?;
        Some(RemoteActionMessage::ShownActionsResponse {
            correlation: waiting.correlation,
            result: Ok(host(person, &waiting.ask.actions)),
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

/// What answers a window's questions in place of its device when no driver serves it: a live
/// window a harness opens on its own, answered as [`Driver::decide_shown_actions`] answers.
pub(in crate::app) struct StandIn(Served);

impl StandIn {
    /// `device` deciding the questions of the window `person` is at.
    pub(in crate::app) fn new(device: ShownDevice, person: Acting) -> Self {
        Self(Served {
            providers: HashMap::new(),
            reads: HashMap::new(),
            held: Vec::new(),
            shown: Some((Box::new(move |_, actions| device(actions)), person, None)),
        })
    }

    /// The answer to `message` when it asks what the window shows: `None` for anything else, and
    /// for an ask waiting until the answer changes.
    pub(in crate::app) fn asked(
        &mut self,
        message: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        self.0.asked(message)
    }

    /// The answer a kept ask is owed now that the device's verdicts changed.
    pub(in crate::app) fn unasked(&mut self) -> Option<RemoteActionMessage> {
        self.0.unasked()
    }

    fn host(&self) -> Option<(&ShownHost, &Acting)> {
        self.0.host()
    }
}

/// Decide by `host` what `permissions` read and nothing asked yet: whether that moved them.
fn decide_unasked(
    host: &dyn Fn(&[ShownAction]) -> Vec<ActionDecision>,
    permissions: &tilo_ui_base::permissions::SharedPermissions,
) -> bool {
    permissions.update(|verdicts| {
        let before = verdicts.clone();
        verdicts.take_missed();
        let asked = verdicts.asked();
        if !asked.is_empty() {
            verdicts.decide(&host(&asked));
        }
        *verdicts != before
    })
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

    /// Answer the reads of `workspace`'s records with `reads`, as the workspace answers a window
    /// reading a record of another workspace than its own (the person's own records): a followed
    /// read is held until the record moves past the revision it names. Reads asked before this
    /// workspace was served were held for it, and are answered now. Connects the driver's peer
    /// when it is not.
    pub fn serve_records(&mut self, workspace: impl Into<String>, reads: ServedReads) {
        self.served_mut().reads.insert(workspace.into(), reads);
        self.run_frame();
    }

    /// Answer what the window's components read, as a window's root asks its device: a driven
    /// window of bare components has none that asks. The device standing in decides
    /// ([`Self::decide_shown_actions`], or the one its process was handed); without one, a device
    /// of the workspace each question names, which the person at the window owns. Whether that
    /// moved what the window shows.
    pub(super) fn decide_unasked(&self) -> bool {
        let mut followed: Vec<tilo_ui_base::permissions::SharedPermissions> = Vec::new();
        let root = crate::bound_dispatch::callback_binding(&*self.app.root_widget);
        let components = self
            .app
            .bindings
            .nodes
            .values()
            .filter_map(|node| node.binding.as_ref())
            .map(|binding| {
                crate::bound_dispatch::callback_binding_of(&**binding)
                    .permissions
                    .clone()
            });
        for permissions in root
            .map(|binding| binding.permissions)
            .into_iter()
            .chain(components)
        {
            if !followed.iter().any(|known| known.ptr_eq(&permissions)) {
                followed.push(permissions);
            }
        }
        let person = self.app.local_author().clone();
        let owner = move |actions: &[ShownAction]| {
            actions
                .iter()
                .flat_map(|action| {
                    crate::harness_device::HarnessDevice::member_of(
                        action
                            .workspace
                            .as_deref()
                            .unwrap_or(tilo_ui_base::permissions::LOCAL_WORKSPACE),
                        &person,
                        tilo_ui_base::permissions::Role::Owner,
                    )
                    .decide(std::slice::from_ref(action))
                })
                .collect()
        };
        let served = self
            .peer
            .as_ref()
            .and_then(|peer| peer.served_by_caller.host())
            .or_else(|| {
                self.app
                    .collaboration
                    .stand_in
                    .as_ref()
                    .and_then(StandIn::host)
            });
        let host = |actions: &[ShownAction]| match served {
            Some((host, person)) => host(person, actions),
            None => owner(actions),
        };
        // Every one is answered, whether or not an earlier one moved.
        let mut moved = false;
        for permissions in &followed {
            moved |= decide_unasked(&host, permissions);
        }
        moved
    }

    /// Decide with `host`, as the device's daemon does, the actions the window shows: what the
    /// window asks of them is answered as it asks, and again whenever permissions change. The
    /// same host decides each call of such an action a served provider is asked for, as the
    /// workspace holding the record admits it for its caller. Connects the driver's peer when it
    /// is not.
    pub fn decide_shown_actions(&mut self, host: ShownHost) {
        let waiting = self
            .served_mut()
            .shown
            .take()
            .and_then(|(.., waiting)| waiting);
        let person = Acting::member(self.app.local_author.clone());
        self.served_mut().shown = Some((host, person, waiting));
        self.run_frame();
    }
}

/// The typed value or text a call of an action carries, as admitting it reads them.
pub(super) fn carried(
    argument: &tilo_actions::remote::InvokeArgument,
) -> (
    Option<&serde_json::Map<String, serde_json::Value>>,
    Option<&str>,
) {
    match argument {
        tilo_actions::remote::InvokeArgument::Json(arguments) => (Some(arguments), None),
        tilo_actions::remote::InvokeArgument::Text(text) => (None, Some(text.as_str())),
        _ => (None, None),
    }
}

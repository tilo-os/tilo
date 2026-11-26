//! The calls, jobs and controls a driven app runs, and what it answered about them.
//!
//! A job is started and cancelled as the workspace's broker starts and cancels one: delivered to
//! the app through the driver's peer ([`Driver::connect_peer`]), so it runs on the app's own
//! collaboration path for its invoker, and its progress and outcome are what the app answers its
//! peer. What the app refused is read where the app says it (at the control, in the window's bar)
//! and from the refusals it keeps for its host.

use std::io::{self, Write};

use accesskit::{Action as AkAction, Role};
use serde::Serialize;
use tilo_actions::{ActionAddress, ActionOutput, ApiError, InvocationId, InvocationUpdate};

use super::Driver;
#[cfg(feature = "model")]
use tilo_ui_base::collaboration::CollabRequest;
use tilo_ui_base::{widget::ResourceOperationStart, widget::Widget};

/// What the driver keeps of the calls it made and the requests it read.
#[derive(Default)]
pub(super) struct Jobs {
    /// The outcomes of the scenario's invocations, once asked to keep them
    /// ([`Driver::retain_invocation_results`]).
    results: Option<InvocationResults>,
    /// How many of the requests the app made of its peer were already taken
    /// ([`Driver::take_collaboration_requests`]).
    taken: usize,
}

impl Driver {
    #[cfg(feature = "model")]
    /// Every update the job `invocation` reported to its caller so far, oldest first: its
    /// progress, then what it ended as.
    #[must_use]
    pub fn job_updates(&self, invocation: InvocationId) -> Vec<InvocationUpdate> {
        self.call_updates(invocation)
    }

    #[cfg(feature = "model")]
    /// Start `address` with `arguments` as the workspace's broker delivers an admitted call, for
    /// `invoker` (an agent, a member) or, with none, for the person at this device; sent again with
    /// the person's approve when `approval` holds it ([`Self::approval`]). The broker's admission
    /// runs first against what the app publishes, so an unknown, unavailable or unapproved action
    /// is refused before it reaches the app. The
    /// invocation whose progress [`Self::job_updates`] reads, and how it started: completed at
    /// once, or accepted to run on.
    ///
    /// # Errors
    /// The admission's refusal, or the app's failure when the call failed at once.
    ///
    /// # Panics
    /// When the driver's peer is not connected ([`Self::connect_peer`]).
    pub fn start_bound_job_for(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
    ) -> Result<(InvocationId, ResourceOperationStart), ApiError> {
        self.start_job_within(invoker, None, address, arguments, approval, None)
    }

    #[cfg(feature = "model")]
    /// Cancel the running job `invocation`, as its caller does through the broker: the job decides
    /// where it can stop and answers with what it ended as, which is also in
    /// [`Self::job_updates`]. `None` when it had already ended, or has not answered yet.
    ///
    /// # Panics
    /// When `invocation` was not started through the driver's peer.
    pub fn cancel_job(&mut self, invocation: InvocationId) -> Option<InvocationUpdate> {
        if self
            .call_updates(invocation)
            .iter()
            .any(InvocationUpdate::is_terminal)
        {
            return None;
        }
        self.cancel_call(invocation);
        self.call_updates(invocation)
            .into_iter()
            .find(InvocationUpdate::is_terminal)
    }

    #[cfg(feature = "model")]
    /// The resources the app's hosted components publish, as their permissions let their bound
    /// actions be invoked. With `id`, only that resource: an app may resolve it lazily (a search
    /// hit, an open document), so it can be absent from the root listing and still be
    /// addressable.
    ///
    /// # Errors
    /// Refuses a closed or unavailable component.
    pub fn bound_resources(
        &self,
        id: Option<&str>,
    ) -> Result<Vec<tilo_actions::ResourceSpec>, ApiError> {
        self.bound_resources_of(None, id)
    }

    #[cfg(feature = "model")]
    /// [`Self::bound_resources`] of one hosted component instance, such as one window of a shell;
    /// `None` for every hosted component.
    ///
    /// # Errors
    /// Refuses a closed or unavailable component.
    pub fn bound_resources_of(
        &self,
        instance: Option<crate::application::InstanceId>,
        id: Option<&str>,
    ) -> Result<Vec<tilo_actions::ResourceSpec>, ApiError> {
        let _wakes = self.app.enter_wake_scope();
        let lazy = id.map(|id| ActionAddress::new(id, ""));
        let resources = crate::bound_dispatch::discover_bound_resources(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            lazy.as_ref(),
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_api())?
        .into_iter()
        .filter(|snapshot| instance.is_none_or(|instance| snapshot.binding.instance_id == instance))
        .flat_map(|snapshot| snapshot.into_resources())
        .collect::<Vec<_>>();
        Ok(match id {
            Some(id) => resources
                .iter()
                .find(|resource| resource.id.as_str() == id)
                .or_else(|| crate::bound_dispatch::find_action_resource(&resources, id))
                .cloned()
                .into_iter()
                .collect(),
            None => resources,
        })
    }

    /// Why the invocations made in the app that did not run did not, oldest first: what the app
    /// refused of the person and of itself, then the calls and jobs that ended failed or
    /// cancelled, as the app answered its peer.
    #[must_use]
    pub fn control_failures(&self) -> Vec<String> {
        let mut failures = self
            .app
            .actions
            .refused
            .reasons()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        #[cfg(feature = "model")]
        {
            failures.extend(self.call_failures());
            failures.extend(
                self.peer_requests()
                    .into_iter()
                    .filter_map(|request| match request {
                        CollabRequest::ResourceLocalEffect { update, .. } => ended_badly(&update),
                        _ => None,
                    }),
            );
        }
        failures
    }

    /// Every operable control the exported accessibility tree carries now, the window's bar
    /// included, in tree order: what a person or a screen reader can press, fill or move.
    #[must_use]
    pub fn controls(&self) -> Vec<crate::headless::Control> {
        let titles = self.row_titles();
        let focused = self.a11y_focus().map(|(id, _)| id.0.to_string());
        self.a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() != Role::Window)
            .filter_map(|(id, node)| {
                let verbs: Vec<String> = VERBS
                    .iter()
                    .filter(|(action, _)| node.supports_action(*action))
                    .map(|(_, name)| (*name).to_owned())
                    .collect();
                if verbs.is_empty() && !is_pressable(node.role()) {
                    return None;
                }
                let id_text = id.0.to_string();
                Some(crate::headless::Control {
                    focused: focused.as_deref() == Some(id_text.as_str()),
                    id: id_text,
                    role: node.role(),
                    name: node.label().map(str::to_owned),
                    title: titles.get(&id).cloned(),
                    value: node.value().map(str::to_owned),
                    bounds: node.bounds().map(|bounds| {
                        vello::kurbo::Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
                    }),
                    disabled: node.is_disabled(),
                    verbs,
                })
            })
            .collect()
    }

    /// The row title each exported node's accessible name starts with, by its exported id: the line
    /// a person finds a list row by, beside the row's title.
    #[must_use]
    pub fn a11y_row_titles(&self) -> Vec<(accesskit::NodeId, String)> {
        self.row_titles().into_iter().collect()
    }

    /// The row title each exported node's accessible name starts with, for a list row named by its
    /// title and its secondary line.
    pub(super) fn row_titles(&self) -> std::collections::HashMap<accesskit::NodeId, String> {
        let app = &*self.app;
        let exported = app.bindings.exported_nodes(&app.layout.node_map);
        let palette = app.palette_presented();
        let (_, titles) = if app.history_widget().is_none() && !palette && app.window_box.is_drawn()
        {
            let roots: [&dyn Widget; 2] = [app.root_widget.as_ref(), &app.window_box];
            tilo_ui_base::accesskit_bridge::build_presentation_tree_with_titles_bound(
                &roots,
                &exported.nodes,
                exported.focus(app.focused_node),
                1.0,
            )
        } else if app.history_widget().is_none() && !palette {
            tilo_ui_base::accesskit_bridge::build_tree_update_with_titles_bound(
                app.root_widget.as_ref(),
                &exported.nodes,
                exported.focus(app.focused_node),
                1.0,
            )
        } else {
            tilo_ui_base::accesskit_bridge::build_presentation_tree_with_titles_bound(
                &app.presentation_roots(),
                &exported.nodes,
                exported.focus(app.focused_node),
                1.0,
            )
        };
        titles.into_iter().collect()
    }

    /// Why an invocation the person made did not run, wherever the app says it: at the control
    /// they used, else in the window's bar.
    #[must_use]
    pub fn refusal_said(&self) -> Option<String> {
        self.app
            .control_notice()
            .map(|notice| notice.text.clone())
            .or_else(|| {
                crate::window_toolbar::notice()
                    .filter(|notice| notice.kind == crate::widget::toast::ToastKind::Error)
                    .map(|notice| notice.text)
            })
    }

    /// Whether the record the window was opened on has not arrived: the component shows its
    /// opening view, and what it will publish is read from the record once that is here.
    fn record_still_arriving(&self) -> bool {
        #[cfg(feature = "model")]
        let arriving = crate::bound_dispatch::callback_binding(&*self.app.root_widget)
            .is_some_and(|binding| binding.records.read(&binding.checkout.id, |_| ()).is_none());
        // Without Tilo's model a window is opened on no record.
        #[cfg(not(feature = "model"))]
        let arriving = false;
        arriving
    }

    #[cfg(feature = "model")]
    /// Every action contract violation for which the app would withdraw a hosted component's
    /// published surface: it withdraws a whole component when one of its actions breaks its
    /// contract, so a test asserts this is empty rather than finding the surface gone at runtime.
    pub fn component_contract_violations(&mut self) -> Vec<tilo_actions::ContractViolation> {
        let _wakes = self.app.enter_wake_scope();
        let components = match crate::bound_dispatch::discover_cached_bound_resources(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            &self.app.actions.bound_resource_cache,
            self.app.actions.resource_generation.get(),
        ) {
            Ok(components) => components,
            // A record still on its way has no resources to hold to their contracts yet: the
            // window shows its opening view, and the check runs once the record opens.
            Err(_) if self.record_still_arriving() => Vec::new(),
            Err(error) => {
                return vec![tilo_actions::ContractViolation {
                    address: "application-root".into(),
                    reason: format!("bound resources are not discoverable: {error}"),
                }];
            }
        };
        let mut violations = Vec::new();
        for component in components {
            match crate::bound_dispatch::bound_contract_violations(
                self.app.root_widget.as_mut(),
                &component.binding,
                component.resources(),
            ) {
                Ok(Some(found)) => violations.extend(found),
                Ok(None) => violations.push(tilo_actions::ContractViolation {
                    address: format!("{:?}", component.binding.instance_id),
                    reason: "component is not mounted".into(),
                }),
                Err(error) => violations.push(tilo_actions::ContractViolation {
                    address: format!("{:?}", component.binding.instance_id),
                    reason: format!("{error:?}"),
                }),
            }
        }
        violations
    }

    /// Keep the outcomes of the scenario invocations run from now on ([`Self::perform`]), for
    /// [`Self::invocation_results_json`]: what each answered, never the arguments it was given.
    pub fn retain_invocation_results(&mut self) {
        self.jobs.results = Some(InvocationResults::new(self.app.local_author.clone()));
    }

    /// The kept outcomes, Completed, Accepted or Refused, in invocation order. Accepted means only
    /// that the app took the call, never that a job it started succeeded.
    ///
    /// # Errors
    /// When the outcomes are not kept, or no longer fit their bound.
    pub fn invocation_results_json(&self) -> Result<String, String> {
        let results = self
            .jobs
            .results
            .as_ref()
            .ok_or("invocation result retention is disabled")?;
        serde_json::to_writer(
            &mut Limited {
                remaining: MAX_BYTES,
            },
            results,
        )
        .map_err(|error| format!("retained invocation envelope exceeds its limit: {error}"))?;
        serde_json::to_string(results).map_err(|error| error.to_string())
    }

    /// Keep what the invocation of `address` answered, when outcomes are kept.
    pub(super) fn retain_invocation_outcome(
        &mut self,
        address: &ActionAddress,
        result: &Result<ResourceOperationStart, ApiError>,
    ) -> Result<(), String> {
        match &mut self.jobs.results {
            Some(results) => results.record(address, result),
            None => Ok(()),
        }
    }

    /// Set how many bytes the kept outcomes may still take, so a test reaches the retention bound
    /// without filling it: what the store would hit on a long run.
    #[cfg(test)]
    pub(crate) fn set_invocation_result_bytes(&mut self, remaining: usize) {
        if let Some(results) = &mut self.jobs.results {
            results.remaining = remaining;
        }
    }

    #[cfg(feature = "model")]
    /// What the app asked of its peer since this was last asked, oldest first: the transport
    /// requests a live window's host takes from it.
    ///
    /// # Panics
    /// When the driver's peer is not connected ([`Self::connect_peer`]).
    pub fn take_collaboration_requests(&mut self) -> Vec<CollabRequest> {
        assert!(
            self.peer.is_some(),
            "connect the driver's peer first (Driver::connect_peer)"
        );
        let requests = self.peer_requests();
        let fresh = requests.get(self.jobs.taken..).unwrap_or_default().to_vec();
        self.jobs.taken = requests.len();
        fresh
    }

    /// Every on-screen route to the action at `address`, usable, visible and short ones first: the
    /// control a person would press for the call an agent makes.
    pub fn action_ui_routes(&mut self, address: &ActionAddress) -> Vec<crate::ActionUiRoute> {
        self.app.action_ui_routes(address)
    }
}

#[cfg(feature = "model")]
/// What an `update` that ended failed or cancelled says of why.
fn ended_badly(update: &InvocationUpdate) -> Option<String> {
    match update {
        InvocationUpdate::Failed { error, .. } => {
            Some(format!("{}: {}", error.code, error.message))
        }
        InvocationUpdate::Cancelled { .. } => Some("cancelled".to_owned()),
        _ => None,
    }
}

/// The refusal a call to `address` that failed at once is to its caller: the code the app
/// answered with kept as data.
pub(super) fn failure_refusal(
    address: &ActionAddress,
    error: tilo_actions::ActionFailure,
) -> ApiError {
    // An unavailable action names why in its details ([`ApiError::failure_details`]), whichever
    // code its failure went under.
    let detail = |key: &str| {
        error
            .details
            .as_ref()
            .and_then(|details| details.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    if let (Some(code), Some(reason)) = (detail("code"), detail("reason")) {
        return ApiError::Unavailable { code, reason };
    }
    match error.code.as_str() {
        "unknown_action" => ApiError::Unknown(address.clone()),
        "invalid_arguments" => ApiError::InvalidArguments(error.message),
        "action_confirmation_required" => ApiError::ConfirmationRequired,
        "action_failed" => ApiError::Failed(error.message),
        _ => ApiError::Refused {
            code: error.code,
            reason: error.message,
        },
    }
}

/// The accessibility verbs a control answers, by the names a scenario uses for them.
const VERBS: &[(AkAction, &str)] = &[
    (AkAction::Click, "activate"),
    (AkAction::Focus, "focus"),
    (AkAction::SetValue, "set-value"),
    (AkAction::Increment, "increment"),
    (AkAction::Decrement, "decrement"),
    (AkAction::Expand, "expand"),
    (AkAction::Collapse, "collapse"),
    (AkAction::ShowContextMenu, "show-context-menu"),
];

/// Whether a node of `role` is a control a person presses, even when it names no verb.
fn is_pressable(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::CheckBox
            | Role::RadioButton
            | Role::Switch
            | Role::Tab
            | Role::MenuItem
            | Role::Link
            | Role::ListItem
            | Role::TreeItem
    )
}

/// How many outcomes are kept, and how many bytes they may take serialized.
const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Completed { output: ActionOutput },
    Accepted,
    Refused { code: String, message: String },
}

#[derive(Serialize)]
struct Entry {
    address: ActionAddress,
    #[serde(flatten)]
    outcome: Outcome,
}

/// The kept outcomes of a scenario's invocations, with whom they ran for.
#[derive(Serialize)]
struct InvocationResults {
    schema_version: u8,
    author: tilo_ui_base::permissions::MemberId,
    entries: Vec<Entry>,
    /// Why outcomes after the kept ones are missing: a log that stopped short says so, so it is
    /// never read as complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_error: Option<String>,
    #[serde(skip)]
    remaining: usize,
}

impl InvocationResults {
    fn new(author: tilo_ui_base::permissions::MemberId) -> Self {
        Self {
            schema_version: 1,
            author,
            entries: Vec::new(),
            retention_error: None,
            remaining: MAX_BYTES.saturating_sub(1024),
        }
    }

    fn record(
        &mut self,
        address: &ActionAddress,
        result: &Result<ResourceOperationStart, ApiError>,
    ) -> Result<(), String> {
        if let Some(error) = &self.retention_error {
            return Err(error.clone());
        }
        let outcome = match result {
            Ok(ResourceOperationStart::Completed(output)) => Outcome::Completed {
                output: output.clone(),
            },
            Ok(ResourceOperationStart::Accepted) => Outcome::Accepted,
            Err(error) => Outcome::Refused {
                code: error.code().into(),
                message: error.to_string(),
            },
        };
        let entry = Entry {
            address: address.clone(),
            outcome,
        };
        let mut counter = Limited {
            remaining: self.remaining,
        };
        if self.entries.len() >= MAX_ENTRIES || serde_json::to_writer(&mut counter, &entry).is_err()
        {
            let message = format!(
                "invocation result retention limit reached after action {}; its actual result is not retained",
                self.entries.len().saturating_add(1)
            );
            self.retention_error = Some(message.clone());
            return Err(message);
        }
        self.remaining = counter.remaining.saturating_sub(1);
        self.entries.push(entry);
        Ok(())
    }
}

/// A writer that counts down the bytes left and refuses past them.
struct Limited {
    remaining: usize,
}

impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("result limit exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

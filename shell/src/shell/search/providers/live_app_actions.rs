//! Dynamic provider backed by the resource/action surfaces live qOS apps
//! publish through the session collaboration broker.

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::search::ActionReceipt;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};

pub const PROVIDER_ID: &str = "app-actions";
const ID_SEPARATOR: char = '\u{1f}';

pub fn job_status_id(id: u64) -> String {
    format!("job{ID_SEPARATOR}{id}")
}

pub fn job_cancel_id(id: u64) -> String {
    format!("job-cancel{ID_SEPARATOR}{id}")
}

pub struct LiveAppActionsProvider {
    broker: AppActionBroker,
}

struct PeerInvocation<'a> {
    peer_id: u64,
    resource: &'a str,
    action: &'a str,
    argument: Option<&'a str>,
    actor: &'a str,
    confirmation: &'a qos_actions::ConfirmationPolicy,
    confirmed: bool,
    phrase: Option<&'a str>,
}

fn start_on_peer(
    broker: &AppActionBroker,
    invocation: &PeerInvocation<'_>,
) -> Result<crate::shell::collab_broker_capnp::InvocationJob, String> {
    match invocation.confirmation {
        qos_actions::ConfirmationPolicy::None => broker.start(
            invocation.peer_id,
            invocation.resource,
            invocation.action,
            invocation.argument,
            invocation.actor,
        ),
        qos_actions::ConfirmationPolicy::Confirm => {
            if !invocation.confirmed {
                return Err("explicit confirmation is required".into());
            }
            let token = broker.prepare_confirmation(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                None,
            )?;
            broker.start_confirmed(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                &token,
            )
        }
        qos_actions::ConfirmationPolicy::Phrase { .. } => {
            let token = broker.prepare_confirmation(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                invocation.phrase,
            )?;
            broker.start_confirmed(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                &token,
            )
        }
    }
}

fn deadline_after(duration: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(duration).unwrap_or(now)
}

fn receipt_for_job(
    broker: &AppActionBroker,
    job: crate::shell::collab_broker_capnp::InvocationJob,
) -> ActionReceipt {
    let update = broker
        .status(job.id)
        .unwrap_or_else(|| qos_actions::InvocationUpdate::Accepted {
            invocation: qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job.id))),
        });
    drop(job.completion);
    ActionReceipt {
        broker_invocation_id: Some(job.id),
        update,
    }
}

fn invocation_argument(
    args: &crate::shell::search::action_api::ActionArgs,
    typed_arguments: bool,
    wants_content: bool,
) -> (Option<String>, bool, Option<String>) {
    let encoded = typed_arguments.then(|| {
        let mut application_args = args.to_json();
        application_args.remove("confirm");
        application_args.remove("confirmation_phrase");
        format!(
            "{}{}",
            qos_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(application_args)
        )
    });
    let argument = encoded.or_else(|| {
        if wants_content {
            args.text("content").map(str::to_owned)
        } else {
            args.text("argument").map(str::to_owned)
        }
    });
    (
        argument,
        args.boolean("confirm") == Some(true),
        args.text("confirmation_phrase").map(str::to_owned),
    )
}

impl LiveAppActionsProvider {
    pub fn new(broker: AppActionBroker) -> Self {
        Self { broker }
    }

    pub fn capability_target(id: &str) -> Option<(&str, qos_actions::PermissionRequirement, &str)> {
        let mut parts = id.split(ID_SEPARATOR);
        let app = parts.next()?;
        let permission = match parts.next()? {
            "none" => qos_actions::PermissionRequirement::None,
            "read" => qos_actions::PermissionRequirement::Read,
            "comment" => qos_actions::PermissionRequirement::Comment,
            "write" => qos_actions::PermissionRequirement::Write,
            "invite" => qos_actions::PermissionRequirement::Invite,
            "admin" => qos_actions::PermissionRequirement::Admin,
            _ => return None,
        };
        let _workspace = parts.next()?;
        let _record = parts.next()?;
        let _resource = parts.next()?;
        let action = parts.next()?;
        parts.next().is_none().then_some((app, permission, action))
    }

    fn result_id(surface: &PublishedSurface, action: &PublishedAction) -> String {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}",
            surface.app_id,
            ID_SEPARATOR,
            match action.contract.permission {
                qos_actions::PermissionRequirement::None => "none",
                qos_actions::PermissionRequirement::Read => "read",
                qos_actions::PermissionRequirement::Comment => "comment",
                qos_actions::PermissionRequirement::Write => "write",
                qos_actions::PermissionRequirement::Invite => "invite",
                qos_actions::PermissionRequirement::Admin => "admin",
            },
            ID_SEPARATOR,
            surface.route.cluster_id,
            ID_SEPARATOR,
            surface.route.record_id,
            ID_SEPARATOR,
            action.resource,
            ID_SEPARATOR,
            action.action
        )
    }

    fn matches_scope(surface: &PublishedSurface, ctx: &SearchContext<'_>) -> bool {
        let active_room = ctx
            .chrome
            .tree()
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        match ctx.chrome.search_scope {
            crate::shell::chrome_state::SearchScope::All => true,
            crate::shell::chrome_state::SearchScope::Workspace => {
                active_room.as_deref() == Some(surface.route.cluster_id.as_str())
            }
            crate::shell::chrome_state::SearchScope::ActiveApp => {
                active_room.as_deref() == Some(surface.route.cluster_id.as_str())
                    && ctx
                        .chrome
                        .active_app_id()
                        .is_some_and(|app| app.eq_ignore_ascii_case(&surface.app_id))
            }
        }
    }

    fn action_params(action: &PublishedAction) -> Vec<ActionParam> {
        let mut params = if action.params.is_empty() {
            match action.input {
                PublishedInput::None => Vec::new(),
                PublishedInput::Text {
                    wants_content: true,
                    ..
                } => vec![ActionParam::required(
                    "content",
                    ParamType::Text,
                    "Content supplied to the application action",
                )],
                PublishedInput::Text { .. } => vec![ActionParam::required(
                    "argument",
                    ParamType::Text,
                    "Text supplied to the application action",
                )],
            }
        } else {
            action.params.clone()
        };
        match &action.contract.confirmation {
            qos_actions::ConfirmationPolicy::None => {}
            qos_actions::ConfirmationPolicy::Confirm => params.push(ActionParam::required(
                "confirm",
                ParamType::Boolean,
                "Explicitly confirm this consequential action",
            )),
            qos_actions::ConfirmationPolicy::Phrase { .. } => {
                params.push(ActionParam::required(
                    "confirmation_phrase",
                    ParamType::Text,
                    "Type the action's confirmation phrase exactly",
                ));
            }
        }
        params
    }
}

impl SearchProvider for LiveAppActionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Application actions"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn owns_tab(&self, tab_id: u64, chrome: &crate::shell::chrome_state::ChromeState) -> bool {
        chrome.active_tab_id() == Some(tab_id)
            && chrome.active_app_id().is_some_and(|active| {
                self.broker
                    .surfaces()
                    .iter()
                    .any(|surface| active.eq_ignore_ascii_case(&surface.app_id))
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the query constructs paired palette/headless callbacks from one captured action contract; splitting them would duplicate or desynchronise confirmation, launch, and job lifecycle state"
    )]
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let needle = ctx.needle.trim().to_lowercase();
        let actor = ctx.chrome.active_local_user();
        let mut results = Vec::new();
        if needle.is_empty() || "jobs status progress".contains(&needle) {
            for (id, update) in self.broker.statuses(&actor) {
                let state = match &update {
                    qos_actions::InvocationUpdate::Accepted { .. } => "accepted",
                    qos_actions::InvocationUpdate::Progress { .. } => "running",
                    qos_actions::InvocationUpdate::Item { .. } => "streaming",
                    qos_actions::InvocationUpdate::Succeeded { .. } => "succeeded",
                    qos_actions::InvocationUpdate::Failed { .. } => "failed",
                    qos_actions::InvocationUpdate::Cancelled { .. } => "cancelled",
                };
                let status_broker = self.broker.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Custom("action-job".into()),
                    provider_id: PROVIDER_ID.into(),
                    id: job_status_id(id),
                    title: format!("Action job {id}"),
                    subtitle: Some(state.into()),
                    score: -10,
                    params: Vec::new(),
                    contract: None,
                    on_open: std::rc::Rc::new(|_, _, _, _| Ok(())),
                    on_invoke: Some(std::rc::Rc::new(move |_, _, _, _| {
                        let update = status_broker
                            .status(id)
                            .ok_or_else(|| ActionError::Refused("unknown invocation id".into()))?;
                        Ok(ActionReceipt {
                            broker_invocation_id: Some(id),
                            update,
                        })
                    })),
                });
                if self.broker.can_cancel(id, &actor) {
                    let broker = self.broker.clone();
                    let actor = actor.clone();
                    let invoke_broker = broker.clone();
                    let invoke_actor = actor.clone();
                    results.push(SearchResult {
                        kind: ResourceKind::Action,
                        provider_id: PROVIDER_ID.into(),
                        id: job_cancel_id(id),
                        title: format!("Cancel action job {id}"),
                        subtitle: Some("Requests cooperative cancellation".into()),
                        score: 5,
                        params: Vec::new(),
                        contract: None,
                        on_open: std::rc::Rc::new(move |_, _, _, _| {
                            broker.cancel(id, &actor).map_err(ActionError::Refused)
                        }),
                        on_invoke: Some(std::rc::Rc::new(move |_, _, _, _| {
                            invoke_broker
                                .cancel(id, &invoke_actor)
                                .map_err(ActionError::Refused)?;
                            let update = invoke_broker.status(id).ok_or_else(|| {
                                ActionError::Refused("unknown invocation id".into())
                            })?;
                            Ok(ActionReceipt {
                                broker_invocation_id: Some(id),
                                update,
                            })
                        })),
                    });
                }
            }
        }
        for surface in self.broker.surfaces() {
            if !Self::matches_scope(&surface, &ctx) {
                continue;
            }
            for action in &surface.actions {
                let unavailable_reason = match &action.contract.availability {
                    qos_actions::Availability::Available => None,
                    qos_actions::Availability::Unavailable { reason, .. } => Some(reason.clone()),
                };
                let searchable = format!(
                    "{} {} {} {} {}",
                    action.action_label,
                    action.resource_label,
                    action.action,
                    action.resource,
                    surface.app_id
                )
                .to_lowercase();
                let score = if needle.is_empty() {
                    0
                } else if action.action_label.to_lowercase().starts_with(&needle) {
                    100
                } else if searchable.contains(&needle) {
                    50
                } else {
                    continue;
                };
                let params = Self::action_params(action);
                let broker = self.broker.clone();
                let peer_id = surface.peer_id;
                let resource = action.resource.clone();
                let action_id = action.action.clone();
                let actor = actor.clone();
                let wants_content = matches!(
                    action.input,
                    PublishedInput::Text {
                        wants_content: true,
                        ..
                    }
                );
                let typed_arguments = !action.params.is_empty();
                let confirmation = action.contract.confirmation.clone();
                let surface_live = surface.live;
                let app_id = surface.app_id.clone();
                let cluster_id = surface.route.cluster_id.clone();
                let action_label = action.action_label.clone();
                let invoke_broker = broker.clone();
                let invoke_resource = resource.clone();
                let invoke_action_id = action_id.clone();
                let invoke_actor = actor.clone();
                let invoke_confirmation = confirmation.clone();
                let invoke_app_id = app_id.clone();
                let invoke_cluster_id = cluster_id.clone();
                let invoke_unavailable_reason = unavailable_reason.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Action,
                    provider_id: PROVIDER_ID.to_string(),
                    id: Self::result_id(&surface, action),
                    title: action.action_label.clone(),
                    subtitle: Some(unavailable_reason.clone().unwrap_or_else(|| {
                        action.description.clone().unwrap_or_else(|| {
                            if surface_live {
                                format!("{} · {}", action.resource_label, surface.app_id)
                            } else {
                                format!(
                                    "{} · {} · launches application",
                                    action.resource_label, surface.app_id
                                )
                            }
                        })
                    })),
                    score,
                    params,
                    contract: Some(action.contract.clone()),
                    on_open: std::rc::Rc::new(move |_doc, _root, chrome, args| {
                        // Availability on an archived launchable descriptor is
                        // only a last-known hint. Launch first and let the
                        // freshly published surface decide; otherwise an app
                        // closed while temporarily busy could never be opened
                        // through that action again.
                        if surface_live && let Some(reason) = &unavailable_reason {
                            return Err(ActionError::Refused(reason.clone()));
                        }
                        let encoded_arguments = typed_arguments.then(|| {
                            let mut application_args = args.to_json();
                            application_args.remove("confirm");
                            application_args.remove("confirmation_phrase");
                            format!(
                                "{}{}",
                                qos_actions::ARGUMENTS_JSON_PREFIX,
                                serde_json::Value::Object(application_args)
                            )
                        });
                        let argument = if let Some(arguments) = encoded_arguments.as_deref() {
                            Some(arguments)
                        } else if wants_content {
                            args.text("content")
                        } else {
                            args.text("argument")
                        };
                        let confirmed = args.boolean("confirm") == Some(true);
                        let phrase = args.text("confirmation_phrase").map(str::to_owned);
                        if !surface_live {
                            // Validate the human confirmation before starting
                            // another process; the broker binds a fresh one-use
                            // grant to the reconnected peer immediately before
                            // delivery.
                            match &confirmation {
                                qos_actions::ConfirmationPolicy::Confirm if !confirmed => {
                                    return Err(ActionError::Refused(
                                        "explicit confirmation is required".into(),
                                    ));
                                }
                                qos_actions::ConfirmationPolicy::Phrase { phrase: expected }
                                    if phrase.as_deref() != Some(expected.as_str()) =>
                                {
                                    return Err(ActionError::Refused(
                                        "the confirmation phrase did not match".into(),
                                    ));
                                }
                                _ => {}
                            }
                            crate::shell::search::providers::applications::launch_installed_app(
                                chrome, &app_id, &app_id,
                            )?;
                            let broker = broker.clone();
                            let resource = resource.clone();
                            let action_id = action_id.clone();
                            let argument = argument.map(str::to_owned);
                            let actor = actor.clone();
                            let confirmation = confirmation.clone();
                            let app_id = app_id.clone();
                            let cluster_id = cluster_id.clone();
                            let action_label = action_label.clone();
                            std::thread::Builder::new()
                                .name("qos-deferred-action".into())
                                .spawn(move || {
                                    let deadline =
                                        deadline_after(std::time::Duration::from_secs(30));
                                    while std::time::Instant::now() < deadline {
                                        let target =
                                            broker.surfaces().into_iter().find(|surface| {
                                                surface.live
                                                    && surface.app_id == app_id
                                                    && surface.route.cluster_id == cluster_id
                                                    && surface.actions.iter().any(|candidate| {
                                                        candidate.resource == resource
                                                            && candidate.action == action_id
                                                    })
                                            });
                                        if let Some(target) = target {
                                            match start_on_peer(
                                                &broker,
                                                &PeerInvocation {
                                                    peer_id: target.peer_id,
                                                    resource: &resource,
                                                    action: &action_id,
                                                    argument: argument.as_deref(),
                                                    actor: &actor,
                                                    confirmation: &confirmation,
                                                    confirmed,
                                                    phrase: phrase.as_deref(),
                                                },
                                            ) {
                                                Ok(job) => {
                                                    tracing::debug!(
                                                        invocation_id = job.id,
                                                        "deferred application action accepted"
                                                    );
                                                    drop(job.completion);
                                                }
                                                Err(error) => tracing::warn!(
                                                    %error,
                                                    action = %action_label,
                                                    "deferred application action was refused"
                                                ),
                                            }
                                            return;
                                        }
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                    tracing::warn!(
                                        action = %action_label,
                                        app = %app_id,
                                        "application did not publish its launchable action in time"
                                    );
                                })
                                .map_err(|error| ActionError::Refused(error.to_string()))?;
                            return Ok(());
                        }
                        let job = start_on_peer(
                            &broker,
                            &PeerInvocation {
                                peer_id,
                                resource: &resource,
                                action: &action_id,
                                argument,
                                actor: &actor,
                                confirmation: &confirmation,
                                confirmed,
                                phrase: phrase.as_deref(),
                            },
                        )
                        .map_err(ActionError::Refused)?;
                        tracing::debug!(invocation_id = job.id, "application action accepted");
                        // Completion remains owned by the broker's job registry;
                        // dropping this compatibility receiver does not erase status.
                        drop(job.completion);
                        // Admission is synchronous; execution is observed as a
                        // broker job rather than blocking the compositor.
                        Ok(())
                    }),
                    on_invoke: Some(std::rc::Rc::new(move |_doc, _root, chrome, args| {
                        if surface_live && let Some(reason) = &invoke_unavailable_reason {
                            return Err(ActionError::Refused(reason.clone()));
                        }
                        let (argument, confirmed, phrase) =
                            invocation_argument(args, typed_arguments, wants_content);
                        if !surface_live {
                            match &invoke_confirmation {
                                qos_actions::ConfirmationPolicy::Confirm if !confirmed => {
                                    return Err(ActionError::Refused(
                                        "explicit confirmation is required".into(),
                                    ));
                                }
                                qos_actions::ConfirmationPolicy::Phrase { phrase: expected }
                                    if phrase.as_deref() != Some(expected.as_str()) =>
                                {
                                    return Err(ActionError::Refused(
                                        "the confirmation phrase did not match".into(),
                                    ));
                                }
                                _ => {}
                            }
                            crate::shell::search::providers::applications::launch_installed_app(
                                chrome,
                                &invoke_app_id,
                                &invoke_app_id,
                            )?;
                            let deadline = deadline_after(std::time::Duration::from_secs(30));
                            while std::time::Instant::now() < deadline {
                                let target = invoke_broker.surfaces().into_iter().find(|surface| {
                                    surface.live
                                        && surface.app_id == invoke_app_id
                                        && surface.route.cluster_id == invoke_cluster_id
                                        && surface.actions.iter().any(|candidate| {
                                            candidate.resource == invoke_resource
                                                && candidate.action == invoke_action_id
                                        })
                                });
                                if let Some(target) = target {
                                    let job = start_on_peer(
                                        &invoke_broker,
                                        &PeerInvocation {
                                            peer_id: target.peer_id,
                                            resource: &invoke_resource,
                                            action: &invoke_action_id,
                                            argument: argument.as_deref(),
                                            actor: &invoke_actor,
                                            confirmation: &invoke_confirmation,
                                            confirmed,
                                            phrase: phrase.as_deref(),
                                        },
                                    )
                                    .map_err(ActionError::Refused)?;
                                    return Ok(receipt_for_job(&invoke_broker, job));
                                }
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            return Err(ActionError::Refused(
                                "application did not publish its launchable action in time".into(),
                            ));
                        }
                        let job = start_on_peer(
                            &invoke_broker,
                            &PeerInvocation {
                                peer_id,
                                resource: &invoke_resource,
                                action: &invoke_action_id,
                                argument: argument.as_deref(),
                                actor: &invoke_actor,
                                confirmation: &invoke_confirmation,
                                confirmed,
                                phrase: phrase.as_deref(),
                            },
                        )
                        .map_err(ActionError::Refused)?;
                        Ok(receipt_for_job(&invoke_broker, job))
                    })),
                });
            }
        }
        results
    }
}

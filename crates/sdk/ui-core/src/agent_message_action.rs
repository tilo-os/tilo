//! Bounded discovery and invocation of Chat's certified Agent Message action.
//!
//! Agent identity carried by presence is descriptive. This state machine only
//! invokes the unique, freshly resolved action published by Chat for that
//! semantic identity, through the caller already pinned into [`RemoteActions`].

use std::collections::{HashSet, VecDeque};

use tilo_actions::remote::{CataloguePage, CatalogueQuery, InvokeArgument, RemoteActionTarget};

use crate::LocalState;
use crate::remote_actions::{CatalogueCall, InvocationCall, RemoteActions, RemoteCall};
use crate::time::{Duration, Instant};

pub const RESOLVE_WINDOW: Duration = Duration::from_secs(5);
pub const MAX_PAGES: u16 = 64;
const ASYNC_RESPONSE_POLL: Duration = Duration::from_millis(50);

enum ProvidersFailure<'a> {
    Deadline,
    PageBudget,
    RemoteRefused(&'a tilo_actions::remote::ActionHostError),
    ChangedCall,
    InvalidPageFlags,
    TooManyProviders,
    ProviderRouteMismatch,
}

fn providers_failed(failure: ProvidersFailure<'_>) -> AgentMessagePoll {
    let (class, remote_class) = match failure {
        ProvidersFailure::Deadline => ("deadline_exhausted", None),
        ProvidersFailure::PageBudget => ("page_budget_exhausted", None),
        ProvidersFailure::RemoteRefused(error) => ("remote_refused", Some(error.code())),
        ProvidersFailure::ChangedCall => ("changed_call", None),
        ProvidersFailure::InvalidPageFlags => ("invalid_page_flags", None),
        ProvidersFailure::TooManyProviders => ("too_many_providers", None),
        ProvidersFailure::ProviderRouteMismatch => ("provider_route_mismatch", None),
    };
    observe_providers_poll("failed", Some(class), remote_class);
    AgentMessagePoll::Failed
}

fn observe_providers_poll(result: &str, class: Option<&str>, remote_class: Option<&str>) {
    observe_message_poll("providers", result, class, remote_class);
}

fn observe_message_poll(
    phase: &str,
    result: &str,
    class: Option<&str>,
    remote_class: Option<&str>,
) {
    #[cfg(test)]
    let row = serde_json::json!({
        "phase": phase, "result": result,
        "refusal_class": class, "remote_class": remote_class,
    });
    #[cfg(test)]
    PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| {
        if let Some(rows) = rows {
            rows.push(row.clone());
        }
    });
    tracing::debug!(
        target: "tilo_agent_delivery",
        phase,
        result,
        refusal_class = class.unwrap_or("none"),
        remote_class = remote_class.unwrap_or("none"),
        "native-message-discovery"
    );
}

fn discovery_failed(phase: &str, class: &str) -> AgentMessagePoll {
    observe_message_poll(phase, "failed", Some(class), None);
    AgentMessagePoll::Failed
}

fn remote_failed(phase: &str, error: &tilo_actions::remote::ActionHostError) -> AgentMessagePoll {
    observe_message_poll(phase, "failed", Some("remote_refused"), Some(error.code()));
    AgentMessagePoll::Failed
}

fn submission_phase(submission: &AgentMessageSubmission) -> &'static str {
    match submission {
        AgentMessageSubmission::Providers(_) => "providers",
        AgentMessageSubmission::LaunchRows(_) => "launch_rows",
        AgentMessageSubmission::LaunchConfirm { .. } => "launch_confirm",
        AgentMessageSubmission::LaunchInvoke { .. } => "launch_invoke",
        AgentMessageSubmission::AwaitPublisher { .. } => "await_publisher",
        AgentMessageSubmission::Rows(_) => "rows",
        AgentMessageSubmission::Confirm { .. } => "confirm",
        AgentMessageSubmission::Invoke { .. } => "invoke",
    }
}

#[cfg(test)]
thread_local! {
    static PROVIDERS_FAILURE_ROWS: std::cell::RefCell<Option<Vec<serde_json::Value>>> = const {
        std::cell::RefCell::new(None)
    };
    static INGEST_PAGE_COUNTS: std::cell::RefCell<Option<Vec<serde_json::Value>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[derive(Clone)]
pub enum AgentMessageSubmission {
    Providers(LocalState<CatalogueCall>),
    LaunchRows(Box<AgentLaunchRows>),
    LaunchConfirm {
        target: RemoteActionTarget,
        revision: String,
        call: LocalState<CatalogueCall>,
    },
    LaunchInvoke {
        invocation: tilo_actions::InvocationId,
        call: LocalState<InvocationCall>,
    },
    AwaitPublisher {
        call: Option<LocalState<CatalogueCall>>,
        retry_at: Instant,
        until: Instant,
    },
    Rows(Box<AgentMessageRows>),
    Confirm {
        target: RemoteActionTarget,
        revision: String,
        call: LocalState<CatalogueCall>,
    },
    Invoke {
        invocation: tilo_actions::InvocationId,
        call: LocalState<InvocationCall>,
    },
}

#[derive(Clone)]
pub struct AgentLaunchRows {
    revision: Option<String>,
    cursor: Vec<u8>,
    call: Option<LocalState<CatalogueCall>>,
    target: Option<(RemoteActionTarget, String)>,
    seen_cursors: HashSet<Vec<u8>>,
}

#[derive(Clone)]
pub struct AgentMessageRows {
    providers: VecDeque<String>,
    provider: Option<String>,
    parents: VecDeque<String>,
    parent: Option<String>,
    revision: Option<String>,
    cursor: Vec<u8>,
    call: Option<LocalState<CatalogueCall>>,
    matches: Vec<(RemoteActionTarget, String)>,
    seen_requests: HashSet<(String, String, Vec<u8>)>,
    seen_resources: HashSet<(String, String)>,
}

pub enum AgentMessagePoll {
    Waiting,
    Advance {
        submission: AgentMessageSubmission,
        pages_remaining: u16,
    },
    Succeeded,
    Failed,
}

#[derive(Clone)]
pub struct AgentMessageOffer {
    target: RemoteActionTarget,
    revision: String,
}

pub enum AgentMessageOfferPoll {
    Waiting,
    Advance {
        submission: AgentMessageSubmission,
        pages_remaining: u16,
    },
    Available(AgentMessageOffer),
    Failed,
}

#[must_use]
pub fn begin(actions: &RemoteActions, workspace: &str) -> AgentMessageSubmission {
    AgentMessageSubmission::Providers(actions.catalogue(CatalogueQuery {
        canonical_route: Some(canonical_chat_route(workspace)),
        max_results: 0,
        ..CatalogueQuery::default()
    }))
}

fn canonical_chat_route(workspace: &str) -> tilo_actions::remote::CanonicalActionRoute {
    tilo_actions::remote::CanonicalActionRoute {
        room: workspace.to_owned(),
        app_id: "so.tilo.chat".into(),
        record_id: String::new(),
    }
}

/// Discover and freshly confirm the exact Chat Message action without
/// invoking it. Cold discovery may launch signed Chat, but never submits the
/// user's draft. The returned offer is descriptive until re-confirmed by
/// [`begin_from_offer`] at submission time.
#[must_use]
pub fn poll_offer(
    actions: &RemoteActions,
    workspace: &str,
    agent: &str,
    submission: &AgentMessageSubmission,
    resolve_deadline: Instant,
    pages_remaining: u16,
) -> AgentMessageOfferPoll {
    if crate::time::now() >= resolve_deadline || pages_remaining == 0 {
        return AgentMessageOfferPoll::Failed;
    }
    if let AgentMessageSubmission::Confirm {
        target,
        revision,
        call,
    } = submission
    {
        let page = match call.get() {
            RemoteCall::Pending => return AgentMessageOfferPoll::Waiting,
            RemoteCall::Refused(_) => return AgentMessageOfferPoll::Failed,
            RemoteCall::Done(page) => page,
        };
        if !is_exact_confirmation(&page, agent, target, revision) {
            return AgentMessageOfferPoll::Failed;
        }
        return AgentMessageOfferPoll::Available(AgentMessageOffer {
            target: target.clone(),
            revision: revision.clone(),
        });
    }
    match poll(
        actions,
        workspace,
        agent,
        "",
        submission,
        resolve_deadline,
        pages_remaining,
    ) {
        AgentMessagePoll::Waiting => AgentMessageOfferPoll::Waiting,
        AgentMessagePoll::Advance {
            submission,
            pages_remaining,
        } => AgentMessageOfferPoll::Advance {
            submission,
            pages_remaining,
        },
        AgentMessagePoll::Succeeded | AgentMessagePoll::Failed => AgentMessageOfferPoll::Failed,
    }
}

/// Re-confirm a previously discovered exact offer before invoking it.
#[must_use]
pub fn begin_from_offer(
    actions: &RemoteActions,
    offer: &AgentMessageOffer,
) -> AgentMessageSubmission {
    AgentMessageSubmission::Confirm {
        target: offer.target.clone(),
        revision: offer.revision.clone(),
        call: actions.catalogue_page(CatalogueQuery::resolve(
            &offer.target,
            offer.revision.clone(),
        )),
    }
}

/// Whether an invocation in this submission was admitted but its host can no
/// longer confirm a terminal outcome. This must not be presented as failure or
/// retried under a new invocation identity.
#[must_use]
pub fn outcome_uncertain(actions: &RemoteActions, submission: &AgentMessageSubmission) -> bool {
    let (AgentMessageSubmission::Invoke { invocation, call }
    | AgentMessageSubmission::LaunchInvoke { invocation, call }) = submission
    else {
        return false;
    };
    !call.get().is_settled() && actions.invocation_outcome_uncertain(*invocation)
}

/// Earliest pump needed by a native owner that has no callback wake scope.
/// Catalogue replies are sampled only through the bounded discovery window;
/// invocation replies use the transport's real receipt/Observe schedule.
#[must_use]
pub fn next_wake_deadline(
    actions: &RemoteActions,
    submission: &AgentMessageSubmission,
    resolve_deadline: Instant,
) -> Option<Instant> {
    let now = crate::time::now();
    let catalogue = |call: &LocalState<CatalogueCall>| {
        if call.get().is_pending() {
            Some((now + ASYNC_RESPONSE_POLL).min(resolve_deadline))
        } else {
            Some(now)
        }
    };
    match submission {
        AgentMessageSubmission::Providers(call)
        | AgentMessageSubmission::LaunchConfirm { call, .. }
        | AgentMessageSubmission::Confirm { call, .. } => catalogue(call),
        AgentMessageSubmission::LaunchRows(state) => {
            state.call.as_ref().map_or(Some(now), catalogue)
        }
        AgentMessageSubmission::LaunchInvoke { .. } | AgentMessageSubmission::Invoke { .. } => {
            actions.next_observation_deadline()
        }
        AgentMessageSubmission::AwaitPublisher {
            call,
            retry_at,
            until,
        } => call.as_ref().map_or_else(
            || Some((*retry_at).min(*until)),
            |call| catalogue(call).map(|deadline| deadline.min(*until)),
        ),
        AgentMessageSubmission::Rows(state) => state.call.as_ref().map_or(Some(now), catalogue),
    }
}

#[must_use]
pub fn poll(
    actions: &RemoteActions,
    workspace: &str,
    agent: &str,
    text: &str,
    submission: &AgentMessageSubmission,
    resolve_deadline: Instant,
    pages_remaining: u16,
) -> AgentMessagePoll {
    if !matches!(
        submission,
        AgentMessageSubmission::LaunchInvoke { .. }
            | AgentMessageSubmission::AwaitPublisher { .. }
            | AgentMessageSubmission::Invoke { .. }
    ) && (crate::time::now() >= resolve_deadline || pages_remaining == 0)
    {
        if matches!(submission, AgentMessageSubmission::Providers(_)) {
            return providers_failed(if crate::time::now() >= resolve_deadline {
                ProvidersFailure::Deadline
            } else {
                ProvidersFailure::PageBudget
            });
        }
        return discovery_failed(
            submission_phase(submission),
            if crate::time::now() >= resolve_deadline {
                "deadline_exhausted"
            } else {
                "page_budget_exhausted"
            },
        );
    }
    let result = match submission {
        AgentMessageSubmission::Providers(call) => poll_providers(workspace, call, pages_remaining),
        AgentMessageSubmission::LaunchRows(state) => {
            poll_launch_rows(actions, workspace, state, pages_remaining)
        }
        AgentMessageSubmission::LaunchConfirm {
            target,
            revision,
            call,
        } => poll_launch_confirmation(actions, workspace, target, revision, call, pages_remaining),
        AgentMessageSubmission::LaunchInvoke { call, .. } => {
            poll_launch_invocation(actions, call, pages_remaining)
        }
        AgentMessageSubmission::AwaitPublisher {
            call,
            retry_at,
            until,
        } => poll_await_publisher(
            actions,
            workspace,
            call.as_ref(),
            *retry_at,
            *until,
            pages_remaining,
        ),
        AgentMessageSubmission::Rows(state) => poll_rows(actions, agent, state, pages_remaining),
        AgentMessageSubmission::Confirm {
            target,
            revision,
            call,
        } => poll_confirmation(
            actions,
            agent,
            text,
            target,
            revision,
            call,
            pages_remaining,
        ),
        AgentMessageSubmission::Invoke { call, .. } => poll_invocation(call),
    };
    observe_submission_result(submission, &result);
    result
}

fn observe_submission_result(submission: &AgentMessageSubmission, result: &AgentMessagePoll) {
    if matches!(submission, AgentMessageSubmission::Providers(_)) {
        return;
    }
    let class = match result {
        AgentMessagePoll::Waiting => "waiting",
        AgentMessagePoll::Advance { .. } => "advance",
        AgentMessagePoll::Succeeded => "succeeded",
        AgentMessagePoll::Failed => return,
    };
    observe_message_poll(submission_phase(submission), class, None, None);
}

fn poll_providers(
    workspace: &str,
    call: &LocalState<CatalogueCall>,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let RemoteCall::Done(page) = call.get() else {
        let current = call.get();
        return if current.is_pending() {
            observe_providers_poll("waiting", None, None);
            AgentMessagePoll::Waiting
        } else {
            providers_failed(match &current {
                RemoteCall::Refused(error) => ProvidersFailure::RemoteRefused(error),
                _ => ProvidersFailure::ChangedCall,
            })
        };
    };
    if page.stale_cursor || page.budget_exhausted || !page.exhausted || !page.next_cursor.is_empty()
    {
        return providers_failed(ProvidersFailure::InvalidPageFlags);
    }
    if page.providers.len() > 1 {
        return providers_failed(ProvidersFailure::TooManyProviders);
    }
    if page.providers.iter().any(|provider| {
        provider.app_id != "so.tilo.chat"
            || provider.room != workspace
            || !provider.record_id.is_empty()
    }) {
        return providers_failed(ProvidersFailure::ProviderRouteMismatch);
    }
    let mut providers = page
        .providers
        .into_iter()
        .filter(|provider| provider.app_id == "so.tilo.chat" && provider.room == workspace)
        .map(|provider| provider.provider)
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();
    observe_providers_poll("advance", None, None);
    if providers.is_empty() {
        return AgentMessagePoll::Advance {
            submission: AgentMessageSubmission::LaunchRows(Box::new(AgentLaunchRows {
                revision: None,
                cursor: Vec::new(),
                call: None,
                target: None,
                seen_cursors: HashSet::new(),
            })),
            pages_remaining: pages_remaining.saturating_sub(1),
        };
    }
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::Rows(Box::new(AgentMessageRows {
            providers: providers.into(),
            provider: None,
            parents: VecDeque::new(),
            parent: None,
            revision: None,
            cursor: Vec::new(),
            call: None,
            matches: Vec::new(),
            seen_requests: HashSet::new(),
            seen_resources: HashSet::new(),
        })),
        pages_remaining: pages_remaining.saturating_sub(1),
    }
}

fn poll_launch_rows(
    actions: &RemoteActions,
    workspace: &str,
    state: &AgentLaunchRows,
    mut pages_remaining: u16,
) -> AgentMessagePoll {
    let mut revision = state.revision.clone();
    let mut cursor = state.cursor.clone();
    let mut target = state.target.clone();
    let mut seen_cursors = state.seen_cursors.clone();
    if let Some(call) = &state.call {
        let page = match call.get() {
            RemoteCall::Pending => return AgentMessagePoll::Waiting,
            RemoteCall::Refused(error) => return remote_failed("launch_rows", &error),
            RemoteCall::Done(page) => page,
        };
        pages_remaining = pages_remaining.saturating_sub(1);
        if page.stale_cursor
            || page.budget_exhausted
            || revision
                .as_ref()
                .is_some_and(|expected| expected != &page.revision)
        {
            return discovery_failed("launch_rows", "invalid_page_or_revision");
        }
        for row in &page.rows {
            if is_chat_application_open_row(row, workspace) {
                let Some(found) = row.target() else {
                    return discovery_failed("launch_rows", "missing_target");
                };
                if target.is_some() {
                    return discovery_failed("launch_rows", "ambiguous_target");
                }
                target = Some((found, page.revision.clone()));
            }
        }
        if page.next_cursor.is_empty() {
            let Some((target, revision)) = target else {
                return discovery_failed("launch_rows", "no_match");
            };
            return AgentMessagePoll::Advance {
                submission: AgentMessageSubmission::LaunchConfirm {
                    call: actions.catalogue(CatalogueQuery::resolve(&target, revision.clone())),
                    target,
                    revision,
                },
                pages_remaining,
            };
        }
        revision = Some(page.revision);
        cursor = page.next_cursor;
    }
    if !seen_cursors.insert(cursor.clone()) {
        return discovery_failed("launch_rows", "repeated_cursor");
    }
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::LaunchRows(Box::new(AgentLaunchRows {
            call: Some(actions.catalogue_page(CatalogueQuery {
                revision: revision.clone(),
                cursor: cursor.clone(),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            })),
            revision,
            cursor,
            target,
            seen_cursors,
        })),
        pages_remaining,
    }
}

fn poll_launch_confirmation(
    actions: &RemoteActions,
    workspace: &str,
    target: &RemoteActionTarget,
    revision: &str,
    call: &LocalState<CatalogueCall>,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let page = match call.get() {
        RemoteCall::Pending => return AgentMessagePoll::Waiting,
        RemoteCall::Refused(error) => return remote_failed("launch_confirm", &error),
        RemoteCall::Done(page) => page,
    };
    let resolved = (page.rows.len() == 1).then(|| page.rows.first()).flatten();
    if page.stale_cursor
        || page.budget_exhausted
        || page.revision != revision
        || !resolved.is_some_and(|row| {
            is_chat_application_open_row(row, workspace) && row.target().as_ref() == Some(target)
        })
    {
        return discovery_failed("launch_confirm", "changed_confirmation");
    }
    let (invocation, call) = actions.invoke_tracked(target.clone(), InvokeArgument::None, None);
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::LaunchInvoke { invocation, call },
        pages_remaining: pages_remaining.saturating_sub(1),
    }
}

fn poll_launch_invocation(
    actions: &RemoteActions,
    call: &LocalState<InvocationCall>,
    pages_remaining: u16,
) -> AgentMessagePoll {
    match call.get() {
        RemoteCall::Pending
        | RemoteCall::Done(
            tilo_actions::InvocationUpdate::Accepted { .. }
            | tilo_actions::InvocationUpdate::Progress { .. }
            | tilo_actions::InvocationUpdate::Item { .. },
        ) => AgentMessagePoll::Waiting,
        RemoteCall::Refused(error) => remote_failed("launch_invoke", &error),
        RemoteCall::Done(tilo_actions::InvocationUpdate::Failed { .. }) => {
            discovery_failed("launch_invoke", "invocation_failed")
        }
        RemoteCall::Done(tilo_actions::InvocationUpdate::Cancelled { .. }) => {
            discovery_failed("launch_invoke", "invocation_cancelled")
        }
        RemoteCall::Done(tilo_actions::InvocationUpdate::Succeeded { .. }) => {
            let now = crate::time::now();
            AgentMessagePoll::Advance {
                submission: AgentMessageSubmission::AwaitPublisher {
                    call: Some(actions.catalogue(CatalogueQuery {
                        max_results: 0,
                        ..CatalogueQuery::default()
                    })),
                    retry_at: now,
                    until: now.checked_add(RESOLVE_WINDOW).unwrap_or(now),
                },
                pages_remaining,
            }
        }
    }
}

fn poll_await_publisher(
    actions: &RemoteActions,
    workspace: &str,
    call: Option<&LocalState<CatalogueCall>>,
    retry_at: Instant,
    until: Instant,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let now = crate::time::now();
    if now >= until || pages_remaining == 0 {
        return discovery_failed(
            "await_publisher",
            if now >= until {
                "deadline_exhausted"
            } else {
                "page_budget_exhausted"
            },
        );
    }
    let Some(call) = call else {
        if now < retry_at {
            return AgentMessagePoll::Waiting;
        }
        return AgentMessagePoll::Advance {
            submission: AgentMessageSubmission::AwaitPublisher {
                call: Some(actions.catalogue(CatalogueQuery {
                    canonical_route: Some(canonical_chat_route(workspace)),
                    max_results: 0,
                    ..CatalogueQuery::default()
                })),
                retry_at,
                until,
            },
            pages_remaining,
        };
    };
    let page = match call.get() {
        RemoteCall::Pending => return AgentMessagePoll::Waiting,
        RemoteCall::Refused(error) => return remote_failed("await_publisher", &error),
        RemoteCall::Done(page) => page,
    };
    if page.stale_cursor || page.budget_exhausted || !page.exhausted || !page.next_cursor.is_empty()
    {
        return discovery_failed("await_publisher", "invalid_page_flags");
    }
    if page.providers.len() > 1
        || page.providers.iter().any(|provider| {
            provider.app_id != "so.tilo.chat"
                || provider.room != workspace
                || !provider.record_id.is_empty()
        })
    {
        return discovery_failed("await_publisher", "provider_count_or_route_mismatch");
    }
    let mut providers = page
        .providers
        .into_iter()
        .filter(|provider| provider.app_id == "so.tilo.chat" && provider.room == workspace)
        .map(|provider| provider.provider)
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();
    let pages_remaining = pages_remaining.saturating_sub(1);
    if !providers.is_empty() {
        return AgentMessagePoll::Advance {
            submission: AgentMessageSubmission::Rows(Box::new(AgentMessageRows {
                providers: providers.into(),
                provider: None,
                parents: VecDeque::new(),
                parent: None,
                revision: None,
                cursor: Vec::new(),
                call: None,
                matches: Vec::new(),
                seen_requests: HashSet::new(),
                seen_resources: HashSet::new(),
            })),
            pages_remaining,
        };
    }
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::AwaitPublisher {
            call: None,
            retry_at: now.checked_add(Duration::from_millis(200)).unwrap_or(now),
            until,
        },
        pages_remaining,
    }
}

fn is_chat_application_open_row(row: &tilo_actions::remote::CatalogueRow, room: &str) -> bool {
    let Some(action) = row.action.as_ref() else {
        return false;
    };
    row.provider == tilo_actions::remote::workspace_provider(room)
        && row.app_id == "so.tilo.chat"
        && row.resource.kind == "applications.app"
        && action.kind == tilo_actions::OperationKind::Effect
        && action.address.resource == row.resource.id
        && action.address.action == tilo_actions::remote::APPLICATION_OPEN_ACTION
}

fn poll_rows(
    actions: &RemoteActions,
    agent: &str,
    state: &AgentMessageRows,
    mut pages_remaining: u16,
) -> AgentMessagePoll {
    let mut state = state.clone();
    if let Some(answer) = state.call.as_ref() {
        let page = match answer.get() {
            RemoteCall::Pending => return AgentMessagePoll::Waiting,
            RemoteCall::Refused(error) => return remote_failed("rows", &error),
            RemoteCall::Done(page) => page,
        };
        pages_remaining = pages_remaining.saturating_sub(1);
        if page.stale_cursor {
            observe_ingest_page(&page, agent);
            return restart_stale_rows(&state, &page, pages_remaining);
        }
        if let Err(class) = ingest_page(&mut state, page, agent) {
            return discovery_failed("rows", class);
        }
    }
    if state.provider.is_some() && state.parent.is_none() {
        state.parent = state.parents.pop_front();
        state.cursor.clear();
        if state.parent.is_none() {
            state.provider = None;
            state.revision = None;
        }
    }
    if state.provider.is_none() {
        state.provider = state.providers.pop_front();
        state.revision = None;
        state.cursor.clear();
        state.parents = VecDeque::from([String::new()]);
        state.parent = state.parents.pop_front();
    }
    let Some(provider) = state.provider.clone() else {
        return confirm_unique(actions, state, pages_remaining);
    };
    let Some(parent) = state.parent.clone() else {
        return discovery_failed("rows", "missing_parent");
    };
    if !state
        .seen_requests
        .insert((provider.clone(), parent.clone(), state.cursor.clone()))
    {
        return discovery_failed("rows", "repeated_cursor");
    }
    state.call = Some(actions.catalogue_page(CatalogueQuery {
        provider: Some(provider),
        parent_resource: (!parent.is_empty()).then_some(parent),
        revision: state.revision.clone(),
        cursor: state.cursor.clone(),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    }));
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::Rows(Box::new(state)),
        pages_remaining,
    }
}

fn restart_stale_rows(
    state: &AgentMessageRows,
    page: &CataloguePage,
    pages_remaining: u16,
) -> AgentMessagePoll {
    if page.revision.is_empty()
        || !page.rows.is_empty()
        || !page.providers.is_empty()
        || page.locator.is_some()
        || !page.next_cursor.is_empty()
        || page.exhausted
        || page.budget_exhausted
    {
        return discovery_failed("rows", "malformed_stale_page");
    }
    let Some(provider) = state
        .provider
        .as_ref()
        .filter(|provider| !provider.is_empty())
    else {
        return discovery_failed("rows", "stale_cursor");
    };
    if state.parent.is_none()
        || !state.providers.is_empty()
        || !state
            .revision
            .as_ref()
            .is_some_and(|revision| !revision.is_empty())
    {
        return discovery_failed("rows", "stale_cursor");
    }
    if pages_remaining == 0 {
        return discovery_failed("rows", "page_budget_exhausted");
    }
    tracing::debug!(target: "tilo_agent_delivery", phase = "rows", "native-message-stale-restart");
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::Rows(Box::new(AgentMessageRows {
            providers: VecDeque::new(),
            provider: Some(provider.clone()),
            parents: VecDeque::new(),
            parent: Some(String::new()),
            revision: None,
            cursor: Vec::new(),
            call: None,
            matches: Vec::new(),
            seen_requests: HashSet::new(),
            seen_resources: HashSet::new(),
        })),
        pages_remaining,
    }
}

fn confirm_unique(
    actions: &RemoteActions,
    mut state: AgentMessageRows,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let Some((target, revision)) = state.matches.pop() else {
        return discovery_failed("rows", "no_match");
    };
    let call = actions.catalogue_page(CatalogueQuery::resolve(&target, revision.clone()));
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::Confirm {
            target,
            revision,
            call,
        },
        pages_remaining,
    }
}

fn poll_confirmation(
    actions: &RemoteActions,
    agent: &str,
    text: &str,
    target: &RemoteActionTarget,
    revision: &str,
    call: &LocalState<CatalogueCall>,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let page = match call.get() {
        RemoteCall::Pending => return AgentMessagePoll::Waiting,
        RemoteCall::Refused(error) => return remote_failed("confirm", &error),
        RemoteCall::Done(page) => page,
    };
    if !is_exact_confirmation(&page, agent, target, revision) {
        return discovery_failed(
            "confirm",
            confirmation_failure(&page, agent, target, revision),
        );
    }
    let (invocation, call) =
        actions.invoke_tracked(target.clone(), InvokeArgument::Text(text.to_owned()), None);
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::Invoke { invocation, call },
        pages_remaining: pages_remaining.saturating_sub(1),
    }
}

fn poll_invocation(call: &LocalState<InvocationCall>) -> AgentMessagePoll {
    match call.get() {
        RemoteCall::Refused(error) => remote_failed("invoke", &error),
        RemoteCall::Done(update) if update.is_terminal() => {
            if matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }) {
                AgentMessagePoll::Succeeded
            } else {
                discovery_failed(
                    "invoke",
                    if matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. }) {
                        "invocation_cancelled"
                    } else {
                        "invocation_failed"
                    },
                )
            }
        }
        RemoteCall::Pending | RemoteCall::Done(_) => AgentMessagePoll::Waiting,
    }
}

fn unique_target(page: &CataloguePage, agent: &str) -> Option<RemoteActionTarget> {
    let mut matching = page.rows.iter().filter(|row| is_message_row(row, agent));
    let target = matching.next()?.target()?;
    matching.next().is_none().then_some(target)
}

fn is_exact_confirmation(
    page: &CataloguePage,
    agent: &str,
    target: &RemoteActionTarget,
    revision: &str,
) -> bool {
    !revision.is_empty()
        && page.revision == revision
        && page.exhausted
        && page.next_cursor.is_empty()
        && !page.stale_cursor
        && !page.budget_exhausted
        && page.rows.len() == 1
        && unique_target(page, agent).as_ref() == Some(target)
}

fn confirmation_failure(
    page: &CataloguePage,
    agent: &str,
    target: &RemoteActionTarget,
    revision: &str,
) -> &'static str {
    if revision.is_empty() {
        "empty_revision"
    } else if page.revision != revision {
        "revision_changed"
    } else if !page.exhausted {
        "inconsistent_completion"
    } else if !page.next_cursor.is_empty() {
        "cursor_not_empty"
    } else if page.stale_cursor {
        "stale_cursor"
    } else if page.budget_exhausted {
        "page_budget_exhausted"
    } else if page.rows.len() != 1 {
        "row_count_mismatch"
    } else if unique_target(page, agent).as_ref() != Some(target) {
        "changed_target"
    } else {
        "changed_confirmation"
    }
}

fn is_message_row(row: &tilo_actions::remote::CatalogueRow, agent: &str) -> bool {
    let key = format!("chat.person:{agent}");
    row.app_id == "so.tilo.chat"
        && row.resource.kind == "chat.agent"
        && row.action.as_ref().is_some_and(|action| {
            // Runtime resource addresses can be opaque. The semantic key
            // identifies the Agent; the row and descriptor must agree on
            // the separate concrete resource that will actually be invoked.
            action.address.resource == row.resource.id
                && action.address.action == "message"
                && action.resource_kind == "chat.agent"
                && action.kind == tilo_actions::OperationKind::Document
                && action
                    .semantic_identity
                    .as_ref()
                    .is_some_and(|identity| identity.kind == "chat.agent" && identity.key == key)
        })
}

fn observe_ingest_page(page: &CataloguePage, agent: &str) {
    let limit = usize::try_from(tilo_actions::remote::MAX_CATALOGUE_PAGE).unwrap_or(256);
    let row_count = page.rows.len().min(limit);
    let (mut child_count, mut action_count, mut message_match_count) = (0usize, 0usize, 0usize);
    for row in page.rows.iter().take(limit) {
        child_count = child_count.saturating_add(usize::from(row.has_children));
        action_count = action_count.saturating_add(usize::from(row.action.is_some()));
        message_match_count =
            message_match_count.saturating_add(usize::from(is_message_row(row, agent)));
    }
    #[cfg(test)]
    INGEST_PAGE_COUNTS.with_borrow_mut(|rows| {
        if let Some(rows) = rows {
            rows.push(serde_json::json!({
                "phase": "rows", "row_count": row_count, "child_count": child_count,
                "action_count": action_count, "message_match_count": message_match_count,
            }));
        }
    });
    tracing::debug!(
        target: "tilo_agent_delivery",
        phase = "rows", row_count, child_count, action_count, message_match_count,
        "native-message-page"
    );
}

fn ingest_page(
    state: &mut AgentMessageRows,
    page: CataloguePage,
    agent: &str,
) -> Result<(), &'static str> {
    observe_ingest_page(&page, agent);
    let Some(provider) = state.provider.as_deref() else {
        return Err("missing_provider");
    };
    let Some(parent) = state.parent.as_deref() else {
        return Err("missing_parent");
    };
    if page.revision.is_empty() {
        return Err("empty_revision");
    }
    if page.stale_cursor {
        return Err("stale_cursor");
    }
    if page.budget_exhausted {
        return Err("page_budget_exhausted");
    }
    if page.next_cursor.is_empty() != page.exhausted {
        return Err("inconsistent_completion");
    }
    if state
        .revision
        .as_ref()
        .is_some_and(|expected| expected != &page.revision)
    {
        return Err("revision_changed");
    }
    if page
        .rows
        .iter()
        .any(|row| row.provider != provider || row.parent_resource != parent)
    {
        return Err("row_route_mismatch");
    }
    state.revision.get_or_insert_with(|| page.revision.clone());
    for row in &page.rows {
        if row.has_children
            && state
                .seen_resources
                .insert((row.provider.clone(), row.resource.id.clone()))
        {
            state.parents.push_back(row.resource.id.clone());
        }
        if is_message_row(row, agent) {
            let target = row.target().ok_or("missing_target")?;
            state.matches.push((target, page.revision.clone()));
            if state.matches.len() > 1 {
                return Err("ambiguous_target");
            }
        }
    }
    if page.next_cursor.is_empty() {
        state.parent = None;
        state.cursor.clear();
    } else {
        state.cursor = page.next_cursor;
    }
    state.call = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CatalogueResource, CatalogueRow};
    use tilo_actions::{
        ActionAddress, ActionContract, ActionDescriptor, ActionOutput, ExecutionLocus, InputSpec,
        InvocationId, InvocationUpdate, OperationKind, ResourceSemanticIdentity,
    };

    fn complete_providers_page() -> CataloguePage {
        let provider = tilo_actions::remote::CatalogueProvider {
            provider: "PRIVATE-PROVIDER".into(),
            app_id: "so.tilo.chat".into(),
            room: "room-a".into(),
            record_id: String::new(),
            browsable: true,
        };
        CataloguePage {
            exhausted: true,
            providers: vec![provider],
            ..CataloguePage::default()
        }
    }

    fn assert_providers_failure(
        call: CatalogueCall,
        expired: bool,
        pages: u16,
        expected: &str,
        remote: Option<&str>,
    ) {
        PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| *rows = Some(Vec::new()));
        let now = crate::time::now();
        let deadline = if expired { now } else { now + RESOLVE_WINDOW };
        assert!(matches!(
            poll(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                "PRIVATE-TEXT",
                &AgentMessageSubmission::Providers(LocalState::new(call)),
                deadline,
                pages
            ),
            AgentMessagePoll::Failed
        ));
        let rows = PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| rows.take().unwrap());
        assert_eq!(
            rows,
            vec![serde_json::json!({
                "phase": "providers", "result": "failed",
                "refusal_class": expected, "remote_class": remote,
            })]
        );
        assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE"));
    }

    #[test]
    fn providers_poll_reports_closed_budgets_and_remote_refusals() {
        use tilo_actions::remote::ActionHostError;
        let cases = [
            (
                RemoteCall::Pending,
                true,
                MAX_PAGES,
                "deadline_exhausted",
                None,
            ),
            (RemoteCall::Pending, false, 0, "page_budget_exhausted", None),
            (
                RemoteCall::Refused(ActionHostError::NotPermitted),
                false,
                MAX_PAGES,
                "remote_refused",
                Some("not_permitted"),
            ),
            (
                RemoteCall::Refused(ActionHostError::Unavailable {
                    reason: "PRIVATE-ERROR".into(),
                }),
                false,
                MAX_PAGES,
                "remote_refused",
                Some("remote_actions_unavailable"),
            ),
        ];
        for (call, expired, pages, expected, remote) in cases {
            assert_providers_failure(call, expired, pages, expected, remote);
        }
    }

    #[test]
    fn providers_poll_reports_invalid_pages_and_exact_route_failures() {
        use tilo_actions::remote::CatalogueProvider;
        let complete = complete_providers_page();
        let provider = complete.providers[0].clone();
        let mut cases = Vec::new();
        for page in [
            CataloguePage {
                stale_cursor: true,
                ..complete.clone()
            },
            CataloguePage {
                budget_exhausted: true,
                ..complete.clone()
            },
            CataloguePage {
                exhausted: false,
                ..complete.clone()
            },
            CataloguePage {
                next_cursor: b"PRIVATE-CURSOR".to_vec(),
                ..complete.clone()
            },
        ] {
            cases.push((
                RemoteCall::Done(page),
                false,
                MAX_PAGES,
                "invalid_page_flags",
                None,
            ));
        }
        cases.push((
            RemoteCall::Done(CataloguePage {
                providers: vec![provider.clone(), provider.clone()],
                ..complete.clone()
            }),
            false,
            MAX_PAGES,
            "too_many_providers",
            None,
        ));
        for changed in [
            CatalogueProvider {
                app_id: "PRIVATE-APP".into(),
                ..provider.clone()
            },
            CatalogueProvider {
                room: "PRIVATE-ROOM".into(),
                ..provider.clone()
            },
            CatalogueProvider {
                record_id: "PRIVATE-RECORD".into(),
                ..provider.clone()
            },
        ] {
            cases.push((
                RemoteCall::Done(CataloguePage {
                    providers: vec![changed],
                    ..complete.clone()
                }),
                false,
                MAX_PAGES,
                "provider_route_mismatch",
                None,
            ));
        }
        for (call, expired, pages, expected, remote) in cases {
            assert_providers_failure(call, expired, pages, expected, remote);
        }
    }

    #[test]
    fn providers_poll_observes_waiting_and_advance_without_private_data() {
        for (call, expected) in [
            (RemoteCall::Pending, "waiting"),
            (RemoteCall::Done(complete_providers_page()), "advance"),
            (
                RemoteCall::Done(CataloguePage {
                    exhausted: true,
                    ..CataloguePage::default()
                }),
                "advance",
            ),
        ] {
            PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| *rows = Some(Vec::new()));
            let result = poll(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                "PRIVATE-TEXT",
                &AgentMessageSubmission::Providers(LocalState::new(call)),
                crate::time::now() + RESOLVE_WINDOW,
                MAX_PAGES,
            );
            if expected == "waiting" {
                assert!(matches!(result, AgentMessagePoll::Waiting));
            } else {
                assert!(matches!(result, AgentMessagePoll::Advance { .. }));
            }
            let rows = PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| rows.take().unwrap());
            assert_eq!(
                rows,
                vec![serde_json::json!({
                    "phase": "providers", "result": expected,
                    "refusal_class": null, "remote_class": null,
                })]
            );
            assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE"));
        }
    }

    fn diagnostic_rows(call: CatalogueCall) -> AgentMessageRows {
        AgentMessageRows {
            providers: VecDeque::new(),
            provider: Some("chat-provider".into()),
            parents: VecDeque::new(),
            parent: Some(String::new()),
            revision: None,
            cursor: Vec::new(),
            call: Some(LocalState::new(call)),
            matches: Vec::new(),
            seen_requests: HashSet::new(),
            seen_resources: HashSet::new(),
        }
    }

    fn diagnostic_catalogue_submission(phase: &str, call: CatalogueCall) -> AgentMessageSubmission {
        let call = LocalState::new(call);
        let target = message_row("scribe").target().unwrap();
        match phase {
            "launch_rows" => AgentMessageSubmission::LaunchRows(Box::new(AgentLaunchRows {
                revision: None,
                cursor: Vec::new(),
                call: Some(call),
                target: None,
                seen_cursors: HashSet::new(),
            })),
            "launch_confirm" => AgentMessageSubmission::LaunchConfirm {
                target,
                revision: "PRIVATE-REVISION".into(),
                call,
            },
            "await_publisher" => AgentMessageSubmission::AwaitPublisher {
                call: Some(call),
                retry_at: crate::time::now(),
                until: crate::time::now() + RESOLVE_WINDOW,
            },
            "rows" => AgentMessageSubmission::Rows(Box::new(diagnostic_rows(call.get()))),
            "confirm" => AgentMessageSubmission::Confirm {
                target,
                revision: "PRIVATE-REVISION".into(),
                call,
            },
            _ => panic!("unknown test phase"),
        }
    }

    fn assert_message_diagnostic(
        submission: AgentMessageSubmission,
        expected_result: &str,
        cause: Option<&str>,
        remote: Option<&str>,
    ) {
        PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| *rows = Some(Vec::new()));
        let result = poll(
            &RemoteActions::session(),
            "room-a",
            "scribe",
            "PRIVATE-TEXT",
            &submission,
            crate::time::now() + RESOLVE_WINDOW,
            MAX_PAGES,
        );
        assert!(match expected_result {
            "failed" => matches!(result, AgentMessagePoll::Failed),
            "waiting" => matches!(result, AgentMessagePoll::Waiting),
            "advance" => matches!(result, AgentMessagePoll::Advance { .. }),
            "succeeded" => matches!(result, AgentMessagePoll::Succeeded),
            _ => false,
        });
        let rows = PROVIDERS_FAILURE_ROWS.with_borrow_mut(|rows| rows.take().unwrap());
        assert_eq!(
            rows,
            vec![serde_json::json!({
                "phase": submission_phase(&submission), "result": expected_result,
                "refusal_class": cause, "remote_class": remote,
            })]
        );
        assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE"));
    }

    #[test]
    fn all_catalogue_phases_observe_actual_pending_and_private_remote_refusal() {
        for phase in [
            "launch_rows",
            "launch_confirm",
            "await_publisher",
            "rows",
            "confirm",
        ] {
            assert_message_diagnostic(
                diagnostic_catalogue_submission(phase, RemoteCall::Pending),
                "waiting",
                None,
                None,
            );
            assert_message_diagnostic(
                diagnostic_catalogue_submission(
                    phase,
                    RemoteCall::Refused(tilo_actions::remote::ActionHostError::Refused {
                        reason: "PRIVATE-ERROR".into(),
                    }),
                ),
                "failed",
                Some("remote_refused"),
                Some("refused"),
            );
        }
    }

    #[test]
    fn actual_rows_poll_emits_only_bounded_page_counts() {
        let mut matching = confirmed_page("scribe", "PRIVATE-REVISION");
        matching.rows[0].resource.label = "PRIVATE-LABEL".into();
        let mut container = matching.clone();
        container.rows[0].action = None;
        container.rows[0].has_children = true;
        let oversized = CataloguePage {
            rows: vec![matching.rows[0].clone(); 257],
            ..matching.clone()
        };
        for (page, expected, advances) in [
            (
                CataloguePage {
                    revision: "PRIVATE-REVISION".into(),
                    exhausted: true,
                    ..CataloguePage::default()
                },
                (0, 0, 0, 0),
                false,
            ),
            (matching, (1, 0, 1, 1), true),
            (container, (1, 1, 0, 0), true),
            (oversized, (256, 0, 256, 256), false),
        ] {
            INGEST_PAGE_COUNTS.with_borrow_mut(|rows| *rows = Some(Vec::new()));
            let result = poll(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                "PRIVATE-TEXT",
                &diagnostic_catalogue_submission("rows", RemoteCall::Done(page)),
                crate::time::now() + RESOLVE_WINDOW,
                MAX_PAGES,
            );
            assert!(if advances {
                matches!(result, AgentMessagePoll::Advance { .. })
            } else {
                matches!(result, AgentMessagePoll::Failed)
            });
            let rows = INGEST_PAGE_COUNTS.with_borrow_mut(|rows| rows.take().unwrap());
            let (row_count, child_count, action_count, message_match_count) = expected;
            assert_eq!(
                rows,
                vec![serde_json::json!({
                    "phase": "rows", "row_count": row_count, "child_count": child_count,
                    "action_count": action_count, "message_match_count": message_match_count,
                })]
            );
            assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE"));
        }
    }

    #[test]
    fn catalogue_leaf_failures_are_distinguished_without_page_payloads() {
        for (phase, page, cause) in [
            (
                "launch_rows",
                CataloguePage {
                    exhausted: true,
                    ..CataloguePage::default()
                },
                "no_match",
            ),
            (
                "launch_rows",
                CataloguePage {
                    stale_cursor: true,
                    ..CataloguePage::default()
                },
                "invalid_page_or_revision",
            ),
            (
                "launch_confirm",
                CataloguePage::default(),
                "changed_confirmation",
            ),
            (
                "confirm",
                confirmed_page("PRIVATE-AGENT", "PRIVATE-REVISION"),
                "changed_target",
            ),
            (
                "await_publisher",
                CataloguePage::default(),
                "invalid_page_flags",
            ),
            ("rows", CataloguePage::default(), "empty_revision"),
        ] {
            assert_message_diagnostic(
                diagnostic_catalogue_submission(phase, RemoteCall::Done(page)),
                "failed",
                Some(cause),
                None,
            );
        }
        let mut wrong_route = confirmed_page("scribe", "PRIVATE-REVISION");
        wrong_route.rows[0].provider = "PRIVATE-PROVIDER".into();
        assert_message_diagnostic(
            diagnostic_catalogue_submission("rows", RemoteCall::Done(wrong_route)),
            "failed",
            Some("row_route_mismatch"),
            None,
        );
        let mut state = diagnostic_rows(RemoteCall::Pending);
        state.call = None;
        state.provider = None;
        assert_message_diagnostic(
            AgentMessageSubmission::Rows(Box::new(state)),
            "failed",
            Some("no_match"),
            None,
        );
        let mut state = diagnostic_rows(RemoteCall::Pending);
        state.call = None;
        state
            .seen_requests
            .insert(("chat-provider".into(), String::new(), Vec::new()));
        assert_message_diagnostic(
            AgentMessageSubmission::Rows(Box::new(state)),
            "failed",
            Some("repeated_cursor"),
            None,
        );
    }

    #[test]
    fn rows_and_confirmation_metadata_failures_have_precise_closed_causes() {
        let valid = confirmed_page("scribe", "PRIVATE-REVISION");
        let mut cases = Vec::new();
        let mut page = valid.clone();
        page.revision.clear();
        cases.push((page, "empty_revision", "revision_changed"));
        let mut page = valid.clone();
        page.stale_cursor = true;
        cases.push((page, "malformed_stale_page", "stale_cursor"));
        let mut page = valid.clone();
        page.budget_exhausted = true;
        cases.push((page, "page_budget_exhausted", "page_budget_exhausted"));
        let mut page = valid.clone();
        page.exhausted = false;
        cases.push((page, "inconsistent_completion", "inconsistent_completion"));
        for (page, rows_cause, confirm_cause) in cases {
            assert_message_diagnostic(
                diagnostic_catalogue_submission("rows", RemoteCall::Done(page.clone())),
                "failed",
                Some(rows_cause),
                None,
            );
            assert_message_diagnostic(
                diagnostic_catalogue_submission("confirm", RemoteCall::Done(page)),
                "failed",
                Some(confirm_cause),
                None,
            );
        }
        let mut state = diagnostic_rows(RemoteCall::Done(valid.clone()));
        state.revision = Some("PRIVATE-OLD-REVISION".into());
        assert_message_diagnostic(
            AgentMessageSubmission::Rows(Box::new(state)),
            "failed",
            Some("revision_changed"),
            None,
        );
        let mut page = valid.clone();
        page.next_cursor = b"PRIVATE-CURSOR".to_vec();
        assert_message_diagnostic(
            diagnostic_catalogue_submission("confirm", RemoteCall::Done(page)),
            "failed",
            Some("cursor_not_empty"),
            None,
        );
        let mut page = valid;
        page.rows.clear();
        assert_message_diagnostic(
            diagnostic_catalogue_submission("confirm", RemoteCall::Done(page)),
            "failed",
            Some("row_count_mismatch"),
            None,
        );
    }

    #[test]
    fn both_invocation_phases_observe_progress_success_and_private_terminal_failure() {
        for launch in [false, true] {
            let invocation = InvocationId::new();
            let calls = [
                (RemoteCall::Pending, "waiting", None, None),
                (
                    RemoteCall::Done(InvocationUpdate::Accepted { invocation }),
                    "waiting",
                    None,
                    None,
                ),
                (
                    RemoteCall::Done(InvocationUpdate::Succeeded {
                        invocation,
                        output: ActionOutput::default(),
                    }),
                    if launch { "advance" } else { "succeeded" },
                    None,
                    None,
                ),
                (
                    RemoteCall::Done(InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "PRIVATE-CODE".into(),
                            message: "PRIVATE-ERROR".into(),
                            details: Some(serde_json::json!({"PRIVATE": "PRIVATE-TEXT"})),
                            retryable: false,
                        },
                    }),
                    "failed",
                    Some("invocation_failed"),
                    None,
                ),
                (
                    RemoteCall::Done(InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("PRIVATE-ERROR".into()),
                    }),
                    "failed",
                    Some("invocation_cancelled"),
                    None,
                ),
                (
                    RemoteCall::Refused(tilo_actions::remote::ActionHostError::NotPermitted),
                    "failed",
                    Some("remote_refused"),
                    Some("not_permitted"),
                ),
            ];
            for (call, result, cause, remote) in calls {
                let call = LocalState::new(call);
                let submission = if launch {
                    AgentMessageSubmission::LaunchInvoke { invocation, call }
                } else {
                    AgentMessageSubmission::Invoke { invocation, call }
                };
                assert_message_diagnostic(submission, result, cause, remote);
            }
        }
    }

    fn advance_discovery(
        actions: &RemoteActions,
        submission: &AgentMessageSubmission,
        deadline: Instant,
        pages: u16,
    ) -> (AgentMessageSubmission, u16) {
        let AgentMessagePoll::Advance {
            submission,
            pages_remaining,
        } = poll(
            actions,
            "room-a",
            "scribe",
            "PRIVATE-TEXT",
            submission,
            deadline,
            pages,
        )
        else {
            panic!("expected an actual discovery advance")
        };
        (submission, pages_remaining)
    }

    fn answer_discovery(submission: &AgentMessageSubmission, page: CataloguePage) {
        match submission {
            AgentMessageSubmission::Rows(state) => {
                state.call.as_ref().unwrap().set(RemoteCall::Done(page))
            }
            AgentMessageSubmission::Confirm { call, .. } => call.set(RemoteCall::Done(page)),
            _ => panic!("expected a catalogue call"),
        }
    }

    fn stale_discovery() -> CataloguePage {
        CataloguePage {
            revision: "new-revision".into(),
            stale_cursor: true,
            ..CataloguePage::default()
        }
    }

    #[test]
    fn stale_rows_restart_same_provider_and_confirm_only_fresh_target_once() {
        use crate::collaboration::CollabRequest;
        use crate::widget::Collaborative;
        use tilo_actions::remote::RemoteActionMessage;
        let mut actions = RemoteActions::session();
        let deadline = crate::time::now() + RESOLVE_WINDOW;
        let mut state = diagnostic_rows(RemoteCall::Pending);
        state.call = None;
        let initial = AgentMessageSubmission::Rows(Box::new(state));
        let (pending, pages) = advance_discovery(&actions, &initial, deadline, MAX_PAGES);
        let mut old = confirmed_page("scribe", "old-revision");
        old.rows[0].has_children = true;
        let old_target = old.rows[0].target().unwrap();
        answer_discovery(&pending, old);
        let (pending, pages) = advance_discovery(&actions, &pending, deadline, pages);
        answer_discovery(&pending, stale_discovery());
        let (restarted, pages) = advance_discovery(&actions, &pending, deadline, pages);
        assert_eq!(pages, MAX_PAGES - 2);
        let AgentMessageSubmission::Rows(state) = &restarted else {
            panic!("expected Rows")
        };
        assert_eq!(state.provider.as_deref(), Some("chat-provider"));
        assert_eq!(state.parent.as_deref(), Some(""));
        assert!(state.providers.is_empty() && state.parents.is_empty() && state.matches.is_empty());
        assert!(state.revision.is_none() && state.cursor.is_empty() && state.call.is_none());
        assert!(state.seen_resources.is_empty() && state.seen_requests.is_empty());
        let (pending, pages) = advance_discovery(&actions, &restarted, deadline, pages);
        let mut fresh = confirmed_page("scribe", "new-revision");
        fresh.rows[0].resource.id = "fresh-opaque-resource".into();
        fresh.rows[0].action.as_mut().unwrap().address.resource = "fresh-opaque-resource".into();
        let fresh_target = fresh.rows[0].target().unwrap();
        answer_discovery(&pending, fresh.clone());
        let (confirmed, pages) = advance_discovery(&actions, &pending, deadline, pages);
        let AgentMessageSubmission::Confirm {
            target, revision, ..
        } = &confirmed
        else {
            panic!("expected Confirm")
        };
        assert_eq!(target, &fresh_target);
        assert_ne!(target, &old_target);
        assert_eq!(revision, "new-revision");
        answer_discovery(&confirmed, fresh);
        let (invoking, pages) = advance_discovery(&actions, &confirmed, deadline, pages);
        assert_eq!(pages, MAX_PAGES - 4);
        assert!(matches!(invoking, AgentMessageSubmission::Invoke { .. }));
        let requests = actions.take_collab_requests();
        let invokes = requests
            .iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(request),
                } => Some(request),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(invokes.len(), 1);
        assert_eq!(invokes[0].target, fresh_target);
        assert_eq!(
            invokes[0].argument,
            InvokeArgument::Text("PRIVATE-TEXT".into())
        );
        let queries = requests
            .iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::CatalogueRequest { query, .. },
                } => Some(query),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(queries.len(), 4);
        assert!(
            queries
                .iter()
                .all(|query| query.provider.as_deref() == Some("chat-provider")
                    && query.canonical_route.is_none())
        );
        assert!(
            queries[2].parent_resource.is_none()
                && queries[2].revision.is_none()
                && queries[2].cursor.is_empty()
        );
    }

    #[test]
    fn stale_rows_never_extend_budget_deadline_or_accept_malformed_route() {
        use crate::widget::Collaborative;
        let valid = stale_discovery();
        let mut state = diagnostic_rows(RemoteCall::Done(valid.clone()));
        state.revision = Some("old-revision".into());
        let mut actions = RemoteActions::session();
        let submission = AgentMessageSubmission::Rows(Box::new(state.clone()));
        assert!(matches!(
            poll(
                &actions,
                "room-a",
                "scribe",
                "PRIVATE-TEXT",
                &submission,
                crate::time::now() + RESOLVE_WINDOW,
                1
            ),
            AgentMessagePoll::Failed
        ));
        assert!(matches!(
            poll(
                &actions,
                "room-a",
                "scribe",
                "PRIVATE-TEXT",
                &submission,
                crate::time::now(),
                MAX_PAGES
            ),
            AgentMessagePoll::Failed
        ));
        let mut malformed = Vec::new();
        let mut page = valid.clone();
        page.rows.push(message_row("scribe"));
        malformed.push(page);
        let mut page = valid.clone();
        page.providers = complete_providers_page().providers;
        malformed.push(page);
        let mut page = valid.clone();
        page.revision.clear();
        malformed.push(page);
        let mut page = valid.clone();
        page.exhausted = true;
        malformed.push(page);
        let mut page = valid.clone();
        page.budget_exhausted = true;
        malformed.push(page);
        let mut page = valid;
        page.next_cursor = b"PRIVATE-CURSOR".to_vec();
        malformed.push(page);
        for page in malformed {
            let mut state = state.clone();
            state.call = Some(LocalState::new(RemoteCall::Done(page)));
            assert!(matches!(
                poll(
                    &actions,
                    "room-a",
                    "scribe",
                    "PRIVATE-TEXT",
                    &AgentMessageSubmission::Rows(Box::new(state)),
                    crate::time::now() + RESOLVE_WINDOW,
                    MAX_PAGES
                ),
                AgentMessagePoll::Failed
            ));
        }
        for changed in [0, 1, 2, 3] {
            let mut state = state.clone();
            match changed {
                0 => state.provider = None,
                1 => state.parent = None,
                2 => state.providers.push_back("foreign-provider".into()),
                _ => state.revision = None,
            }
            assert!(matches!(
                poll(
                    &actions,
                    "room-a",
                    "scribe",
                    "PRIVATE-TEXT",
                    &AgentMessageSubmission::Rows(Box::new(state)),
                    crate::time::now() + RESOLVE_WINDOW,
                    MAX_PAGES
                ),
                AgentMessagePoll::Failed
            ));
        }
        assert!(actions.take_collab_requests().is_empty());
    }

    #[test]
    fn continuously_stale_rows_consume_original_budget_without_invoking() {
        use crate::collaboration::CollabRequest;
        use crate::widget::Collaborative;
        use tilo_actions::remote::RemoteActionMessage;
        let mut actions = RemoteActions::session();
        let deadline = crate::time::now() + RESOLVE_WINDOW;
        let mut state = diagnostic_rows(RemoteCall::Pending);
        state.call = None;
        let mut submission = AgentMessageSubmission::Rows(Box::new(state));
        let mut pages = 4;
        for attempt in 0..2 {
            (submission, pages) = advance_discovery(&actions, &submission, deadline, pages);
            let mut root = confirmed_page("scribe", "old-revision");
            root.rows[0].action = None;
            root.rows[0].has_children = true;
            answer_discovery(&submission, root);
            (submission, pages) = advance_discovery(&actions, &submission, deadline, pages);
            answer_discovery(&submission, stale_discovery());
            let result = poll(
                &actions,
                "room-a",
                "scribe",
                "PRIVATE-TEXT",
                &submission,
                deadline,
                pages,
            );
            if attempt == 0 {
                let AgentMessagePoll::Advance {
                    submission: advanced,
                    pages_remaining,
                } = result
                else {
                    panic!("first stale page should restart")
                };
                submission = advanced;
                pages = pages_remaining;
                assert_eq!(pages, 2);
            } else {
                assert!(matches!(result, AgentMessagePoll::Failed));
            }
        }
        let requests = actions.take_collab_requests();
        assert_eq!(requests.len(), 4);
        assert!(requests.iter().all(|request| matches!(request,
            CollabRequest::RemoteAction { message: RemoteActionMessage::CatalogueRequest { query, .. } }
                if query.provider.as_deref() == Some("chat-provider") && query.canonical_route.is_none())));
    }

    fn message_row(agent: &str) -> CatalogueRow {
        let resource = format!("chat.person:{agent}");
        CatalogueRow {
            provider: "chat-provider".into(),
            app_id: "so.tilo.chat".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.clone(),
                kind: "chat.agent".into(),
                label: "Agent".into(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                address: ActionAddress::new(&resource, "message"),
                semantic_identity: Some(ResourceSemanticIdentity {
                    kind: "chat.agent".into(),
                    key: resource,
                }),
                resource_kind: "chat.agent".into(),
                resource_label: "Agent".into(),
                action_label: "Message".into(),
                description: None,
                locus: ExecutionLocus::ClientLocal,
                kind: OperationKind::Document,
                contract: ActionContract::default(),
                input: InputSpec::None,
                params: Vec::new(),
                view: None,
            }),
            has_children: false,
            visibility: None,
        }
    }

    fn confirmed_page(agent: &str, revision: &str) -> CataloguePage {
        CataloguePage {
            revision: revision.into(),
            rows: vec![message_row(agent)],
            exhausted: true,
            ..CataloguePage::default()
        }
    }

    fn offer_from_page(page: CataloguePage, target: RemoteActionTarget) -> AgentMessageOfferPoll {
        let revision = page.revision.clone();
        poll_offer(
            &RemoteActions::session(),
            "room-a",
            "scribe",
            &AgentMessageSubmission::Confirm {
                target,
                revision,
                call: LocalState::new(RemoteCall::Done(page)),
            },
            Instant::now().checked_add(RESOLVE_WINDOW).unwrap(),
            MAX_PAGES,
        )
    }

    #[test]
    fn absent_chat_provider_enters_the_certified_app_launch_walk() {
        let providers = LocalState::new(RemoteCall::Done(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }));
        let result = poll(
            &RemoteActions::session(),
            "room-a",
            "scribe",
            "hello",
            &AgentMessageSubmission::Providers(providers),
            crate::time::now().checked_add(RESOLVE_WINDOW).unwrap(),
            MAX_PAGES,
        );
        assert!(matches!(
            result,
            AgentMessagePoll::Advance {
                submission: AgentMessageSubmission::LaunchRows(_),
                ..
            }
        ));
    }

    #[test]
    fn incomplete_canonical_provider_lookup_never_launches_or_selects_a_publisher() {
        use crate::widget::Collaborative;
        let providers = LocalState::new(RemoteCall::Done(CataloguePage::default()));
        let mut actions = RemoteActions::session();
        let result = poll(
            &actions,
            "room-a",
            "scribe",
            "hello",
            &AgentMessageSubmission::Providers(providers),
            crate::time::now().checked_add(RESOLVE_WINDOW).unwrap(),
            MAX_PAGES,
        );
        assert!(matches!(result, AgentMessagePoll::Failed));
        assert!(actions.take_collab_requests().is_empty());
    }

    #[test]
    fn discovery_requests_exact_canonical_chat_room_record() {
        use crate::collaboration::CollabRequest;
        use crate::widget::Collaborative;
        use tilo_actions::remote::RemoteActionMessage;
        let mut actions = RemoteActions::session();
        let _submission = begin(&actions, "room-a");
        assert!(matches!(actions.take_collab_requests().as_slice(),
            [CollabRequest::RemoteAction { message: RemoteActionMessage::CatalogueRequest { query, .. } }]
            if query.max_results == 0 && query.canonical_route == Some(canonical_chat_route("room-a"))
                && query.provider.is_none()));
    }

    #[test]
    fn admitted_cold_launch_is_observed_past_discovery_expiry() {
        let invocation = InvocationId::new();
        let call = LocalState::new(RemoteCall::Done(InvocationUpdate::Accepted { invocation }));
        let submission = AgentMessageSubmission::LaunchInvoke {
            invocation,
            call: call.clone(),
        };
        assert!(matches!(
            poll(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                "hello",
                &submission,
                crate::time::now(),
                0,
            ),
            AgentMessagePoll::Waiting
        ));
        call.set(RemoteCall::Done(InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput::default(),
        }));
        assert!(matches!(
            poll(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                "hello",
                &submission,
                crate::time::now(),
                MAX_PAGES,
            ),
            AgentMessagePoll::Advance {
                submission: AgentMessageSubmission::AwaitPublisher { .. },
                ..
            }
        ));
    }

    #[test]
    fn offer_discovery_confirms_membership_without_invoking_the_message() {
        let row = message_row("scribe");
        let target = row.target().expect("message target");
        let call = LocalState::new(RemoteCall::Done(CataloguePage {
            revision: "chat-r7".into(),
            rows: vec![row],
            exhausted: true,
            ..CataloguePage::default()
        }));
        let submission = AgentMessageSubmission::Confirm {
            target,
            revision: "chat-r7".into(),
            call,
        };
        let AgentMessageOfferPoll::Available(offer) = poll_offer(
            &RemoteActions::session(),
            "room-a",
            "scribe",
            &submission,
            Instant::now().checked_add(RESOLVE_WINDOW).unwrap(),
            MAX_PAGES,
        ) else {
            panic!("the exact current agent Message row should be offered");
        };
        assert!(matches!(
            begin_from_offer(&RemoteActions::session(), &offer),
            AgentMessageSubmission::Confirm { .. }
        ));
    }

    #[test]
    fn confirmed_semantic_agent_offer_invokes_its_exact_opaque_runtime_resource() {
        use crate::collaboration::CollabRequest;
        use crate::widget::Collaborative;
        use tilo_actions::remote::RemoteActionMessage;

        let mut page = confirmed_page("scribe", "chat-r9");
        page.rows[0].resource.id = "opaque-agent-row".into();
        page.rows[0]
            .action
            .as_mut()
            .expect("message descriptor")
            .address
            .resource = "opaque-agent-row".into();
        let target = page.rows[0].target().expect("opaque message target");
        assert!(matches!(
            offer_from_page(page.clone(), target.clone()),
            AgentMessageOfferPoll::Available(_)
        ));
        let mut actions = RemoteActions::session();
        let result = poll(
            &actions,
            "room-a",
            "scribe",
            "hello",
            &AgentMessageSubmission::Confirm {
                target: target.clone(),
                revision: "chat-r9".into(),
                call: LocalState::new(RemoteCall::Done(page)),
            },
            crate::time::now().checked_add(RESOLVE_WINDOW).unwrap(),
            MAX_PAGES,
        );
        assert!(matches!(
            result,
            AgentMessagePoll::Advance {
                submission: AgentMessageSubmission::Invoke { .. },
                ..
            }
        ));
        assert!(matches!(actions.take_collab_requests().as_slice(),
            [CollabRequest::RemoteAction { message: RemoteActionMessage::Invoke(request) }]
                if request.target == target && request.argument == InvokeArgument::Text("hello".into())));
    }

    #[test]
    fn offer_discovery_rejects_a_withdrawn_or_different_agent() {
        let row = message_row("other-agent");
        let target = row.target().expect("message target");
        let call = LocalState::new(RemoteCall::Done(CataloguePage {
            revision: "chat-r8".into(),
            rows: vec![row],
            exhausted: true,
            ..CataloguePage::default()
        }));
        let submission = AgentMessageSubmission::Confirm {
            target,
            revision: "chat-r8".into(),
            call,
        };
        assert!(matches!(
            poll_offer(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                &submission,
                Instant::now().checked_add(RESOLVE_WINDOW).unwrap(),
                MAX_PAGES,
            ),
            AgentMessageOfferPoll::Failed
        ));
    }

    #[test]
    fn offer_confirmation_fails_closed_on_partial_or_mismatched_catalogue_truth() {
        let valid = confirmed_page("scribe", "chat-r9");
        let target = valid.rows[0].target().expect("message target");

        let mut cases = Vec::new();
        let mut page = valid.clone();
        page.revision.clear();
        cases.push(page);
        let mut page = valid.clone();
        page.exhausted = false;
        cases.push(page);
        let mut page = valid.clone();
        page.next_cursor = vec![1];
        cases.push(page);
        let mut page = valid.clone();
        page.stale_cursor = true;
        cases.push(page);
        let mut page = valid.clone();
        page.budget_exhausted = true;
        cases.push(page);
        let mut page = valid.clone();
        page.rows[0].provider = "other-provider".into();
        cases.push(page);
        let mut page = valid.clone();
        page.rows[0].resource.id = "chat.person:other".into();
        cases.push(page);
        let mut page = valid.clone();
        page.rows[0]
            .action
            .as_mut()
            .expect("message action")
            .address
            .resource = "chat.person:other".into();
        cases.push(page);
        let mut page = valid.clone();
        page.rows[0].action.as_mut().expect("message action").kind = OperationKind::View;
        cases.push(page);

        for page in cases {
            assert!(matches!(
                offer_from_page(page, target.clone()),
                AgentMessageOfferPoll::Failed
            ));
        }
    }

    #[test]
    fn pending_offer_expires_instead_of_remaining_a_writable_ghost() {
        assert!(matches!(
            poll_offer(
                &RemoteActions::session(),
                "room-a",
                "scribe",
                &AgentMessageSubmission::Confirm {
                    target: message_row("scribe").target().expect("message target"),
                    revision: "chat-r10".into(),
                    call: LocalState::new(RemoteCall::Pending),
                },
                Instant::now(),
                MAX_PAGES,
            ),
            AgentMessageOfferPoll::Failed
        ));
    }

    #[test]
    fn discovery_page_rejects_rows_from_an_unrequested_provider_or_parent() {
        let state = AgentMessageRows {
            providers: VecDeque::new(),
            provider: Some("chat-provider".into()),
            parents: VecDeque::new(),
            parent: Some(String::new()),
            revision: None,
            cursor: Vec::new(),
            call: None,
            matches: Vec::new(),
            seen_requests: HashSet::new(),
            seen_resources: HashSet::new(),
        };
        let mut wrong_provider = confirmed_page("scribe", "chat-r11");
        wrong_provider.rows[0].provider = "other-provider".into();
        assert!(ingest_page(&mut state.clone(), wrong_provider, "scribe").is_err());

        let mut wrong_parent = confirmed_page("scribe", "chat-r11");
        wrong_parent.rows[0].parent_resource = "chat.channel:other".into();
        assert!(ingest_page(&mut state.clone(), wrong_parent, "scribe").is_err());
    }
}

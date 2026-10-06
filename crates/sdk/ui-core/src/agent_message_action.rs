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

#[derive(Clone)]
pub enum AgentMessageSubmission {
    Providers(LocalState<CatalogueCall>),
    LaunchRows(Box<AgentLaunchRows>),
    LaunchConfirm {
        target: RemoteActionTarget,
        revision: String,
        call: LocalState<CatalogueCall>,
    },
    LaunchInvoke(LocalState<InvocationCall>),
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
    Invoke(LocalState<InvocationCall>),
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
pub fn begin(actions: &RemoteActions) -> AgentMessageSubmission {
    AgentMessageSubmission::Providers(actions.catalogue(CatalogueQuery {
        max_results: 0,
        ..CatalogueQuery::default()
    }))
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
        AgentMessageSubmission::LaunchInvoke(_)
            | AgentMessageSubmission::AwaitPublisher { .. }
            | AgentMessageSubmission::Invoke(_)
    ) && (crate::time::now() >= resolve_deadline || pages_remaining == 0)
    {
        return AgentMessagePoll::Failed;
    }
    match submission {
        AgentMessageSubmission::Providers(call) => poll_providers(workspace, call, pages_remaining),
        AgentMessageSubmission::LaunchRows(state) => {
            poll_launch_rows(actions, workspace, state, pages_remaining)
        }
        AgentMessageSubmission::LaunchConfirm {
            target,
            revision,
            call,
        } => poll_launch_confirmation(actions, workspace, target, revision, call, pages_remaining),
        AgentMessageSubmission::LaunchInvoke(call) => {
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
        AgentMessageSubmission::Invoke(call) => poll_invocation(call),
    }
}

fn poll_providers(
    workspace: &str,
    call: &LocalState<CatalogueCall>,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let RemoteCall::Done(page) = call.get() else {
        return if call.get().is_pending() {
            AgentMessagePoll::Waiting
        } else {
            AgentMessagePoll::Failed
        };
    };
    if page.stale_cursor || page.budget_exhausted {
        return AgentMessagePoll::Failed;
    }
    let mut providers = page
        .providers
        .into_iter()
        .filter(|provider| provider.app_id == "so.tilo.chat" && provider.room == workspace)
        .map(|provider| provider.provider)
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();
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
            RemoteCall::Refused(_) => return AgentMessagePoll::Failed,
            RemoteCall::Done(page) => page,
        };
        pages_remaining = pages_remaining.saturating_sub(1);
        if page.stale_cursor
            || page.budget_exhausted
            || revision
                .as_ref()
                .is_some_and(|expected| expected != &page.revision)
        {
            return AgentMessagePoll::Failed;
        }
        for row in &page.rows {
            if is_chat_application_open_row(row, workspace) {
                let Some(found) = row.target() else {
                    return AgentMessagePoll::Failed;
                };
                if target.is_some() {
                    return AgentMessagePoll::Failed;
                }
                target = Some((found, page.revision.clone()));
            }
        }
        if page.next_cursor.is_empty() {
            let Some((target, revision)) = target else {
                return AgentMessagePoll::Failed;
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
        return AgentMessagePoll::Failed;
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
        RemoteCall::Refused(_) => return AgentMessagePoll::Failed,
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
        return AgentMessagePoll::Failed;
    }
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::LaunchInvoke(actions.invoke(
            target.clone(),
            InvokeArgument::None,
            None,
        )),
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
        RemoteCall::Refused(_)
        | RemoteCall::Done(
            tilo_actions::InvocationUpdate::Failed { .. }
            | tilo_actions::InvocationUpdate::Cancelled { .. },
        ) => AgentMessagePoll::Failed,
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
        return AgentMessagePoll::Failed;
    }
    let Some(call) = call else {
        if now < retry_at {
            return AgentMessagePoll::Waiting;
        }
        return AgentMessagePoll::Advance {
            submission: AgentMessageSubmission::AwaitPublisher {
                call: Some(actions.catalogue(CatalogueQuery {
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
        RemoteCall::Refused(_) => return AgentMessagePoll::Failed,
        RemoteCall::Done(page) => page,
    };
    if page.stale_cursor || page.budget_exhausted {
        return AgentMessagePoll::Failed;
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
            RemoteCall::Refused(_) => return AgentMessagePoll::Failed,
            RemoteCall::Done(page) => page,
        };
        pages_remaining = pages_remaining.saturating_sub(1);
        if ingest_page(&mut state, page, agent).is_err() {
            return AgentMessagePoll::Failed;
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
        return AgentMessagePoll::Failed;
    };
    if !state
        .seen_requests
        .insert((provider.clone(), parent.clone(), state.cursor.clone()))
    {
        return AgentMessagePoll::Failed;
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

fn confirm_unique(
    actions: &RemoteActions,
    mut state: AgentMessageRows,
    pages_remaining: u16,
) -> AgentMessagePoll {
    let Some((target, revision)) = state.matches.pop() else {
        return AgentMessagePoll::Failed;
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
        RemoteCall::Refused(_) => return AgentMessagePoll::Failed,
        RemoteCall::Done(page) => page,
    };
    if !is_exact_confirmation(&page, agent, target, revision) {
        return AgentMessagePoll::Failed;
    }
    AgentMessagePoll::Advance {
        submission: AgentMessageSubmission::Invoke(actions.invoke(
            target.clone(),
            InvokeArgument::Text(text.to_owned()),
            None,
        )),
        pages_remaining: pages_remaining.saturating_sub(1),
    }
}

fn poll_invocation(call: &LocalState<InvocationCall>) -> AgentMessagePoll {
    match call.get() {
        RemoteCall::Refused(_) => AgentMessagePoll::Failed,
        RemoteCall::Done(update) if update.is_terminal() => {
            if matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }) {
                AgentMessagePoll::Succeeded
            } else {
                AgentMessagePoll::Failed
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

fn ingest_page(state: &mut AgentMessageRows, page: CataloguePage, agent: &str) -> Result<(), ()> {
    let Some(provider) = state.provider.as_deref() else {
        return Err(());
    };
    let Some(parent) = state.parent.as_deref() else {
        return Err(());
    };
    if page.revision.is_empty()
        || page.stale_cursor
        || page.budget_exhausted
        || (page.next_cursor.is_empty() != page.exhausted)
        || state
            .revision
            .as_ref()
            .is_some_and(|expected| expected != &page.revision)
    {
        return Err(());
    }
    if page
        .rows
        .iter()
        .any(|row| row.provider != provider || row.parent_resource != parent)
    {
        return Err(());
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
            let target = row.target().ok_or(())?;
            state.matches.push((target, page.revision.clone()));
            if state.matches.len() > 1 {
                return Err(());
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
        let providers = LocalState::new(RemoteCall::Done(CataloguePage::default()));
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
    fn admitted_cold_launch_is_observed_past_discovery_expiry() {
        let invocation = InvocationId::new();
        let call = LocalState::new(RemoteCall::Done(InvocationUpdate::Accepted { invocation }));
        let submission = AgentMessageSubmission::LaunchInvoke(call.clone());
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
                submission: AgentMessageSubmission::Invoke(_),
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

//! Native discovery on the existing collaboration lane, separate from file custody.
use super::selected_open::components;
use crate::i18n::t;
use crate::open_with::{OpenWithApp, OpenWithCandidates};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};
use ui::{
    collaboration::{
        CollabEvent, CollabRequest,
        open_selected::{discovery, v2::SelectedAccess},
    },
    time::{Duration, Instant},
};

const FRESH_FOR: Duration = Duration::from_secs(2);
const REQUEST_BUDGET: Duration = Duration::from_secs(12);
const MAX_PATHS: usize = 128;
#[derive(Default)]
pub(super) struct Discovery(RefCell<State>);
#[derive(Default)]
struct State {
    next: u64,
    origins: BTreeMap<String, ui::tilo_actions::InvocationId>,
    queued: VecDeque<(PathBuf, discovery::Request)>,
    pending: Option<(u64, PathBuf, discovery::Request, Instant)>,
    snapshots: BTreeMap<PathBuf, (Instant, Result<discovery::Reply, String>)>,
    action_reply: Option<(PathBuf, String, Result<discovery::Reply, String>)>,
}
impl Discovery {
    fn enqueue(
        &self,
        path: &Path,
        force: bool,
        scope: Option<(discovery::InvocationScope, ui::tilo_actions::InvocationId)>,
    ) -> Option<String> {
        let components = components(path).ok()?;
        let mut state = self.0.borrow_mut();
        if let Some((_, held, request, _)) = &state.pending
            && held == path
            && !force
        {
            return Some(request.request_id.clone());
        }
        if force {
            let removed = state
                .queued
                .iter()
                .filter(|(held, _)| held == path)
                .map(|(_, request)| request.request_id.clone())
                .collect::<Vec<_>>();
            state.queued.retain(|(held, _)| held != path);
            for id in removed {
                state.origins.remove(&id);
            }
        } else if let Some((_, request)) = state.queued.iter().find(|(held, _)| held == path) {
            return Some(request.request_id.clone());
        }
        if !force
            && state
                .snapshots
                .get(path)
                .is_some_and(|(at, _)| ui::time::now().duration_since(*at) < FRESH_FOR)
        {
            return None;
        }
        if state.queued.len() >= MAX_PATHS {
            return None;
        }
        let (invocation, origin) =
            scope.map_or((None, None), |(scope, origin)| (Some(scope), Some(origin)));
        let request = discovery::Request {
            invocation,
            protocol: discovery::PROTOCOL.into(),
            schema: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            components,
        };
        if !request.valid() {
            return None;
        }
        let id = request.request_id.clone();
        if let Some(origin) = origin {
            state.origins.insert(id.clone(), origin);
        }
        if force {
            state.queued.push_front((path.to_owned(), request));
        } else {
            state.queued.push_back((path.to_owned(), request));
        }
        Some(id)
    }
    pub(super) fn refresh(
        &self,
        path: &Path,
        scope: discovery::InvocationScope,
        invocation: ui::tilo_actions::InvocationId,
    ) -> Result<String, String> {
        self.enqueue(path, true, Some((scope, invocation)))
            .ok_or_else(|| t("files-open-with-launch-unavailable"))
    }
    pub(super) fn fresh_reply(
        &self,
        path: &Path,
        request_id: &str,
    ) -> Option<Result<discovery::Reply, String>> {
        let mut state = self.0.borrow_mut();
        if !state
            .action_reply
            .as_ref()
            .is_some_and(|(held, id, _)| held == path && id == request_id)
        {
            return None;
        }
        state.action_reply.take().map(|(_, _, result)| result)
    }
    fn snapshot(&self, path: &Path) -> Option<discovery::Reply> {
        self.enqueue(path, false, None);
        let state = self.0.borrow();
        let (at, reply) = state.snapshots.get(path)?;
        (ui::time::now().duration_since(*at) < FRESH_FOR)
            .then(|| reply.as_ref().ok().cloned())
            .flatten()
    }
    pub(super) fn candidates(&self, path: &Path) -> OpenWithCandidates {
        self.snapshot(path)
            .map_or_else(OpenWithCandidates::default, |reply| OpenWithCandidates {
                type_label: reply.type_label,
                apps: reply
                    .targets
                    .into_iter()
                    .map(|target| OpenWithApp {
                        id: target.app_id,
                        name: target.name,
                        is_default: target.is_default,
                    })
                    .collect(),
            })
    }
    pub(super) fn access(&self, path: &Path, app: &str) -> Option<SelectedAccess> {
        self.snapshot(path)?
            .targets
            .into_iter()
            .find(|target| target.app_id == app)
            .map(|target| target.access)
    }
    pub(super) fn available(&self, path: &Path) -> bool {
        self.snapshot(path)
            .is_some_and(|reply| !reply.targets.is_empty())
    }
    pub(super) fn take_request(&self) -> Option<CollabRequest> {
        let mut state = self.0.borrow_mut();
        if state
            .pending
            .as_ref()
            .is_some_and(|(_, _, _, deadline)| ui::time::now() >= *deadline)
            && let Some((_, path, request, _)) = state.pending.take()
        {
            let error = Err(t("files-open-with-timeout"));
            if request.invocation.is_some() {
                state.action_reply = Some((path, request.request_id, error));
            } else {
                state.snapshots.insert(path, (ui::time::now(), error));
            }
        }
        if state.pending.is_some() {
            return None;
        }
        let (path, command) = state.queued.pop_front()?;
        state.next = state.next.wrapping_add(1).max(1);
        let request_id = state.next;
        state.pending = Some((
            request_id,
            path,
            command.clone(),
            ui::time::now() + REQUEST_BUDGET,
        ));
        let on_behalf_of = state.origins.remove(&command.request_id);
        Some(CollabRequest::SelectedDiscovery {
            request_id,
            command,
            on_behalf_of,
        })
    }
    pub(super) fn event(&self, event: &CollabEvent) -> bool {
        let CollabEvent::SelectedDiscoveryResult { request_id, result } = event else {
            return false;
        };
        let mut state = self.0.borrow_mut();
        if state
            .pending
            .as_ref()
            .is_none_or(|(id, _, _, _)| id != request_id)
        {
            return false;
        }
        let Some((_, path, request, deadline)) = state.pending.take() else {
            return false;
        };
        let result = result
            .clone()
            .map_err(|_| t("files-open-with-launch-unavailable"))
            .and_then(|reply| {
                if ui::time::now() < deadline && reply.matches(&request) {
                    Ok(reply)
                } else {
                    Err(t("files-open-with-selection-changed"))
                }
            });
        if request.invocation.is_some() {
            state.action_reply = Some((path, request.request_id, result));
        } else {
            if state.snapshots.len() >= MAX_PATHS && !state.snapshots.contains_key(&path) {
                state.snapshots.clear();
            }
            state.snapshots.insert(path, (ui::time::now(), result));
        }
        true
    }
}

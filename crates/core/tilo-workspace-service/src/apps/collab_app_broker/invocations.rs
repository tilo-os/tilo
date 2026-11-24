//! A workspace's invocations, newest first, as its dispatch lists them.
//!
//! The one source is the device's record of each invocation it was asked for
//! (`tilo_actions::remote::invocation_record::Invocations`): who asked, the request, when it was
//! asked and how it stands, asked again of the provider it runs at on every read. No listing is
//! kept beside it, so a call that is still going, waiting for its person or ended shows as it is
//! now.
//!
//! This is the one reader of those records. The questions that wait for a person
//! are the same listing with [`InvocationStatus::Waiting`], never a
//! second walk over the records.
//!
//! A caller reads the invocations its grants let it read now, each record checked on its own:
//! a call on a record by that record's node and the Read answer there, a call on no record by a
//! Read across the workspace. Who asked does not decide it; a member invited into a workspace
//! reads the calls on what the workspace's grants open to them.

use std::sync::Arc;

use tilo_action_policy::{ActionKind, TreePath};
use tilo_actions::remote::invocation_record::{Invocations, Recorded};
use tilo_actions::remote::workspace_invocations::{
    Invocation, InvocationBy, InvocationStatus, Invocations as Listed, InvocationsQuery, MOST,
    Origin,
};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueQuery, InvokeRequest, resource_parts,
};
use tilo_actions::{EffectReceiptStatus, InvocationUpdate};

use super::AppBrokerState;

impl AppBrokerState {
    /// Share `records`, the device's record of the invocations it was asked for, with this
    /// dispatch: what a listing of `workspace`'s invocations reads from now on.
    pub fn serve_invocations(&self, records: Arc<Invocations>) {
        let _ = self.invocations.set(records);
    }

    /// Read records outside their app through `reads`, the device's one record reader: what the
    /// apps and agents this broker admits read with from now on.
    pub fn serve_records(&self, reads: Arc<dyn tilo_actions::remote::ResourceReads>) {
        let _ = self.records.set(reads);
    }

    /// Keep a read the person is asked about through `asked`, the device's dispatch: what an
    /// app's join of such a record waits on from now on ([`Self::app_reads_record`]).
    pub fn serve_asked_reads(&self, asked: Arc<dyn tilo_actions::remote::asked::AskedReads>) {
        let _ = self.asked_reads.set(asked);
    }

    /// Take `placement` as the host that placed this device's window: the device whose link
    /// alone carries the person's word for the page actions it hands the window
    /// ([`tilo_actions::remote::browser::host_answered`]). Set once, by the session the host
    /// started for that window.
    pub fn placed_by(&self, placement: tilo_actions::remote::browser::Placement) {
        let _ = self.placement.set(placement);
    }

    /// Where a read the person is asked about waits for them, once its embedder served it.
    pub(crate) fn asked_reads(&self) -> Option<Arc<dyn tilo_actions::remote::asked::AskedReads>> {
        self.asked_reads.get().cloned()
    }

    /// The device's one record reader, once its embedder served it.
    pub(crate) fn records(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.records.get().cloned()
    }

    /// The page of `workspace`'s invocations `query` asks for, as `caller` reads it: one record
    /// per call the device was asked for, newest first, each one only when the caller's grants
    /// let it read what the call addressed.
    ///
    /// # Errors
    ///
    /// [`ActionCallError::NotPermitted`] for a caller no grant of the workspace admits; an
    /// unavailable dispatch for a device that shared no record with it.
    pub fn invocation_listing(
        &self,
        workspace: &str,
        caller: &Caller,
        query: &InvocationsQuery,
    ) -> Result<Listed, ActionCallError> {
        self.invocations_of(workspace, caller, query, query.status)
    }

    /// [`Self::invocation_listing`] as the one query function: `only` narrows it to one status
    /// where `query` names none, which is how the questions that wait for a person read the same
    /// listing.
    pub fn invocations_of(
        &self,
        workspace: &str,
        caller: &Caller,
        query: &InvocationsQuery,
        only: Option<InvocationStatus>,
    ) -> Result<Listed, ActionCallError> {
        if self.decide_for(caller, workspace, |_, _, _| ()).is_none() {
            return Err(ActionCallError::NotPermitted);
        }
        let records = self
            .invocations
            .get()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this workspace's dispatch keeps no record of its invocations".into(),
            })?;
        let mut items: Vec<Invocation> = records
            .listed(workspace)
            .iter()
            .filter_map(|recorded| self.invocation_of(caller, workspace, recorded))
            .collect();
        items.sort_by_key(|invocation| std::cmp::Reverse(key(invocation)));
        let items = of_invocation(items, query.invocation.as_deref());
        let items = of_member(items, query.by.as_deref());
        let items = of_status(items, only);
        Ok(page(items, query))
    }

    /// The workspace's invocations as a record read, whole (an empty id) or one by its id, as
    /// `caller` may read each call there. What a program, an agent and a view read through the
    /// record API, never as an action.
    pub(crate) fn read_invocations(
        &self,
        caller: &Caller,
        workspace: &str,
        record: &str,
        asked: Vec<tilo_actions::remote::asked::Waiting>,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        super::action_provider::check_execution(caller)?;
        let one = (!record.is_empty()).then(|| record.to_owned());
        let mut page = self.invocations_of(
            workspace,
            caller,
            &InvocationsQuery {
                limit: MOST,
                invocation: one,
                ..InvocationsQuery::default()
            },
            None,
        )?;
        let kept = page.items.len();
        with_questions(
            &mut page.items,
            asked,
            (!record.is_empty()).then_some(record),
        );
        if record.is_empty() {
            let waiting = page
                .items
                .iter()
                .filter(|invocation| invocation.status == InvocationStatus::Waiting)
                .count();
            let total = page.total + (page.items.len() - kept);
            return Ok(tilo_actions::remote::ResourceCopy::reading(
                serde_json::json!({ "items": page.items, "total": total, "waiting": waiting }),
                None,
            ));
        }
        Ok(match page.items.into_iter().next() {
            Some(invocation) => tilo_actions::remote::ResourceCopy::reading(
                serde_json::to_value(invocation).map_err(|error| ActionCallError::Unavailable {
                    reason: error.to_string(),
                })?,
                None,
            ),
            None => tilo_actions::remote::ResourceCopy::Absent,
        })
    }

    /// Wake `wake` once the workspace's invocations next change, decided as a read is: the
    /// record of a call recorded there, or of one that has not ended, moving.
    pub(crate) fn watch_invocations(
        &self,
        caller: &Caller,
        workspace: &str,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        if self.decide_for(caller, workspace, |_, _, _| ()).is_none() {
            return Err(ActionCallError::NotPermitted);
        }
        let records = self
            .invocations
            .get()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this workspace's dispatch keeps no record of its invocations".into(),
            })?;
        records.watch(workspace, wake);
        Ok(())
    }

    /// Whether `caller` may write the resource `origin` names in `workspace` now: what stopping
    /// or running again an invocation someone else made asks of the resource it was made from.
    /// Decided at the resource's node with the caller's actor, as a record's read is
    /// ([`Self::may_read`]).
    #[must_use]
    pub fn may_write_origin(&self, caller: &Caller, workspace: &str, origin: &Origin) -> bool {
        if super::action_provider::check_execution(caller).is_err() {
            return false;
        }
        let app = self.origin_app(&origin.kind);
        self.decide_for(caller, workspace, |workspace, actor, now| {
            let node = tilo_actions::resource_node(
                &workspace.policy.node(),
                &app,
                &origin.kind,
                &origin.id,
            );
            actor.may_in(workspace, &node, ActionKind::Undoable, now)
        })
        .unwrap_or(false)
    }

    /// The app that publishes `kind`, as a kind names its app (`chat.workspace.channels` is
    /// Chat's): the surface an app published of that kind says it, else the name the kind
    /// begins with, which is the app itself.
    fn origin_app(&self, kind: &str) -> String {
        let published = self.published_surfaces();
        if let Some(surface) = published.iter().find(|surface| {
            surface.actions.iter().any(|action| {
                action.resource_kind == kind
                    || action.resource_kind.starts_with(&format!("{kind}."))
                    || kind.starts_with(&format!("{}.", action.resource_kind))
            })
        }) {
            return surface.app_id.clone();
        }
        tilo_actions::tree::type_address(kind)
            .unwrap_or(kind)
            .to_owned()
    }

    /// The invocation `recorded` was asked for, as `caller` reads it: `None` when the caller's
    /// grants do not let them read the record it addressed now.
    fn invocation_of(
        &self,
        caller: &Caller,
        workspace: &str,
        recorded: &Recorded,
    ) -> Option<Invocation> {
        let addressed = resource_parts(&recorded.request.target.provider);
        if !self.may_read(caller, workspace, addressed) {
            return None;
        }
        Some(described(recorded, addressed))
    }

    /// Whether `caller`'s grants let them read what a call addressed now: the record `app` and
    /// `record`, through its node and the Read answer there, or, for a call on no record, a Read
    /// across the workspace.
    fn may_read(&self, caller: &Caller, workspace: &str, addressed: Option<(&str, &str)>) -> bool {
        self.decide_for(caller, workspace, |workspace, actor, now| match addressed {
            Some((app, record)) => {
                let node = TreePath::workspace(workspace.id().to_string())
                    .app(app)
                    .rest()
                    .resource(record);
                actor
                    .answer_in(workspace, &node, ActionKind::Read, now)
                    .admits(actor.direct())
            }
            None => actor.reads_in(workspace, now),
        })
        .unwrap_or(false)
    }

    /// A page of `workspace`'s invocations as a published tree
    /// (`tilo_actions::remote::workspace_invocations::tree_rows`): the invocations newest first,
    /// read as far as the page reaches. Its revision is how many there are, so a page read on
    /// from a cursor reads the listing it was cut from.
    pub fn invocation_page(
        &self,
        workspace: &str,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
        let wanted = query
            .offset()
            .saturating_add(query.page_size())
            .saturating_add(1);
        let mut listed = Vec::new();
        let mut cursor = None;
        let mut total;
        loop {
            let page = self.invocation_listing(
                workspace,
                caller,
                &InvocationsQuery {
                    limit: MOST,
                    cursor: cursor.take(),
                    ..InvocationsQuery::default()
                },
            )?;
            total = page.total;
            listed.extend(page.items);
            match page.next_cursor {
                Some(next) if listed.len() < wanted => cursor = Some(next),
                _ => break,
            }
        }
        let rows = tilo_actions::remote::workspace_invocations::tree_rows(&listed);
        Ok(CataloguePage::from_rows(total.to_string(), rows, query))
    }
}

/// One recorded invocation as the listing states it.
fn described(recorded: &Recorded, addressed: Option<(&str, &str)>) -> Invocation {
    let request = &recorded.request;
    let target = &request.target;
    Invocation {
        id: Some(recorded.request.invocation.0.to_string()),
        calls: vec![recorded.request.invocation.0.to_string()],
        origin: recorded.origin.clone(),
        retries: recorded.retries.map(|original| original.0.to_string()),
        spend: recorded.spend.clone(),
        step: None,
        by: by(&recorded.asked),
        title: title(request),
        status: if recorded.undone {
            InvocationStatus::Undone
        } else {
            status(recorded)
        },
        failure: recorded.state.as_ref().and_then(
            |state| match EffectReceiptStatus::from_terminal(&state.update) {
                Some(EffectReceiptStatus::Failed { code }) => Some(code),
                _ => None,
            },
        ),
        started_ms: i64::try_from(recorded.started_ms).unwrap_or(i64::MAX),
        ended_ms: recorded
            .ended_ms
            .map(|ended| i64::try_from(ended).unwrap_or(i64::MAX)),
        action: Some(match addressed {
            Some((app, _)) => format!("{}#{}", app_path(app), target.address.action),
            None => format!("{}#{}", target.address.resource, target.address.action),
        }),
        summary: None,
        output: recorded
            .state
            .as_ref()
            .and_then(|state| match &state.update {
                InvocationUpdate::Succeeded { output, .. } => output.value.clone(),
                _ => None,
            }),
        changes: usize::try_from(recorded.changes).unwrap_or(usize::MAX),
        undone: usize::from(recorded.undone),
        records: Vec::new(),
        undoable: false,
        undo_blocked: None,
        recorded: false,
        // A broker invocation's ask waits in the person's asked record, not in this listing.
        person: None,
        asked: None,
        remembered: true,
    }
}

/// Who asked for the invocation: its name, its kind, and the member it is.
pub(super) fn by(caller: &Caller) -> InvocationBy {
    let acting = caller.acting();
    let kind = if acting.is_agent() {
        "agent"
    } else if acting.is_bot() {
        "bot"
    } else if acting.is_automation() {
        "automation"
    } else {
        "person"
    };
    let name = caller
        .agent_called()
        .map(str::to_owned)
        .or_else(|| {
            acting
                .agent_id()
                .or_else(|| acting.bot_id())
                .map(str::to_owned)
        })
        .or_else(|| {
            acting
                .person_id()
                .map(|person| person.chars().take(8).collect())
        })
        .unwrap_or_else(|| "someone".to_owned());
    InvocationBy {
        name,
        kind: kind.to_owned(),
        id: acting.member_id().map(|member| member.id().to_owned()),
    }
}

/// What its client called the invocation, when it named it: never the session's own name, which
/// the group is named by anyway.
fn title(request: &InvokeRequest) -> Option<String> {
    let label = request.context.as_ref()?.group.as_ref()?.label.as_deref()?;
    let title = tilo_actions::context::one_line(label, tilo_actions::context::MAX_LABEL_CHARS)?;
    (request.said_session_name().as_deref() != Some(title.as_str())).then_some(title)
}

/// How the invocation stands now.
fn status(recorded: &Recorded) -> InvocationStatus {
    match recorded.state.as_ref().map(|state| &state.update) {
        Some(InvocationUpdate::Failed { .. }) => InvocationStatus::Failed,
        Some(InvocationUpdate::Cancelled { .. }) => InvocationStatus::Cancelled,
        Some(update) if update.is_terminal() => InvocationStatus::Ended,
        _ if recorded.waiting => InvocationStatus::Waiting,
        _ => InvocationStatus::Running,
    }
}

/// The app as a path names it: the last part of its id, in lower case.
fn app_path(app_id: &str) -> String {
    app_id.rsplit('.').next().unwrap_or(app_id).to_lowercase()
}

/// The runs of the member `by`, when the query asks for one member's, so an agent reads its own
/// work in a workspace where others work too; every run when it asks for none.
fn of_member(invocations: Vec<Invocation>, by: Option<&str>) -> Vec<Invocation> {
    match by {
        Some(by) => invocations
            .into_iter()
            .filter(|invocation| invocation.by.id.as_deref() == Some(by))
            .collect(),
        None => invocations,
    }
}

/// The invocation `named` names, when the query names one: the one a call of that id is, or the
/// one its id is; every invocation when it names none.
fn of_invocation(invocations: Vec<Invocation>, named: Option<&str>) -> Vec<Invocation> {
    let Some(named) = named else {
        return invocations;
    };
    invocations
        .into_iter()
        .filter(|invocation| {
            invocation.id.as_deref() == Some(named) || invocation.calls.iter().any(|id| id == named)
        })
        .collect()
}

/// The invocations in `only`, when it names a status; every invocation when it does not.
fn of_status(invocations: Vec<Invocation>, only: Option<InvocationStatus>) -> Vec<Invocation> {
    match only {
        Some(only) => invocations
            .into_iter()
            .filter(|invocation| invocation.status == only)
            .collect(),
        None => invocations,
    }
}

/// Where an invocation sorts, newest first, and what a cursor names it by.
fn key(invocation: &Invocation) -> (i64, String) {
    let name = invocation
        .id
        .clone()
        .unwrap_or_else(|| invocation.step.clone().unwrap_or_default());
    (invocation.started_ms, name)
}

/// The cursor a page ends with, naming its last invocation.
fn cursor_of(invocation: &Invocation) -> String {
    let (started, name) = key(invocation);
    format!("{started}:{name}")
}

/// The page of `items` (newest first) `query` asks for.
fn page(items: Vec<Invocation>, query: &InvocationsQuery) -> Listed {
    let total = items.len();
    let waiting = items
        .iter()
        .filter(|invocation| invocation.status == InvocationStatus::Waiting)
        .count();
    let after = query.cursor.as_deref().and_then(|cursor| {
        let (started, name) = cursor.split_once(':')?;
        Some((started.parse::<i64>().ok()?, name.to_owned()))
    });
    let mut rest = items
        .into_iter()
        .filter(|invocation| after.as_ref().is_none_or(|after| key(invocation) < *after))
        .peekable();
    let mut listed = Vec::new();
    while listed.len() < query.page() {
        let Some(invocation) = rest.next() else {
            break;
        };
        listed.push(invocation);
    }
    let next_cursor = rest
        .peek()
        .is_some()
        .then(|| listed.last().map(cursor_of))
        .flatten();
    Listed {
        items: listed,
        total,
        waiting,
        pending: false,
        next_cursor,
    }
}

/// The workspace's invocations as a dispatch that keeps no record of its own reads them: the calls
/// in `waiting` that wait for their person, whole or `record` alone.
///
/// # Errors
/// A call that does not read as JSON.
pub fn questions_reading(
    waiting: Vec<tilo_actions::remote::asked::Waiting>,
    record: &str,
) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
    let mut items = Vec::new();
    with_questions(&mut items, waiting, (!record.is_empty()).then_some(record));
    if record.is_empty() {
        let waiting = items.len();
        return Ok(tilo_actions::remote::ResourceCopy::reading(
            serde_json::json!({ "items": items, "total": waiting, "waiting": waiting }),
            None,
        ));
    }
    Ok(match items.into_iter().next() {
        Some(invocation) => tilo_actions::remote::ResourceCopy::reading(
            serde_json::to_value(invocation).map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })?,
            None,
        ),
        None => tilo_actions::remote::ResourceCopy::Absent,
    })
}

/// `items`, a reading of a workspace's invocations, with what each call that waits in
/// `waiting` asks its person, and each such call the device keeps no record of (it waited across
/// a restart, or another dispatch here keeps it) listed beside them: only `only`, when the
/// reading names one.
pub fn with_questions(
    items: &mut Vec<Invocation>,
    waiting: Vec<tilo_actions::remote::asked::Waiting>,
    only: Option<&str>,
) {
    use tilo_actions::remote::asked::AskedBy;
    for question in waiting {
        let id = question.invocation.0.to_string();
        if only.is_some_and(|only| only != id) {
            continue;
        }
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.id.as_deref() == Some(id.as_str()))
        {
            item.status = InvocationStatus::Waiting;
            item.person = Some(question.person);
            item.asked = Some(Box::new(question.asked));
            continue;
        }
        let by = match &question.asked.facts.by {
            AskedBy::Agent(agent) => InvocationBy {
                name: agent.name.clone(),
                kind: "agent".to_owned(),
                id: Some(agent.id.clone()),
            },
            AskedBy::App { app, title, .. } => InvocationBy {
                name: title.clone(),
                kind: "app".to_owned(),
                id: Some(app.clone()),
            },
            AskedBy::Device(device) => InvocationBy {
                name: device.name.clone(),
                kind: "device".to_owned(),
                id: Some(device.id.clone()),
            },
            AskedBy::Program { name, app_id } => InvocationBy {
                name: name.clone(),
                kind: "program".to_owned(),
                id: app_id.clone(),
            },
        };
        let row = Invocation {
            id: Some(id.clone()),
            calls: vec![id],
            by,
            status: InvocationStatus::Waiting,
            started_ms: i64::try_from(question.asked.asked_at_ms).unwrap_or(i64::MAX),
            action: Some(question.asked.action.clone()),
            remembered: true,
            person: Some(question.person),
            asked: Some(Box::new(question.asked)),
            ..Invocation::default()
        };
        // Newest first, as the reading is.
        let at = items
            .iter()
            .position(|item| item.started_ms <= row.started_ms)
            .unwrap_or(items.len());
        items.insert(at, row);
    }
}

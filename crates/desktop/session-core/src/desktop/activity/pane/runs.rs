//! Every agent session as the timeline of what it did ([`wire::SESSIONS`]), and what a command it ran
//! printed ([`wire::SESSION_OUTPUT`]).
//!
//! A session is either a recorded marker (an instruction an agent worked across records, with its
//! steps) or a work group the history keeps (every invocation of one agent connection, or every
//! step of one agent's session of a task, which the history groups as `agent-session` and `task`). Both
//! read as the same timeline: each action is one record's share of the session, opening as what that
//! record is. Both undo the same way: by asking each changed document's own history to undo the
//! session it grouped under, so a task and a connection's group are named alike.
//!
//! A command is never copied into the history. An agent asks a shell session to session a command
//! line, which queues a request in the session's record; the shell sessions it and records the session as
//! an item of that same record, under the request's id, with its directory, exit status, times
//! and output. An action that queued a request therefore reads its session from the session's record
//! as the change left it, and a request the shell has not answered is the command running now.

use std::collections::{HashMap, HashSet};

use automerge::ChangeHash;
use serde_json::Value;
use tilo_sort::CompactSort as _;

use crate::desktop::activity::vocabulary as wire;
use tilo_actions::terminal_text;

use super::serve::{author, hex, millis, scope};
use super::{
    ActivityTree, DocumentId, HistoryEntry, HistoryEntryId, IndexedAgentSession, UndoRun,
    WorkspaceActivityPane, humanize_app_id, t,
};

/// The app whose records are web pages.
use ui::apps::BROWSER as BROWSER_APP;
/// The app whose records are shell sessions: a step there ran a program.
pub use ui::apps::TERMINAL as TERMINAL_APP;
/// The field of a shell session's record that queues the command lines asked of it.
const REQUESTS: &str = "requests";
/// The field of a shell session's record that keeps the command lines it recorded.
const RUNS: &str = "runs";
/// The group kind of an agent session's invocations.
const AGENT_SESSION: &str = ui::history_source::steps::AGENT_SESSION;
/// The most bytes of a command's output an action carries.
const COMMAND_TAIL_BYTES: usize = 4 * 1024;
/// The most bytes of a command's output a [`wire::SESSION_OUTPUT`] read answers: as much as a shell
/// keeps of one session.
const SESSION_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// Output tails kept between reads, by content hash: a session's output never changes once recorded.
const MAX_TAILS: usize = 512;

/// One command line an action asked a shell to session, as the session's record holds it.
struct Asked {
    command: String,
    /// The session's id: the request's, unless the shell answered with another.
    execution_id: String,
    /// The session, once the shell recorded it.
    execution: Option<Value>,
    /// The shell answered the request, with a session or without one.
    settled: bool,
}

/// A session's output as its record names it: `blake3:<hex>:<len>`.
fn output_hash(session: &Value) -> Option<String> {
    let reference = session.get("output")?.as_str()?;
    let rest = reference.strip_prefix("blake3:")?;
    let (hash, _) = rest.split_once(':')?;
    Some(hash.to_owned())
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The text argument `key` of the action a change carried out, when it named one.
fn argument(entry: &HistoryEntry, key: &str) -> Option<String> {
    let context = entry.context.as_ref()?;
    let tilo_actions::Did::Verb(call) = context.what.as_ref()? else {
        return None;
    };
    call.args.get(key)?.as_str().map(str::to_owned)
}

fn is_web_address(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

/// The changes of one session in one record, back to back: one action of its timeline.
struct Segment<'a> {
    app: String,
    record: String,
    entries: Vec<&'a HistoryEntry>,
}

/// The session `entry` is a change of, when it is one Activity lists every change of: an agent
/// connection's work group, or an agent's session of a task. Both are named by their group, which is
/// what each document's history undoes, and a session a marker records is listed as that marker
/// rather than again here.
fn run_group(entry: &HistoryEntry) -> Option<&tilo_actions::WorkGroup> {
    entry
        .context
        .as_ref()
        .and_then(|context| context.group.as_ref())
        .filter(|group| group.kind == AGENT_SESSION)
}

impl WorkspaceActivityPane {
    /// Every agent session the history holds in `query`'s scope, newest first, each as the actions it
    /// took. A session none of whose changes the history lists here is left out, as the feed leaves
    /// out what it does not hold.
    pub fn serve_runs(
        &mut self,
        query: &wire::SessionsQuery,
        tree: &impl ActivityTree,
    ) -> wire::Sessions {
        self.set_scope(scope(query.scope), tree);
        self.served_documents.borrow_mut().clear();
        let workspace = tree
            .active_workspace()
            .map(|workspace| workspace.id().to_string());
        let entries = self.timeline.entries();
        let mut covered: HashSet<HistoryEntryId> = HashSet::new();
        let mut sessions = Vec::new();
        for indexed in &self.agent_sessions {
            if let Some(session) = self.marker_session(
                indexed,
                entries,
                workspace.as_deref(),
                query.changes_page(),
                &mut covered,
            ) {
                sessions.push(session);
            }
        }
        sessions.extend(self.group_sessions(
            entries,
            workspace.as_deref(),
            query.changes_page(),
            &covered,
        ));
        sessions.compact_sort_by(|a, b| b.last_ms.cmp(&a.last_ms).then_with(|| a.id.cmp(&b.id)));
        let total = sessions.len();
        sessions.truncate(query.page());
        wire::Sessions {
            sessions,
            total,
            mutations: self.host.mutations_available(),
            ..wire::Sessions::default()
        }
    }

    /// A recorded session: one action per record it changed, with the changes of the history that are
    /// that action, and a command per line it asked a shell to session.
    fn marker_session(
        &self,
        indexed: &IndexedAgentSession,
        entries: &[HistoryEntry],
        workspace: Option<&str>,
        page: usize,
        covered: &mut HashSet<HistoryEntryId>,
    ) -> Option<wire::AgentSession> {
        let mut actions = Vec::new();
        let mut segments = Vec::new();
        let mut first: Option<&HistoryEntry> = None;
        let mut last: Option<&HistoryEntry> = None;
        for step in &indexed.session.steps {
            let mut changes: Vec<&HistoryEntry> = entries
                .iter()
                .filter(|entry| self.agent_step_contains(indexed, step, &entry.id))
                .collect();
            changes.compact_sort_by_key(|entry| entry.timestamp);
            let (Some(oldest), Some(newest)) = (changes.first().copied(), changes.last().copied())
            else {
                continue;
            };
            covered.extend(changes.iter().map(|entry| entry.id));
            if first.is_none_or(|held| oldest.timestamp < held.timestamp) {
                first = Some(oldest);
            }
            if last.is_none_or(|held| newest.timestamp > held.timestamp) {
                last = Some(newest);
            }
            let segment = Segment {
                app: step.app.clone(),
                record: step.record.clone(),
                entries: changes,
            };
            let summary = step.sentence();
            let undo = self.can_undo_agent_record(step).then(|| wire::Offer {
                action: wire::DetailAction::UndoEntry,
                label: t("activity-detail-undo-step"),
            });
            actions.extend(self.segment_actions(&segment, workspace, Some(&summary), undo));
            segments.push(segment);
        }
        let (first, last) = (first?, last?);
        let (in_effect, changes_total) = self.changes_in_effect(&segments, page);
        let session = &indexed.session;
        let undoable = self.host.mutations_available()
            && session.steps.iter().all(|step| self.record_is_open(step));
        // A session is an agent's work whatever its changes' author record says, and it acted for
        // whoever issued it: the person and their agent never read alike.
        let mut agent = author(&first.author);
        agent.name = self.agent_name(session);
        agent.relation = wire::Relation::Software;
        if agent.on_behalf_of.is_none() && session.issued_by.is_some() {
            agent.on_behalf_of = Some(self.issuer_name(session));
        }
        Some(wire::AgentSession {
            id: session.id.clone(),
            agent,
            workspace: workspace_facet(first),
            instruction: session
                .instruction
                .clone()
                .unwrap_or_else(|| session.label.clone()),
            decision: session.decision.clone(),
            started_ms: millis(first.timestamp),
            last_ms: millis(last.timestamp),
            range: wire::Range {
                from: hex(&first.id),
                to: hex(&last.id),
            },
            now: now(&actions),
            actions,
            in_effect,
            changes_total,
            undo_blocked: (!undoable).then(|| t("activity-undo-task-needs-open")),
        })
    }

    /// Every session the history keeps as a work group that no recorded session holds: one agent
    /// connection's work, or one agent's session of a task, by the agent whose work it is and what
    /// it was asked, its actions what it changed in the order it changed it.
    fn group_sessions(
        &self,
        entries: &[HistoryEntry],
        workspace: Option<&str>,
        page: usize,
        covered: &HashSet<HistoryEntryId>,
    ) -> Vec<wire::AgentSession> {
        let mut groups: Vec<(&tilo_actions::WorkGroup, Vec<&HistoryEntry>)> = Vec::new();
        for entry in entries {
            if covered.contains(&entry.id) {
                continue;
            }
            let Some(group) = run_group(entry) else {
                continue;
            };
            match groups.iter_mut().find(|(held, _)| held.id == group.id) {
                Some((_, members)) => members.push(entry),
                None => groups.push((group, vec![entry])),
            }
        }
        let mut sessions = Vec::new();
        for (group, mut members) in groups {
            members.compact_sort_by_key(|entry| entry.timestamp);
            let (Some(first), Some(last)) = (members.first().copied(), members.last().copied())
            else {
                continue;
            };
            let mut segments: Vec<Segment<'_>> = Vec::new();
            for entry in &members {
                let (app, record) = self.origin_of(entry);
                match segments.last_mut() {
                    Some(segment) if segment.app == app && segment.record == record => {
                        segment.entries.push(entry);
                    }
                    _ => segments.push(Segment {
                        app,
                        record,
                        entries: vec![entry],
                    }),
                }
            }
            let mut actions = Vec::new();
            for segment in &segments {
                let undo = self.segment_undo(segment);
                actions.extend(self.segment_actions(segment, workspace, None, undo));
            }
            let (in_effect, changes_total) = self.changes_in_effect(&segments, page);
            let mut agent = author(&first.author);
            agent.relation = wire::Relation::Software;
            let instruction = group
                .label
                .clone()
                .or_else(|| {
                    members.iter().find_map(|entry| {
                        entry
                            .context
                            .as_ref()
                            .and_then(|context| context.purpose.clone())
                            .filter(|purpose| !purpose.trim().is_empty())
                    })
                })
                .or_else(|| actions.first().map(|action| action.summary.clone()))
                .unwrap_or_default();
            sessions.push(wire::AgentSession {
                id: group.id.clone(),
                agent,
                workspace: workspace_facet(first),
                instruction,
                decision: None,
                started_ms: millis(first.timestamp),
                last_ms: millis(last.timestamp),
                range: wire::Range {
                    from: hex(&first.id),
                    to: hex(&last.id),
                },
                now: now(&actions),
                actions,
                in_effect,
                changes_total,
                undo_blocked: self.undo_blocked(&self.run_steps(&members)),
            });
        }
        sessions
    }

    /// The app and record a change is of, as the history's index names its document: a change of
    /// the workspace's own layout is of no record.
    fn origin_of(&self, entry: &HistoryEntry) -> (String, String) {
        match self
            .projected_origins
            .get(&entry.id)
            .map(|projected| &projected.origin)
        {
            Some(super::super::index::HistoryOrigin::AppRecord { app, id, .. }) => {
                (app.clone(), id.clone())
            }
            _ => (
                entry
                    .context
                    .as_ref()
                    .and_then(|context| context.app.as_ref())
                    .map(|app| app.id.clone())
                    .unwrap_or_default(),
                String::new(),
            ),
        }
    }

    /// The offer to take back one segment's record on its own, when its document is open here: it
    /// goes through the host like the whole session, one document of it at a time.
    fn segment_undo(&self, segment: &Segment<'_>) -> Option<wire::Offer> {
        if !Self::step_is_undoable(&segment.app, &segment.record) {
            return None;
        }
        let step = (segment.app.clone(), segment.record.clone());
        self.undo_blocked(std::slice::from_ref(&step))
            .is_none()
            .then(|| wire::Offer {
                action: wire::DetailAction::UndoEntry,
                label: t("activity-detail-undo-step"),
            })
    }

    /// Whether the change `entry` still stands: no document's history has taken it back.
    fn entry_in_effect(&self, entry: &HistoryEntry) -> bool {
        self.projected_origins
            .get(&entry.id)
            .is_none_or(|id| self.history_index.in_effect(id))
    }

    /// The change in its record's own history, which its undo names as the step: the timeline
    /// names a foreign record's changes by a projection of the record's own ids.
    fn change_step(&self, entry: &HistoryEntry) -> String {
        self.projected_origins
            .get(&entry.id)
            .map_or_else(|| entry.id.hex(), |id| id.change.hex())
    }

    /// One change still in effect as its row reads it: what it changed, where, and how to open it
    /// and take it back.
    fn segment_change(&self, segment: &Segment<'_>, entry: &HistoryEntry) -> wire::SessionChange {
        wire::SessionChange {
            range: wire::Range::one(&hex(&entry.id)),
            step: self.change_step(entry),
            at_ms: millis(entry.timestamp),
            summary: entry.summary.clone(),
            app_id: segment.app.clone(),
            record: segment.record.clone(),
            target: self.segment_target(segment, entry),
            undo: self.segment_undo(segment),
        }
    }

    /// A session's changes still in effect, newest first, at most `page` of them, and how many
    /// there are: what its card lists, a page at a time, however long the session ran. Only the
    /// page is built, so a session that ran for thousands of changes reads as a page does.
    fn changes_in_effect(
        &self,
        segments: &[Segment<'_>],
        page: usize,
    ) -> (Vec<wire::SessionChange>, usize) {
        let mut changes: Vec<(&Segment<'_>, &HistoryEntry)> = segments
            .iter()
            // A change of the workspace's own layout is no record's to take back, so no
            // document's history says whether it still stands: the feed lists it, not a session.
            .filter(|segment| !segment.record.is_empty())
            .flat_map(|segment| {
                segment
                    .entries
                    .iter()
                    .filter(|entry| self.entry_in_effect(entry))
                    .map(move |entry| (segment, *entry))
            })
            .collect();
        changes.compact_sort_by_key(|(_, entry)| entry.timestamp);
        let total = changes.len();
        let shown = total.saturating_sub(page);
        let shown = changes
            .drain(shown..)
            .rev()
            .map(|(segment, entry)| self.segment_change(segment, entry))
            .collect();
        (shown, total)
    }

    /// The documents `entries` are changes of, as the steps undoing a session asks each history for:
    /// each distinct app and record, but for a command line the session asked a shell session to session
    /// and a change of the workspace's own layout, which no document takes back.
    fn run_steps(&self, entries: &[&HistoryEntry]) -> Vec<(String, String)> {
        let mut steps: Vec<(String, String)> = Vec::new();
        for entry in entries {
            let (app, record) = self.origin_of(entry);
            let step = (app, record);
            if Self::step_is_undoable(&step.0, &step.1) && !steps.contains(&step) {
                steps.push(step);
            }
        }
        steps
    }

    /// Whether a change of `app`'s `record` is one its own history takes back: a shell session
    /// keeps no history to undo, and the workspace's own layout is no record's.
    fn step_is_undoable(app: &str, record: &str) -> bool {
        app != TERMINAL_APP && !record.is_empty()
    }

    /// The session whose work group the timeline selection names, with the documents its changes in
    /// the selection are: the selection is one session's work.
    pub(super) fn selected_group_session(&self) -> Option<UndoRun> {
        let entries = self.selected_entries();
        let mut id: Option<String> = None;
        for entry in entries {
            let Some(group) = run_group(entry) else {
                continue;
            };
            match &id {
                Some(held) if held != &group.id => return None,
                _ => id = Some(group.id.clone()),
            }
        }
        let id = id?;
        let members = entries
            .iter()
            .filter(|entry| run_group(entry).is_some_and(|group| group.id == id))
            .collect::<Vec<_>>();
        Some(UndoRun {
            steps: self.run_steps(&members),
            // Each document's history knows the connection by its group, which is also how the
            // activity shows the session.
            shown: id.clone(),
            named: id,
        })
    }

    /// The actions one session's changes in one record are: one per command line it asked a shell
    /// session to session, else one that opens the record. `summary` is what the session's marker said of
    /// the step, when it said anything.
    fn segment_actions(
        &self,
        segment: &Segment<'_>,
        workspace: Option<&str>,
        summary: Option<&str>,
        undo: Option<wire::Offer>,
    ) -> Vec<wire::SessionAction> {
        let (Some(oldest), Some(newest)) = (
            segment.entries.first().copied(),
            segment.entries.last().copied(),
        ) else {
            return Vec::new();
        };
        let target = self.segment_target(segment, oldest);
        if segment.app == TERMINAL_APP {
            let commands = self.commands(segment, workspace, &target);
            if !commands.is_empty() {
                return commands;
            }
        }
        let summary = summary.filter(|summary| !summary.is_empty()).map_or_else(
            || self.timeline.summary_of(newest).to_owned(),
            str::to_owned,
        );
        let view = Self::record_view(segment, newest, target);
        vec![wire::SessionAction {
            range: wire::Range {
                from: hex(&oldest.id),
                to: hex(&newest.id),
            },
            changes: segment.entries.len(),
            at_ms: millis(oldest.timestamp),
            summary,
            view,
            app_id: segment.app.clone(),
            record: segment.record.clone(),
            undo,
        }]
    }

    /// The resource a segment's changes touched, where it opens.
    fn segment_target(&self, segment: &Segment<'_>, entry: &HistoryEntry) -> wire::Target {
        let app_label = self
            .host
            .app_display_name(&segment.app)
            .unwrap_or_else(|| humanize_app_id(&segment.app));
        let app_id = Some(segment.app.clone()).filter(|app| !app.is_empty());
        let named = entry
            .context
            .as_ref()
            .and_then(|context| context.resource.as_ref())
            .map(|resource| wire::Target {
                app_id: app_id.clone(),
                app_label: Some(app_label.clone()),
                id: resource.id.clone(),
                kind: resource.kind.clone(),
                label: tilo_ui_base::human_label(&resource.label, &resource.id)
                    .unwrap_or(&resource.id)
                    .to_owned(),
            });
        named.unwrap_or_else(|| {
            let path = DocumentId::parse(&segment.record).and_then(|record| record.file_path());
            wire::Target {
                app_id,
                app_label: Some(app_label),
                id: segment.record.clone(),
                kind: String::new(),
                label: path.as_ref().and_then(|path| path.file_name()).map_or_else(
                    || segment.record.clone(),
                    |name| name.to_string_lossy().into_owned(),
                ),
            }
        })
    }

    /// What a segment that ran no command shows: the file it changed, the page it acted on, or
    /// else the record.
    fn record_view(
        segment: &Segment<'_>,
        newest: &HistoryEntry,
        target: wire::Target,
    ) -> wire::ActionView {
        if let Some(path) = DocumentId::parse(&segment.record).and_then(|record| record.file_path())
        {
            return wire::ActionView::File {
                target,
                path: path.to_string_lossy().into_owned(),
                lines: Vec::new(),
            };
        }
        match segment.app.as_str() {
            TERMINAL_APP => wire::ActionView::Command {
                command: argument(newest, "command").unwrap_or_else(|| target.label.clone()),
                target,
                cwd: String::new(),
                output: Vec::new(),
                more: false,
                exit: None,
                started_ms: None,
                ended_ms: None,
                execution: None,
            },
            BROWSER_APP => {
                // The page the action named, else the page it acted on; its title is the page's
                // label, when that is more than its address.
                let url = argument(newest, "url")
                    .or_else(|| is_web_address(&target.id).then(|| target.id.clone()))
                    .unwrap_or_else(|| target.label.clone());
                let title = if target.label == url || is_web_address(&target.label) {
                    String::new()
                } else {
                    target.label.clone()
                };
                wire::ActionView::Page { target, url, title }
            }
            _ => wire::ActionView::Record { target },
        }
    }

    /// The command lines `segment`'s changes asked its shell session to run, each as an action
    /// with the run the session recorded for it.
    fn commands(
        &self,
        segment: &Segment<'_>,
        workspace: Option<&str>,
        target: &wire::Target,
    ) -> Vec<wire::SessionAction> {
        let wanted: HashSet<[u8; 16]> = segment
            .entries
            .iter()
            .filter_map(|entry| {
                self.projected_origins
                    .get(&entry.id)
                    .map(|projected| *projected.change.as_bytes())
            })
            .collect();
        let asked = self
            .with_record_document(&segment.app, &segment.record, |doc| {
                let changes: HashMap<[u8; 16], automerge::Change> = doc
                    .get_changes(&[])
                    .into_iter()
                    .filter_map(|change| {
                        let mut prefix = [0_u8; 16];
                        prefix.copy_from_slice(change.hash().0.get(..16)?);
                        wanted.contains(&prefix).then_some((prefix, change))
                    })
                    .collect();
                let heads = doc.get_heads();
                let now = tilo_automerge::items::outbox_entries(doc, &heads, REQUESTS);
                segment
                    .entries
                    .iter()
                    .filter_map(|entry| {
                        let change = self
                            .projected_origins
                            .get(&entry.id)
                            .and_then(|projected| changes.get(projected.change.as_bytes()))?;
                        Some((*entry, queued(doc, change, &heads, &now)))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        asked
            .into_iter()
            .flat_map(|(entry, asked)| {
                asked
                    .into_iter()
                    .map(move |asked| self.command_action(segment, entry, target, workspace, asked))
            })
            .collect()
    }

    fn command_action(
        &self,
        segment: &Segment<'_>,
        entry: &HistoryEntry,
        target: &wire::Target,
        workspace: Option<&str>,
        asked: Asked,
    ) -> wire::SessionAction {
        let (output, more) = asked
            .execution
            .as_ref()
            .and_then(output_hash)
            .and_then(|hash| self.output_tail(workspace, &segment.app, &hash))
            .unwrap_or_default();
        let ran = asked.execution.as_ref();
        let number = |key: &str| ran.and_then(|run| run.get(key)).and_then(Value::as_i64);
        let view = wire::ActionView::Command {
            target: target.clone(),
            command: ran
                .map(|run| str_of(run, "command"))
                .filter(|command| !command.is_empty())
                .unwrap_or(&asked.command)
                .to_owned(),
            cwd: ran
                .map(|run| str_of(run, "cwd").to_owned())
                .unwrap_or_default(),
            output,
            more: more || ran.is_some_and(|run| run.get("truncated") == Some(&Value::Bool(true))),
            exit: number("exit").and_then(|exit| i32::try_from(exit).ok()),
            started_ms: number("started_ms"),
            ended_ms: number("ended_ms"),
            execution: (ran.is_some() || !asked.settled).then(|| wire::CommandExecution {
                workspace: workspace.map(str::to_owned),
                app_id: segment.app.clone(),
                record: segment.record.clone(),
                execution: asked.execution_id.clone(),
            }),
        };
        // A request the shell answered without a run never ran: it has no time of its own.
        let at_ms = number("started_ms").unwrap_or_else(|| millis(entry.timestamp));
        wire::SessionAction {
            range: wire::Range::one(&hex(&entry.id)),
            changes: 1,
            at_ms,
            summary: asked.command,
            view,
            app_id: segment.app.clone(),
            record: segment.record.clone(),
            undo: None,
        }
    }

    /// `read` over the document the host holds of `app`'s record `record`, as it stands. One read
    /// of the runs copies each record it reads once.
    fn with_record_document<R>(
        &self,
        app: &str,
        record: &str,
        read: impl FnOnce(&automerge::Automerge) -> R,
    ) -> Option<R> {
        let key = format!("{app}\n{record}");
        let mut copies = self.served_documents.borrow_mut();
        if !copies.contains_key(&key) {
            let held = self
                .detached_records
                .iter()
                .find(|held| held.app == app && held.id == record)
                .and_then(|held| held.document.clone())?;
            copies.insert(key.clone(), held);
        }
        copies.get_mut(&key).map(|doc| read(doc.document()))
    }

    /// The last lines a run printed, from the content this device keeps of `app`'s records.
    fn output_tail(
        &self,
        workspace: Option<&str>,
        app: &str,
        hash: &str,
    ) -> Option<(Vec<String>, bool)> {
        if let Some(kept) = self.command_tails.borrow().get(hash) {
            return Some(kept.clone());
        }
        let bytes = self.host.document_content(workspace?, app, hash)?;
        let tail = terminal_text::tail(&bytes, wire::COMMAND_TAIL_LINES, COMMAND_TAIL_BYTES);
        let mut tails = self.command_tails.borrow_mut();
        if tails.len() >= MAX_TAILS {
            tails.clear();
        }
        tails.insert(hash.to_owned(), tail.clone());
        Some(tail)
    }

    /// Everything the run `query` names printed, as far as this device keeps it.
    pub fn serve_run_output(
        &mut self,
        query: &wire::CommandExecution,
        tree: &impl ActivityTree,
    ) -> wire::SessionOutput {
        let unavailable = wire::SessionOutput {
            unavailable: true,
            ..wire::SessionOutput::default()
        };
        self.served_documents.borrow_mut().clear();
        let Some((run, running)) = self.with_record_document(&query.app_id, &query.record, |doc| {
            let heads = doc.get_heads();
            let run =
                tilo_automerge::items::collection_item_json(doc, &heads, RUNS, &query.execution)
                    .filter(|run| run.get("command").is_some());
            // A request the shell has not answered yet is a run still going.
            let running = tilo_automerge::items::outbox_entries(doc, &heads, REQUESTS)
                .into_iter()
                .any(|row| row.id == query.execution && row.settled.is_none());
            (run, running)
        }) else {
            return unavailable;
        };
        let Some(run) = run else {
            return wire::SessionOutput {
                running,
                unavailable: !running,
                ..wire::SessionOutput::default()
            };
        };
        let workspace = query.workspace.clone().or_else(|| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        });
        let Some(bytes) = output_hash(&run).map_or(Some(Vec::new()), |hash| {
            self.host
                .document_content(workspace.as_deref()?, &query.app_id, &hash)
        }) else {
            return unavailable;
        };
        let (lines, cut) =
            terminal_text::tail(&bytes, wire::SESSION_OUTPUT_LINES, SESSION_OUTPUT_BYTES);
        wire::SessionOutput {
            lines,
            truncated: cut || run.get("truncated") == Some(&Value::Bool(true)),
            unavailable: false,
            running: false,
        }
    }
}

/// The command lines the change `change` queued in a shell session's record, each with what the
/// session holds of it now (`now`, its requests at `heads`): those in its queue after the change
/// and not before it.
fn queued(
    doc: &automerge::Automerge,
    change: &automerge::Change,
    heads: &[ChangeHash],
    now: &[tilo_automerge::items::OutboxRow],
) -> Vec<Asked> {
    let before: HashSet<String> =
        tilo_automerge::items::outbox_entries(doc, change.deps(), REQUESTS)
            .into_iter()
            .map(|row| row.id)
            .collect();
    tilo_automerge::items::outbox_entries(doc, &[change.hash()], REQUESTS)
        .into_iter()
        .filter(|row| !before.contains(&row.id))
        .map(|row| {
            let held = now.iter().find(|held| held.id == row.id);
            let settled = held.is_some_and(|held| held.settled.is_some());
            // The run takes the request's id, unless the shell answered with another.
            let answered = held
                .and_then(|held| held.settled.as_ref())
                .and_then(|settled| settled.pointer("/result/run"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let run_id = answered.unwrap_or_else(|| row.id.clone());
            let run = tilo_automerge::items::collection_item_json(doc, heads, RUNS, &run_id)
                .filter(|run| run.get("command").is_some());
            Asked {
                command: str_of(&row.value, "command").to_owned(),
                execution_id: run_id,
                execution: run,
                settled,
            }
        })
        .collect()
}

/// The workspace a change says it was made in.
fn workspace_facet(entry: &HistoryEntry) -> Option<String> {
    entry.context.as_ref().and_then(|context| {
        context
            .facets
            .iter()
            .find(|facet| facet.key == "workspace")
            .map(|facet| facet.value.clone())
    })
}

/// What a run whose actions are `actions` is doing: a command still running, else its newest
/// action.
fn now(actions: &[wire::SessionAction]) -> Option<wire::Now> {
    if let Some(running) = actions.iter().rev().find(|action| action.view.is_running()) {
        let wire::ActionView::Command { command, .. } = &running.view else {
            return None;
        };
        return Some(wire::Now::Running {
            command: command.clone(),
            since_ms: running.at_ms,
        });
    }
    let last = actions.iter().max_by_key(|action| action.at_ms)?;
    let at_ms = last.at_ms;
    Some(match &last.view {
        wire::ActionView::File { path, .. } => wire::Now::Editing {
            path: path.clone(),
            at_ms,
        },
        wire::ActionView::Command { command, .. } => wire::Now::Ran {
            command: command.clone(),
            at_ms,
        },
        wire::ActionView::Page { url, title, .. } => wire::Now::Browsing {
            url: url.clone(),
            title: title.clone(),
            at_ms,
        },
        wire::ActionView::Record { .. } => wire::Now::Changing {
            summary: last.summary.clone(),
            at_ms,
        },
    })
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod runs_tests;

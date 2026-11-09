//! Every agent run as the timeline of what it did ([`wire::RUNS`]), and what a command it ran
//! printed ([`wire::RUN_OUTPUT`]).
//!
//! A run is either a recorded marker (an instruction an agent worked across records, with its
//! steps) or an agent connection's work group (every invocation of one MCP session, which the
//! history groups as `agent-run`). Both read as the same timeline: each action is one record's
//! share of the run, opening as what that record is.
//!
//! A command is never copied into the history. An agent asks a shell session to run a command
//! line, which queues a request in the session's record; the shell runs it and records the run as
//! an item of that same record, under the request's id, with its directory, exit status, times
//! and output. An action that queued a request therefore reads its run from the session's record
//! as the change left it, and a request the shell has not answered is the command running now.

use std::collections::{HashMap, HashSet};

use automerge::ChangeHash;
use serde_json::Value;
use tilo_sort::CompactSort as _;

use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::terminal_text;

use super::serve::{author, hex, millis, scope};
use super::{
    ActivityTree, HistoryEntry, HistoryEntryId, IndexedAgentRun, RecordId, WorkspaceActivityPane,
    humanize_app_id, t,
};

/// The app whose records are shell sessions: a step there ran a program.
pub(super) const TERMINAL_APP: &str = "so.tilo.terminal";
/// The app whose records are web pages.
const BROWSER_APP: &str = "so.tilo.browser";
/// The field of a shell session's record that queues the command lines asked of it.
const REQUESTS: &str = "requests";
/// The field of a shell session's record that keeps the runs it recorded.
const RUNS: &str = "runs";
/// The group kind of an agent connection's invocations.
const AGENT_RUN: &str = ui::history_source::steps::AGENT_RUN;
/// The most bytes of a command's output an action carries.
const COMMAND_TAIL_BYTES: usize = 4 * 1024;
/// The most bytes of a command's output a [`wire::RUN_OUTPUT`] read answers: as much as a shell
/// keeps of one run.
const RUN_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// Output tails kept between reads, by content hash: a run's output never changes once recorded.
const MAX_TAILS: usize = 512;

/// One command line an action asked a shell to run, as the session's record holds it.
struct Asked {
    command: String,
    /// The run's id: the request's, unless the shell answered with another.
    run_id: String,
    /// The run, once the shell recorded it.
    run: Option<Value>,
    /// The shell answered the request, with a run or without one.
    settled: bool,
}

/// A run's output as its record names it: `blake3:<hex>:<len>`.
fn output_hash(run: &Value) -> Option<String> {
    let reference = run.get("output")?.as_str()?;
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

/// The changes of one run in one record, back to back: one action of its timeline.
struct Segment<'a> {
    app: String,
    record: String,
    entries: Vec<&'a HistoryEntry>,
}

impl WorkspaceActivityPane {
    /// Every agent run the history holds in `query`'s scope, newest first, each as the actions it
    /// took. A run none of whose changes the history lists here is left out, as the feed leaves
    /// out what it does not hold.
    pub fn serve_runs(&mut self, query: &wire::RunsQuery, tree: &impl ActivityTree) -> wire::Runs {
        self.set_scope(scope(query.scope), tree);
        self.served_documents.borrow_mut().clear();
        let workspace = tree
            .active_workspace()
            .map(|workspace| workspace.id().to_string());
        let entries = self.timeline.entries();
        let mut covered: HashSet<HistoryEntryId> = HashSet::new();
        let mut runs = Vec::new();
        for indexed in &self.agent_runs {
            if let Some(run) = self.marker_run(indexed, entries, workspace.as_deref(), &mut covered)
            {
                runs.push(run);
            }
        }
        runs.extend(self.group_runs(entries, workspace.as_deref(), &covered));
        runs.compact_sort_by(|a, b| b.last_ms.cmp(&a.last_ms).then_with(|| a.id.cmp(&b.id)));
        let total = runs.len();
        runs.truncate(query.page());
        wire::Runs {
            runs,
            total,
            pending: self.feed_coverage.pending_tree,
            mutations: self.host.mutations_available(),
        }
    }

    /// A recorded run: one action per record it changed, with the changes of the history that are
    /// that action, and a command per line it asked a shell to run.
    fn marker_run(
        &self,
        indexed: &IndexedAgentRun,
        entries: &[HistoryEntry],
        workspace: Option<&str>,
        covered: &mut HashSet<HistoryEntryId>,
    ) -> Option<wire::AgentRun> {
        let mut actions = Vec::new();
        let mut first: Option<&HistoryEntry> = None;
        let mut last: Option<&HistoryEntry> = None;
        for step in &indexed.run.steps {
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
                action: wire::DetailAction::UndoRecord,
                label: t("activity-detail-undo-step"),
            });
            actions.extend(self.segment_actions(&segment, workspace, Some(&summary), undo));
        }
        let (first, last) = (first?, last?);
        let run = &indexed.run;
        let undoable = self.host.mutations_available()
            && run.task.is_some()
            && run.steps.iter().all(|step| self.record_is_open(step));
        // A run is an agent's work whatever its changes' author record says, and it acted for
        // whoever issued it: the person and their agent never read alike.
        let mut agent = author(&first.author);
        agent.name = self.agent_name(run);
        agent.relation = wire::Relation::Software;
        if agent.on_behalf_of.is_none() && run.issued_by.is_some() {
            agent.on_behalf_of = Some(self.issuer_name(run));
        }
        Some(wire::AgentRun {
            id: run.id.clone(),
            agent,
            workspace: workspace_facet(first),
            instruction: run.instruction.clone().unwrap_or_else(|| run.label.clone()),
            decision: run.decision.clone(),
            started_ms: millis(first.timestamp),
            last_ms: millis(last.timestamp),
            range: wire::Range {
                from: hex(&first.id),
                to: hex(&last.id),
            },
            now: now(&actions),
            actions,
            undo_blocked: (!undoable).then(|| t("activity-undo-task-needs-open")),
        })
    }

    /// Every agent connection's work no recorded run holds: each MCP session is one run, named as
    /// its connection named it ("Claude Code #2"), its actions the records it changed in the
    /// order it changed them.
    fn group_runs(
        &self,
        entries: &[HistoryEntry],
        workspace: Option<&str>,
        covered: &HashSet<HistoryEntryId>,
    ) -> Vec<wire::AgentRun> {
        let mut groups: Vec<(&tilo_actions::WorkGroup, Vec<&HistoryEntry>)> = Vec::new();
        for entry in entries {
            if covered.contains(&entry.id) {
                continue;
            }
            let Some(group) = entry
                .context
                .as_ref()
                .and_then(|context| context.group.as_ref())
                .filter(|group| group.kind == AGENT_RUN)
            else {
                continue;
            };
            match groups.iter_mut().find(|(held, _)| held.id == group.id) {
                Some((_, members)) => members.push(entry),
                None => groups.push((group, vec![entry])),
            }
        }
        let mut runs = Vec::new();
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
                let undo = self.segment_undoable(segment).then(|| wire::Offer {
                    action: wire::DetailAction::Undo,
                    label: t("activity-detail-undo-step"),
                });
                actions.extend(self.segment_actions(segment, workspace, None, undo));
            }
            let mut agent = author(&first.author);
            agent.name.clone_from(&group.label);
            agent.relation = wire::Relation::Software;
            let instruction = members
                .iter()
                .find_map(|entry| {
                    entry
                        .context
                        .as_ref()
                        .and_then(|context| context.purpose.clone())
                        .filter(|purpose| !purpose.trim().is_empty())
                })
                .or_else(|| actions.first().map(|action| action.summary.clone()))
                .unwrap_or_else(|| group.label.clone());
            runs.push(wire::AgentRun {
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
                undo_blocked: Some(t("activity-undo-run-by-action")),
            });
        }
        runs
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
                (app.clone(), id.as_str().to_owned())
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

    /// Whether every change of `segment` can be taken back here as one band.
    fn segment_undoable(&self, segment: &Segment<'_>) -> bool {
        self.host.mutations_available()
            && segment.app != TERMINAL_APP
            && segment.entries.iter().all(|entry| {
                entry.is_revertible()
                    && self
                        .projected_origins
                        .get(&entry.id)
                        .is_some_and(|projected| self.host.writable(&projected.origin))
            })
    }

    /// The actions one run's changes in one record are: one per command line it asked a shell
    /// session to run, else one that opens the record. `summary` is what the run's marker said of
    /// the step, when it said anything.
    fn segment_actions(
        &self,
        segment: &Segment<'_>,
        workspace: Option<&str>,
        summary: Option<&str>,
        undo: Option<wire::Offer>,
    ) -> Vec<wire::RunAction> {
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
        vec![wire::RunAction {
            range: wire::Range {
                from: hex(&oldest.id),
                to: hex(&newest.id),
            },
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
                label: ui::human_label(&resource.label, &resource.id)
                    .unwrap_or(&resource.id)
                    .to_owned(),
            });
        named.unwrap_or_else(|| {
            let path = RecordId::parse(&segment.record).and_then(|record| record.file_path());
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
        if let Some(path) = RecordId::parse(&segment.record).and_then(|record| record.file_path()) {
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
                run: None,
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
    ) -> Vec<wire::RunAction> {
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
    ) -> wire::RunAction {
        let (output, more) = asked
            .run
            .as_ref()
            .and_then(output_hash)
            .and_then(|hash| self.output_tail(workspace, &segment.app, &hash))
            .unwrap_or_default();
        let ran = asked.run.as_ref();
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
            run: (ran.is_some() || !asked.settled).then(|| wire::CommandRun {
                workspace: workspace.map(str::to_owned),
                app_id: segment.app.clone(),
                record: segment.record.clone(),
                run: asked.run_id.clone(),
            }),
        };
        // A request the shell answered without a run never ran: it has no time of its own.
        let at_ms = number("started_ms").unwrap_or_else(|| millis(entry.timestamp));
        wire::RunAction {
            range: wire::Range::one(&hex(&entry.id)),
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
                .find(|held| held.app == app && held.checkout.id.as_str() == record)
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
        let bytes = self.host.record_content(workspace?, app, hash)?;
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
        query: &wire::CommandRun,
        tree: &impl ActivityTree,
    ) -> wire::RunOutput {
        let unavailable = wire::RunOutput {
            unavailable: true,
            ..wire::RunOutput::default()
        };
        self.served_documents.borrow_mut().clear();
        let Some((run, running)) = self.with_record_document(&query.app_id, &query.record, |doc| {
            let heads = doc.get_heads();
            let run = tilo_automerge::items::collection_item_json(doc, &heads, RUNS, &query.run)
                .filter(|run| run.get("command").is_some());
            // A request the shell has not answered yet is a run still going.
            let running = tilo_automerge::items::outbox_entries(doc, &heads, REQUESTS)
                .into_iter()
                .any(|row| row.id == query.run && row.settled.is_none());
            (run, running)
        }) else {
            return unavailable;
        };
        let Some(run) = run else {
            return wire::RunOutput {
                running,
                unavailable: !running,
                ..wire::RunOutput::default()
            };
        };
        let workspace = query.workspace.clone().or_else(|| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        });
        let Some(bytes) = output_hash(&run).map_or(Some(Vec::new()), |hash| {
            self.host
                .record_content(workspace.as_deref()?, &query.app_id, &hash)
        }) else {
            return unavailable;
        };
        let (lines, cut) = terminal_text::tail(&bytes, wire::RUN_OUTPUT_LINES, RUN_OUTPUT_BYTES);
        wire::RunOutput {
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
                run_id,
                run,
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
fn now(actions: &[wire::RunAction]) -> Option<wire::Now> {
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

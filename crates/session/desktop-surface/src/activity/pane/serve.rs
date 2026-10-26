//! The pane's history as the session serves it to apps
//! ([`tilo_actions::remote::workspace_activity`]): the reads an app follows
//! and the writes it asks for, each addressed by history id rather than by
//! a row on a screen.

use std::hash::{Hash as _, Hasher as _};

use tilo_actions::remote::workspace_activity as wire;

use super::{
    ActivityDeepLink, ActivityTree, ActivityView, BoundaryDirection, DetailAction,
    GroupNameRequest, HistoryEntry, HistoryEntryId, LensSelection, Period, RecordId, RecordStore,
    RoomActivityOutcome, RoomActivityPane, RoomScope, SystemTime, WorkSessionBoundary,
    WorkspaceHistoryId, comparison_change_labels, comparison_field_changes, detail_provenance,
    handled_outcome, humanize_app_id, local_resume_snapshot, t, t_args,
};

fn millis(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

fn author(author: &ui::widget::history::HistoryAuthor) -> wire::Author {
    let (r, g, b) = author.rgb();
    wire::Author {
        name: author.name.clone(),
        rgb: [r, g, b],
        identity: author.identity_short.clone(),
        relation: match author.relation {
            ui::widget::history::AuthorRelation::Reader => wire::Relation::Reader,
            ui::widget::history::AuthorRelation::Person => wire::Relation::Person,
            ui::widget::history::AuthorRelation::Software => wire::Relation::Software,
        },
        on_behalf_of: author
            .attribution
            .as_ref()
            .and_then(|attribution| attribution.on_behalf_of_name.clone()),
    }
}

const fn kind(kind: ui::widget::EntryKind) -> wire::Kind {
    match kind {
        ui::widget::EntryKind::Text => wire::Kind::Text,
        ui::widget::EntryKind::Move => wire::Kind::Move,
        ui::widget::EntryKind::Details => wire::Kind::Details,
        ui::widget::EntryKind::Comment => wire::Kind::Comment,
        ui::widget::EntryKind::Delete => wire::Kind::Delete,
        ui::widget::EntryKind::Restore => wire::Kind::Restore,
        ui::widget::EntryKind::Access => wire::Kind::Access,
        ui::widget::EntryKind::Compaction => wire::Kind::Compaction,
        ui::widget::EntryKind::Redaction => wire::Kind::Redaction,
    }
}

fn target(target: &ui::HistoryResourceTarget) -> wire::Target {
    wire::Target {
        app_id: target.app_id.clone(),
        app_label: target.app_label.clone(),
        id: target.id.clone(),
        kind: target.kind.clone(),
        label: target.label.clone(),
    }
}

fn history_target(target: &wire::Target) -> ui::HistoryResourceTarget {
    ui::HistoryResourceTarget {
        app_id: target.app_id.clone(),
        app_label: target.app_label.clone(),
        id: target.id.clone(),
        kind: target.kind.clone(),
        label: target.label.clone(),
    }
}

const fn scope(scope: wire::Scope) -> RoomScope {
    match scope {
        wire::Scope::ThisWorkspace => RoomScope::ThisRoom,
        wire::Scope::AllWorkspaces => RoomScope::AllWorkspaces,
    }
}

const fn wire_scope(scope: RoomScope) -> wire::Scope {
    match scope {
        RoomScope::ThisRoom => wire::Scope::ThisWorkspace,
        RoomScope::AllWorkspaces => wire::Scope::AllWorkspaces,
    }
}

const fn period(period: wire::Period) -> Period {
    match period {
        wire::Period::AllTime => Period::AllTime,
        wire::Period::Today => Period::Today,
        wire::Period::Week => Period::Week,
        wire::Period::Month => Period::Month,
    }
}

/// What [`wire::FeedQuery::lens`] names `lens` by.
fn lens_key(lens: &LensSelection) -> String {
    match lens {
        LensSelection::All => "all".into(),
        LensSelection::AgentTasks => "agent-tasks".into(),
        LensSelection::App(app) => format!("app:{app}"),
        LensSelection::ResourceKind(kind) => format!("kind:{kind}"),
        LensSelection::Resource(resource) => format!("resource:{resource}"),
        LensSelection::WorkGroup(group) => format!("group:{group}"),
        LensSelection::Facet(key, value) => format!("facet:{key}={value}"),
        LensSelection::Unclassified => "unclassified".into(),
    }
}

/// The lens [`lens_key`] names `key` by.
fn lens_of(key: Option<&str>) -> LensSelection {
    let Some(key) = key else {
        return LensSelection::All;
    };
    match key {
        "agent-tasks" => return LensSelection::AgentTasks,
        "unclassified" => return LensSelection::Unclassified,
        _ => {}
    }
    let Some((kind, value)) = key.split_once(':') else {
        return LensSelection::All;
    };
    let value = value.to_owned();
    match kind {
        "app" => LensSelection::App(value),
        "kind" => LensSelection::ResourceKind(value),
        "resource" => LensSelection::Resource(value),
        "group" => LensSelection::WorkGroup(value),
        "facet" => value
            .split_once('=')
            .map_or(LensSelection::All, |(key, value)| {
                LensSelection::Facet(key.to_owned(), value.to_owned())
            }),
        _ => LensSelection::All,
    }
}

fn entry_id(hex: &str) -> Option<HistoryEntryId> {
    wire::entry_id(hex).map(HistoryEntryId::from_bytes)
}

fn hex(id: &HistoryEntryId) -> String {
    wire::entry_hex(id.as_bytes())
}

/// The arguments an action carries, read as `T`.
fn read<T: serde::de::DeserializeOwned>(
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::Object(arguments.clone()))
        .map_err(|error| format!("the action's arguments were not understood: {error}"))
}

/// The desktop a served read or write is about, as its host holds it: read
/// through, or written to only by what writes.
pub trait ServedTree {
    type Tree: ActivityTree;
    fn read<R>(&mut self, read: impl FnOnce(&Self::Tree) -> R) -> R;
    fn write<R>(&mut self, write: impl FnOnce(&mut Self::Tree) -> R) -> R;
}

/// A desktop the host holds outright.
impl<T: ActivityTree> ServedTree for &mut T {
    type Tree = T;
    fn read<R>(&mut self, read: impl FnOnce(&T) -> R) -> R {
        read(self)
    }
    fn write<R>(&mut self, write: impl FnOnce(&mut T) -> R) -> R {
        write(self)
    }
}

/// Whether `action` is one only the person at the device asks for: what
/// they saw, which sources they hear about each morning.
#[must_use]
pub fn is_personal(action: &str) -> bool {
    [
        wire::MARK_VIEWED,
        wire::FOCUSED,
        wire::CHOOSE_SOURCE,
        wire::SOURCE,
    ]
    .contains(&action)
}

/// Whether `action` is a read an app follows.
#[must_use]
pub fn is_read(action: &str) -> bool {
    [
        wire::FEED,
        wire::SUMMARY,
        wire::REPLAY,
        wire::DETAIL,
        wire::FOCUS,
    ]
    .contains(&action)
}

/// Whether `action` changes the workspace's shared history: naming,
/// splitting, merging, undoing, restoring.
#[must_use]
pub fn is_document_write(action: &str) -> bool {
    [
        wire::NAME,
        wire::SPLIT,
        wire::MERGE,
        wire::MOVE_BOUNDARY,
        wire::UNDO,
        wire::RESTORE,
        wire::UNDO_RUN,
        wire::UNDO_RECORD,
    ]
    .contains(&action)
}

/// Why an agent may not ask for `action` with `arguments`, if it may not:
/// what this device has seen and the sources it hears about are the
/// person's own, and so is every workspace's history but the one the agent
/// works in.
#[must_use]
pub fn refused_to_agent(
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let all_workspaces = action == wire::FEED
        && read::<wire::FeedQuery>(arguments)
            .is_ok_and(|query| query.scope == wire::Scope::AllWorkspaces);
    (is_personal(action) || all_workspaces).then(|| t("activity-served-personal-only"))
}

impl RoomActivityPane {
    /// A fingerprint of everything the served reads show: it moves when
    /// one of them would read differently, and an app following them is
    /// read again only then.
    pub fn served_revision(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for entry in self.timeline.entries() {
            entry.id.hash(&mut hasher);
            entry.summary.hash(&mut hasher);
        }
        for range in &self.named {
            range.id.hash(&mut hasher);
            range.label.hash(&mut hasher);
            range.start.hash(&mut hasher);
            range.end.hash(&mut hasher);
        }
        for brief in &self.digest.briefs {
            brief.entry_ids.hash(&mut hasher);
            brief.title.hash(&mut hasher);
        }
        self.narrative
            .as_ref()
            .map(|narrative| &narrative.text)
            .hash(&mut hasher);
        self.replay_frames.len().hash(&mut hasher);
        self.replay_frames
            .last()
            .map(|frame| frame.entry_id)
            .hash(&mut hasher);
        self.replay_unavailable.hash(&mut hasher);
        self.agent_runs.len().hash(&mut hasher);
        self.agent_undo_notice.hash(&mut hasher);
        self.unreverted.hash(&mut hasher);
        self.undo_pending.is_some().hash(&mut hasher);
        self.comparison
            .as_ref()
            .map(|comparison| comparison.patches.len())
            .hash(&mut hasher);
        self.morning_command_message.hash(&mut hasher);
        self.chosen_morning_sources().hash(&mut hasher);
        self.feed_coverage.pending_tree.hash(&mut hasher);
        self.room_name.hash(&mut hasher);
        self.scope.hash(&mut hasher);
        self.focus
            .as_ref()
            .map(|focus| focus.serial)
            .hash(&mut hasher);
        hasher.finish()
    }

    /// Read `action` with `arguments`, as an app following it gets it.
    ///
    /// # Errors
    /// The action is no read, or its arguments were not understood.
    pub fn serve_read(
        &mut self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        tree: &mut impl ServedTree,
        records: Option<&RecordStore>,
    ) -> Result<serde_json::Value, String> {
        let value = match action {
            wire::FEED => {
                let query: wire::FeedQuery = read(arguments)?;
                serde_json::to_value(tree.read(|tree| self.serve_feed(&query, tree)))
            }
            wire::SUMMARY => serde_json::to_value(self.serve_summary()),
            wire::REPLAY => serde_json::to_value(self.serve_replay()),
            wire::FOCUS => serde_json::to_value(self.serve_focus()),
            wire::DETAIL => {
                let query: wire::DetailQuery = read(arguments)?;
                serde_json::to_value(tree.write(|tree| self.serve_detail(&query, tree, records)))
            }
            other => return Err(format!("`{other}` is not a read")),
        };
        value.map_err(|error| error.to_string())
    }

    /// Narrow the timeline as `query` says, then read it.
    pub fn serve_feed(&mut self, query: &wire::FeedQuery, tree: &impl ActivityTree) -> wire::Feed {
        self.set_scope(scope(query.scope), tree);
        let lens = lens_of(query.lens.as_deref());
        let narrowed =
            self.period != period(query.period) || self.author != query.author || self.lens != lens;
        if narrowed {
            self.period = period(query.period);
            self.author.clone_from(&query.author);
            self.lens = lens;
            self.reinstall_filter();
        }
        let groups_by_entry = self.groups_by_entry();
        let entries = self.timeline.entries();
        wire::Feed {
            workspace: self.room_name.clone(),
            scope: wire_scope(self.scope),
            total: entries.len(),
            entries: entries
                .iter()
                .enumerate()
                .rev()
                .take(query.page())
                .map(|(index, entry)| self.wire_entry(entry, groups_by_entry.get(index)))
                .collect(),
            groups: self
                .named
                .iter()
                .map(|range| wire::Group {
                    id: range.id.clone(),
                    label: range.label.clone(),
                    start: hex(&range.start),
                    end: range.end.as_ref().map(hex),
                    named_by: range.named_by.as_ref().map(|author| author.name.clone()),
                })
                .collect(),
            people: self.contributors.clone(),
            lenses: self
                .lens_candidates()
                .into_iter()
                .filter(|(lens, _)| *lens != LensSelection::All)
                .map(|(lens, label)| wire::Lens {
                    key: lens_key(&lens),
                    label,
                })
                .collect(),
            pending: self.feed_coverage.pending_tree,
            mutations: self.host.mutations_available(),
        }
    }

    /// The named group each timeline entry sits in, by index.
    fn groups_by_entry(&self) -> Vec<Option<String>> {
        let mut groups = vec![None; self.timeline.entries().len()];
        for (range, start, end) in self.named_positions() {
            for slot in groups.iter_mut().take(end.saturating_add(1)).skip(start) {
                *slot = Some(range.id.clone());
            }
        }
        groups
    }

    fn wire_entry(&self, entry: &HistoryEntry, group: Option<&Option<String>>) -> wire::Entry {
        let context = entry.context.as_ref();
        wire::Entry {
            id: hex(&entry.id),
            author: author(&entry.author),
            kind: kind(entry.entry_kind),
            at_ms: millis(entry.timestamp),
            summary: self.timeline.summary_of(entry).to_owned(),
            workspace: context.and_then(|context| {
                context
                    .facets
                    .iter()
                    .find(|facet| facet.key == "workspace")
                    .map(|facet| facet.value.clone())
            }),
            app: context
                .and_then(|context| context.app.as_ref())
                .map(|app| app.label.clone())
                .filter(|label| !label.is_empty()),
            resource: context
                .and_then(|context| context.resource.as_ref())
                .and_then(|resource| ui::human_label(&resource.label, &resource.id))
                .map(str::to_owned),
            group: group.cloned().flatten(),
            redacted: entry.redaction.is_some(),
        }
    }

    /// What changed since this device last looked.
    pub fn serve_summary(&self) -> wire::Summary {
        let sources = self
            .chosen_morning_sources()
            .iter()
            .map(|source| {
                let key = source.key();
                let evidence = self.morning_evidence(&key);
                wire::Source {
                    label: self.morning_display_label(&key, evidence.as_ref()),
                    text: self.morning_payload_text(&key),
                    refreshable: self
                        .host
                        .morning_payload(&self.room, source)
                        .is_some_and(|payload| payload.refreshable),
                    status: self.host.morning_command_status(&self.room, source),
                    key,
                }
            })
            .collect();
        wire::Summary {
            workspace: self.room_name.clone(),
            briefs: self
                .digest
                .briefs
                .iter()
                .map(|brief| wire::Brief {
                    title: brief.title.clone(),
                    detail: brief.detail.clone(),
                    authors: brief.authors.iter().map(author).collect(),
                    apps: brief.apps.clone(),
                    resources: brief.resources.clone(),
                    changes: brief.changes,
                    actions: brief
                        .actions
                        .iter()
                        .map(|action| wire::BriefAction {
                            author: author(&action.author),
                            summary: action.summary.clone(),
                            at_ms: millis(action.timestamp),
                        })
                        .collect(),
                    first_ms: millis(brief.first_at),
                    last_ms: millis(brief.last_at),
                    group: brief.group_id.clone(),
                    work_session: brief.work_session_id.clone(),
                    entries: brief.entry_ids.iter().map(hex).collect(),
                })
                .collect(),
            narrative: self
                .narrative
                .as_ref()
                .map(|narrative| narrative.text.clone()),
            resume: local_resume_snapshot(self.timeline.entries(), &self.local_user()).map(
                |resume| wire::Resume {
                    target: target(&resume.target),
                    author: author(&resume.author),
                    app: resume.app,
                    at_ms: millis(resume.timestamp),
                },
            ),
            away: self.last_active.is_some()
                && self.last_active == self.presence_workspace
                && self.away_digest.total_changes > 0
                && self.digest == self.away_digest,
            sources_available: self.host.morning_sources_available()
                && self.scope == RoomScope::ThisRoom,
            sources,
            pending: self.feed_coverage.pending_tree,
        }
    }

    /// The room's layout at each recent change, from the first the
    /// workspace stood in: the changes that made the room before it show
    /// nothing to replay.
    pub fn serve_replay(&self) -> wire::Replay {
        wire::Replay {
            frames: self
                .replay_frames
                .iter()
                .skip_while(|frame| frame.workspaces.is_empty())
                .map(|frame| wire::Frame {
                    entry: hex(&frame.entry_id),
                    author: author(&frame.author),
                    summary: frame.summary.clone(),
                    at_ms: millis(frame.timestamp),
                    workspaces: frame
                        .workspaces
                        .iter()
                        .map(|workspace| wire::ReplayWorkspace {
                            name: workspace.name.clone(),
                            groups: workspace
                                .groups
                                .iter()
                                .map(|group| wire::ReplayGroup {
                                    tabs: group.tabs.clone(),
                                    active: group.active,
                                    width_permille: group.width_permille,
                                })
                                .collect(),
                        })
                        .collect(),
                    pointers: frame
                        .pointers
                        .iter()
                        .map(|pointer| wire::ReplayPointer {
                            name: pointer.name.clone(),
                            rgb: [pointer.color.0, pointer.color.1, pointer.color.2],
                        })
                        .collect(),
                })
                .collect(),
            unavailable: self.replay_unavailable,
            note: self.replay_presentation_note.clone(),
            pending: self.feed_coverage.pending_tree,
        }
    }

    /// Select the run `range` names in the timeline as it is narrowed now;
    /// whether both ends are in it.
    pub fn select_range(&mut self, range: &wire::Range) -> bool {
        let position = |hex: &str| {
            let id = entry_id(hex)?;
            self.entry_position(&id)
        };
        let (Some(from), Some(to)) = (position(&range.from), position(&range.to)) else {
            return false;
        };
        let selection = Some((from.min(to), from.max(to)));
        if self.timeline.selection() != selection {
            self.timeline.set_selection(selection);
            self.clear_comparison();
        }
        true
    }

    /// What the run `query` names did and what can be done with it.
    pub fn serve_detail(
        &mut self,
        query: &wire::DetailQuery,
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
    ) -> wire::Detail {
        if !self.select_range(&query.range) {
            return wire::Detail {
                gone: true,
                ..wire::Detail::default()
            };
        }
        let Some(detail) = self.selection_detail() else {
            return wire::Detail {
                gone: true,
                ..wire::Detail::default()
            };
        };
        let comparison = if query.compare && detail.can_compare {
            if self.comparison_selection != self.selected_ids() || !self.comparison_attempted {
                self.apply_detail_action(DetailAction::Compare, tree, records);
            }
            Some(
                self.comparison
                    .as_ref()
                    .map_or(wire::Comparison::Unavailable, |comparison| {
                        wire::Comparison::Changes {
                            lines: comparison_change_labels(&comparison.patches),
                        }
                    }),
            )
        } else {
            None
        };
        wire::Detail {
            title: detail.title.clone(),
            provenance: detail_provenance(&detail),
            authors: detail.authors.clone(),
            resources: detail.resources.iter().map(target).collect(),
            group: detail.group_id.clone(),
            group_label: detail.group_label.clone(),
            offers: self.offers(&detail),
            run: self.detail_run(),
            comparison,
            changes: self
                .comparison
                .as_ref()
                .map_or_else(Vec::new, comparison_field_changes),
            gone: false,
        }
    }

    /// The agent task the selection is, as its detail shows it: what was
    /// asked, by whom, each record it changed and what Undo can reach.
    fn detail_run(&self) -> Option<wire::Run> {
        self.selected_agent_run().map(|indexed| {
            let run = &indexed.run;
            wire::Run {
                instruction: run.instruction.clone().unwrap_or_else(|| run.label.clone()),
                byline: t_args(
                    "activity-agent-task-people",
                    &[
                        ("issuer", self.issuer_name(run).into()),
                        ("agent", self.agent_name(run).into()),
                    ],
                ),
                decision: run.decision.clone(),
                records: run
                    .steps
                    .iter()
                    .map(|step| {
                        let app = self
                            .host
                            .app_display_name(&step.app)
                            .unwrap_or_else(|| humanize_app_id(&step.app));
                        let record = RecordId::parse(&step.record)
                            .and_then(|id| id.file_path())
                            .and_then(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                            })
                            .unwrap_or_else(|| step.record.clone());
                        wire::RunRecord {
                            record,
                            app,
                            id: step.record.clone(),
                            app_id: step.app.clone(),
                            change: step.sentence(),
                            undo: self.can_undo_agent_record(step).then(|| wire::Offer {
                                action: wire::DetailAction::UndoRecord,
                                label: t("activity-detail-undo-step"),
                            }),
                        }
                    })
                    .collect(),
                undo_blocked: (!self.can_undo_agent_run())
                    .then(|| t("activity-undo-task-needs-open")),
                undo_status: self
                    .agent_undo_notice
                    .as_ref()
                    .filter(|(id, _)| *id == run.id)
                    .map(|(_, notice)| notice.clone()),
                unreverted: if self
                    .agent_undo_notice
                    .as_ref()
                    .is_some_and(|(id, _)| *id == run.id)
                {
                    self.unreverted.clone()
                } else {
                    Vec::new()
                },
            }
        })
    }

    /// What can be done with the selected run, worded for it.
    fn offers(&self, detail: &ui::HistorySelectionDetail) -> Vec<wire::Offer> {
        [
            DetailAction::MorningSource,
            DetailAction::Open,
            DetailAction::Compare,
            DetailAction::Restore,
            DetailAction::Undo,
            DetailAction::UndoTask,
            DetailAction::Split,
            DetailAction::Merge,
        ]
        .into_iter()
        .filter(|action| {
            self.host.mutations_available()
                || matches!(
                    action,
                    DetailAction::MorningSource | DetailAction::Open | DetailAction::Compare
                )
        })
        .filter(|action| match action {
            DetailAction::MorningSource => self.selected_morning_source().is_some(),
            DetailAction::Open => !detail.resources.is_empty(),
            DetailAction::Compare | DetailAction::Restore => detail.can_compare,
            DetailAction::Undo => detail.can_undo && self.selected_agent_run().is_none(),
            DetailAction::UndoTask => self.can_undo_agent_run(),
            DetailAction::Split => self.split_candidate().is_some(),
            DetailAction::Merge => self.merge_candidate().is_some(),
        })
        .map(|action| wire::Offer {
            action: match action {
                DetailAction::MorningSource => wire::DetailAction::ChooseSource,
                DetailAction::Open => wire::DetailAction::Open,
                DetailAction::Compare => wire::DetailAction::Compare,
                DetailAction::Restore => wire::DetailAction::Restore,
                DetailAction::Undo => wire::DetailAction::Undo,
                DetailAction::UndoTask => wire::DetailAction::UndoRun,
                DetailAction::Split => wire::DetailAction::Split,
                DetailAction::Merge => wire::DetailAction::Merge,
            },
            label: self.detail_action_label(action),
        })
        .collect()
    }

    /// Start showing what changed since this device last looked, from the
    /// frontier it holds now. What it marks seen moves the frontier only
    /// when the showing ends ([`Self::end_summary`]) or it says so.
    pub fn begin_summary(&mut self, workspace: u64) {
        self.begin_view(workspace);
        self.view = ActivityView::CatchUp;
    }

    /// Stop showing what changed: what was shown counts as seen.
    pub fn end_summary(&mut self, workspace: u64) {
        self.finish_view(workspace);
    }

    /// Count the changes `viewed` names as shown to the person in
    /// `workspace`; with `now`, as seen at once.
    pub fn mark_viewed(&mut self, workspace: u64, viewed: &wire::Viewed) {
        if self.scope != RoomScope::ThisRoom || self.last_active != Some(workspace) {
            return;
        }
        let shown = self.viewed_changes.entry(workspace).or_default();
        for id in viewed.entries.iter().filter_map(|hex| entry_id(hex)) {
            if self
                .external_entry
                .as_ref()
                .is_some_and(|entry| entry.id == id)
            {
                continue;
            }
            shown.insert(
                self.projected_origins
                    .get(&id)
                    .cloned()
                    .unwrap_or(WorkspaceHistoryId {
                        origin: crate::activity::index::HistoryOrigin::DesktopTree,
                        change: id,
                    }),
            );
        }
        if !viewed.later {
            self.finish_view(workspace);
        }
    }

    /// What the person last asked to be shown from outside the app, until
    /// an app says it showed it ([`Self::take_focus`]).
    pub fn serve_focus(&self) -> Option<wire::Focus> {
        self.focus.clone()
    }

    /// An app showed the focus numbered `serial`: it is not offered again,
    /// unless a newer one replaced it meanwhile.
    pub fn take_focus(&mut self, serial: u64) {
        if self
            .focus
            .as_ref()
            .is_some_and(|focus| focus.serial == serial)
        {
            self.focus = None;
        }
    }

    /// Offer `view` to the app that shows the history, as it stands.
    pub fn focus_view(&mut self, view: ActivityView) {
        self.focus_serial = self.focus_serial.wrapping_add(1);
        self.focus = Some(wire::Focus {
            serial: self.focus_serial,
            view: match view {
                ActivityView::CatchUp => wire::View::WhatsNew,
                ActivityView::Explore => wire::View::History,
                ActivityView::Replay => wire::View::Replay,
            },
            ..wire::Focus::default()
        });
    }

    /// Offer the change, person, work group or resource `link` names to
    /// the app that shows the history.
    pub fn focus_deep_link(&mut self, link: &ActivityDeepLink) {
        let entry = link
            .entry_id
            .or(link.resource_version)
            .map(HistoryEntryId::from_bytes)
            .filter(|id| self.entry_position(id).is_some())
            .or_else(|| {
                let resource = link.resource_id.as_deref()?;
                self.timeline
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.context.as_ref().is_some_and(|context| {
                            context.resource.as_ref().is_some_and(|item| {
                                item.id == resource
                                    || item.id.strip_prefix("file:") == Some(resource)
                            })
                        }) || self.projected_origins.get(&entry.id).is_some_and(|origin| {
                            matches!(
                                &origin.origin,
                                crate::activity::index::HistoryOrigin::Record { id, .. }
                                    if id.as_str() == resource
                            )
                        })
                    })
                    .map(|entry| entry.id)
            });
        let lens = link
            .resource_id
            .clone()
            .map(LensSelection::Resource)
            .or_else(|| link.group_id.clone().map(LensSelection::WorkGroup));
        self.focus_serial = self.focus_serial.wrapping_add(1);
        self.focus = Some(wire::Focus {
            serial: self.focus_serial,
            view: wire::View::History,
            author: link.author.clone(),
            lens: lens.as_ref().map(lens_key),
            entry: entry.as_ref().map(hex),
        });
    }

    /// Run write `action` with `arguments` on `tree`, as the person at this
    /// device asked; what it did, for the host to finish.
    ///
    /// # Errors
    /// The action is unknown, its arguments were not understood, or the
    /// changes it names are no longer in the history.
    pub fn serve_action(
        &mut self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        tree: &mut impl ActivityTree,
        records: Option<&RecordStore>,
    ) -> Result<RoomActivityOutcome, String> {
        let detail = |pane: &mut Self, range: &wire::Range, action, tree: &mut _| {
            if !pane.select_range(range) {
                return Err(t("activity-history-gone"));
            }
            Ok(pane.apply_detail_action(action, tree, records))
        };
        match action {
            wire::FOCUSED => {
                let focused: wire::Focused = read(arguments)?;
                self.take_focus(focused.serial);
                Ok(handled_outcome())
            }
            wire::MARK_VIEWED => {
                let workspace = tree.active.raw();
                self.mark_viewed(workspace, &read(arguments)?);
                Ok(handled_outcome())
            }
            wire::NAME => {
                let name: wire::Name = read(arguments)?;
                let (Some(start), Some(end)) =
                    (entry_id(&name.range.from), entry_id(&name.range.to))
                else {
                    return Err(t("activity-history-gone"));
                };
                let named = self.apply_name(
                    tree,
                    &GroupNameRequest {
                        record_id: name.record,
                        start,
                        end,
                        label: name.label,
                    },
                );
                Ok(RoomActivityOutcome {
                    handled: true,
                    named,
                    ..RoomActivityOutcome::default()
                })
            }
            wire::SPLIT => detail(self, &read(arguments)?, DetailAction::Split, tree),
            wire::MERGE => detail(self, &read(arguments)?, DetailAction::Merge, tree),
            wire::UNDO => detail(self, &read(arguments)?, DetailAction::Undo, tree),
            wire::RESTORE => detail(self, &read(arguments)?, DetailAction::Restore, tree),
            wire::UNDO_RUN => detail(self, &read(arguments)?, DetailAction::UndoTask, tree),
            wire::UNDO_RECORD => {
                let query: wire::RecordQuery = read(arguments)?;
                if !self.select_range(&query.range) {
                    return Err(t("activity-history-gone"));
                }
                let Some(task) = self
                    .selected_agent_run()
                    .and_then(|indexed| indexed.run.task.clone())
                else {
                    return Err(t("activity-undo-task-unavailable"));
                };
                // Asked of the task, so undo names the run's group in the
                // document: the same step a task-wide undo would take
                // back, one record at a time.
                Ok(if self.undo_task_record(&task, &query.app_id, &query.id) {
                    handled_outcome()
                } else {
                    RoomActivityOutcome::default()
                })
            }
            wire::CHOOSE_SOURCE => {
                detail(self, &read(arguments)?, DetailAction::MorningSource, tree)
            }
            wire::OPEN => {
                let open: wire::Target = read(arguments)?;
                Ok(RoomActivityOutcome {
                    handled: true,
                    open_target: Some(history_target(&open)),
                    ..RoomActivityOutcome::default()
                })
            }
            wire::MOVE_BOUNDARY => Ok(self.serve_boundary(&read(arguments)?, tree)?),
            wire::SOURCE => Ok(self.serve_source(&read(arguments)?)),
            other => Err(format!("workspace activity has no action `{other}`")),
        }
    }

    /// Move the boundary between the work sessions `boundary` names.
    fn serve_boundary(
        &mut self,
        boundary: &wire::Boundary,
        tree: &mut impl ActivityTree,
    ) -> Result<RoomActivityOutcome, String> {
        let brief = |hex: &str| {
            let id = entry_id(hex)?;
            self.digest
                .briefs
                .iter()
                .position(|brief| brief.entry_ids.contains(&id))
        };
        let (Some(older), Some(newer)) = (brief(&boundary.older), brief(&boundary.newer)) else {
            return Err(t("activity-history-gone"));
        };
        let named = self.move_work_session_boundary(
            tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: match boundary.toward {
                    wire::Toward::Newer => BoundaryDirection::IntoNewer,
                    wire::Toward::Older => BoundaryDirection::IntoOlder,
                },
            },
        );
        Ok(RoomActivityOutcome {
            handled: true,
            named,
            ..RoomActivityOutcome::default()
        })
    }

    /// Run `request` on a chosen morning source.
    fn serve_source(&mut self, request: &wire::SourceRequest) -> RoomActivityOutcome {
        let mut outcome = handled_outcome();
        let command = match request.command {
            wire::SourceCommand::Open => {
                outcome.open_target = self.activate_morning_source(&request.key, false);
                return outcome;
            }
            wire::SourceCommand::Remove => {
                self.remove_morning_source(&request.key);
                return outcome;
            }
            wire::SourceCommand::Refresh => super::MorningCommand::Refresh,
            wire::SourceCommand::DraftRefresh => super::MorningCommand::DraftRefresh,
            wire::SourceCommand::NextSource => super::MorningCommand::NextSource,
        };
        self.submit_morning_command(&request.key, command);
        outcome
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;

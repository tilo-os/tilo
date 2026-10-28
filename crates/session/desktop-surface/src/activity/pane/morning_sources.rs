//! Private source choices and provenance in the existing workspace catch-up.
use super::{
    HistorySelectionDetail, MorningSource, RoomActivityPane, RoomScope, SystemTime,
    format_relative_time, t, t_args,
};

/// The app a chosen source lives in, as a person would name it. Never the
/// resource id: a source whose names are all identifiers reads generically.
fn morning_source_name(target: &ui::HistoryResourceTarget) -> String {
    target
        .app_label
        .as_deref()
        .and_then(|label| ui::human_label(label, target.app_id.as_deref().unwrap_or_default()))
        .or_else(|| target.display_name())
        .map_or_else(|| t("activity-morning-source-unnamed"), str::to_owned)
}

impl RoomActivityPane {
    /// Explicit device-private choices; never inferred from history or app installation.
    pub fn chosen_morning_sources(&self) -> &[MorningSource] {
        self.morning_sources
            .get(&self.morning_workspace)
            .map_or(&[], |preferences| preferences.for_room(&self.room))
    }

    pub(super) fn selected_morning_source(&self) -> Option<MorningSource> {
        if !self.host.morning_sources_available() || self.scope != RoomScope::ThisRoom {
            return None;
        }
        let (start, end) = self.timeline.selection()?;
        if self
            .timeline
            .entries()
            .get(start.min(end)..=start.max(end))?
            .iter()
            .any(|entry| entry.redaction.is_some())
        {
            return None;
        }
        let detail = self.selection_detail()?;
        if detail.resources.len() != 1 {
            return None;
        }
        let source = MorningSource::from_target(detail.resources.first()?)?;
        (self.chosen_morning_sources().len() < crate::activity::morning::MAX_SOURCES
            || self.chosen_morning_sources().contains(&source))
        .then_some(source)
    }

    pub(super) fn toggle_morning_selection(&mut self) {
        if self.scope != RoomScope::ThisRoom {
            return;
        }
        let Some(source) = self.selected_morning_source() else {
            return;
        };
        if self
            .morning_sources
            .entry(self.morning_workspace)
            .or_default()
            .toggle(&self.room, source)
        {
            self.host.save_morning_sources(&self.morning_sources);
        }
    }

    pub(super) fn remove_morning_source(&mut self, key: &str) {
        let Some(source) = self
            .chosen_morning_sources()
            .iter()
            .find(|source| source.key() == key)
            .cloned()
        else {
            return;
        };
        if self
            .morning_sources
            .entry(self.morning_workspace)
            .or_default()
            .toggle(&self.room, source)
        {
            self.host.save_morning_sources(&self.morning_sources);
        }
    }

    /// Resolves only this room's presently visible, unredacted history. An old
    /// saved name is never displayed after its evidence disappears or is revoked.
    pub(super) fn morning_evidence(
        &self,
        key: &str,
    ) -> Option<(ui::HistoryResourceTarget, SystemTime)> {
        if self.scope != RoomScope::ThisRoom {
            return None;
        }
        let source = self
            .chosen_morning_sources()
            .iter()
            .find(|source| source.key() == key)?;
        let (index, entry) = self
            .timeline
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.redaction.is_none()
                    && entry.context.as_ref().is_some_and(|context| {
                        context.app.as_ref().is_some_and(|app| app.id == source.app)
                            && context
                                .resource
                                .as_ref()
                                .is_some_and(|resource| resource.id == source.resource)
                    })
            })
            .max_by_key(|(_, entry)| entry.timestamp)?;
        let detail = HistorySelectionDetail::from_range(self.timeline.entries(), (index, index))?;
        Some((
            detail
                .resources
                .into_iter()
                .find(|target| source.matches(target))?,
            entry.timestamp,
        ))
    }
    pub(super) fn activate_morning_source(
        &mut self,
        key: &str,
        remove: bool,
    ) -> Option<ui::HistoryResourceTarget> {
        if remove {
            self.remove_morning_source(key);
            None
        } else {
            let source = self
                .chosen_morning_sources()
                .iter()
                .find(|source| source.key() == key)?;
            if self.morning_evidence(key).is_some()
                && self
                    .host
                    .submit_morning_command(&self.room, source, super::MorningCommand::Open)
                    .is_ok()
            {
                None
            } else {
                self.morning_target(key)
            }
        }
    }

    pub(super) fn morning_target(&self, key: &str) -> Option<ui::HistoryResourceTarget> {
        self.morning_evidence(key).map(|(target, _)| target)
    }

    pub(super) fn morning_source_label(
        &self,
        evidence: Option<&(ui::HistoryResourceTarget, SystemTime)>,
    ) -> String {
        evidence.map_or_else(
            || t("activity-morning-unavailable"),
            |(target, time)| {
                t_args(
                    "activity-morning-source",
                    &[
                        ("source", morning_source_name(target).into()),
                        ("time", format_relative_time(*time, self.now()).into()),
                    ],
                )
            },
        )
    }

    pub(super) fn submit_morning_command(&mut self, key: &str, command: super::MorningCommand) {
        if command == super::MorningCommand::NextSource {
            if self.scope == RoomScope::ThisRoom {
                let choices = self.chosen_morning_sources();
                if let Some(index) = choices.iter().position(|source| source.key() == key) {
                    let offset = index
                        .saturating_add(1)
                        .checked_rem(choices.len())
                        .unwrap_or(0);
                    if choices.get(offset).is_some() {
                        self.morning_source_offset = offset;
                    }
                }
            }
            return;
        }
        let Some(source) = self
            .chosen_morning_sources()
            .iter()
            .find(|source| source.key() == key)
            .cloned()
        else {
            return;
        };
        if self.scope != RoomScope::ThisRoom || self.morning_evidence(key).is_none() {
            return;
        }
        self.morning_command_message = self
            .host
            .submit_morning_command(&self.room, &source, command)
            .err();
    }
    pub(super) fn morning_display_label(
        &self,
        key: &str,
        evidence: Option<&(ui::HistoryResourceTarget, SystemTime)>,
    ) -> String {
        if self.scope != RoomScope::ThisRoom || evidence.is_none() {
            return self.morning_source_label(evidence);
        }
        let payload = self
            .chosen_morning_sources()
            .iter()
            .find(|source| source.key() == key)
            .and_then(|source| self.host.morning_payload(&self.room, source));
        if let (Some(_), Some((target, time))) = (payload, evidence) {
            t_args(
                "activity-morning-recorded",
                &[
                    ("source", morning_source_name(target).into()),
                    ("time", format_relative_time(*time, self.now()).into()),
                ],
            )
        } else {
            self.morning_source_label(evidence)
        }
    }
    pub(super) fn morning_payload_text(&self, key: &str) -> String {
        let Some(source) = self
            .chosen_morning_sources()
            .iter()
            .find(|source| source.key() == key)
        else {
            return String::new();
        };
        if self.scope != RoomScope::ThisRoom || self.morning_evidence(key).is_none() {
            return t("activity-morning-payload-unavailable");
        }
        let mut text = self.host.morning_payload(&self.room, source).map_or_else(
            || t("activity-morning-payload-unavailable"),
            |payload| payload.text(),
        );
        if let Some(status) = self
            .host
            .morning_command_status(&self.room, source)
            .or_else(|| self.morning_command_message.clone())
        {
            text.push('\n');
            text.push_str(&status);
        }
        text
    }
}

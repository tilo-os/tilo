//! Private source choices and provenance in the existing workspace catch-up.
use super::{
    Affine, HistorySelectionDetail, MorningSource, Rect, RoomActivityPane, RoomScope, Scene,
    SystemTime, TextRenderer, TextRole, Theme, at, draw_task_button, format_relative_time, t,
    t_args, text_width,
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

/// The resource's own title, else its app, else the generic source name.
pub(super) fn morning_resource_name(target: &ui::HistoryResourceTarget) -> String {
    target
        .display_name()
        .map_or_else(|| t("activity-morning-source-unnamed"), str::to_owned)
}

struct MorningPaint<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
}

impl RoomActivityPane {
    /// Explicit device-private choices; never inferred from history or app installation.
    pub fn chosen_morning_sources(&self) -> &[MorningSource] {
        self.morning_sources
            .get(&self.morning_workspace)
            .map_or(&[], |preferences| preferences.for_room(&self.task_room))
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
            .toggle(&self.task_room, source)
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
            .toggle(&self.task_room, source)
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
                    .submit_morning_command(&self.task_room, source, super::MorningCommand::Open)
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
                    if let Some(next) = choices.get(offset).map(MorningSource::key) {
                        self.morning_source_offset = offset;
                        self.keyboard_focus =
                            Some(super::ActivityFocus::MorningCommand(next, command));
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
            .submit_morning_command(&self.task_room, &source, command)
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
            .and_then(|source| self.host.morning_payload(&self.task_room, source));
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
        let mut text = self
            .host
            .morning_payload(&self.task_room, source)
            .map_or_else(
                || t("activity-morning-payload-unavailable"),
                |payload| payload.text(),
            );
        if let Some(status) = self
            .host
            .morning_command_status(&self.task_room, source)
            .or_else(|| self.morning_command_message.clone())
        {
            text.push('\n');
            text.push_str(&status);
        }
        text
    }

    pub(super) fn draw_morning_sources(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        mut y: f64,
    ) -> f64 {
        let spacing = theme.tokens.spacing;
        let inset = f64::from(spacing.s3);
        let tight = f64::from(spacing.s1);
        let padding = f64::from(spacing.s4);
        let x0 = self.sheet.x0 + inset;
        // A host without private morning sources (a visitor in the browser)
        // has nothing to choose or show, so the section takes no space.
        if !self.host.morning_sources_available() {
            return y;
        }
        let caption = if self.chosen_morning_sources().is_empty() {
            t("activity-morning-empty")
        } else {
            t("activity-morning-description")
        };
        let width = text_width(self.sheet.width() - 2.0 * inset - f64::from(spacing.s2));
        let (_, height) = tr.measure_style(&caption, Some(width), &theme.text(TextRole::Caption));
        if y + f64::from(height) + padding > self.sheet.y1 {
            return y;
        }
        tr.draw_style(
            scene,
            &caption,
            theme.tokens.colors.text_dim,
            at(transform, x0 + tight, y + tight),
            Some(width),
            &theme.text(TextRole::Caption),
        );
        y += f64::from(height) + padding;
        let count = self.chosen_morning_sources().len();
        self.morning_source_offset = self.morning_source_offset.checked_rem(count).unwrap_or(0);
        let key = self
            .chosen_morning_sources()
            .get(self.morning_source_offset)
            .map(MorningSource::key);
        if let Some(key) = key {
            let mut paint = MorningPaint {
                scene,
                transform,
                text: tr,
                theme,
            };
            y = self.draw_morning_source(&mut paint, &key, count, y);
        }
        y
    }

    fn draw_morning_source(
        &mut self,
        paint: &mut MorningPaint<'_>,
        key: &str,
        count: usize,
        y: f64,
    ) -> f64 {
        let theme = paint.theme;
        let spacing = theme.tokens.spacing;
        let inset = f64::from(spacing.s3);
        let gap = f64::from(spacing.s2);
        let tight = f64::from(spacing.s1);
        let padding = f64::from(spacing.s4);
        let x0 = self.sheet.x0 + inset;
        let x1 = self.sheet.x1 - inset;
        let width = text_width(x1 - x0 - gap - 2.0 * inset);
        let evidence = self.morning_evidence(key);
        let label = format!(
            "{}\n{}",
            self.morning_display_label(key, evidence.as_ref()),
            self.morning_payload_text(key)
        );
        let label = paint
            .text
            .clamp_to_lines(&label, &theme.text(TextRole::Caption), width, 6)
            .join("\n");
        let title = evidence.as_ref().map_or_else(
            || t("activity-morning-unavailable"),
            |(target, _)| morning_resource_name(target),
        );
        let h = ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme);
        let (_, caption_height) =
            paint
                .text
                .measure_style(&label, Some(width), &theme.text(TextRole::Caption));
        let caption_h = f64::from(caption_height) + padding;
        let refreshable = (self.scope == RoomScope::ThisRoom && evidence.is_some())
            && self
                .chosen_morning_sources()
                .iter()
                .find(|source| source.key() == key)
                .and_then(|source| self.host.morning_payload(&self.task_room, source))
                .is_some_and(|payload| payload.refreshable);
        let commands = [
            super::MorningCommand::Refresh,
            super::MorningCommand::DraftRefresh,
        ]
        .into_iter()
        .filter(|_| refreshable)
        .chain((count > 1).then_some(super::MorningCommand::NextSource))
        .collect::<Vec<_>>();
        let command_h = commands.iter().fold(0.0, |height, _| height + h + gap);
        let remove_label = t("activity-morning-remove");
        let (_, remove_height) =
            paint
                .text
                .measure_style(&remove_label, Some(width), &theme.text(TextRole::Caption));
        let remove_h = (f64::from(remove_height) + padding).max(h);
        if y + h + caption_h + remove_h + command_h + padding > self.sheet.y1 - inset {
            return y;
        }
        let rect = Rect::new(x0, y, x1, y + h);
        draw_task_button(
            paint.scene,
            paint.transform,
            paint.text,
            theme,
            rect,
            &title,
            false,
        );
        if evidence.is_some() {
            self.morning_rects.push(((key.to_owned(), false), rect));
        }
        paint.text.draw_style(
            paint.scene,
            &label,
            theme.tokens.colors.text_dim,
            at(paint.transform, x0 + inset, rect.y1 + gap),
            Some(width),
            &theme.text(TextRole::Caption),
        );
        let bottom = rect.y1 + caption_h;
        let rect = Rect::new(x0, bottom + tight, x1, bottom + tight + remove_h);
        draw_task_button(
            paint.scene,
            paint.transform,
            paint.text,
            theme,
            rect,
            &remove_label,
            false,
        );
        self.morning_rects.push(((key.to_owned(), true), rect));
        self.draw_morning_commands(paint, key, &commands, rect.y1 + gap)
    }

    fn draw_morning_commands(
        &mut self,
        paint: &mut MorningPaint<'_>,
        key: &str,
        commands: &[super::MorningCommand],
        mut y: f64,
    ) -> f64 {
        let theme = paint.theme;
        let inset = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.s2);
        let h = ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme);
        for command in commands {
            let rect = Rect::new(self.sheet.x0 + inset, y, self.sheet.x1 - inset, y + h);
            draw_task_button(
                paint.scene,
                paint.transform,
                paint.text,
                theme,
                rect,
                &t(command.label_key()),
                false,
            );
            self.morning_command_rects
                .push(((key.to_owned(), *command), rect));
            y = rect.y1 + gap;
        }
        y
    }
}

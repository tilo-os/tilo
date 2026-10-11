//! Private source choices and provenance in the existing workspace catch-up.
use super::{
    Affine, HistorySelectionDetail, MorningSource, Rect, RoomActivityPane, RoomScope, Scene,
    SystemTime, TextRenderer, TextRole, Theme, at, draw_task_button, format_relative_time, t,
    t_args, text_width,
};

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
            self.morning_target(key)
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
                        (
                            "source",
                            target.app_label.as_deref().unwrap_or(&target.id).into(),
                        ),
                        ("time", format_relative_time(*time, self.now()).into()),
                    ],
                )
            },
        )
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
        let gap = f64::from(spacing.s2);
        let tight = f64::from(spacing.s1);
        let padding = f64::from(spacing.s4);
        let x0 = self.sheet.x0 + inset;
        let x1 = self.sheet.x1 - inset;
        let caption = if !self.host.morning_sources_available() {
            t("activity-morning-unavailable")
        } else if self.chosen_morning_sources().is_empty() {
            t("activity-morning-empty")
        } else {
            t("activity-morning-description")
        };
        let width = text_width(x1 - x0 - gap);
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
        let keys = self
            .chosen_morning_sources()
            .iter()
            .map(MorningSource::key)
            .collect::<Vec<_>>();
        for key in keys {
            let evidence = self.morning_evidence(&key);
            let label = self.morning_source_label(evidence.as_ref());
            let title = evidence.as_ref().map_or_else(
                || t("activity-morning-unavailable"),
                |(target, _)| target.label.clone(),
            );
            let h = ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme);
            let (_, caption_height) = tr.measure_style(
                &label,
                Some(text_width(f64::from(width) - (2.0 * inset))),
                &theme.text(TextRole::Caption),
            );
            let caption_h = f64::from(caption_height) + padding;
            let remove_label = t("activity-morning-remove");
            let (_, remove_height) = tr.measure_style(
                &remove_label,
                Some(text_width(f64::from(width) - (2.0 * inset))),
                &theme.text(TextRole::Caption),
            );
            let remove_h = (f64::from(remove_height) + padding).max(
                ui::widget::ButtonFace::height(ui::widget::ControlSize::Regular, theme),
            );
            if y + h + caption_h + remove_h + padding > self.sheet.y1 - inset {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + h);
            draw_task_button(scene, transform, tr, theme, rect, &title, false);
            if evidence.is_some() {
                self.morning_rects.push(((key.clone(), false), rect));
            }
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                at(transform, x0 + inset, rect.y1 + gap),
                Some(text_width(f64::from(width) - (2.0 * inset))),
                &theme.text(TextRole::Caption),
            );
            let bottom = rect.y1 + caption_h;
            let rect = Rect::new(x0, bottom + tight, x1, bottom + tight + remove_h);
            draw_task_button(scene, transform, tr, theme, rect, &remove_label, false);
            self.morning_rects.push(((key, true), rect));
            y = rect.y1 + gap;
        }
        y
    }
}

//! The notification centre inside Quick Settings: retained notifications
//! grouped by verified app, a keyboard selection, per-row controls and the
//! actions those controls invoke. Paint, pointer hit-testing, keyboard and
//! accessibility all read the rows and geometry defined here.

use std::time::Instant;

use ui::a11y::{AccessNode, AccessState, Role};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};

use crate::shell::i18n::{t, t_args};
use crate::shell::notification::Notification;
use crate::shell::notification_history::{HistoryActivation, HistoryRow, RowIdentity};
use crate::shell::quick_settings::drill_in as qs;
use crate::shell::state::WorkspaceId;

use super::history::{
    notification_history_accessible_description, notification_history_relative_time,
};
use super::{NotificationHistoryLayout, ShellOverlayHost, f64_to_f32_lossy};

/// A trailing icon button on a history row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NotificationRowControl {
    Dismiss,
    HandToAgent,
}

/// Where a press on the history list landed: a row, and optionally one of
/// its trailing controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NotificationRowHit {
    pub row: usize,
    pub control: Option<NotificationRowControl>,
}

/// An agent collaborator present in the workspace a notification came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NotificationAgent {
    pub workspace: WorkspaceId,
    pub room: String,
    pub agent: String,
}

/// Trailing icon buttons of a history row: dismiss on the trailing edge,
/// hand-to-agent one related step inboard of it. They sit under the
/// source/time line so the relative time keeps its place.
pub(super) fn notification_row_controls(
    layout: &NotificationHistoryLayout,
    row: Rect,
    with_agent: bool,
) -> (Rect, Option<Rect>) {
    let top = row.y0 + qs::CONTROL_PAD + layout.caption_lh;
    let room = (row.y1 - top).max(0.0);
    let size = qs::TARGET.min(room);
    let y0 = top + (room - size) / 2.0;
    let x1 = row.x1 - qs::RELATED_GAP;
    let dismiss = Rect::new(x1 - size, y0, x1, y0 + size);
    let agent = with_agent.then(|| {
        let x1 = dismiss.x0 - qs::RELATED_GAP;
        Rect::new(x1 - size, y0, x1, y0 + size)
    });
    (dismiss, agent)
}

impl ShellOverlayHost {
    /// The centre's rows, newest first, honouring the viewer's folded groups.
    pub(super) fn qs_notification_rows(&self) -> Vec<HistoryRow> {
        self.chrome.as_ref().map_or_else(Vec::new, |chrome| {
            chrome
                .borrow()
                .notification_history_rows(&self.qs_notification_collapsed)
        })
    }

    /// Index of the selected row in `rows`, if the selection still exists.
    pub(super) fn qs_notification_selected_row(
        &self,
        rows: &[HistoryRow],
        history: &[Notification],
    ) -> Option<usize> {
        let selected = self.qs_notification_selected.as_ref()?;
        rows.iter()
            .position(|row| row.identity(history).as_ref() == Some(selected))
    }

    /// Select row `index` (clamped) and scroll it into view.
    pub(super) fn qs_select_notification_row(&mut self, index: usize) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let rows = self.qs_notification_rows();
        let Some(last) = rows.len().checked_sub(1) else {
            self.qs_notification_selected = None;
            return;
        };
        let index = index.min(last);
        let identity = {
            let state = chrome.borrow();
            rows.get(index)
                .and_then(|row| row.identity(state.notification_history()))
        };
        if identity != self.qs_notification_selected {
            self.qs_notification_row_control = None;
        }
        self.qs_notification_selected = identity;
        self.qs_notification_action_focus = None;
        let visible = self.qs_notification_visible_rows().max(1);
        if index < self.qs_notification_history_start {
            self.qs_notification_history_start = index;
        } else if index >= self.qs_notification_history_start.saturating_add(visible) {
            self.qs_notification_history_start = index.saturating_add(1).saturating_sub(visible);
        }
        chrome.borrow_mut().bump();
    }

    /// Move the selection by `delta` rows. With nothing selected the first
    /// press selects the first visible row.
    pub(super) fn qs_move_notification_selection(&mut self, delta: isize) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let rows = self.qs_notification_rows();
        let current = {
            let state = chrome.borrow();
            self.qs_notification_selected_row(&rows, state.notification_history())
        };
        let next = match current {
            None => self.qs_notification_history_start,
            Some(index) if delta.is_negative() => index.saturating_sub(delta.unsigned_abs()),
            Some(index) => index.saturating_add(delta.unsigned_abs()),
        };
        self.qs_select_notification_row(next);
    }

    /// The agent a notification may be handed to: admitted to the workspace
    /// it came from and live right now. `None` hides the action entirely.
    pub(super) fn qs_notification_agent(
        &self,
        notification: &Notification,
    ) -> Option<NotificationAgent> {
        let chrome = self.chrome.as_ref()?;
        let (workspace, room, agent) =
            chrome.borrow().notification_agent_candidate(notification)?;
        let room = room.to_string();
        self.room_activity
            .agent_is_present_in(&room, &agent)
            .then_some(NotificationAgent {
                workspace,
                room,
                agent,
            })
    }

    /// The row (and control) under `p` among the currently visible rows.
    pub(super) fn qs_notification_row_hit(
        &self,
        p: vello::kurbo::Point,
    ) -> Option<NotificationRowHit> {
        let chrome = self.chrome.as_ref()?;
        let layout = self.qs_notification_layout();
        let rows = self.qs_notification_rows();
        let visible = layout.visible_rows(rows.len());
        let start = self.qs_notification_history_start;
        let state = chrome.borrow();
        let history = state.notification_history();
        for (offset, row) in rows.iter().enumerate().skip(start).take(visible) {
            let rect = layout.row(offset.saturating_sub(start));
            if !rect.contains(p) {
                continue;
            }
            let with_agent = match row {
                HistoryRow::Item { index, .. } => history
                    .get(*index)
                    .is_some_and(|n| self.qs_notification_agent(n).is_some()),
                HistoryRow::Group { .. } => false,
            };
            let (dismiss, agent) = notification_row_controls(&layout, rect, with_agent);
            let control = if dismiss.contains(p) {
                Some(NotificationRowControl::Dismiss)
            } else if agent.is_some_and(|agent| agent.contains(p)) {
                Some(NotificationRowControl::HandToAgent)
            } else {
                None
            };
            return Some(NotificationRowHit {
                row: offset,
                control,
            });
        }
        None
    }

    /// Act on row `index`: a group header folds or unfolds; a notification
    /// invokes its default action, or the given trailing control.
    pub(super) fn qs_activate_notification_row(
        &mut self,
        index: usize,
        control: Option<NotificationRowControl>,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let rows = self.qs_notification_rows();
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        match (row, control) {
            (_, Some(NotificationRowControl::Dismiss)) => self.qs_dismiss_notification_row(index),
            (HistoryRow::Group { key, .. }, _) => {
                if !self.qs_notification_collapsed.remove(&key) {
                    self.qs_notification_collapsed.insert(key.clone());
                }
                self.qs_notification_selected = Some(RowIdentity::Group(key));
                chrome.borrow_mut().bump();
            }
            (HistoryRow::Item { index: at, .. }, Some(NotificationRowControl::HandToAgent)) => {
                let notification = chrome.borrow().notification_history().get(at).cloned();
                if let Some(notification) = notification {
                    self.qs_hand_notification_to_agent(&notification);
                }
            }
            (HistoryRow::Item { index: at, .. }, None) => {
                let id = chrome.borrow().notification_history().get(at).map(|n| n.id);
                let Some(id) = id else {
                    return;
                };
                let outcome = chrome.borrow_mut().activate_history_notification(id);
                if let HistoryActivation::OpenApp { app_id, label } = &outcome {
                    self.launch_app(app_id, label);
                }
                if matches!(
                    outcome,
                    HistoryActivation::Invoked | HistoryActivation::OpenApp { .. }
                ) {
                    self.qs_reselect_after_removal(index);
                }
            }
        }
    }

    /// Dismiss row `index`: one notification, or every notification of a
    /// group header.
    pub(super) fn qs_dismiss_notification_row(&mut self, index: usize) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let rows = self.qs_notification_rows();
        match rows.get(index) {
            Some(HistoryRow::Group { key, .. }) => {
                let key = key.clone();
                chrome.borrow_mut().dismiss_history_group(&key);
                self.qs_notification_collapsed.remove(&key);
            }
            Some(HistoryRow::Item { index: at, .. }) => {
                let id = chrome
                    .borrow()
                    .notification_history()
                    .get(*at)
                    .map(|n| n.id);
                if let Some(id) = id {
                    chrome.borrow_mut().dismiss_history_notification(id);
                }
            }
            None => return,
        }
        self.qs_reselect_after_removal(index);
    }

    /// Keep a keyboard selection on the row that slid into the removed
    /// row's place, so repeated Delete walks down the list.
    fn qs_reselect_after_removal(&mut self, index: usize) {
        if self.qs_notification_selected.is_none() {
            return;
        }
        if self.qs_notification_rows().is_empty() {
            self.qs_notification_selected = None;
            self.qs_notification_row_control = None;
            return;
        }
        self.qs_select_notification_row(index);
    }

    fn qs_hand_notification_to_agent(&self, notification: &Notification) {
        let Some(agent) = self.qs_notification_agent(notification) else {
            return;
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let task = chrome.borrow_mut().hand_history_notification_to_agent(
            notification.id,
            agent.workspace,
            &agent.agent,
        );
        let Some(task) = task else {
            return;
        };
        if let Err(error) = self.room_activity.dispatch_task_in(&agent.room, &task) {
            tracing::warn!(%task, "handing a notification to {} failed: {error}", agent.agent);
        }
    }

    /// `tilo-control list-targets` names for the history page's rows and
    /// their trailing controls.
    pub(super) fn notification_center_semantic_rects(&self) -> Vec<(String, Rect)> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let layout = self.qs_notification_layout();
        let rows = self.qs_notification_rows();
        let visible = layout.visible_rows(rows.len());
        let start = self.qs_notification_history_start;
        let state = chrome.borrow();
        let history = state.notification_history();
        let mut out = vec![(
            "quick_settings_notifications_back".to_string(),
            layout.back(),
        )];
        if !history.is_empty() {
            out.push((
                "quick_settings_notifications_clear".to_string(),
                layout.clear(),
            ));
        }
        for (offset, row) in rows.iter().enumerate().skip(start).take(visible) {
            let rect = layout.row(offset.saturating_sub(start));
            let agent = match row {
                HistoryRow::Item { index, .. } => history
                    .get(*index)
                    .and_then(|n| self.qs_notification_agent(n)),
                HistoryRow::Group { .. } => None,
            };
            let (dismiss, agent_rect) = notification_row_controls(&layout, rect, agent.is_some());
            out.push((format!("quick_settings_notification_row[{offset}]"), rect));
            out.push((
                format!("quick_settings_notification_dismiss[{offset}]"),
                dismiss,
            ));
            if let Some(agent_rect) = agent_rect {
                out.push((
                    format!("quick_settings_notification_agent[{offset}]"),
                    agent_rect,
                ));
            }
        }
        out
    }

    /// One accessibility node per visible row, matching the painted rows
    /// one-to-one so a focus path names the row it lights. Group headers are
    /// level-one items with an expanded state; their notifications are
    /// level two.
    pub(super) fn qs_notification_accessibility_rows(&self, now: Instant) -> Vec<AccessNode> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let layout = self.qs_notification_layout();
        let rows = self.qs_notification_rows();
        let visible = layout.visible_rows(rows.len());
        let start = self
            .qs_notification_history_start
            .min(rows.len().saturating_sub(visible));
        let shown = visible.min(rows.len().saturating_sub(start));
        let state = chrome.borrow();
        let history = state.notification_history();
        let selected = self.qs_notification_selected_row(&rows, history);
        rows.iter()
            .enumerate()
            .skip(start)
            .take(shown)
            .map(|(offset, row)| {
                let position = offset.saturating_sub(start);
                let rect = layout.row(position);
                let is_selected = selected == Some(offset);
                let row_focused = is_selected && self.qs_notification_row_control.is_none();
                let node = match row {
                    HistoryRow::Group {
                        key,
                        label,
                        count,
                        expanded,
                        ..
                    } => {
                        let (dismiss, _) = notification_row_controls(&layout, rect, false);
                        AccessNode::new(Role::ListItem)
                            .with_semantic_key(format!("notification-group:{key}"))
                            .with_name(label.clone())
                            .with_description(t_args(
                                "notification-center-group-description",
                                &[
                                    ("count", (*count).into()),
                                    ("expanded", if *expanded { "yes" } else { "no" }.into()),
                                ],
                            ))
                            .with_level(1)
                            .with_state(
                                AccessState::new()
                                    .expanded(*expanded)
                                    .focusable(true)
                                    .focused(row_focused)
                                    .selected(is_selected),
                            )
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name(t_args(
                                        "notification-center-dismiss-all",
                                        &[("app", label.as_str().into())],
                                    ))
                                    .with_bounds(dismiss)
                                    .with_state(AccessState::new().focusable(true).focused(
                                        is_selected
                                            && self.qs_notification_row_control
                                                == Some(NotificationRowControl::Dismiss),
                                    )),
                            ])
                    }
                    HistoryRow::Item { index, grouped } => match history.get(*index) {
                        Some(notification) => self.qs_notification_item_node(
                            &layout,
                            rect,
                            notification,
                            *grouped,
                            is_selected,
                            now,
                        ),
                        None => AccessNode::new(Role::ListItem),
                    },
                };
                node.with_position(position.saturating_add(1), shown)
                    .with_bounds(rect)
            })
            .collect()
    }

    /// The accessibility node of one retained notification row and its
    /// trailing controls.
    fn qs_notification_item_node(
        &self,
        layout: &NotificationHistoryLayout,
        rect: Rect,
        notification: &Notification,
        grouped: bool,
        is_selected: bool,
        now: Instant,
    ) -> AccessNode {
        let row_focused = is_selected && self.qs_notification_row_control.is_none();
        let agent = self.qs_notification_agent(notification);
        let (dismiss, agent_rect) = notification_row_controls(layout, rect, agent.is_some());
        let control_focused =
            |control| is_selected && self.qs_notification_row_control == Some(control);
        let mut controls = vec![
            AccessNode::new(Role::Button)
                .with_name(t("notification-center-dismiss"))
                .with_bounds(dismiss)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(control_focused(NotificationRowControl::Dismiss)),
                ),
        ];
        if let (Some(agent), Some(agent_rect)) = (agent, agent_rect) {
            controls.push(
                AccessNode::new(Role::Button)
                    .with_name(t_args(
                        "notification-center-hand-to",
                        &[("agent", agent.agent.as_str().into())],
                    ))
                    .with_description(t("notification-center-hand-to-description"))
                    .with_bounds(agent_rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(control_focused(NotificationRowControl::HandToAgent)),
                    ),
            );
        }
        AccessNode::new(Role::ListItem)
            .with_semantic_key(format!("notification:{}", notification.id))
            .with_name(notification.title.clone())
            .with_description(notification_history_accessible_description(
                notification,
                now,
            ))
            .with_level(if grouped { 2 } else { 1 })
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(row_focused)
                    .selected(is_selected),
            )
            .with_children(controls)
    }

    /// Paint one group header row: fold chevron, app label, count, the
    /// newest member's age and (while folded) its title.
    #[allow(
        clippy::too_many_arguments,
        reason = "a header row paints from the shared layout, its row rect, the render handles and its own group fields"
    )]
    pub(super) fn draw_qs_notification_group_row(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &NotificationHistoryLayout,
        row: Rect,
        header: (&str, usize, bool, &Notification),
    ) {
        let (label, count, expanded, newest) = header;
        let colors = &theme.tokens.colors;
        let caption_size = qs::type_size(text_renderer, theme.tokens.typography.scale.xs);
        let label_size = qs::type_size(text_renderer, theme.tokens.typography.scale.sm);
        let now = Instant::now();
        let glyph = qs::leading_glyph(row, qs::CONTROL_INSET, qs::GLYPH);
        ui::icons::draw_icon(
            scene,
            transform,
            glyph,
            if expanded {
                ui::icons::Icon::ChevronDown
            } else {
                ui::icons::Icon::ChevronRight
            },
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let (dismiss, _) = notification_row_controls(layout, row, false);
        let x = glyph.x1 + qs::RELATED_GAP;
        let relative_time = notification_history_relative_time(newest, now);
        let (time_width, _) = text_renderer.measure(&relative_time, caption_size, None);
        let source_y = row.y0 + qs::CONTROL_PAD;
        let count_label = t_args(
            "notification-center-group-count",
            &[("count", count.into())],
        );
        text_renderer.draw_text(
            scene,
            &count_label,
            caption_size,
            colors.text_dim,
            transform.pre_translate((x, source_y).into()),
            Some(f64_to_f32_lossy(
                row.x1 - qs::CONTROL_INSET - f64::from(time_width) - qs::RELATED_GAP - x,
            )),
        );
        text_renderer.draw_text(
            scene,
            &relative_time,
            caption_size,
            colors.text_dim,
            transform.pre_translate(
                (row.x1 - qs::CONTROL_INSET - f64::from(time_width), source_y).into(),
            ),
            None,
        );
        let text_w = f64_to_f32_lossy(dismiss.x0 - qs::RELATED_GAP - x);
        let title_y = source_y + layout.caption_lh;
        text_renderer.draw_text_weighted(
            scene,
            label,
            label_size,
            colors.text_primary,
            transform.pre_translate((x, title_y).into()),
            Some(text_w),
            TextWeight::Semibold,
        );
        if !expanded {
            text_renderer.draw_text(
                scene,
                &newest.title,
                caption_size,
                colors.text_dim,
                transform.pre_translate((x, title_y + layout.label_lh).into()),
                Some(text_w),
            );
        }
    }

    /// Paint a row's trailing icon buttons as Controls, the hovered one on
    /// the hover tier and the keyboard-focused one ringed.
    pub(super) fn draw_qs_notification_row_controls(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        controls: (Rect, Option<Rect>),
        focused: Option<NotificationRowControl>,
    ) {
        let (dismiss, agent) = controls;
        let cursor = self.last_cursor;
        let glyph = qs::GLYPH.min(dismiss.height());
        let mut paint = |rect: Rect, icon: ui::icons::Icon, control| {
            qs::paint_control(
                scene,
                transform,
                theme,
                rect,
                cursor.is_some_and(|p| rect.contains(p)),
                focused == Some(control),
            );
            let c = rect.center();
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    c.x - glyph / 2.0,
                    c.y - glyph / 2.0,
                    c.x + glyph / 2.0,
                    c.y + glyph / 2.0,
                ),
                icon,
                theme.tokens.colors.text_dim,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        };
        paint(dismiss, ui::icons::Icon::X, NotificationRowControl::Dismiss);
        if let Some(agent) = agent {
            paint(
                agent,
                ui::icons::Icon::Send,
                NotificationRowControl::HandToAgent,
            );
        }
    }

    /// Ring the keyboard-selected row.
    pub(super) fn draw_qs_notification_selection(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        row: Rect,
    ) {
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &RoundedRect::from_rect(row, qs::control_radius(theme, row)),
        );
    }
}

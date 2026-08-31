//! Transient pickers used by the calendar event editor.
//!
//! [`EventPopovers`] owns the mutually-exclusive popover state, the embedded
//! date/time widgets, their layout and drawing, and the hit map used to route
//! pointer input.  The root calendar only receives semantic edits to apply to
//! its model; it does not need to coordinate picker focus or backing scrims.

use automerge::{AutoCommit, ObjId};
use chrono::{NaiveDate, Timelike};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{DatePicker, TimePicker};

use crate::app::{CalTarget, draw_check, fill_round, stroke_round};
use crate::model::Event;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PopoverKind {
    None,
    StartDate,
    EndDate,
    StartTime,
    EndTime,
    Calendar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PopoverEdit {
    Date(PopoverKind, NaiveDate),
    Time(PopoverKind, u8, u8),
    Calendar(CalTarget),
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PopoverAnchors {
    pub start_date: Rect,
    pub end_date: Rect,
    pub calendar: Rect,
}

#[derive(Clone, Debug)]
enum Hit {
    Dismiss,
    Guard,
    Calendar(CalTarget),
}

pub(crate) struct EventPopovers {
    active: PopoverKind,
    date_picker: DatePicker,
    start_time_picker: TimePicker,
    end_time_picker: TimePicker,
    hits: Vec<(Rect, Hit)>,
}

impl EventPopovers {
    pub fn new(today: NaiveDate) -> Self {
        Self {
            active: PopoverKind::None,
            date_picker: DatePicker::new(today),
            start_time_picker: TimePicker::new(12, 0),
            end_time_picker: TimePicker::new(13, 0),
            hits: Vec::new(),
        }
    }

    pub fn active(&self) -> PopoverKind {
        self.active
    }

    pub fn is_open(&self) -> bool {
        self.active != PopoverKind::None
    }

    pub fn close(&mut self) {
        self.active = PopoverKind::None;
        self.date_picker.set_focus(false);
        self.start_time_picker.set_focus(false);
        self.end_time_picker.set_focus(false);
        self.hits.clear();
    }

    /// Open a picker and seed its widget from the event being edited.
    pub fn open(&mut self, kind: PopoverKind, event: &Event) {
        self.close();
        match kind {
            PopoverKind::None => return,
            PopoverKind::StartDate => {
                self.date_picker.set_selected(event.start.date());
                self.date_picker.set_focus(true);
            }
            PopoverKind::EndDate => {
                self.date_picker.set_selected(event.end.date());
                self.date_picker.set_focus(true);
            }
            PopoverKind::StartTime => {
                self.start_time_picker.set_hours(event.start.hour() as u8);
                self.start_time_picker
                    .set_minutes(event.start.minute() as u8);
                self.start_time_picker.set_focus(true);
            }
            PopoverKind::EndTime => {
                self.end_time_picker.set_hours(event.end.hour() as u8);
                self.end_time_picker.set_minutes(event.end.minute() as u8);
                self.end_time_picker.set_focus(true);
            }
            PopoverKind::Calendar => {}
        }
        self.active = kind;
    }

    /// Route input to the active picker and return a semantic model edit.  All
    /// popover pointer hit-testing (outside dismiss, padding guard, calendar
    /// rows) is resolved here using the regions built by [`Self::draw`].
    #[allow(clippy::too_many_arguments)]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        pointer: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> (bool, Option<PopoverEdit>) {
        let kind = self.active;
        if kind == PopoverKind::None {
            return (false, None);
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        ) {
            self.close();
            return (true, None);
        }

        let is_enter = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter | NamedKey::Space),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        );
        let is_click = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        let mut handled = false;
        let mut edit = None;

        self.dispatch_picker_event(
            kind,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
            is_enter,
            is_click,
            &mut handled,
            &mut edit,
        );

        if !handled && is_click {
            self.handle_hit_fallback(pointer, &mut handled, &mut edit);
        }

        (handled, edit)
    }

    /// Route the event into whichever picker the open popover owns (part of
    /// [`Self::handle_event`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "internal event-handling split of an over-length function"
    )]
    fn dispatch_picker_event(
        &mut self,
        kind: PopoverKind,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        is_enter: bool,
        is_click: bool,
        handled: &mut bool,
        edit: &mut Option<PopoverEdit>,
    ) {
        match kind {
            PopoverKind::None | PopoverKind::Calendar => {}
            PopoverKind::StartDate | PopoverKind::EndDate => {
                let before = self.date_picker.selected();
                self.date_picker.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
                let after = self.date_picker.selected();
                if after != before {
                    *edit = Some(PopoverEdit::Date(kind, after));
                }
                if (is_click && *handled && after != before) || is_enter {
                    self.close();
                    *handled = true;
                }
            }
            PopoverKind::StartTime | PopoverKind::EndTime => {
                let picker = if kind == PopoverKind::StartTime {
                    &mut self.start_time_picker
                } else {
                    &mut self.end_time_picker
                };
                let before = (picker.hours(), picker.minutes());
                picker.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
                let after = (picker.hours(), picker.minutes());
                if after != before {
                    *edit = Some(PopoverEdit::Time(kind, after.0, after.1));
                }
                if is_enter {
                    self.close();
                    *handled = true;
                }
            }
        }
    }

    /// A left click the picker didn't consume, resolved against the hit map
    /// — the dismiss scrim, a calendar row, or a popover guard (part of
    /// [`Self::handle_event`]).
    fn handle_hit_fallback(
        &mut self,
        pointer: Point,
        handled: &mut bool,
        edit: &mut Option<PopoverEdit>,
    ) {
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(pointer))
            .map(|(_, hit)| hit.clone());
        match hit {
            Some(Hit::Dismiss) | None => self.close(),
            Some(Hit::Guard) => {}
            Some(Hit::Calendar(target)) => {
                *edit = Some(PopoverEdit::Calendar(target));
                self.close();
            }
        }
        *handled = true;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        panel: Rect,
        anchors: PopoverAnchors,
        pointer: Point,
        calendar_options: &[(CalTarget, String)],
        current_calendar: Option<&str>,
    ) {
        self.hits.clear();
        let kind = self.active;
        if kind == PopoverKind::None {
            return;
        }
        self.hits.push((panel, Hit::Dismiss));
        let anchor = match kind {
            PopoverKind::None => return,
            PopoverKind::StartDate | PopoverKind::StartTime => anchors.start_date,
            PopoverKind::EndDate | PopoverKind::EndTime => anchors.end_date,
            PopoverKind::Calendar => anchors.calendar,
        };
        let colors = &theme.tokens.colors;

        match kind {
            PopoverKind::None => {}
            PopoverKind::StartDate | PopoverKind::EndDate => {
                let (width, height) = (7.0 * 32.0, 32.0 + 20.0 + 6.0 * 32.0);
                let x = anchor.x0.min(panel.x1 - width - 8.0).max(panel.x0 + 8.0);
                let y = (anchor.y1 + 6.0).min(panel.y1 - height - 8.0);
                let rect = Rect::new(x, y, x + width, y + height);
                let backing = rect.inflate(6.0, 6.0);
                fill_round(scene, transform, colors.surfaces.raised, backing, 10.0);
                stroke_round(scene, transform, colors.border_standard, backing, 10.0, 1.0);
                self.date_picker.apply_layout_external(rect);
                self.date_picker
                    .draw(scene, transform, text, theme, presence);
                self.hits.push((rect.inflate(6.0, 6.0), Hit::Guard));
            }
            PopoverKind::StartTime | PopoverKind::EndTime => {
                let picker = if kind == PopoverKind::StartTime {
                    &mut self.start_time_picker
                } else {
                    &mut self.end_time_picker
                };
                let (width, height) = picker.intrinsic_size();
                let x = anchor.x0.max(panel.x0 + 8.0);
                let y = (anchor.y1 + 6.0).min(panel.y1 - height - 8.0);
                let rect = Rect::new(x, y, x + width, y + height);
                let backing = rect.inflate(8.0, 8.0);
                fill_round(scene, transform, colors.surfaces.raised, backing, 10.0);
                stroke_round(scene, transform, colors.border_standard, backing, 10.0, 1.0);
                picker.apply_layout_external(rect);
                picker.draw(scene, transform, text, theme, presence);
                self.hits.push((rect.inflate(8.0, 8.0), Hit::Guard));
            }
            PopoverKind::Calendar => {
                let row_h = 34.0;
                let height = row_h * calendar_options.len() as f64 + 8.0;
                let y = (anchor.y1 + 6.0).min(panel.y1 - height - 8.0);
                let rect = Rect::new(anchor.x0, y, anchor.x1, y + height);
                fill_round(scene, transform, colors.surfaces.raised, rect, 10.0);
                stroke_round(scene, transform, colors.border_standard, rect, 10.0, 1.0);
                self.hits.push((rect, Hit::Guard));
                for (index, (target, label)) in calendar_options.iter().enumerate() {
                    let row_y = rect.y0 + 4.0 + index as f64 * row_h;
                    let row = Rect::new(rect.x0 + 4.0, row_y, rect.x1 - 4.0, row_y + row_h);
                    let selected = match target {
                        CalTarget::Local => current_calendar.is_none(),
                        CalTarget::Remote(id) => current_calendar == Some(id.as_str()),
                    };
                    if row.contains(pointer) || selected {
                        fill_round(scene, transform, colors.surface_white_10, row, 6.0);
                    }
                    text.draw_text(
                        scene,
                        label,
                        13.0,
                        colors.text_primary,
                        transform * Affine::translate((row.x0 + 12.0, row.center().y - 7.0)),
                        Some((row.width() - 40.0) as f32),
                    );
                    if selected {
                        draw_check(
                            scene,
                            transform,
                            colors.accent[5],
                            Rect::new(
                                row.x1 - 24.0,
                                row.center().y - 7.0,
                                row.x1 - 10.0,
                                row.center().y + 7.0,
                            ),
                        );
                    }
                    self.hits.push((row, Hit::Calendar(target.clone())));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};
    use uuid::Uuid;

    use crate::model::{Palette, Recurrence};

    fn event() -> Event {
        let start = NaiveDate::from_ymd_opt(2026, 8, 31)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap();
        Event {
            id: Uuid::new_v4(),
            title: "Test".into(),
            start,
            end: start + Duration::minutes(90),
            all_day: false,
            location: String::new(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            reminder_min: None,
            project: None,
            calendar_id: None,
        }
    }

    #[test]
    fn opening_a_picker_replaces_the_previous_one_and_close_clears_state() {
        let mut popovers = EventPopovers::new(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap());
        popovers.open(PopoverKind::StartDate, &event());
        assert_eq!(popovers.active(), PopoverKind::StartDate);
        popovers.open(PopoverKind::EndTime, &event());
        assert_eq!(popovers.active(), PopoverKind::EndTime);
        popovers.close();
        assert!(!popovers.is_open());
        assert!(popovers.hits.is_empty());
    }

    #[test]
    fn calendar_rows_win_over_the_backing_guard() {
        let target = CalTarget::Remote("work".into());
        let mut popovers = EventPopovers::new(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap());
        popovers
            .hits
            .push((Rect::new(0.0, 0.0, 100.0, 100.0), Hit::Guard));
        popovers.hits.push((
            Rect::new(4.0, 4.0, 96.0, 38.0),
            Hit::Calendar(target.clone()),
        ));
        let hit = popovers
            .hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(Point::new(20.0, 20.0)))
            .map(|(_, h)| h);
        assert!(matches!(hit, Some(Hit::Calendar(found)) if found == &target));
    }
}

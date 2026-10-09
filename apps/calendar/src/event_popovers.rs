//! Transient pickers used by the calendar event editor.
//!
//! [`EventPopovers`] owns the mutually-exclusive popover state, the embedded
//! date/time widgets, their layout and drawing, and the hit map used to route
//! pointer input.  The root calendar only receives semantic edits to apply to
//! its model; it does not need to coordinate picker focus or backing scrims.

use crate::input_context::InputContext;
use chrono::{NaiveDate, Timelike};
use ui::paint::PaintCtx;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::widget::{DatePicker, TimePicker, floating_surface};

use crate::model::Event;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PopoverKind {
    None,
    StartDate,
    EndDate,
    StartTime,
    EndTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PopoverEdit {
    Date(PopoverKind, NaiveDate),
    Time(PopoverKind, u8, u8),
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PopoverAnchors {
    pub start_date: Rect,
    pub end_date: Rect,
    /// The time fields: a time spinner opens under the field it edits, not
    /// under the date beside it.
    pub start_time: Rect,
    pub end_time: Rect,
    /// The area every popover is kept inside (the window); a click on it
    /// outside the open popover dismisses it.
    pub panel: Rect,
}

#[derive(Clone, Debug)]
enum Hit {
    Dismiss,
    Guard,
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
            date_picker: DatePicker::new(today).with_today(today),
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
                // The form shows an inclusive final day for an all-day
                // event, whereas RFC 5545 stores its end date exclusively.
                self.date_picker.set_selected(if event.all_day {
                    event.all_day_last_date()
                } else {
                    event.end.date()
                });
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
        }
        self.active = kind;
    }

    /// Route input to the active picker and return a semantic model edit.  All
    /// popover pointer hit-testing (outside dismiss, padding guard) is
    /// resolved here using the regions built by [`Self::draw`].
    pub fn handle_event(
        &mut self,
        input: &mut InputContext<'_>,
        pointer: Point,
    ) -> Option<PopoverEdit> {
        let kind = self.active;
        if kind == PopoverKind::None {
            return None;
        }
        if matches!(
            input.event,
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
            *input.handled = true;
            return None;
        }

        let is_enter = matches!(
            input.event,
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
            input.event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        let mut edit = None;

        self.dispatch_picker_event(kind, input, is_enter, is_click, &mut edit);

        if !*input.handled && is_click {
            self.handle_hit_fallback(pointer, input.handled);
        }

        edit
    }

    /// Route the event into whichever picker the open popover owns (part of
    /// [`Self::handle_event`]).
    fn dispatch_picker_event(
        &mut self,
        kind: PopoverKind,
        input: &mut InputContext<'_>,
        is_enter: bool,
        is_click: bool,
        edit: &mut Option<PopoverEdit>,
    ) {
        match kind {
            PopoverKind::None => {}
            PopoverKind::StartDate | PopoverKind::EndDate => {
                let before = self.date_picker.selected();
                self.date_picker.handle_event(
                    input.event,
                    &mut ui::widget::EventCtx {
                        scale_factor: input.scale_factor,
                        modifiers: &input.modifiers,
                        state: input.state.reborrow(),
                        path: input.path,
                        handled: input.handled,
                        focus_target: input.focus_target,
                        node_id: input.node_id,
                        taffy: input.taffy,
                    },
                );
                let after = self.date_picker.selected();
                if after != before {
                    *edit = Some(PopoverEdit::Date(kind, after));
                }
                if (is_click && *input.handled && after != before) || is_enter {
                    self.close();
                    *input.handled = true;
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
                    input.event,
                    &mut ui::widget::EventCtx {
                        scale_factor: input.scale_factor,
                        modifiers: &input.modifiers,
                        state: input.state.reborrow(),
                        path: input.path,
                        handled: input.handled,
                        focus_target: input.focus_target,
                        node_id: input.node_id,
                        taffy: input.taffy,
                    },
                );
                let after = (picker.hours(), picker.minutes());
                if after != before {
                    *edit = Some(PopoverEdit::Time(kind, after.0, after.1));
                }
                if is_enter {
                    self.close();
                    *input.handled = true;
                }
            }
        }
    }

    /// A left click the picker didn't consume, resolved against the hit map
    /// — outside the popover dismisses it, its own padding swallows the
    /// click (part of [`Self::handle_event`]).
    fn handle_hit_fallback(&mut self, pointer: Point, handled: &mut bool) {
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(pointer))
            .map(|(_, hit)| hit.clone());
        match hit {
            Some(Hit::Dismiss) | None => self.close(),
            Some(Hit::Guard) => {}
        }
        *handled = true;
    }

    /// Paint the open picker on a floating panel under the field it edits,
    /// kept inside `anchors.panel`.
    pub fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        anchors: PopoverAnchors,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let panel = anchors.panel;
        self.hits.clear();
        let kind = self.active;
        let anchor = match kind {
            PopoverKind::None => return,
            PopoverKind::StartDate => anchors.start_date,
            PopoverKind::EndDate => anchors.end_date,
            PopoverKind::StartTime => anchors.start_time,
            PopoverKind::EndTime => anchors.end_time,
        };
        self.hits.push((panel, Hit::Dismiss));
        let sp = &theme.tokens.spacing;
        let pad = f64::from(sp.s3);
        let gap = f64::from(sp.s1);
        let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
        let place = |width: f64, height: f64| {
            let x = anchor
                .x0
                .min(panel.x1 - width - pad - gap)
                .max(panel.x0 + pad + gap);
            let below = anchor.y1 + gap + pad;
            let y = if below + height + pad <= panel.y1 {
                below
            } else {
                (anchor.y0 - gap - pad - height).max(panel.y0 + pad)
            };
            Rect::new(x, y, x + width, y + height)
        };
        match kind {
            PopoverKind::None => {}
            PopoverKind::StartDate | PopoverKind::EndDate => {
                let m = &theme.tokens.metrics;
                let cell = f64::from(m.min_pointer_target);
                let rect = place(7.0 * cell, cell + f64::from(m.icon_md) + 6.0 * cell);
                let backing = rect.inflate(pad, pad);
                floating_surface::paint(scene, transform, backing, theme);
                self.date_picker.apply_layout_external(rect);
                self.date_picker
                    .draw(scene, transform, text, &on_panel, presence);
                self.hits.push((backing, Hit::Guard));
            }
            PopoverKind::StartTime | PopoverKind::EndTime => {
                let picker = if kind == PopoverKind::StartTime {
                    &mut self.start_time_picker
                } else {
                    &mut self.end_time_picker
                };
                let (width, height) = picker.intrinsic_size();
                let rect = place(width, height);
                let backing = rect.inflate(pad, pad);
                floating_surface::paint(scene, transform, backing, theme);
                picker.apply_layout_external(rect);
                picker.draw(scene, transform, text, &on_panel, presence);
                self.hits.push((backing, Hit::Guard));
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
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: ui::tilo_resource::Set::default(),
            occurrence_overrides: ui::tilo_resource::Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: ui::tilo_resource::Keyed::default(),
            reminder_min: None,
            project: None,
            calendar_id: None,
        }
    }

    #[test]
    fn picker_lifecycle_replaces_and_clears_and_the_guard_wins_hits() {
        // Case `opening_a_picker_replaces_the_previous_one_and_close_clears_state`.
        {
            let mut popovers = EventPopovers::new(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap());
            popovers.open(PopoverKind::StartDate, &event());
            assert_eq!(popovers.active(), PopoverKind::StartDate);
            popovers.open(PopoverKind::EndTime, &event());
            assert_eq!(popovers.active(), PopoverKind::EndTime);
            popovers.close();
            assert!(!popovers.is_open());
            assert!(popovers.hits.is_empty());
        }
        // Case `the_backing_guard_wins_over_the_dismiss_area`.
        {
            let mut popovers = EventPopovers::new(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap());
            popovers
                .hits
                .push((Rect::new(0.0, 0.0, 800.0, 600.0), Hit::Dismiss));
            popovers
                .hits
                .push((Rect::new(4.0, 4.0, 96.0, 38.0), Hit::Guard));
            let hit = popovers
                .hits
                .iter()
                .rev()
                .find(|(r, _)| r.contains(Point::new(20.0, 20.0)))
                .map(|(_, h)| h);
            assert!(matches!(hit, Some(Hit::Guard)));
        }
    }
}

//! `DateField`: a form's date, shown as the date and picked in a popover.
//!
//! The field reads as a control with the calendar icon and the date in the person's format (or
//! "No date"). Pressing it opens a [`DatePicker`] in a popover anchored to the field; picking a
//! day sets the date and closes it. A field whose date may be empty adds "Clear date" under the
//! calendar, as Calendar's due-date popover does.
//!
//! The owner learns of a change through [`DateField::on_change`] and tells the field a date from
//! elsewhere with [`DateField::set_value`]; the field never holds the authoritative value. A
//! described component's field follows the day its binding holds ([`DateField::from_state`]).

use std::cell::Cell;
use std::rc::Rc;

use chrono::NaiveDate;

use crate::icons::Icon;
use crate::widget::arrange::Stack;
use crate::widget::binding::{BoxedBinding, Pulled, derived};
use crate::widget::{
    AppModel, Button, ButtonRole, DatePicker, Decorator, ModelPath, PopoverButton, Widget,
};

#[path = "date_field_component.rs"]
mod component;
pub use component::DateFieldProps;

/// What a press in the popover asked for: a day, or no date.
type Picked = Rc<Cell<Option<Option<NaiveDate>>>>;

/// A day the bound value took while the calendar was open: shown once it closes, so a day set
/// elsewhere never moves the calendar from under the person picking.
#[derive(Clone, Copy)]
enum Waiting {
    /// The binding took no day since the field last showed one.
    Nothing,
    /// The day to show now, `None` for a date the app cleared.
    Day(Option<NaiveDate>),
}

/// See the [module docs](self).
pub struct DateField {
    inner: PopoverButton<Stack>,
    id: String,
    value: Option<NaiveDate>,
    optional: bool,
    picked: Picked,
    on_change: Option<Box<dyn FnMut(Option<NaiveDate>)>>,
    /// The bound day the field follows, when a view binds it.
    bound: Option<BoxedBinding<Option<NaiveDate>>>,
    /// A day the bound value took while the calendar was open: shown once it closes.
    waiting: Waiting,
}

impl DateField {
    /// A field showing `value`, which the person may clear when `optional`.
    #[must_use]
    pub fn new(id: &str, value: Option<NaiveDate>, optional: bool) -> Self {
        let picked: Picked = Rc::default();
        Self {
            inner: field(id, value, optional, &picked),
            id: id.to_owned(),
            value,
            optional,
            picked,
            on_change: None,
            bound: None,
            waiting: Waiting::Nothing,
        }
    }

    /// A field following the day `state` holds as its ISO text (`""` for no date), which the
    /// person may clear when `optional`: a day set there shows in place.
    pub(super) fn from_state(id: &str, state: &crate::LocalState<String>, optional: bool) -> Self {
        let (day, binding) = derived(state, |text| crate::component::moment_day(text));
        let mut field = Self::new(id, day, optional);
        field.bound = Some(binding);
        field
    }

    /// Call `f` with the date the person picked, or `None` when they cleared it.
    #[must_use]
    pub fn on_change<F: FnMut(Option<NaiveDate>) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// The date the field shows.
    #[must_use]
    pub fn value(&self) -> Option<NaiveDate> {
        self.value
    }

    /// Whether the person may clear the date.
    #[must_use]
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// Show `value`, a date set elsewhere. An open popover stays open on the day it shows.
    pub fn set_value(&mut self, value: Option<NaiveDate>) {
        if value == self.value || self.inner.is_open() {
            return;
        }
        self.value = value;
        self.inner = field(&self.id, value, self.optional, &self.picked);
    }

    /// Take what the person picked: close the popover, show the date and report it.
    fn take_pick(&mut self) -> bool {
        let Some(value) = self.picked.take() else {
            return false;
        };
        // Their pick is the newest day there is: a day the bound value took while the calendar
        // was open gives way to it.
        self.waiting = Waiting::Nothing;
        self.inner.close();
        if value != self.value {
            self.value = value;
            self.inner = field(&self.id, value, self.optional, &self.picked);
            if let Some(on_change) = &mut self.on_change {
                on_change(value);
            }
        }
        true
    }

    /// Show the day the bound value took while the calendar was open, now that it closed.
    fn show_waiting(&mut self) -> bool {
        if self.inner.is_open() {
            return false;
        }
        let Waiting::Day(value) = std::mem::replace(&mut self.waiting, Waiting::Nothing) else {
            return false;
        };
        if value == self.value {
            return false;
        }
        self.set_value(value);
        true
    }

    /// What the field reads as: the date, or "No date".
    #[must_use]
    pub fn label(value: Option<NaiveDate>) -> String {
        value.map_or_else(
            || crate::i18n::widget_format("date-field-none", &[]),
            crate::format::date,
        )
    }
}

/// The field's button and its popover: the calendar, and "Clear date" when `optional`.
fn field(
    id: &str,
    value: Option<NaiveDate>,
    optional: bool,
    picked: &Picked,
) -> PopoverButton<Stack> {
    let today = crate::clock::local_now().date_naive();
    let on_day = Rc::clone(picked);
    let picker = DatePicker::new(value.unwrap_or(today))
        .with_today(today)
        .with_accessible_name(crate::i18n::widget_format("date-field-calendar", &[]))
        .on_view(format!("{id}-calendar"), move |day| {
            on_day.set(Some(Some(day)))
        });
    let mut content = Stack::stack().push(Box::new(picker));
    if optional && value.is_some() {
        let on_clear = Rc::clone(picked);
        let clear = Button::new(
            &format!("{id}-clear"),
            &crate::i18n::widget_format("date-field-clear", &[]),
        )
        .role(ButtonRole::Plain)
        .on_view(move || on_clear.set(Some(None)));
        content = content.push(Box::new(clear));
    }
    Button::new(id, &DateField::label(value))
        .with_icon(Icon::Calendar)
        .with_popover(content)
}

impl Decorator for DateField {
    type Inner = PopoverButton<Stack>;

    fn inner(&self) -> &PopoverButton<Stack> {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut PopoverButton<Stack> {
        &mut self.inner
    }

    /// The day the bound value took shows in place; a day the person is picking is not taken
    /// from under the calendar.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(bound) = &mut self.bound
            && let Pulled::Value(day) = bound.pull(state, path)
        {
            if self.inner.is_open() {
                self.waiting = Waiting::Day(day);
            } else {
                self.set_value(day);
            }
        }
        self.inner.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let picked = self.take_pick();
        let shown = self.show_waiting();
        let inner = self.inner.update(state, path);
        picked || shown || inner
    }

    fn name(&self) -> &'static str {
        "DateField"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_day_is_shown_and_reported_and_clearing_reports_no_date() {
        let reported: Rc<Cell<Option<Option<NaiveDate>>>> = Rc::default();
        let seen = Rc::clone(&reported);
        let day = NaiveDate::from_ymd_opt(2026, 10, 24);
        let mut field = DateField::new("due", None, true).on_change(move |value| {
            seen.set(Some(value));
        });
        assert_eq!(DateField::label(None), "No date");
        field.picked.set(Some(day));
        assert!(field.take_pick());
        assert_eq!(field.value(), day);
        assert_eq!(reported.take(), Some(day));
        field.picked.set(Some(None));
        assert!(field.take_pick());
        assert_eq!(field.value(), None);
        assert_eq!(reported.take(), Some(None));
    }

    #[test]
    fn a_date_from_elsewhere_is_shown_without_being_reported() {
        let reported: Rc<Cell<bool>> = Rc::default();
        let seen = Rc::clone(&reported);
        let mut field = DateField::new("due", None, false).on_change(move |_| seen.set(true));
        let day = NaiveDate::from_ymd_opt(2026, 11, 2);
        field.set_value(day);
        assert_eq!(field.value(), day);
        assert!(!field.take_pick());
        assert!(!reported.get());
    }

    #[test]
    fn a_bound_day_shows_and_one_set_while_the_calendar_is_open_waits_for_it() {
        let state = crate::LocalState::new("2026-06-30".to_owned());
        let mut field = DateField::from_state("due", &state, true);
        assert_eq!(
            field.value(),
            NaiveDate::from_ymd_opt(2026, 6, 30),
            "the day the binding holds is shown"
        );
        state.set("2026-07-01".to_owned());
        Decorator::sync(&mut field, &(), &());
        assert_eq!(
            field.value(),
            NaiveDate::from_ymd_opt(2026, 7, 1),
            "a day set elsewhere shows in place"
        );
        // The person is picking: the calendar stays on what they are looking at.
        field.inner_mut().open();
        state.set("2026-07-02".to_owned());
        Decorator::sync(&mut field, &(), &());
        assert_eq!(
            field.value(),
            NaiveDate::from_ymd_opt(2026, 7, 1),
            "a day set while the calendar is open does not move it"
        );
        field.inner_mut().close();
        assert!(Decorator::update(&mut field, &(), &()));
        assert_eq!(
            field.value(),
            NaiveDate::from_ymd_opt(2026, 7, 2),
            "the day it took shows once the calendar closes"
        );
    }
}

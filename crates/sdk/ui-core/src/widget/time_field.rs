//! `TimeField`: the time of day a day is due at, picked under the day.
use std::cell::Cell;
use std::rc::Rc;

use chrono::{NaiveTime, Timelike};

use crate::icons::Icon;
use crate::widget::arrange::Stack;
use crate::widget::binding::{BoxedBinding, Pulled, derived};
use crate::widget::date_field::time_in;
use crate::widget::{AppModel, Button, ButtonRole, Decorator, ModelPath, TimePicker, Widget};

#[path = "time_field_component.rs"]
mod component;
pub use component::TimeFieldProps;

type Picked = Rc<Cell<Option<Option<NaiveTime>>>>;

#[must_use]
pub fn morning() -> NaiveTime {
    NaiveTime::from_hms_opt(9, 0, 0).unwrap_or(NaiveTime::MIN)
}

/// A time set elsewhere while the spinners were being stepped: shown once the step is taken, so
/// a time another device sets never moves the digits from under the person.
#[derive(Clone, Copy)]
enum Waiting {
    Nothing,
    At(Option<NaiveTime>),
}

pub struct TimeField {
    inner: Stack,
    id: String,
    source: Option<crate::LocalState<String>>,
    value: Option<NaiveTime>,
    picked: Picked,
    on_change: Option<Box<dyn FnMut(Option<NaiveTime>)>>,
    bound: Option<BoxedBinding<Option<NaiveTime>>>,
    waiting: Waiting,
    accessible_name: String,
}

impl TimeField {
    #[must_use]
    pub fn new(id: &str, value: Option<NaiveTime>) -> Self {
        let picked: Picked = Rc::default();
        let accessible_name = crate::i18n::widget_format("time-field-name", &[]);
        Self {
            inner: row(id, value, &picked, &accessible_name),
            id: id.to_owned(),
            source: None,
            value,
            picked,
            on_change: None,
            bound: None,
            waiting: Waiting::Nothing,
            accessible_name,
        }
    }

    pub(super) fn from_state(id: &str, state: &crate::LocalState<String>) -> Self {
        let (value, binding) = derived(state, |text: &String| time_in(Some(text)));
        let mut field = Self::new(id, value);
        field.bound = Some(binding);
        field.source = Some(state.clone());
        field
    }

    #[must_use]
    pub fn on_change<F: FnMut(Option<NaiveTime>) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn value(&self) -> Option<NaiveTime> {
        self.value
    }

    pub fn set_value(&mut self, value: Option<NaiveTime>) {
        if value == self.value {
            return;
        }
        self.value = value;
        self.inner = row(&self.id, value, &self.picked, &self.accessible_name);
    }

    fn take_pick(&mut self) -> bool {
        let Some(value) = self.picked.take() else {
            return false;
        };
        self.waiting = Waiting::Nothing;
        if value != self.value {
            self.set_value(value);
            if let Some(on_change) = &mut self.on_change {
                on_change(value);
            }
        }
        true
    }

    fn show_waiting(&mut self) -> bool {
        let Waiting::At(value) = std::mem::replace(&mut self.waiting, Waiting::Nothing) else {
            return false;
        };
        if value == self.value {
            return false;
        }
        self.set_value(value);
        true
    }
}

fn row(id: &str, value: Option<NaiveTime>, picked: &Picked, accessible_name: &str) -> Stack {
    let mut row = Stack::row();
    let Some(time) = value else {
        let on_add = Rc::clone(picked);
        let add = Button::new(
            &format!("{id}-add"),
            &crate::i18n::widget_format("time-field-add", &[]),
        )
        .role(ButtonRole::Plain)
        .with_icon(Icon::Clock)
        .with_placeholder()
        .on_view(move || on_add.set(Some(Some(morning()))));
        return row.push(Box::new(add));
    };
    let on_step = Rc::clone(picked);
    let picker = TimePicker::new(
        u8::try_from(time.hour()).unwrap_or_default(),
        u8::try_from(time.minute()).unwrap_or_default(),
    )
    .with_id(id)
    .with_accessible_name(accessible_name)
    .on_change(move |hours, minutes, _| {
        let at = NaiveTime::from_hms_opt(u32::from(hours), u32::from(minutes), 0);
        on_step.set(Some(at));
    });
    row = row.push(Box::new(picker));
    let on_remove = Rc::clone(picked);
    let remove = Button::new(
        &format!("{id}-remove"),
        &crate::i18n::widget_format("time-field-remove", &[]),
    )
    .role(ButtonRole::Plain)
    .with_placeholder()
    .on_view(move || on_remove.set(Some(None)));
    row.push(Box::new(remove))
}

impl Decorator for TimeField {
    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.inner
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(bound) = &mut self.bound
            && let Pulled::Value(value) = bound.pull(state, path)
        {
            self.waiting = Waiting::At(value);
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
        "TimeField"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nine() -> NaiveTime {
        morning()
    }

    #[test]
    fn adding_a_time_starts_in_the_morning_and_removing_it_reports_none() {
        let reported: Rc<Cell<Option<Option<NaiveTime>>>> = Rc::default();
        let seen = Rc::clone(&reported);
        let mut field = TimeField::new("at", None).on_change(move |value| {
            seen.set(Some(value));
        });
        assert_eq!(field.value(), None);
        field.picked.set(Some(Some(nine())));
        assert!(field.take_pick());
        assert_eq!(field.value(), Some(nine()));
        assert_eq!(reported.take(), Some(Some(nine())));
        field.picked.set(Some(None));
        assert!(field.take_pick());
        assert_eq!(field.value(), None);
        assert_eq!(reported.take(), Some(None));
    }

    #[test]
    fn a_time_from_elsewhere_is_shown_without_being_reported() {
        let reported: Rc<Cell<bool>> = Rc::default();
        let seen = Rc::clone(&reported);
        let mut field = TimeField::new("at", None).on_change(move |_| seen.set(true));
        field.set_value(NaiveTime::from_hms_opt(14, 30, 0));
        assert_eq!(field.value(), NaiveTime::from_hms_opt(14, 30, 0));
        assert!(!field.take_pick());
        assert!(!reported.get());
    }

    #[test]
    fn a_bound_time_shows_the_time_of_day_it_holds() {
        let state = crate::LocalState::new("09:00".to_owned());
        let mut field = TimeField::from_state("at", &state);
        assert_eq!(field.value(), Some(nine()));
        state.set(String::new());
        Decorator::sync(&mut field, &(), &());
        assert!(Decorator::update(&mut field, &(), &()));
        assert_eq!(
            field.value(),
            None,
            "a time cleared elsewhere shows as none"
        );
        state.set("14:30".to_owned());
        Decorator::sync(&mut field, &(), &());
        assert!(Decorator::update(&mut field, &(), &()));
        assert_eq!(
            field.value(),
            NaiveTime::from_hms_opt(14, 30, 0),
            "a time set elsewhere shows in place"
        );
    }
}

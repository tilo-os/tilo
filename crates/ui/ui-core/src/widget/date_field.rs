//! `DateField`: a day, shown as one fact and picked in a popover.

use std::cell::Cell;
use std::rc::Rc;

use jiff::civil::{Date, Time};

use crate::widget::arrange::Stack;
use crate::{widget::Button, widget::ButtonRole, widget::DatePicker, widget::PopoverButton};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{
    widget::AppModel, widget::Decorator, widget::Element, widget::ModelPath, widget::Tone,
    widget::Widget,
};
use tilo_ui_base::{
    widget::binding::BoxedBinding, widget::binding::Pulled, widget::binding::derived,
};

#[path = "date_field_component.rs"]
mod component;
pub use component::DateFieldProps;

type Picked = Rc<Cell<Option<Option<Date>>>>;

/// A day the bound value took while the calendar was open: shown once it closes, so a day set
/// elsewhere never moves the calendar from under the person picking.
#[derive(Clone, Copy)]
enum Waiting {
    Nothing,
    Day(Option<Date>),
}

pub struct DateField {
    inner: PopoverButton<Stack>,
    value: Option<Date>,
    at: Option<Time>,
    label: Option<String>,
    compact: bool,
    optional: bool,
    picked: Picked,
    /// The day the calendar shows and highlights: the day the person picked, and the one the
    /// owner set from elsewhere, so the calendar never disagrees with the chip.
    shown: tilo_ui_base::LocalState<Date>,
    on_change: Option<Box<dyn FnMut(Option<Date>)>>,
    bound: Option<BoxedBinding<Option<Date>>>,
    waiting: Waiting,
}

impl DateField {
    /// A field showing `value`, which the person may clear when `optional`.
    #[must_use]
    pub fn new(id: &str, value: Option<Date>, optional: bool) -> Self {
        Self::built(id, value, None, optional, false, Vec::new(), None)
    }

    fn built(
        id: &str,
        value: Option<Date>,
        at: Option<Time>,
        optional: bool,
        compact: bool,
        time: Vec<Element>,
        label: Option<&str>,
    ) -> Self {
        let picked: Picked = Rc::default();
        let shown = tilo_ui_base::LocalState::new(value.unwrap_or_else(tilo_ui_base::clock::today));
        Self {
            inner: field(
                Chip {
                    id,
                    label,
                    optional,
                    compact,
                    time,
                },
                value,
                at,
                &picked,
                &shown,
            ),
            value,
            at,
            label: label.map(str::to_owned),
            compact,
            optional,
            picked,
            shown,
            on_change: None,
            bound: None,
            waiting: Waiting::Nothing,
        }
    }

    /// A field following the day `state` holds as its ISO text (`""` for no date), which the
    /// person may clear when `optional`, with `time` in its popover as the control
    /// that picks `at`, the time of day the day is due at: a day set there shows in place.
    pub(super) fn from_state(
        id: &str,
        state: &tilo_ui_base::LocalState<String>,
        optional: bool,
        at: Option<&str>,
        time: Vec<Element>,
        label: Option<&str>,
        compact: bool,
    ) -> Self {
        let (day, binding) = derived(state, |text| tilo_ui_base::component::moment_day(text));
        let at = time_in(at);
        let mut field = Self::built(id, day, at, optional, compact, time, label);
        field.bound = Some(binding);
        field
    }

    pub(super) fn popover_content_mut(&mut self) -> &mut Stack {
        self.inner.popover_mut().content_mut()
    }

    #[must_use]
    pub fn on_change<F: FnMut(Option<Date>) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn value(&self) -> Option<Date> {
        self.value
    }

    #[must_use]
    pub fn at(&self) -> Option<Time> {
        self.at
    }

    /// Whether the person may clear the date.
    #[must_use]
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    pub fn set_value(&mut self, value: Option<Date>) {
        if value == self.value {
            return;
        }
        self.value = value;
        self.shown
            .set(value.unwrap_or_else(tilo_ui_base::clock::today));
        self.show_label();
    }

    /// Show `at`, the time of day the day is due at, set elsewhere: only what the chip says
    /// changes.
    pub fn set_at(&mut self, at: Option<Time>) {
        if at == self.at {
            return;
        }
        self.at = at;
        self.show_label();
    }

    fn show_label(&mut self) {
        let (label, tone) = Self::fact(self.value, self.at);
        let spoken = Self::spoken(self.label.as_deref(), self.value, self.at);
        let chip = self.inner.button_mut();
        chip.set_label(
            if self.compact { String::new() } else { label },
            self.value.is_none(),
        );
        chip.set_tone(tone);
        chip.set_accessible_name(&spoken);
    }

    fn take_pick(&mut self) -> bool {
        let Some(value) = self.picked.take() else {
            return false;
        };
        self.waiting = Waiting::Nothing;
        self.inner.close();
        if value != self.value {
            self.set_value(value);
            if let Some(on_change) = &mut self.on_change {
                on_change(value);
            }
        }
        true
    }

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

    /// What the field reads as, and the tone it says it in: the day and the time it is due at
    /// ("Today 9:00 AM", "Overdue since Jun 29"), or "Add date" while it holds none.
    #[must_use]
    pub fn fact(value: Option<Date>, at: Option<Time>) -> (String, Tone) {
        value.map_or_else(
            || {
                (
                    tilo_ui_base::i18n::widget_format("date-field-add", &[]),
                    Tone::Neutral,
                )
            },
            |day| {
                let fact = tilo_ui_base::format::day_fact_today(day, at);
                (fact.text, fact.tone)
            },
        )
    }

    #[must_use]
    pub fn label(value: Option<Date>, at: Option<Time>) -> String {
        Self::fact(value, at).0
    }

    /// What a screen reader hears of the chip: what the day is for and the day it holds
    /// ("Due, Today 9:00 AM"), or the day alone when nothing names it.
    #[must_use]
    pub fn spoken(label: Option<&str>, value: Option<Date>, at: Option<Time>) -> String {
        let day = Self::label(value, at);
        label.map_or(day.clone(), |label| {
            tilo_ui_base::i18n::widget_format(
                "date-field-named",
                &[("label", label.into()), ("day", day.into())],
            )
        })
    }
}

pub(crate) fn with_day(text: &str, day: Option<Date>) -> String {
    let Some(day) = day else {
        return String::new();
    };
    match time_in(Some(text)) {
        Some(time) => instant_text(day, time),
        None => day.to_string(),
    }
}

pub(crate) fn with_time(text: &str, time: Option<Time>) -> String {
    let Some(time) = time else {
        return String::new();
    };
    match tilo_ui_base::component::moment_day(text) {
        Some(day) => instant_text(day, time),
        None => tilo_ui_base::format::machine!(
            "the field's value, read back rather than shown in their style",
            time.strftime("%H:%M").to_string()
        ),
    }
}

/// The instant `day` at `time` in the person's zone, as ISO-8601 UTC text.
pub(crate) fn instant_text(day: Date, time: Time) -> String {
    match tilo_ui_base::clock::instant_of(day.to_datetime(time)) {
        Some(at) => format!("{at:.0}"),
        None => tilo_failure::ignored!(
            "an hour the change of clocks skipped keeps its day",
            day.to_string()
        ),
    }
}

/// The time of day `text` names: the time of day a field holds (`09:00`), or the time of day an
/// instant falls on; `None` for no time.
pub(crate) fn time_in(text: Option<&str>) -> Option<Time> {
    let text = text?;
    if let Ok(time) = Time::strptime("%H:%M", text.trim()) {
        return Some(time);
    }
    tilo_failure::ignored!(
        "a value that is no time is shown as no time",
        text.parse::<jiff::Timestamp>()
            .ok()
            .map(|at| tilo_ui_base::clock::at(at).time())
    )
}

/// What the field's chip says and how it stands: its name, what the day is for, whether the day
/// may be empty, the calendar alone as a control of its own, and the time controls that stand
/// over the calendar.
struct Chip<'a> {
    id: &'a str,
    label: Option<&'a str>,
    optional: bool,
    compact: bool,
    time: Vec<Element>,
}

/// The chip and its popover: the time the day is due at (the `time` part), the calendar, and
/// "Clear date" when the date may be empty.
fn field(
    chip: Chip<'_>,
    value: Option<Date>,
    at: Option<Time>,
    picked: &Picked,
    shown: &tilo_ui_base::LocalState<Date>,
) -> PopoverButton<Stack> {
    let Chip {
        id,
        label,
        optional,
        compact,
        time,
    } = chip;
    let today = tilo_ui_base::clock::local_now().date();
    let on_day = Rc::clone(picked);
    // The calendar is its own name inside the field: the chip carries what the day is for,
    // so a screen reader hears "Due, Today" on the field and "Calendar" on what opens over it,
    // never the same name on both.
    let picker = DatePicker::from_state(shown)
        .with_today(today)
        .with_accessible_name(tilo_ui_base::i18n::widget_format(
            "date-field-calendar",
            &[],
        ))
        .on_choose(format!("{id}-calendar"), move |day| {
            on_day.set(Some(Some(day)))
        });
    let mut content = Stack::stack();
    for view in time {
        content = content.push(view);
    }
    content = content.push(Box::new(picker));
    if optional {
        let on_clear = Rc::clone(picked);
        let clear = Button::new(
            &format!("{id}-clear"),
            &tilo_ui_base::i18n::widget_format("date-field-clear", &[]),
        )
        .role(ButtonRole::Plain)
        .on_open(move || on_clear.set(Some(None)));
        content = content.push(Box::new(clear));
    }
    let (fact, tone) = DateField::fact(value, at);
    let spoken = DateField::spoken(label, value, at);
    if compact {
        return Button::new(id, "")
            .with_icon(Icon::Calendar)
            .role(ButtonRole::Plain)
            .tooltip(&spoken)
            .tone(tone)
            .with_accessible_name(&spoken)
            .with_popover(content);
    }
    let chip = Button::new(id, &fact)
        .with_icon(Icon::Calendar)
        .showing_choice()
        .tone(tone);
    let chip = if value.is_some() {
        chip
    } else {
        chip.with_placeholder()
    };
    chip.with_accessible_name(&spoken).with_popover(content)
}

impl Decorator for DateField {
    type Inner = PopoverButton<Stack>;

    fn inner(&self) -> &PopoverButton<Stack> {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut PopoverButton<Stack> {
        &mut self.inner
    }

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

    tilo_test_env::scenario! {
        /// What a date field shows and reports.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_date_field_shows_reports_and_follows_its_day;

        phase a_picked_day_is_shown_and_reported_and_clearing_reports_no_date;
        {
            let reported: Rc<Cell<Option<Option<Date>>>> = Rc::default();
            let seen = Rc::clone(&reported);
            let day = Date::new(2026, 10, 24).ok();
            let mut field = DateField::new("due", None, true).on_change(move |value| {
                seen.set(Some(value));
            });
            assert_eq!(DateField::label(None, None), "Add date");
            field.picked.set(Some(day));
            assert!(field.take_pick());
            assert_eq!(field.value(), day);
            assert_eq!(reported.take(), Some(day));
            field.picked.set(Some(None));
            assert!(field.take_pick());
            assert_eq!(field.value(), None);
            assert_eq!(reported.take(), Some(None));
        }

        phase a_day_with_its_time_reads_as_one_fact;
        {
            let nine = Time::new(9, 0, 0, 0).ok();
            let day = tilo_ui_base::clock::today();
            assert_eq!(
                DateField::label(Some(day), nine),
                tilo_ui_base::format::day_fact_today(day, nine).text,
                "the chip says the day as the platform reads a day, with its time"
            );
            assert_eq!(
                DateField::label(Some(day), None),
                "Today",
                "a day with no time says only the day"
            );
        }

        phase a_late_day_reads_in_the_overdue_tone;
        {
            let today = tilo_ui_base::clock::today();
            let tomorrow = today.tomorrow().expect("a day");
            let late = today.yesterday().expect("a day");
            assert_eq!(DateField::fact(Some(tomorrow), None).1, Tone::Neutral);
            assert_eq!(DateField::fact(Some(late), None).1, Tone::Danger);
        }

        phase a_date_from_elsewhere_is_shown_without_being_reported;
        {
            let reported: Rc<Cell<bool>> = Rc::default();
            let seen = Rc::clone(&reported);
            let mut field = DateField::new("due", None, false).on_change(move |_| seen.set(true));
            let day = Date::new(2026, 11, 2).expect("a day");
            field.set_value(Some(day));
            assert_eq!(field.value(), Some(day));
            assert!(!field.take_pick());
            assert!(!reported.get());
            assert_eq!(
                field.shown.with(|shown| *shown),
                day,
                "the calendar follows the day the field shows"
            );
        }

        phase a_time_from_elsewhere_changes_only_what_the_chip_says;
        {
            let day = Date::new(2026, 6, 30).ok();
            let mut field = DateField::new("due", day, true);
            field.set_at(Time::new(9, 0, 0, 0).ok());
            assert_eq!(field.at(), Time::new(9, 0, 0, 0).ok());
            assert_eq!(
                field.value(),
                day,
                "the day a time is set for stays the day it was"
            );
        }

        phase a_bound_day_shows_and_one_set_while_the_calendar_is_open_waits_for_it;
        {
            let state = tilo_ui_base::LocalState::new("2026-06-30".to_owned());
            let mut field = DateField::from_state("due", &state, true, None, Vec::new(), None, false);
            assert_eq!(
                field.value(),
                Date::new(2026, 6, 30).ok(),
                "the day the binding holds is shown"
            );
            state.set("2026-07-01".to_owned());
            Decorator::sync(&mut field, &(), &());
            assert_eq!(
                field.value(),
                Date::new(2026, 7, 1).ok(),
                "a day set elsewhere shows in place"
            );
            field.inner_mut().open();
            state.set("2026-07-02".to_owned());
            Decorator::sync(&mut field, &(), &());
            assert_eq!(
                field.value(),
                Date::new(2026, 7, 1).ok(),
                "a day set while the calendar is open does not move it"
            );
            field.inner_mut().close();
            assert!(Decorator::update(&mut field, &(), &()));
            assert_eq!(
                field.value(),
                Date::new(2026, 7, 2).ok(),
                "the day it took shows once the calendar closes"
            );
        }
    }
}

//! `DateField`: a day, shown as one fact and picked in a popover.
//!
//! The field reads as a chip: the calendar icon and the day as the person reads a day
//! ([`crate::format::day_fact_today`]: "Overdue since Jun 29", "Today", "Tomorrow", "Thursday",
//! with the time of day it is due at when it has one — "Today 9:00 AM"), in the tone the fact
//! stands in, or "Add date" in the quieter ink while it holds none, and the chevron every
//! choice control wears. Pressing it opens a [`DatePicker`] in a popover anchored to the chip;
//! picking a day sets the date and closes it. A field whose date may be empty adds "Clear date" under the calendar.
//!
//! The time of day the day is due at is picked in the same popover, in the field's `time` slot:
//! the app keeps one beside the day (`at: Option<Time>` beside `due`), so the two are one
//! fact and one control, never a second chip beside it. What the chip says comes from the `at` the
//! owner shows it, through [`DateField::set_at`]: the field itself changes nothing but the day.
//!
//! The owner learns of a change through [`DateField::on_change`] and tells the field a date from
//! elsewhere with [`DateField::set_value`], which changes the chip in place and never builds the
//! popover again (what it holds — the calendar, the time row — stays what it was). The field
//! never holds the authoritative value. A described component's field follows the day its
//! binding holds ([`DateField::from_state`]).

use std::cell::Cell;
use std::rc::Rc;

use chrono::{NaiveDate, NaiveTime};

use crate::icons::Icon;
use crate::widget::arrange::Stack;
use crate::widget::binding::{BoxedBinding, Pulled, derived};
use crate::widget::{
    AppModel, Button, ButtonRole, DatePicker, Decorator, Element, ModelPath, PopoverButton, Tone,
    Widget,
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
    value: Option<NaiveDate>,
    /// The time of day the day is due at, when its type pairs one with it: what the chip says
    /// after the day, and nothing the field itself picks.
    at: Option<NaiveTime>,
    /// What the day is for ("Due"), as the person's app named it: what a screen reader hears
    /// before the day itself. The calendar inside the popover keeps its own name.
    label: Option<String>,
    optional: bool,
    picked: Picked,
    /// The day the calendar shows and highlights: the day the person picked, and the one the
    /// owner set from elsewhere, so the calendar never disagrees with the chip.
    shown: crate::LocalState<NaiveDate>,
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
        Self::built(id, value, None, optional, Vec::new(), None)
    }

    fn built(
        id: &str,
        value: Option<NaiveDate>,
        at: Option<NaiveTime>,
        optional: bool,
        time: Vec<Element>,
        label: Option<&str>,
    ) -> Self {
        let picked: Picked = Rc::default();
        let shown = crate::LocalState::new(value.unwrap_or_else(crate::clock::today));
        Self {
            inner: field(id, value, at, optional, &picked, &shown, time, label),
            value,
            at,
            label: label.map(str::to_owned),
            optional,
            picked,
            shown,
            on_change: None,
            bound: None,
            waiting: Waiting::Nothing,
        }
    }

    /// A field following the day `state` holds as its ISO text (`""` for no date), which the
    /// person may clear when `optional`, with `time` standing in its popover as the control
    /// that picks `at`, the time of day the day is due at: a day set there shows in place.
    pub(super) fn from_state(
        id: &str,
        state: &crate::LocalState<String>,
        optional: bool,
        at: Option<&str>,
        time: Vec<Element>,
        label: Option<&str>,
    ) -> Self {
        let (day, binding) = derived(state, |text| crate::component::moment_day(text));
        let at = time_in(at);
        let mut field = Self::built(id, day, at, optional, time, label);
        field.bound = Some(binding);
        field
    }

    /// What the popover holds, which the `time` slot changes in place: the time row, the
    /// calendar, "Clear date".
    pub(super) fn popover_content_mut(&mut self) -> &mut Stack {
        self.inner.popover_mut().content_mut()
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

    /// The time of day the chip shows after the day.
    #[must_use]
    pub fn at(&self) -> Option<NaiveTime> {
        self.at
    }

    /// Whether the person may clear the date.
    #[must_use]
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// Show `value`, a date set elsewhere: the chip and the calendar follow it in place.
    pub fn set_value(&mut self, value: Option<NaiveDate>) {
        if value == self.value {
            return;
        }
        self.value = value;
        self.shown.set(value.unwrap_or_else(crate::clock::today));
        self.show_label();
    }

    /// Show `at`, the time of day the day is due at, set elsewhere: only what the chip says
    /// changes.
    pub fn set_at(&mut self, at: Option<NaiveTime>) {
        if at == self.at {
            return;
        }
        self.at = at;
        self.show_label();
    }

    /// Say what the chip stands for now, on the chip it already is: what it shows, and what a
    /// screen reader hears of it.
    fn show_label(&mut self) {
        let (label, tone) = Self::fact(self.value, self.at);
        let spoken = Self::spoken(self.label.as_deref(), self.value, self.at);
        let chip = self.inner.button_mut();
        chip.set_label(label, self.value.is_none());
        chip.set_tone(tone);
        chip.set_accessible_name(&spoken);
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
            self.set_value(value);
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

    /// What the field reads as, and the tone it says it in: the day and the time it is due at
    /// ("Today 9:00 AM", "Overdue since Jun 29"), or "Add date" while it holds none.
    #[must_use]
    pub fn fact(value: Option<NaiveDate>, at: Option<NaiveTime>) -> (String, Tone) {
        value.map_or_else(
            || {
                (
                    crate::i18n::widget_format("date-field-add", &[]),
                    Tone::Neutral,
                )
            },
            |day| {
                let fact = crate::format::day_fact_today(day, at);
                (fact.text, fact.tone)
            },
        )
    }

    /// What the field reads as: [`Self::fact`]'s words.
    #[must_use]
    pub fn label(value: Option<NaiveDate>, at: Option<NaiveTime>) -> String {
        Self::fact(value, at).0
    }

    /// What a screen reader hears of the chip: what the day is for and the day it holds
    /// ("Due, Today 9:00 AM"), or the day alone when nothing names it.
    #[must_use]
    pub fn spoken(label: Option<&str>, value: Option<NaiveDate>, at: Option<NaiveTime>) -> String {
        let day = Self::label(value, at);
        label.map_or(day.clone(), |label| {
            crate::i18n::widget_format(
                "date-field-named",
                &[("label", label.into()), ("day", day.into())],
            )
        })
    }
}

/// The moment `text` holds with its day replaced by `day`, its time of day kept: the ISO text
/// of the instant when it holds a time, the day alone when it holds none, and nothing for no
/// day. A moment is one value whichever part of it the person changes.
pub(crate) fn with_day(text: &str, day: Option<NaiveDate>) -> String {
    let Some(day) = day else {
        return String::new();
    };
    match time_in(Some(text)) {
        Some(time) => instant_text(day, time),
        None => day.format("%Y-%m-%d").to_string(),
    }
}

/// The moment `text` holds with its time of day replaced by `time`, its day kept: the ISO text
/// of the instant when it holds a day, the time of day alone when it holds none (the time of
/// day of another field), and nothing for no time.
pub(crate) fn with_time(text: &str, time: Option<NaiveTime>) -> String {
    let Some(time) = time else {
        return String::new();
    };
    match crate::component::moment_day(text) {
        Some(day) => instant_text(day, time),
        // The field's value is the clock a person picked, which reading the field back reads:
        // machine text, not their clock style.
        // format: machine
        None => time.format("%H:%M").to_string(),
    }
}

/// The instant `day` at `time` in the person's zone, as ISO-8601 UTC text.
fn instant_text(day: NaiveDate, time: NaiveTime) -> String {
    use chrono::TimeZone as _;
    let local = day.and_time(time);
    let at = match chrono::Local.from_local_datetime(&local) {
        chrono::LocalResult::Single(at) | chrono::LocalResult::Ambiguous(at, _) => Some(at),
        // ignored-failure: an hour the change of clocks skipped keeps its day
        chrono::LocalResult::None => None,
    };
    at.map_or_else(
        || day.format("%Y-%m-%d").to_string(),
        |at| {
            at.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        },
    )
}

/// The time of day `text` names: the time of day a field holds (`09:00`), or the time of day an
/// instant falls on; `None` for no time.
pub(crate) fn time_in(text: Option<&str>) -> Option<NaiveTime> {
    let text = text?;
    if let Ok(time) = NaiveTime::parse_from_str(text.trim(), "%H:%M") {
        return Some(time);
    }
    // ignored-failure: a value that is no time is shown as no time
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&chrono::Local).time())
}

/// The chip and its popover: the time the day is due at (the `time` slot), the calendar, and
/// "Clear date" when the date may be empty.
fn field(
    id: &str,
    value: Option<NaiveDate>,
    at: Option<NaiveTime>,
    optional: bool,
    picked: &Picked,
    shown: &crate::LocalState<NaiveDate>,
    time: Vec<Element>,
    label: Option<&str>,
) -> PopoverButton<Stack> {
    let today = crate::clock::local_now().date_naive();
    let on_day = Rc::clone(picked);
    // The calendar is its own name inside the field: the chip carries what the day is for,
    // so a screen reader hears "Due, Today" on the field and "Calendar" on what opens over it,
    // never the same name on both.
    let picker = DatePicker::from_state(shown)
        .with_today(today)
        .with_accessible_name(crate::i18n::widget_format("date-field-calendar", &[]))
        .on_view(format!("{id}-calendar"), move |day| {
            on_day.set(Some(Some(day)))
        });
    // The time comes first: a day and the time it is due at read as one fact, and the row that
    // picks the time stands with the day it belongs to, over the calendar.
    let mut content = Stack::stack();
    for view in time {
        content = content.push(view);
    }
    content = content.push(Box::new(picker));
    if optional {
        let on_clear = Rc::clone(picked);
        let clear = Button::new(
            &format!("{id}-clear"),
            &crate::i18n::widget_format("date-field-clear", &[]),
        )
        .role(ButtonRole::Plain)
        .on_view(move || on_clear.set(Some(None)));
        content = content.push(Box::new(clear));
    }
    let (fact, tone) = DateField::fact(value, at);
    let chip = Button::new(id, &fact)
        .with_icon(Icon::Calendar)
        .showing_choice()
        .tone(tone);
    let chip = if value.is_some() {
        chip
    } else {
        chip.with_placeholder()
    };
    chip.with_accessible_name(&DateField::spoken(label, value, at))
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

    #[test]
    fn a_day_with_its_time_reads_as_one_fact() {
        let nine = NaiveTime::from_hms_opt(9, 0, 0);
        let day = crate::clock::today();
        assert_eq!(
            DateField::label(Some(day), nine),
            crate::format::day_fact_today(day, nine).text,
            "the chip says the day as the platform reads a day, with its time"
        );
        assert_eq!(
            DateField::label(Some(day), None),
            "Today",
            "a day with no time says only the day"
        );
    }

    #[test]
    fn a_late_day_reads_in_the_overdue_tone() {
        let today = crate::clock::today();
        let tomorrow = today.succ_opt().expect("a day");
        let late = today.pred_opt().expect("a day");
        assert_eq!(DateField::fact(Some(tomorrow), None).1, Tone::Neutral);
        assert_eq!(DateField::fact(Some(late), None).1, Tone::Danger);
    }

    #[test]
    fn a_date_from_elsewhere_is_shown_without_being_reported() {
        let reported: Rc<Cell<bool>> = Rc::default();
        let seen = Rc::clone(&reported);
        let mut field = DateField::new("due", None, false).on_change(move |_| seen.set(true));
        let day = NaiveDate::from_ymd_opt(2026, 11, 2).expect("a day");
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

    #[test]
    fn a_time_from_elsewhere_changes_only_what_the_chip_says() {
        let day = NaiveDate::from_ymd_opt(2026, 6, 30);
        let mut field = DateField::new("due", day, true);
        field.set_at(NaiveTime::from_hms_opt(9, 0, 0));
        assert_eq!(field.at(), NaiveTime::from_hms_opt(9, 0, 0));
        assert_eq!(
            field.value(),
            day,
            "the day a time is set for stays the day it was"
        );
    }

    #[test]
    fn a_bound_day_shows_and_one_set_while_the_calendar_is_open_waits_for_it() {
        let state = crate::LocalState::new("2026-06-30".to_owned());
        let mut field = DateField::from_state("due", &state, true, None, Vec::new(), None);
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

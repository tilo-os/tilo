//! Dates and times as people read them, and the `date` control.
//!
//! A `Date` or `Time` reaches the view as its ISO text with a sibling prop `<prop>:as` (`"date"`
//! or `"time"`); [`shown`] formats it the person's way, and [`shown_brief`] as a list reads it,
//! through the toolkit's one [day fact](ui::format::day_fact). A day paired with the time of day
//! it is due ([`ui::format::DayFact`]) gives the sibling prop `<prop>:time`, and the two read as
//! one fact (`Today 09:00`). The `date` control is the toolkit's [`DateField`] over the bound
//! value, which it reports as ISO text (empty for no date) through the same edit every bound
//! input reports.

use crate::{UiNode, Value};
use chrono::{DateTime, Local, NaiveDate, NaiveTime, Utc};
use ui::LocalState;
use ui::widget::{DateField, Decorator, Tone, Widget};

use crate::reconcile::{EventSink, UiEvent, display};

/// The prop `name` of `node` as people read it: a date or a time in their format (a time with
/// its date), anything else as [`display`] shows it.
#[must_use]
pub fn shown(node: &UiNode, name: &str) -> Option<String> {
    let value = node.props.get(name)?;
    let text = display(value);
    if let Some(Value::Text(kind)) = node.props.get(&format!("{name}:as"))
        && kind == "date"
        && let Some(day) = day_of(&text)
        && let Some(time) = paired_time(node, name)
    {
        return Some(ui::format::timestamp(&day.and_time(time)));
    }
    formatted(node, name, ui::format::date, ui::format::timestamp)
}

/// The prop `name` of `node` as a list row reads it: the day fact (`Overdue`, `Today`,
/// `Thursday`, `Jun 29`), with the time of day paired with it where the view names one
/// (`Today 09:00`); a time is [`ui::format::list_date`], as native lists show when a message
/// came. The day something was done on (`<name>_done`) reads as the day it happened
/// (`Yesterday`, `Sunday`, `Jun 21`): what a heading over finished work says, never a day gone
/// by.
#[must_use]
pub fn shown_brief(node: &UiNode, name: &str) -> Option<String> {
    let value = node.props.get(name)?;
    let text = display(value);
    let Some(Value::Text(kind)) = node.props.get(&format!("{name}:as")) else {
        return Some(text);
    };
    let done = done_day(node, name);
    Some(match kind.as_str() {
        "date" if done => day_of(&text).map_or(text, ui::format::day_done_today),
        "date" => day_of(&text).map_or(text, |day| {
            ui::format::day_fact_today(day, paired_time(node, name)).text
        }),
        "time" => moment_of(&text).map_or(text, |at| ui::format::list_date(&at)),
        "duration" => duration_of(&text).map_or(text, ui::format::duration),
        _ => text,
    })
}

/// Whether the prop `name` of `node` is the day the item was done on, as the view says with
/// its `<name>_done` companion: the day it happened, not a day it is due by.
fn done_day(node: &UiNode, name: &str) -> bool {
    crate::reconcile::flag(node, &format!("{name}_done"))
}

/// A choice's case as a row tells it at its trailing edge, where the choice says what its cases
/// hold (`case:as`): a moment as a person reads the hour ("15:00", "Sat 09:00"), a length of
/// time as they read it ("1 hr 30 min"). `None` where the case's value is not of that kind,
/// so a choice of words shows its labels alone.
#[must_use]
pub fn shown_case(node: &UiNode, value: &str) -> Option<String> {
    let Some(Value::Text(kind)) = node.props.get("case:as") else {
        return None;
    };
    match kind.as_str() {
        "time" => moment_of(value).map(|at| ui::format::moment_fact(&at)),
        "duration" => duration_of(value).map(ui::format::duration),
        _ => None,
    }
}

/// How the stored `value` of a day or a time of day (`kind` is `date` or `time`) reads in a
/// sentence where no view can stand beside it: a day as the day fact (`Today`, `Friday`,
/// `Jun 29`), a time of day as a list shows when something came. Anything else, and text that
/// is not a day or an instant, stays as it is stored.
#[must_use]
pub fn shown_text(kind: &str, value: &str) -> String {
    let now = ui::clock::local_now();
    tilo_formats::shown(
        kind,
        value,
        ui::format::region::current(),
        now.date_naive(),
        now.naive_local(),
        i64::from(now.offset().local_minus_utc()),
        ui::format::words(),
    )
}

/// The tone the prop `name` of `node` reads in, when it reads in one: a day gone by is overdue
/// ([`Tone::Danger`]). `None` for anything else, and for a day that is not gone by.
#[must_use]
pub fn shown_tone(node: &UiNode, name: &str) -> Option<Tone> {
    let Some(Value::Text(kind)) = node.props.get(&format!("{name}:as")) else {
        return None;
    };
    // A day something was done on is history: it is never overdue.
    if kind != "date" || done_day(node, name) {
        return None;
    }
    let day = day_of(&display(node.props.get(name)?))?;
    let fact = ui::format::day_fact_today(day, paired_time(node, name));
    (fact.tone != Tone::Neutral).then_some(fact.tone)
}

/// The time of day paired with the day `name` holds, in the person's zone: the view gives it as
/// the sibling prop `<name>:time`.
fn paired_time(node: &UiNode, name: &str) -> Option<NaiveTime> {
    let text = display(node.props.get(&format!("{name}:time"))?);
    moment_of(&text).map(|at| at.with_timezone(&Local).time())
}

fn formatted(
    node: &UiNode,
    name: &str,
    date: impl Fn(NaiveDate) -> String,
    time: impl Fn(&DateTime<Utc>) -> String,
) -> Option<String> {
    let value = node.props.get(name)?;
    let text = display(value);
    let Some(Value::Text(kind)) = node.props.get(&format!("{name}:as")) else {
        return Some(text);
    };
    Some(match kind.as_str() {
        "date" => day_of(&text).map_or(text, date),
        "time" => moment_of(&text).map_or(text, |at| time(&at)),
        "duration" => duration_of(&text).map_or(text, ui::format::duration),
        _ => text,
    })
}

/// The calendar day `YYYY-MM-DD`.
fn day_of(text: &str) -> Option<NaiveDate> {
    // ignored-failure: a value that is not a day is shown as the text it is
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// A length of time, held as its number of seconds.
fn duration_of(text: &str) -> Option<std::time::Duration> {
    // ignored-failure: a value that is no number of seconds is shown as the text it is
    text.trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

/// The instant `YYYY-MM-DDTHH:MM:SSZ`.
fn moment_of(text: &str) -> Option<DateTime<Utc>> {
    // ignored-failure: a value that is not an instant is shown as the text it is
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// The `date` control: a [`DateField`] following the bound value the window keeps in `value`.
pub struct DateControl {
    field: DateField,
    value: LocalState<String>,
}

impl DateControl {
    /// The control of the bound node `key`, showing `value`; a date that may be empty when
    /// `optional`.
    #[must_use]
    pub fn new(key: &str, value: &LocalState<String>, sink: &EventSink, optional: bool) -> Self {
        let day = value.with(|text| day_of(text));
        let sink = std::rc::Rc::clone(sink);
        let reported = value.clone();
        let edited = key.to_owned();
        let field = DateField::new(key, day, optional).on_change(move |day| {
            let text = day
                .map(|day| day.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            reported.set(text.clone());
            sink.push(UiEvent::Edit(edited.clone(), text));
        });
        Self {
            field,
            value: value.clone(),
        }
    }
}

impl DateControl {
    /// Show the value the window last read, a date set elsewhere.
    fn follow_value(&mut self) -> bool {
        let day = self.value.with(|text| day_of(text));
        if day == self.field.value() {
            return false;
        }
        self.field.set_value(day);
        true
    }
}

impl Decorator for DateControl {
    type Inner = DateField;

    fn inner(&self) -> &DateField {
        &self.field
    }

    fn inner_mut(&mut self) -> &mut DateField {
        &mut self.field
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let followed = self.follow_value();
        let inner = Widget::update(&mut self.field, state, path);
        followed || inner
    }

    fn name(&self) -> &'static str {
        "TilolangDate"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(props: &[(&str, &str)]) -> UiNode {
        let mut node = UiNode {
            key: "n".to_owned(),
            kind: "text".to_owned(),
            props: std::collections::BTreeMap::new(),
            binding: None,
            events: std::collections::BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        };
        for (name, value) in props {
            node.props.insert((*name).to_owned(), Value::text(*value));
        }
        node
    }

    #[test]
    fn a_date_reads_in_the_persons_format_and_other_text_as_it_is() {
        let day = node(&[("value", "2026-10-24"), ("value:as", "date")]);
        assert_eq!(
            shown(&day, "value").as_deref(),
            Some(ui::format::date(NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()).as_str())
        );
        let none = node(&[("value", ""), ("value:as", "date")]);
        assert_eq!(shown(&none, "value").as_deref(), Some(""));
        let plain = node(&[("value", "2026-10-24")]);
        assert_eq!(shown(&plain, "value").as_deref(), Some("2026-10-24"));
    }

    #[test]
    fn a_picked_day_is_reported_as_iso_text() {
        let value = LocalState::new(String::new());
        let sink: EventSink = std::rc::Rc::default();
        let mut control = DateControl::new("due", &value, &sink, true);
        value.set("2026-11-02".to_owned());
        assert!(control.follow_value());
        assert_eq!(control.field.value(), NaiveDate::from_ymd_opt(2026, 11, 2));
        assert!(sink.is_empty(), "showing a value reports no edit");
    }

    /// The instant today's person's clock shows at `hour`.
    fn at(hour: u32) -> String {
        use chrono::TimeZone;
        Local
            .with_ymd_and_hms(2026, 6, 30, hour, 0, 0)
            .single()
            .expect("nine in the morning exists")
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    #[test]
    fn a_day_reads_as_briefly_as_a_list_states_it() {
        let today = ui::clock::today();
        let yesterday = today - chrono::Days::new(1);
        let day = yesterday.format("%Y-%m-%d").to_string();
        let row = node(&[("value", &day), ("value:as", "date")]);
        assert_eq!(
            shown_brief(&row, "value"),
            Some(ui::format::day_fact_today(yesterday, None).text)
        );
        assert_eq!(shown_tone(&row, "value"), Some(Tone::Danger));

        let today_node = node(&[("value", &today.to_string()), ("value:as", "date")]);
        assert_eq!(shown_tone(&today_node, "value"), None, "today is not late");
    }

    #[test]
    fn a_done_day_reads_as_the_day_it_happened_and_is_never_overdue() {
        let today = ui::clock::today();
        let done = today - chrono::Days::new(40);
        let day = done.format("%Y-%m-%d").to_string();
        let mut row = node(&[("group", &day), ("group:as", "date")]);
        // The runtime lowers a flag prop as a whole number, as the view's `true` reaches a node.
        row.props.insert("group_done".to_owned(), Value::Int(1));
        assert_eq!(
            shown_brief(&row, "group"),
            Some(ui::format::day_done_today(done)),
            "a heading over finished work says the day it happened"
        );
        assert_eq!(
            shown_tone(&row, "group"),
            None,
            "what was finished is history, not late"
        );
    }

    #[test]
    fn a_day_and_the_time_it_is_due_read_as_one_fact() {
        let today = ui::clock::today();
        let day = today.format("%Y-%m-%d").to_string();
        let time = NaiveTime::from_hms_opt(9, 0, 0).expect("nine");
        let alone = node(&[("value", &day), ("value:as", "date")]);
        let paired = node(&[
            ("value", &day),
            ("value:as", "date"),
            ("value:time", &at(9)),
        ]);
        assert_eq!(
            shown_brief(&paired, "value"),
            Some(ui::format::day_fact_today(today, Some(time)).text)
        );
        assert_ne!(
            shown_brief(&paired, "value"),
            shown_brief(&alone, "value"),
            "the time of the day is part of the fact"
        );
        assert_eq!(
            shown(&paired, "value"),
            Some(ui::format::timestamp(&today.and_time(time))),
            "a form states the day and its time together"
        );
        assert_eq!(
            shown_tone(&paired, "value"),
            None,
            "a day still to come is on time"
        );
    }
}

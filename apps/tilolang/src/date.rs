//! Dates and times as people read them, and the `date` control.
//!
//! A `Date` or `Time` reaches the view as its ISO text with a sibling prop `<prop>:as` (`"date"`
//! or `"time"`); [`shown`] formats it the person's way. The `date` control is the toolkit's
//! [`DateField`] over the bound value, which it reports as ISO text (empty for no date) through
//! the same edit every bound input reports.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use tilolang_tilo::runtime::{UiNode, Value};
use ui::LocalState;
use ui::widget::{DateField, Decorator, Widget};

use crate::reconcile::{EventSink, UiEvent, display};

/// The prop `name` of `node` as people read it: a date or a time in their format (a time with
/// its date), anything else as [`display`] shows it.
#[must_use]
pub fn shown(node: &UiNode, name: &str) -> Option<String> {
    formatted(node, name, ui::format::date, ui::format::timestamp)
}

/// The prop `name` of `node` as a list row reads it: as briefly as it is still clear today. A
/// time is `16:02`, `Yesterday` or `Jun 30`, as native lists show when a message came; a day is
/// `Tomorrow`, `Oct 24`, or with its year when it is not this year's.
#[must_use]
pub fn shown_brief(node: &UiNode, name: &str) -> Option<String> {
    formatted(node, name, brief_day, ui::format::list_date)
}

/// A day as briefly as it is clear from today.
fn brief_day(day: NaiveDate) -> String {
    let today = ui::clock::today();
    if let Some(near) = ui::format::near_day(day.signed_duration_since(today).num_days()) {
        return near;
    }
    if day.year() == today.year() {
        ui::format::month_day(day)
    } else {
        ui::format::month_day_year(day)
    }
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
        _ => text,
    })
}

/// The calendar day `YYYY-MM-DD`.
fn day_of(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// The instant `YYYY-MM-DDTHH:MM:SSZ`.
fn moment_of(text: &str) -> Option<DateTime<Utc>> {
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
            sink.borrow_mut().push(UiEvent::Edit(edited.clone(), text));
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
        assert!(sink.borrow().is_empty(), "showing a value reports no edit");
    }
}

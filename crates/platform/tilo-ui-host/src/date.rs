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
///
/// A row's day fact standing under a heading that says the same day — the group, card or
/// section above it — is the heading's to say: the row keeps only the time of day it is due
/// (`09:00`), and says nothing where it names no time. That is no day fact of the row's own,
/// and none where the row stands under no heading (`under`).
#[must_use]
pub fn shown_brief(node: &UiNode, name: &str, under: Option<&str>) -> Option<String> {
    let value = node.props.get(name)?;
    let text = display(value);
    let Some(Value::Text(kind)) = node.props.get(&format!("{name}:as")) else {
        return Some(text);
    };
    let done = done_day(node, name);
    if let Some(left) = under.and_then(|heading| said_by_heading(node, name, heading)) {
        return left;
    }
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

/// The prop a row states the day it happened or is due in at its trailing edge, where its kind
/// has one: the fact a heading over the row can say for it.
fn trailing_day(node: &UiNode) -> Option<&'static str> {
    match crate::adapter::component_name(&node.kind)? {
        "ListRow" => Some("meta"),
        "TreeViewRow" => Some("trailing"),
        _ => None,
    }
}

/// What is left of the row `node`'s trailing day fact where `heading` says the day: the time of
/// day it is due alone, or nothing where it names no time. `None` where the fact is the row's
/// own to say, and for a prop that is no trailing day fact of a row.
fn said_by_heading(node: &UiNode, name: &str, heading: &str) -> Option<Option<String>> {
    if trailing_day(node) != Some(name) || done_day(node, name) {
        return None;
    }
    let day = day_of(&display(node.props.get(name)?))?;
    if !says_day(heading, day) {
        return None;
    }
    Some(paired_time(node, name).map(|time| ui::format::time(&time)))
}

/// The day the heading over the rows `node` holds says, where it heads them by a day: its
/// `group` prop. `None` where it names none, and where the group is the day a row was done on —
/// a heading over finished work heads the day it was done, never the day a row is due.
#[must_use]
pub fn group_heading(node: &UiNode) -> Option<String> {
    if done_day(node, "group") {
        return None;
    }
    day_heading(node, "group")
}

/// The words the prop `name` of `node` reads as a heading over what it holds says the day in
/// (`Today`, `Overdue since Jun 29`): the day fact without a time of day. `None` where the prop
/// holds no day.
fn day_heading(node: &UiNode, name: &str) -> Option<String> {
    if !matches!(node.props.get(&format!("{name}:as")), Some(Value::Text(kind)) if kind == "date") {
        return None;
    }
    let day = day_of(&display(node.props.get(name)?))?;
    Some(ui::format::day_fact_today(day, None).text)
}

/// Whether `heading`, the words over a row, says `day`: the day fact without a time of day.
fn says_day(heading: &str, day: NaiveDate) -> bool {
    heading
        .trim()
        .eq_ignore_ascii_case(&ui::format::day_fact_today(day, None).text)
}

/// The words `node` heads what it holds with: a card's, a section's or a window's title. `None`
/// for a node that heads nothing, and for a title with no words.
#[must_use]
pub fn heading_of(node: &UiNode) -> Option<String> {
    match crate::adapter::component_name(&node.kind)? {
        "Card" | "Section" | "NavShell" => {}
        _ => return None,
    }
    match node.props.get("title") {
        Some(Value::Text(words)) if !words.trim().is_empty() => Some(words.clone()),
        _ => None,
    }
}

/// The tree with each row's trailing day fact dropped where a heading over it says that day:
/// the group, card or section above the rows says the day already, so the row keeps the time of
/// day it is due (`09:00`), and shows nothing where it names no time.
#[must_use]
pub fn under_headings(node: UiNode) -> UiNode {
    fn walk(mut node: UiNode, heading: Option<&str>) -> UiNode {
        let here = heading_of(&node)
            .or_else(|| group_heading(&node))
            .or_else(|| heading.map(str::to_owned));
        if let Some(name) = trailing_day(&node)
            && let Some(left) = said_by_heading(&node, name, here.as_deref().unwrap_or_default())
        {
            match left {
                Some(of_day) => {
                    node.props.insert(name.to_owned(), Value::text(of_day));
                }
                None => {
                    node.props.remove(name);
                }
            }
            node.props.remove(&format!("{name}:as"));
            node.props.remove(&format!("{name}:time"));
        }
        node.children = node
            .children
            .into_iter()
            .map(|child| walk(child, here.as_deref()))
            .collect();
        node
    }
    walk(node, None)
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
pub fn shown_tone(node: &UiNode, name: &str, under: Option<&str>) -> Option<Tone> {
    let Some(Value::Text(kind)) = node.props.get(&format!("{name}:as")) else {
        return None;
    };
    // A day something was done on is history: it is never overdue.
    if kind != "date" || done_day(node, name) {
        return None;
    }
    let day = day_of(&display(node.props.get(name)?))?;
    // A day the heading over the row says is no fact of the row's own: no tone of its own.
    if under.is_some_and(|heading| says_day(heading, day)) && trailing_day(node) == Some(name) {
        return None;
    }
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
            shown_brief(&row, "value", None),
            Some(ui::format::day_fact_today(yesterday, None).text)
        );
        assert_eq!(shown_tone(&row, "value", None), Some(Tone::Danger));

        let today_node = node(&[("value", &today.to_string()), ("value:as", "date")]);
        assert_eq!(
            shown_tone(&today_node, "value", None),
            None,
            "today is not late"
        );
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
            shown_brief(&row, "group", None),
            Some(ui::format::day_done_today(done)),
            "a heading over finished work says the day it happened"
        );
        assert_eq!(
            shown_tone(&row, "group", None),
            None,
            "what was finished is history, not late"
        );
    }

    /// A row of a kind that states a day fact at its trailing edge, of `props`.
    fn row(kind: &str, props: &[(&str, &str)]) -> UiNode {
        let mut row = node(props);
        row.kind = crate::adapter::kind(kind);
        row
    }

    /// The words the day fact of `day` says, as a heading over a row says it.
    fn heading(day: NaiveDate) -> String {
        ui::format::day_fact_today(day, None).text
    }

    #[test]
    fn a_day_fact_a_heading_over_the_row_says_is_the_headings_to_say() {
        let today = ui::clock::today();
        let day = today.format("%Y-%m-%d").to_string();
        let fact = row("ListRow", &[("meta", &day), ("meta:as", "date")]);
        assert_eq!(
            shown_brief(&fact, "meta", Some(&heading(today))),
            None,
            "the heading says the day: the row says nothing of its own"
        );
        assert_eq!(
            shown_brief(&fact, "meta", None),
            Some(heading(today)),
            "under no heading the row says the day"
        );
        assert_eq!(
            shown_brief(&fact, "meta", Some(&heading(today + chrono::Days::new(1)))),
            Some(heading(today)),
            "a heading naming another day leaves the row its own"
        );
        assert_eq!(
            shown_tone(&fact, "meta", Some(&heading(today))),
            None,
            "a day the heading says is no fact of the row's, and reads in no tone"
        );
    }

    #[test]
    fn a_row_under_its_day_keeps_the_time_of_day_it_is_due() {
        let today = ui::clock::today();
        let day = today.format("%Y-%m-%d").to_string();
        let at_nine = NaiveTime::from_hms_opt(9, 0, 0).expect("nine");
        let timed = row(
            "ListRow",
            &[("meta", &day), ("meta:as", "date"), ("meta:time", &at(9))],
        );
        assert_eq!(
            shown_brief(&timed, "meta", Some(&heading(today))).as_deref(),
            Some(ui::format::time(&at_nine).as_str()),
            "the day is the heading's, and the time of day is the row's"
        );
        assert_eq!(
            shown_brief(&timed, "meta", None).as_deref(),
            Some(
                ui::format::day_fact_today(today, Some(at_nine))
                    .text
                    .as_str()
            ),
            "under no heading the day and its time read as one fact"
        );
    }

    #[test]
    fn the_tree_drops_a_rows_day_fact_a_heading_over_it_says() {
        let today = ui::clock::today();
        let day = today.format("%Y-%m-%d").to_string();
        let later = (today + chrono::Days::new(2))
            .format("%Y-%m-%d")
            .to_string();
        let mut card = row("Card", &[("title", &heading(today))]);
        card.children.push(row(
            "ListRow",
            &[
                ("title", "Water the plants"),
                ("meta", &day),
                ("meta:as", "date"),
            ],
        ));
        card.children.push(row(
            "ListRow",
            &[
                ("title", "Book the ferry"),
                ("meta", &later),
                ("meta:as", "date"),
            ],
        ));
        let shown = under_headings(card);
        assert!(
            !shown.children[0].props.contains_key("meta"),
            "the card says the day: the row says it no more"
        );
        assert_eq!(
            shown.children[1].props.get("meta"),
            Some(&Value::text(&later)),
            "a row due another day keeps its own"
        );
        assert_eq!(
            shown_brief(&shown.children[1], "meta", Some(&heading(today))),
            Some(heading(today + chrono::Days::new(2))),
            "and reads as that day under the card"
        );
    }

    #[test]
    fn the_tree_drops_a_rows_day_fact_a_group_over_it_says() {
        let today = ui::clock::today();
        let day = today.format("%Y-%m-%d").to_string();
        let at_nine = NaiveTime::from_hms_opt(9, 0, 0).expect("nine");
        let mut item = node(&[("group", &day), ("group:as", "date")]);
        item.kind = crate::adapter::kind("ListItem");
        item.children.push(row(
            "ListRow",
            &[
                ("title", "Water the plants"),
                ("meta", &day),
                ("meta:as", "date"),
                ("meta:time", &at(9)),
            ],
        ));
        let shown = under_headings(item.clone());
        assert_eq!(
            shown.children[0].props.get("meta"),
            Some(&Value::text(ui::format::time(&at_nine))),
            "the group says the day: the row keeps the time of day it is due"
        );

        // A group of finished work heads the day a row was done on, never the day it is due.
        item.props.insert("group_done".to_owned(), Value::Int(1));
        let done = under_headings(item);
        assert_eq!(
            done.children[0].props.get("meta"),
            Some(&Value::text(&day)),
            "a day finished on is no day a row is due by"
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
            shown_brief(&paired, "value", None),
            Some(ui::format::day_fact_today(today, Some(time)).text)
        );
        assert_ne!(
            shown_brief(&paired, "value", None),
            shown_brief(&alone, "value", None),
            "the time of the day is part of the fact"
        );
        assert_eq!(
            shown(&paired, "value"),
            Some(ui::format::timestamp(&today.and_time(time))),
            "a form states the day and its time together"
        );
        assert_eq!(
            shown_tone(&paired, "value", None),
            None,
            "a day still to come is on time"
        );
    }
}

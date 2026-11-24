//! A shortcut's triggers as text: the `when ...`, `every ...` and `once at ...`
//! lines a program opens with, written and read.
//!
//! A trigger is one line, and that line is the trigger's whole state: which
//! event it waits for, and every filter the event carries. The writer and the
//! reader live beside each other, so a change to one is a change to the other;
//! they are held to each other by [`super`]'s round-trip tests.

use std::fmt::Write as _;

use serde_json::Value;

use crate::shortcut::{Schedule, TriggerSpec};

use super::{literal_text, number_at, quoted, split_word, text_at};

#[allow(
    clippy::too_many_lines,
    reason = "one arm per kind of trigger, each saying it the way a person reads it"
)]
pub(super) fn trigger_text(trigger: &TriggerSpec) -> String {
    match trigger {
        TriggerSpec::Keyboard { chord } => format!("when someone presses {}", quoted(chord)),
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds },
            catch_up,
        } => format!("every {seconds} seconds{}", catch_up_text(*catch_up)),
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar { at, weekdays },
            catch_up,
        } => calendar_text(*at, weekdays, *catch_up),
        TriggerSpec::Once { at_ms } => format!("once at {}", moment_text(*at_ms)),
        TriggerSpec::AppOpened { app_id } => {
            format!("when the app opens {}", quoted(app_id))
        }
        TriggerSpec::WorkspaceJoined { workspace: wanted } => match wanted {
            Some(workspace) => format!("when someone joins workspace {}", quoted(workspace)),
            None => "when someone joins a workspace".to_owned(),
        },
        TriggerSpec::FileChanged { capability } => {
            format!("when a file changes in {}", quoted(capability))
        }
        TriggerSpec::FocusMode { mode, transition } => format!(
            "when focus {mode} is {}",
            match transition {
                crate::shortcut::FocusTransition::Enter => "entered",
                crate::shortcut::FocusTransition::Leave => "left",
            }
        ),
        TriggerSpec::NetworkChanged { network, connected } => {
            let mut line = String::from("when the network changes");
            if let Some(network) = network {
                let _ = write!(line, " to {}", quoted(network));
            }
            if let Some(connected) = connected {
                line.push_str(if *connected {
                    " connected"
                } else {
                    " disconnected"
                });
            }
            line
        }
        TriggerSpec::DeviceConnected { device_class } => {
            format!("when a {} connects", quoted(device_class))
        }
        TriggerSpec::ResourceChanged {
            kind,
            actions,
            fields,
            by,
            filter,
        } => {
            let mut line = format!("when a {} changes", quoted(kind));
            if !actions.is_empty() {
                let _ = write!(line, " by {}", quoted_list(actions));
            }
            if !fields.is_empty() {
                let _ = write!(line, " in {}", quoted_list(fields));
            }
            line.push_str(match by {
                crate::shortcut::ActorFilter::Anyone => "",
                crate::shortcut::ActorFilter::People => " by people",
                crate::shortcut::ActorFilter::NotBots => " by anyone but a bot",
            });
            line.push_str(&filter_text(filter));
            line
        }
        TriggerSpec::Connector {
            app_id,
            source,
            filter,
        } => {
            let mut line = format!("when {} sends {}", quoted(app_id), quoted(source));
            for (field, value) in filter {
                let _ = write!(line, " where {field} is {}", literal_text(value));
            }
            line
        }
        TriggerSpec::IncomingMessage { app_id, from } => {
            let mut line = String::from("when a message arrives");
            if let Some(app_id) = app_id {
                let _ = write!(line, " in {}", quoted(app_id));
            }
            if let Some(from) = from {
                let _ = write!(line, " from {}", quoted(from));
            }
            line
        }
        TriggerSpec::AppEvent { app_id, kind } => {
            format!("when {} announces {}", quoted(app_id), quoted(kind))
        }
        TriggerSpec::NearbyDevice { device } => match device {
            Some(device) => format!("when {} comes near", quoted(device)),
            None => "when a device comes near".to_owned(),
        },
        time @ TriggerSpec::ResourceTime { .. } => resource_time_text(time),
        changed @ TriggerSpec::SourceChanged { .. } => source_change_text(changed),
        TriggerSpec::Webhook {
            url, events, path, ..
        } => {
            let mut line = format!("when this workspace's events are posted to {}", quoted(url));
            if !events.is_empty() {
                let _ = write!(line, " as {}", quoted_list(events));
            }
            if let Some(path) = path {
                let _ = write!(line, " of {}", quoted(path));
            }
            line
        }
    }
}

/// A trigger on something new in a source: the kind of source, which one of
/// that kind when it names one, how often it is looked at when it says so,
/// and each match that narrows what is new. Nothing for any other trigger.
fn source_change_text(trigger: &TriggerSpec) -> String {
    let TriggerSpec::SourceChanged {
        kind,
        at,
        every_secs,
        filter,
    } = trigger
    else {
        return String::new();
    };
    let mut line = format!("when something new is in {}", quoted(kind));
    if !at.is_empty() {
        let _ = write!(line, " at {}", quoted(at));
    }
    if let Some(seconds) = every_secs {
        let _ = write!(line, " looked at every {seconds} seconds");
    }
    for (name, value) in filter {
        let _ = write!(line, " where {name} is {}", literal_text(value));
    }
    line
}

/// A trigger at a time a resource holds: how far its moment is from that
/// time, the field and the kind that hold it, and each match that narrows
/// the items. Nothing for any other trigger.
fn resource_time_text(trigger: &TriggerSpec) -> String {
    let TriggerSpec::ResourceTime {
        kind,
        field,
        offset_secs,
        filter,
    } = trigger
    else {
        return String::new();
    };
    let mut line = match offset_secs.unsigned_abs() {
        0 => String::from("at"),
        span if *offset_secs < 0 => format!("{} before", span_text(span)),
        span => format!("{} after", span_text(span)),
    };
    let _ = write!(line, " {} in a {}", quoted(field), quoted(kind));
    for (name, value) in filter {
        let _ = write!(line, " where {name} is {}", literal_text(value));
    }
    line
}

/// The units a span is written in, largest first, each with its seconds.
const SPAN_UNITS: [(&str, u64); 5] = [
    ("week", 604_800),
    ("day", 86_400),
    ("hour", 3_600),
    ("minute", 60),
    ("second", 1),
];

/// A span as the text writes it, in the largest unit that holds it whole:
/// `15 minutes`, `1 day`, `90 seconds`.
fn span_text(seconds: u64) -> String {
    let (count, unit) = SPAN_UNITS
        .iter()
        .find(|(_, size)| seconds.is_multiple_of(*size))
        .and_then(|(unit, size)| Some((seconds.checked_div(*size)?, *unit)))
        .unwrap_or((seconds, "second"));
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// A daily schedule, on the days it names.
fn calendar_text(
    at: crate::shortcut::TimeOfDay,
    weekdays: &[crate::shortcut::Weekday],
    catch_up: crate::shortcut::CatchUp,
) -> String {
    let days: Vec<&str> = weekdays.iter().map(|day| weekday_name(*day)).collect();
    let when = if days.is_empty() {
        "day".to_owned()
    } else {
        days.join(", ")
    };
    format!(
        "every {when} at {:02}:{:02}{}",
        at.hour,
        at.minute,
        catch_up_text(catch_up)
    )
}

/// A moment as the text writes it: its date and time in UTC, to the
/// millisecond when it has any (`2026-11-17T11:32:00Z`). A moment no year of
/// four digits holds is written as its milliseconds since 1970
/// (`253402300800000 ms`), so every stored trigger has a line that reads back.
fn moment_text(at_ms: i64) -> String {
    jiff::Timestamp::from_millisecond(at_ms)
        .ok()
        .filter(|at| at.to_zoned(jiff::tz::TimeZone::UTC).year() >= 1)
        .map_or_else(
            || format!("{at_ms} ms"),
            |at| {
                if at_ms.rem_euclid(1000) == 0 {
                    format!("{at:.0}")
                } else {
                    format!("{at:.3}")
                }
            },
        )
}

/// Several names as the text writes them: each in quotation marks, joined by
/// `or`.
fn quoted_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| quoted(name))
        .collect::<Vec<String>>()
        .join(" or ")
}

fn catch_up_text(catch_up: crate::shortcut::CatchUp) -> &'static str {
    match catch_up {
        crate::shortcut::CatchUp::Skip => "",
        crate::shortcut::CatchUp::RunOnce => ", running once for what was missed",
    }
}

fn weekday_name(day: crate::shortcut::Weekday) -> &'static str {
    use crate::shortcut::Weekday::{
        Friday, Monday, Saturday, Sunday, Thursday, Tuesday, Wednesday,
    };
    match day {
        Monday => "Monday",
        Tuesday => "Tuesday",
        Wednesday => "Wednesday",
        Thursday => "Thursday",
        Friday => "Friday",
        Saturday => "Saturday",
        Sunday => "Sunday",
    }
}

// --------------------------------------------------------------- reading ---

/// Whether a line declares a trigger.
pub(super) fn is_trigger(text: &str) -> bool {
    [
        "when someone presses ",
        "every ",
        "once at ",
        "when the app opens ",
        "when the network changes",
        "when a message arrives",
        "when a device comes near",
        "when focus ",
        "when someone joins ",
        "when a file changes in ",
        "when something new is in ",
        "when a ",
        "when \"",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
        || offset_at(text).is_some()
}

/// What a trigger at a time a resource holds opens with: how far from that
/// time its moment is, in seconds and negative for before it, and the rest
/// of the line. `at "..."` is the time itself, `15 minutes before "..."` and
/// `1 day after "..."` a span from it.
fn offset_at(text: &str) -> Option<(i64, &str)> {
    if let Some(rest) = text.strip_prefix("at ") {
        return rest.trim_start().starts_with('"').then_some((0, rest));
    }
    let (count, rest) = split_word(text)?;
    let count: i64 = count.parse().ok().filter(|count| *count > 0)?;
    let (unit, rest) = split_word(rest)?;
    let size = SPAN_UNITS
        .iter()
        .find(|(name, _)| unit == *name || unit.strip_suffix('s') == Some(*name))
        .and_then(|(_, size)| i64::try_from(*size).ok())?;
    let seconds = count.checked_mul(size)?;
    let (direction, rest) = split_word(rest)?;
    match direction {
        "before" => Some((seconds.checked_neg()?, rest)),
        "after" => Some((seconds, rest)),
        _ => None,
    }
}

/// The rest of a trigger at a time a resource holds: the field in quotation
/// marks, the kind it is a field of, and the matches that narrow its items.
fn resource_time(offset_secs: i64, rest: &str) -> Result<TriggerSpec, String> {
    let (field, rest) = text_at(rest)?;
    let Some(rest) = rest.trim_start().strip_prefix("in a ") else {
        return Err(
            "write `15 minutes before \"FIELD\" in a \"KIND\"`, `at \"FIELD\" in a \"KIND\"` or \
             `1 day after \"FIELD\" in a \"KIND\"`"
                .to_owned(),
        );
    };
    let (kind, rest) = text_at(rest)?;
    let mut filter = serde_json::Map::new();
    let mut rest = rest.trim_start().to_owned();
    while let Some(matched) = rest.strip_prefix("where ").map(str::to_owned) {
        let (name, value, tail) = match_of(&matched)?;
        filter.insert(name, value);
        rest = tail;
    }
    require_nothing_after(&rest, "the time")?;
    Ok(TriggerSpec::ResourceTime {
        kind,
        field,
        offset_secs,
        filter,
    })
}

/// The rest of a trigger on something new in a source: the kind of source
/// in quotation marks, which one of that kind after `at`, how often it is
/// looked at after `looked at every`, and the matches that narrow what is
/// new.
fn source_change(rest: &str) -> Result<TriggerSpec, String> {
    let (kind, rest) = text_at(rest)?;
    let (at, rest) = match rest.trim_start().strip_prefix("at ") {
        Some(named) => text_at(named)?,
        None => (String::new(), rest),
    };
    let (every_secs, rest) = match rest.trim_start().strip_prefix("looked at every ") {
        Some(period) => {
            let (seconds, rest) = period.split_once(" seconds").ok_or_else(|| {
                "write how often a source is looked at as `looked at every N seconds`".to_owned()
            })?;
            let seconds: u64 = seconds
                .trim()
                .parse()
                .map_err(|_| format!("`{seconds}` is not a number of seconds"))?;
            (Some(seconds), rest)
        }
        None => (None, rest),
    };
    let mut filter = serde_json::Map::new();
    let mut rest = rest.trim_start().to_owned();
    while let Some(matched) = rest.strip_prefix("where ").map(str::to_owned) {
        let (name, value, tail) = match_of(&matched)?;
        filter.insert(name, value);
        rest = tail;
    }
    require_nothing_after(&rest, "the source")?;
    Ok(TriggerSpec::SourceChanged {
        kind,
        at,
        every_secs,
        filter,
    })
}

pub(super) fn trigger_of(text: &str) -> Result<TriggerSpec, String> {
    let text = text.trim();
    if let Some((offset_secs, rest)) = offset_at(text) {
        return resource_time(offset_secs, rest);
    }
    if let Some(rest) = text.strip_prefix("when someone presses ") {
        let (chord, after) = text_at(rest)?;
        require_nothing_after(after, "the keys")?;
        return Ok(TriggerSpec::Keyboard { chord });
    }
    if let Some(rest) = text.strip_prefix("every ") {
        return schedule_of(rest);
    }
    if let Some(rest) = text.strip_prefix("once at ") {
        return once_of(rest);
    }
    if let Some(rest) = text.strip_prefix("when the app opens ") {
        let (app_id, after) = text_at(rest)?;
        require_nothing_after(after, "the app")?;
        return Ok(TriggerSpec::AppOpened { app_id });
    }
    if let Some(rest) = text.strip_prefix("when focus ") {
        let Some((mode, after)) = rest.split_once(" is ") else {
            return Err("write `when focus MODE is entered` or `is left`".to_owned());
        };
        let transition = match after.trim() {
            "entered" => crate::shortcut::FocusTransition::Enter,
            "left" => crate::shortcut::FocusTransition::Leave,
            other => return Err(format!("`{other}` is not `entered` or `left`")),
        };
        return Ok(TriggerSpec::FocusMode {
            mode: mode.trim().to_owned(),
            transition,
        });
    }
    if let Some(rest) = text.strip_prefix("when someone joins ") {
        if rest.trim() == "a workspace" {
            return Ok(TriggerSpec::WorkspaceJoined { workspace: None });
        }
        // The name follows the noun the writer puts before it; `workspace`
        // is optional so the line reads as well without it.
        let asked = rest.trim_start().strip_prefix("workspace ").unwrap_or(rest);
        let (workspace, after) = text_at(asked)?;
        require_nothing_after(after, "the workspace")?;
        return Ok(TriggerSpec::WorkspaceJoined {
            workspace: Some(workspace),
        });
    }
    if let Some(rest) = text.strip_prefix("when a file changes in ") {
        let (capability, after) = text_at(rest)?;
        require_nothing_after(after, "the folder")?;
        return Ok(TriggerSpec::FileChanged { capability });
    }
    if let Some(rest) = text.strip_prefix("when a message arrives") {
        return message_of(rest, None);
    }
    if let Some(rest) = text.strip_prefix("when \"") {
        return quoted_event(rest);
    }
    if let Some(rest) = text.strip_prefix("when the network changes") {
        let (network, rest) = match rest.trim_start().strip_prefix("to ") {
            Some(rest) => {
                let (network, after) = text_at(rest)?;
                (Some(network), after)
            }
            None => (None, rest),
        };
        let connected = match rest.trim() {
            "" => None,
            "connected" => Some(true),
            "disconnected" => Some(false),
            other => return Err(format!("`{other}` is not `connected` or `disconnected`")),
        };
        return Ok(TriggerSpec::NetworkChanged { network, connected });
    }
    if let Some(rest) = text.strip_prefix("when a device comes near") {
        if rest.trim().is_empty() {
            return Ok(TriggerSpec::NearbyDevice { device: None });
        }
        let (device, after) = text_at(rest.trim_start())?;
        require_nothing_after(after, "the device")?;
        return Ok(TriggerSpec::NearbyDevice {
            device: Some(device),
        });
    }
    if let Some(rest) = text.strip_prefix("when something new is in ") {
        return source_change(rest);
    }
    if let Some(rest) = text.strip_prefix("when a ") {
        return quoted_change(rest);
    }
    Err(format!("`{text}` is not a trigger"))
}

/// A message arriving, from the app and the person it names. `app_id` is the
/// app when the line already named it before `arrives`.
fn message_of(rest: &str, app_id: Option<String>) -> Result<TriggerSpec, String> {
    let rest = rest.trim_start();
    let (app_id, rest) = match (app_id, rest.strip_prefix("in ")) {
        (Some(app_id), _) => (Some(app_id), rest),
        (None, Some(in_app)) => {
            let (app, after) = text_at(in_app)?;
            (Some(app), after)
        }
        (None, None) => (None, rest),
    };
    let (from, rest) = match rest.trim_start().strip_prefix("from ") {
        Some(from) => {
            let (from, after) = text_at(from)?;
            (Some(from), after)
        }
        None => (None, rest),
    };
    require_nothing_after(rest, "the message")?;
    Ok(TriggerSpec::IncomingMessage { app_id, from })
}

/// `when a "camera" connects`, and the change to a record of a kind: each
/// names its subject in quotation marks, so the rest of the line decides which
/// it is.
fn quoted_change(rest: &str) -> Result<TriggerSpec, String> {
    let (subject, rest) = text_at(rest)?;
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix("connects") {
        require_nothing_after(after, "the device")?;
        return Ok(TriggerSpec::DeviceConnected {
            device_class: subject,
        });
    }
    let Some(after) = rest.strip_prefix("changes") else {
        return Err("write `when a KIND changes` or `when a CLASS connects`".to_owned());
    };
    document_change(subject, after)
}

/// The lines that name their subject in quotation marks before saying what it
/// does: `when "APP" sends "SOURCE"`, `when "APP" announces "KIND"`, and
/// `when "DEVICE" comes near`.
fn quoted_event(rest: &str) -> Result<TriggerSpec, String> {
    let opened = format!("\"{rest}");
    let (name, after) = text_at(&opened)?;
    let after = after.trim_start();
    if let Some(after) = after.strip_prefix("comes near") {
        require_nothing_after(after, "the device")?;
        return Ok(TriggerSpec::NearbyDevice { device: Some(name) });
    }
    if let Some(after) = after.strip_prefix("announces ") {
        let (kind, rest) = text_at(after)?;
        require_nothing_after(rest, "the event")?;
        return Ok(TriggerSpec::AppEvent { app_id: name, kind });
    }
    let Some(after) = after.strip_prefix("sends ") else {
        return Err(
            "write `when APP sends SOURCE`, `when APP announces KIND`, `when DEVICE comes near` or `when the app opens APP`"
                .to_owned(),
        );
    };
    let (source, rest) = text_at(after)?;
    let mut filter = serde_json::Map::new();
    // The matches are read one after another, each rewriting what is left
    // of the line, exactly as a list's values are.
    let mut rest = rest.trim_start().to_owned();
    while let Some(matched) = rest.strip_prefix("where ").map(str::to_owned) {
        let (field, value, tail) = match_of(&matched)?;
        filter.insert(field, value);
        rest = tail;
    }
    require_nothing_after(&rest, "the event")?;
    Ok(TriggerSpec::Connector {
        app_id: name,
        source,
        filter,
    })
}

/// One match of a connector's event or of an item that holds a time, as the
/// field it names, the value it holds and what is left of the line after it.
fn match_of(text: &str) -> Result<(String, Value, String), String> {
    let (field, rest) = split_word(text).unwrap_or((text, ""));
    let rest = rest.trim_start();
    let after = rest
        .strip_prefix("is ")
        .ok_or_else(|| "write a match as `where FIELD is VALUE`".to_owned())?;
    let (value, tail) = literal_and_rest(after)?;
    Ok((field.to_owned(), value, tail.trim_start().to_owned()))
}

/// A change filter as its `where FIELD is VALUE` clauses, each after a space.
fn filter_text(filter: &crate::shortcut::ChangeFilter) -> String {
    let mut text = String::new();
    for test in &filter.0 {
        let (verb, value) = match &test.test {
            crate::shortcut::Test::Is(value) => ("is", value),
            crate::shortcut::Test::IsNot(value) => ("is not", value),
            crate::shortcut::Test::Has(value) => ("has", value),
            crate::shortcut::Test::Lacks(value) => ("lacks", value),
        };
        let _ = write!(text, " where {} {verb} {}", test.field, literal_text(value));
    }
    text
}

/// The rest of `when a KIND changes`.
fn document_change(kind: String, rest: &str) -> Result<TriggerSpec, String> {
    // Each part is optional, and what it does not read is left for the next.
    let listed = |rest: &str, prefix: &str| match rest.trim_start().strip_prefix(prefix) {
        Some(list) if list.trim_start().starts_with('"') => quoted_or_list(Some(list), " or "),
        _ => Ok((Vec::new(), rest.to_owned())),
    };
    let (actions, rest) = listed(rest, "by ")?;
    let (fields, rest) = listed(&rest, "in ")?;
    let (who, wheres) = match rest.find("where ") {
        Some(at) => rest.split_at(at),
        None => (rest.as_str(), ""),
    };
    let by = match who.trim() {
        "" => crate::shortcut::ActorFilter::Anyone,
        "by people" => crate::shortcut::ActorFilter::People,
        "by anyone but a bot" => crate::shortcut::ActorFilter::NotBots,
        other => return Err(format!("`{other}` is not who made the change")),
    };
    let mut tests = Vec::new();
    let mut rest = wheres.to_owned();
    while let Some(matched) = rest.strip_prefix("where ").map(str::to_owned) {
        let (field, after) = split_word(&matched).unwrap_or((&matched, ""));
        let after = after.trim_start();
        let (test, after): (fn(Value) -> crate::shortcut::Test, &str) =
            if let Some(after) = after.strip_prefix("is not ") {
                (crate::shortcut::Test::IsNot, after)
            } else if let Some(after) = after.strip_prefix("is ") {
                (crate::shortcut::Test::Is, after)
            } else if let Some(after) = after.strip_prefix("has ") {
                (crate::shortcut::Test::Has, after)
            } else if let Some(after) = after.strip_prefix("lacks ") {
                (crate::shortcut::Test::Lacks, after)
            } else {
                return Err(
                    "write a test as `where FIELD is VALUE`, `is not`, `has` or `lacks`".to_owned(),
                );
            };
        let (value, tail) = literal_and_rest(after)?;
        tests.push(crate::shortcut::FieldTest {
            field: field.to_owned(),
            test: test(value),
        });
        tail.trim_start().clone_into(&mut rest);
    }
    require_nothing_after(&rest, "the change")?;
    Ok(TriggerSpec::ResourceChanged {
        kind,
        actions,
        fields,
        by,
        filter: crate::shortcut::ChangeFilter(tests),
    })
}

/// Several quoted names joined by `joiner`, and what follows them.
fn quoted_or_list(text: Option<&str>, joiner: &str) -> Result<(Vec<String>, String), String> {
    let Some(text) = text else {
        return Ok((Vec::new(), String::new()));
    };
    let mut names = Vec::new();
    let mut rest = text;
    loop {
        let (name, after) = text_at(rest)?;
        names.push(name);
        let trimmed = after.trim_start();
        let Some(tail) = trimmed.strip_prefix(joiner) else {
            return Ok((names, trimmed.to_owned()));
        };
        rest = tail;
    }
}

/// A constant written where a value is expected, and what follows it.
fn literal_and_rest(text: &str) -> Result<(Value, String), String> {
    let text = text.trim_start();
    if text.starts_with('"') {
        let (words, rest) = text_at(text)?;
        return Ok((Value::from(words), rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix("yes") {
        return Ok((Value::Bool(true), rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix("no") {
        return Ok((Value::Bool(false), rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix("nothing") {
        return Ok((Value::Null, rest.to_owned()));
    }
    if let Some((value, rest)) = number_at(text) {
        return Ok((value, rest.to_owned()));
    }
    Err("write the value as text in quotation marks, a number, `yes`, `no` or `nothing`".to_owned())
}

/// Nothing may follow the words that already said everything.
fn require_nothing_after(after: &str, subject: &str) -> Result<(), String> {
    if after.trim().is_empty() {
        return Ok(());
    }
    Err(format!("`{}` follows {subject}", after.trim()))
}

/// One moment, as `trigger_text` writes it: a date and time with its offset
/// from UTC, or milliseconds since 1970.
fn once_of(rest: &str) -> Result<TriggerSpec, String> {
    let rest = rest.trim();
    if let Some(ms) = rest.strip_suffix(" ms") {
        let at_ms = ms
            .trim()
            .parse()
            .map_err(|_| format!("`{ms}` is not a number of milliseconds"))?;
        return Ok(TriggerSpec::Once { at_ms });
    }
    rest.parse::<jiff::Timestamp>()
        .map(|at| TriggerSpec::Once {
            at_ms: at.as_millisecond(),
        })
        .map_err(|_| {
            "write the moment as a date and time with its offset from UTC: `once at 2026-11-17T11:32:00Z`"
                .to_owned()
        })
}

/// A schedule, as `trigger_text` writes it: a period in seconds, or a local
/// wall-clock time on the days it lists.
fn schedule_of(rest: &str) -> Result<TriggerSpec, String> {
    let (schedule, catch_up) = match rest.split_once(", running once for what was missed") {
        Some((schedule, after)) => {
            require_nothing_after(after, "the days")?;
            (schedule, crate::shortcut::CatchUp::RunOnce)
        }
        None => (rest, crate::shortcut::CatchUp::Skip),
    };
    if let Some(seconds) = schedule.strip_suffix(" seconds") {
        let seconds: u64 = seconds
            .trim()
            .parse()
            .map_err(|_| format!("`{seconds}` is not a number of seconds"))?;
        return Ok(TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds },
            catch_up,
        });
    }
    let Some((days, at)) = schedule.split_once(" at ") else {
        return Err("write a schedule as `every N seconds` or `every DAY at HH:MM`".to_owned());
    };
    let (hour, minute) = at
        .split_once(':')
        .ok_or_else(|| "write the time as `HH:MM`".to_owned())?;
    let at = crate::shortcut::TimeOfDay {
        hour: hour
            .trim()
            .parse()
            .map_err(|_| "write the hour as a number".to_owned())?,
        minute: minute
            .trim()
            .parse()
            .map_err(|_| "write the minutes as a number".to_owned())?,
    };
    let weekdays = if days.trim() == "day" {
        Vec::new()
    } else {
        let mut weekdays = Vec::new();
        for day in days.split(", ") {
            weekdays.push(weekday_of(day.trim())?);
        }
        weekdays
    };
    Ok(TriggerSpec::Schedule {
        schedule: Schedule::Calendar { at, weekdays },
        catch_up,
    })
}

fn weekday_of(name: &str) -> Result<crate::shortcut::Weekday, String> {
    use crate::shortcut::Weekday::{
        Friday, Monday, Saturday, Sunday, Thursday, Tuesday, Wednesday,
    };
    match name {
        "Monday" => Ok(Monday),
        "Tuesday" => Ok(Tuesday),
        "Wednesday" => Ok(Wednesday),
        "Thursday" => Ok(Thursday),
        "Friday" => Ok(Friday),
        "Saturday" => Ok(Saturday),
        "Sunday" => Ok(Sunday),
        other => Err(format!("`{other}` is not a day of the week")),
    }
}

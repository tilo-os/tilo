//! A shortcut's triggers as text: the `when ...` and `every ...` lines a
//! program opens with, written and read.
//!
//! A trigger is one line, and that line is the trigger's whole state: which
//! event it waits for, and every filter the event carries. The writer and the
//! reader live beside each other, so a change to one is a change to the other;
//! they are held to each other by [`super`]'s round-trip tests.

use std::fmt::Write as _;

use serde_json::Value;

use crate::shortcut::{Schedule, TriggerSpec};

use super::{literal_text, number_at, quoted, split_word, text_at};

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
        TriggerSpec::AppOpened { app_id } => {
            format!("when the app opens {}", quoted(app_id))
        }
        TriggerSpec::WorkspaceJoined { room } => match room {
            Some(room) => format!("when someone joins workspace {}", quoted(room)),
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
        TriggerSpec::RecordChanged {
            kind,
            actions,
            fields,
            by,
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
        TriggerSpec::NearbyDevice { device } => match device {
            Some(device) => format!("when {} comes near", quoted(device)),
            None => "when a device comes near".to_owned(),
        },
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
        "when the app opens ",
        "when the network changes",
        "when a message arrives",
        "when a device comes near",
        "when focus ",
        "when someone joins ",
        "when a file changes in ",
        "when a ",
        "when \"",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

pub(super) fn trigger_of(text: &str) -> Result<TriggerSpec, String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("when someone presses ") {
        let (chord, after) = text_at(rest)?;
        require_nothing_after(after, "the keys")?;
        return Ok(TriggerSpec::Keyboard { chord });
    }
    if let Some(rest) = text.strip_prefix("every ") {
        return schedule_of(rest);
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
            return Ok(TriggerSpec::WorkspaceJoined { room: None });
        }
        // The room follows the noun the writer puts before it; `workspace` is
        // optional so the line reads as well without it.
        let asked = rest.trim_start().strip_prefix("workspace ").unwrap_or(rest);
        let (room, after) = text_at(asked)?;
        require_nothing_after(after, "the workspace")?;
        return Ok(TriggerSpec::WorkspaceJoined { room: Some(room) });
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
    record_change(subject, after)
}

/// The lines that name their subject in quotation marks before saying what it
/// does: `when "APP" sends "SOURCE"`, and `when "DEVICE" comes near`.
fn quoted_event(rest: &str) -> Result<TriggerSpec, String> {
    let opened = format!("\"{rest}");
    let (name, after) = text_at(&opened)?;
    let after = after.trim_start();
    if let Some(after) = after.strip_prefix("comes near") {
        require_nothing_after(after, "the device")?;
        return Ok(TriggerSpec::NearbyDevice { device: Some(name) });
    }
    let Some(after) = after.strip_prefix("sends ") else {
        return Err(
            "write `when APP sends SOURCE`, `when DEVICE comes near` or `when the app opens APP`"
                .to_owned(),
        );
    };
    let (source, rest) = text_at(after)?;
    let mut filter = serde_json::Map::new();
    let mut rest = rest.trim_start().to_owned();
    loop {
        // The matches are read one after another, each rewriting what is left
        // of the line, exactly as a list's values are.
        let Some(after) = rest.strip_prefix("where ").map(str::to_owned) else {
            break;
        };
        let (field, after) = split_word(&after).unwrap_or((&after, ""));
        let after = after.trim_start();
        let after = after
            .strip_prefix("is ")
            .ok_or_else(|| "write a connector's match as `where FIELD is VALUE`".to_owned())?;
        let (value, tail) = literal_and_rest(after)?;
        filter.insert(field.to_owned(), value);
        rest.clear();
        rest.push_str(tail.trim_start());
    }
    require_nothing_after(&rest, "the event")?;
    Ok(TriggerSpec::Connector {
        app_id: name,
        source,
        filter,
    })
}

/// The rest of `when a KIND changes`.
fn record_change(kind: String, rest: &str) -> Result<TriggerSpec, String> {
    let (actions, rest) = quoted_or_list(rest.trim_start().strip_prefix("by "), " or ")?;
    let (fields, rest) = quoted_or_list(rest.trim_start().strip_prefix("in "), " or ")?;
    let by = match rest.trim() {
        "" => crate::shortcut::ActorFilter::Anyone,
        "by people" => crate::shortcut::ActorFilter::People,
        "by anyone but a bot" => crate::shortcut::ActorFilter::NotBots,
        other => return Err(format!("`{other}` is not who made the change")),
    };
    Ok(TriggerSpec::RecordChanged {
        kind,
        actions,
        fields,
        by,
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

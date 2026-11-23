//! A task's moment and its reminder: setting the time of day it is due at moves the reminder
//! that was set for the old moment with it, and a task with no day made a moment is due on the
//! day the moment names.

use tilolang_runtime::Value;

use crate::fixtures::Replica;
use crate::tasks::{call, create, read, workspace};

/// The moment a task holds in `field`, as its page reads it.
fn moment(r: &Replica, task: &Value, field: &str) -> Option<String> {
    match read(r, task, field) {
        Value::Opt(Some(held)) => match *held {
            Value::Text(at) => Some(at),
            other => panic!("a moment: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("a moment: {other:?}"),
    }
}

/// "Actually make it seven": a task made a later time of day keeps its reminder, which was set
/// for the old moment, on the new one; the day it is due stays.
#[test]
fn a_task_made_a_later_time_takes_its_reminder_with_its_moment() {
    let (ada, root) = workspace();
    let made = call(&ada, "new_task", create(&root, "Call mum"));
    call(
        &ada,
        "schedule",
        vec![made.clone(), Value::some(Value::text("2026-11-18"))],
    );
    call(
        &ada,
        "set_time",
        vec![
            made.clone(),
            Value::some(Value::text("2026-11-18T17:00:00Z")),
        ],
    );
    call(
        &ada,
        "remind_at",
        vec![made.clone(), Value::text("2026-11-18T17:00:00Z")],
    );
    ada.commit();
    call(
        &ada,
        "set_time",
        vec![
            made.clone(),
            Value::some(Value::text("2026-11-18T19:00:00Z")),
        ],
    );
    assert_eq!(
        moment(&ada, &made, "at").as_deref(),
        Some("2026-11-18T19:00:00Z"),
        "the moment it was made"
    );
    assert_eq!(
        moment(&ada, &made, "remind").as_deref(),
        Some("2026-11-18T19:00:00Z"),
        "the reminder that was set for the old moment moved with it"
    );
    assert_eq!(
        read(&ada, &made, "due"),
        Value::some(Value::text("2026-11-18")),
        "the day it is due stays"
    );
}

/// A task with no day made a moment is due on the day the moment names: a time of day is always
/// a task's moment on a day, never one with none.
#[test]
fn a_task_with_no_day_made_a_moment_is_due_on_that_day() {
    let (ada, root) = workspace();
    let made = call(&ada, "new_task", create(&root, "Call mum"));
    ada.commit();
    assert_eq!(read(&ada, &made, "due"), Value::none(), "it names no day");
    call(
        &ada,
        "set_time",
        vec![
            made.clone(),
            Value::some(Value::text("2026-11-18T19:00:00Z")),
        ],
    );
    assert_eq!(
        moment(&ada, &made, "at").as_deref(),
        Some("2026-11-18T19:00:00Z"),
        "its moment"
    );
    assert_eq!(
        read(&ada, &made, "due"),
        Value::some(Value::text("2026-11-18")),
        "the day the moment names"
    );
}

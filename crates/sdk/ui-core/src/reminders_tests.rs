use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ScalarValue};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value as Json, json};
use tilolang::meta::TypeMeta;

use super::*;

const TASKS: &str = r#"app t.remind "Remind"

type Task {
    title: Text (title)
    due: Option<Date> = .none
    at: Option<Time> = .none
    who: Option<Principal> = .none
    remind: Option<std.Reminder> (reminds: due, at, who: who) = .none
}

type Note {
    title: Text (title)
    body: Text (body)
}
"#;

const BEA: &str = "u-bea";
const ADA: &str = "u-ada";

fn compiled() -> tilolang::PackageMeta {
    let compiled = tilolang::compile_package_with(
        TASKS,
        &tilolang::IdsLock::new(),
        &[],
        &mut tilolang::SeededEntropy(7),
    )
    .unwrap_or_else(|d| panic!("{d:#?}"));
    compiled.meta
}

fn task_ty(meta: &tilolang::PackageMeta) -> &TypeMeta {
    meta.types.iter().find(|ty| ty.name == "Task").unwrap()
}

fn id(ty: &TypeMeta, name: &str) -> String {
    ty.fields
        .iter()
        .find(|field| field.name == name)
        .unwrap()
        .id
        .clone()
}

fn put(doc: &mut AutoCommit, fields: &ObjId, id: &str, value: &Json) {
    let bytes = serde_json::to_vec(value).unwrap();
    doc.put(fields, id, ScalarValue::Bytes(bytes)).unwrap();
}

fn some(text: &str) -> Json {
    json!({ "some": text })
}

/// A task with the fields named, each an option as the runtime stores it.
fn task(ty: &TypeMeta, due: &Json, at: &Json, who: &Json, remind: &Json) -> (AutoCommit, ObjId) {
    let mut doc = AutoCommit::new();
    let fields = doc.put_object(ROOT, "task", ObjType::Map).unwrap();
    put(&mut doc, &fields, &id(ty, "due"), due);
    put(&mut doc, &fields, &id(ty, "at"), at);
    put(&mut doc, &fields, &id(ty, "who"), who);
    put(&mut doc, &fields, &id(ty, "remind"), remind);
    (doc, fields)
}

/// The due day as it begins in Los Angeles: midnight there is 07:00 UTC that day.
fn midnight_in_los_angeles(day: NaiveDate) -> Option<DateTime<Utc>> {
    Some(DateTime::from_naive_utc_and_offset(
        day.and_hms_opt(7, 0, 0)?,
        Utc,
    ))
}

fn due(ty: &TypeMeta, due: &Json, at: &Json, who: &Json, remind: &Json) -> Option<super::Due> {
    let (doc, fields) = task(ty, due, at, who, remind);
    super::due(&doc, &fields, ty, midnight_in_los_angeles, ADA)
}

#[test]
fn a_types_reminder_names_the_fields_it_reads_by_id() {
    let meta = compiled();
    let ty = task_ty(&meta);
    let reminder = Reminder::of(ty).expect("Task reminds");
    assert_eq!(reminder.trigger, id(ty, "remind"));
    assert_eq!(reminder.date, id(ty, "due"));
    let at = id(ty, "at");
    assert_eq!(reminder.time, Some(at.as_str()));
    let who = id(ty, "who");
    assert_eq!(reminder.who, Some(who.as_str()));
    let note = meta.types.iter().find(|ty| ty.name == "Note").unwrap();
    assert_eq!(Reminder::of(note), None, "a type that declares none");
}

#[test]
fn a_reminder_comes_at_the_time_of_day_it_names_moved_by_its_trigger() {
    let meta = compiled();
    let ty = task_ty(&meta);
    let found = due(
        ty,
        &some("2026-10-27"),
        &some("2026-10-27T09:00:00Z"),
        &json!({ "some": { "principal": BEA } }),
        &some("-PT15M"),
    )
    .expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 08:45:00 UTC");
    assert_eq!(found.person, BEA);
}

#[test]
fn a_reminder_with_no_time_of_day_reads_the_start_of_its_day_in_the_persons_zone() {
    let meta = compiled();
    let ty = task_ty(&meta);
    // Midnight on the 27th in Los Angeles is 07:00 UTC that day.
    let found = due(
        ty,
        &some("2026-10-27"),
        &json!({ "none": null }),
        &json!({ "none": null }),
        &some("-PT15M"),
    )
    .expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 06:45:00 UTC");
    assert_eq!(found.person, ADA, "nobody named: the record's creator");
    let nine = due(
        ty,
        &some("2026-10-27"),
        &json!({ "none": null }),
        &json!({ "none": null }),
        &some("PT9H"),
    )
    .expect("a reminder");
    assert_eq!(
        nine.at.to_string(),
        "2026-10-27 16:00:00 UTC",
        "09:00 local"
    );
}

#[test]
fn a_reminder_of_a_time_of_day_reads_no_zone() {
    let meta = compiled();
    let ty = task_ty(&meta);
    let (doc, fields) = task(
        ty,
        &some("2026-10-27"),
        &some("2026-10-27T09:00:00Z"),
        &json!({ "none": null }),
        &some("-PT15M"),
    );
    let found = super::due(
        &doc,
        &fields,
        ty,
        |_| panic!("a reminder with a time of day needs no day's start"),
        ADA,
    )
    .expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 08:45:00 UTC");
}

#[test]
fn a_record_with_no_reminder_has_none() {
    let meta = compiled();
    let ty = task_ty(&meta);
    assert_eq!(
        due(
            ty,
            &some("2026-10-27"),
            &json!({ "none": null }),
            &json!({ "none": null }),
            &json!({ "none": null }),
        ),
        None
    );
    assert_eq!(
        due(
            ty,
            &json!({ "none": null }),
            &json!({ "none": null }),
            &json!({ "none": null }),
            &some("-PT15M"),
        ),
        None,
        "a reminder of a record with no day never comes"
    );
}

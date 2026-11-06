use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ScalarValue};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde_json::{Value as Json, json};
use tilolang::meta::TypeMeta;

use super::*;

const TASKS: &str = r#"app t.remind "Remind"

type Task {
    title: Text (title)
    done: Bool (done)
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

/// An option holding `value`, as a record stores one.
fn some(value: &str) -> Json {
    json!({ "some": value })
}

/// An option holding the person `who`.
fn person(who: &str) -> Json {
    json!({ "some": { "principal": who } })
}

/// An option holding nothing.
fn no() -> Json {
    json!({ "none": null })
}

/// Put the option `option` under the field `id` of the record at `fields`, in the map form the
/// runtime stores registers in: `some` holding the value, or `none` holding nothing.
fn put(doc: &mut AutoCommit, fields: &ObjId, id: &str, option: &Json) {
    let held = doc.put_object(fields, id, ObjType::Map).unwrap();
    match option.get("some") {
        Some(Json::Object(inside)) => {
            let value = doc.put_object(&held, "some", ObjType::Map).unwrap();
            for (key, inside) in inside {
                doc.put(&value, key.as_str(), inside.as_str().unwrap_or_default())
                    .unwrap();
            }
        }
        Some(Json::String(text)) => doc.put(&held, "some", text.as_str()).unwrap(),
        Some(other) => doc.put(&held, "some", other.to_string()).unwrap(),
        None => doc.put(&held, "none", ScalarValue::Null).unwrap(),
    }
}

/// The record of a task with the fields named, each an option as the runtime stores it, and
/// whether it is done.
fn task(due: &Json, at: &Json, who: &Json, remind: &Json, done: bool) -> DynRecord {
    let meta = compiled();
    meta.register_schemas();
    let ty = task_ty(&meta);
    let mut doc = AutoCommit::new();
    let fields = doc.put_object(ROOT, "task", ObjType::Map).unwrap();
    doc.put(&fields, crate::schema::TYPE_KEY, ty.id.as_str())
        .unwrap();
    let title = doc
        .put_object(&fields, id(ty, "title"), ObjType::Text)
        .unwrap();
    doc.splice_text(&title, 0, 0, "Call the landlord").unwrap();
    doc.put(&fields, id(ty, "done"), done).unwrap();
    put(&mut doc, &fields, &id(ty, "due"), due);
    put(&mut doc, &fields, &id(ty, "at"), at);
    put(&mut doc, &fields, &id(ty, "who"), who);
    put(&mut doc, &fields, &id(ty, "remind"), remind);
    DynRecord::read_whole(&doc, &fields).expect("the record reads")
}

/// A day, and a time of day on it, in Los Angeles in summer: seven hours behind UTC.
fn los_angeles(day: NaiveDate, time: Option<NaiveTime>) -> Option<DateTime<Utc>> {
    let wall = day.and_time(time.unwrap_or(NaiveTime::MIN));
    Some(DateTime::from_naive_utc_and_offset(
        wall.checked_add_signed(chrono::TimeDelta::hours(7))?,
        Utc,
    ))
}

fn due(due: &Json, at: &Json, who: &Json, remind: &Json) -> Option<super::Due> {
    let record = task(due, at, who, remind, false);
    super::due(&record, &los_angeles)
}

#[test]
fn a_types_reminder_names_the_fields_it_reads_by_id() {
    let meta = compiled();
    let ty = task_ty(&meta);
    let schemas = meta.schemas();
    let task = schemas
        .iter()
        .find(|schema| schema.name == "Task")
        .expect("Task");
    let reminds = task.reminds().expect("Task reminds");
    assert_eq!(reminds.trigger, id(ty, "remind"));
    assert_eq!(reminds.date, id(ty, "due"));
    let at = id(ty, "at");
    assert_eq!(reminds.time, Some(at.as_str()));
    let who = id(ty, "who");
    assert_eq!(reminds.who, Some(who.as_str()));
    let note = schemas
        .iter()
        .find(|schema| schema.name == "Note")
        .expect("Note");
    assert_eq!(note.reminds(), None, "a type that declares none");
}

#[test]
fn a_reminder_comes_at_the_time_of_day_it_names_moved_by_its_trigger() {
    let found = due(
        &some("2026-10-27"),
        &some("2026-10-27T09:00:00Z"),
        &person(BEA),
        &some("-PT15M"),
    )
    .expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 08:45:00 UTC");
    assert_eq!(
        found.deadline.to_string(),
        "2026-10-27 09:00:00 UTC",
        "the moment the task itself is due"
    );
    assert_eq!(found.person.as_deref(), Some(BEA));
}

#[test]
fn a_wall_clock_time_of_day_is_read_in_the_persons_zone() {
    // 09:00 in Los Angeles is 16:00 UTC; a quarter of an hour before is 15:45 UTC.
    let found = due(
        &some("2026-10-27"),
        &some("2026-10-27T09:00"),
        &no(),
        &some("-PT15M"),
    )
    .expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 15:45:00 UTC");
    assert_eq!(found.deadline.to_string(), "2026-10-27 16:00:00 UTC");
}

#[test]
fn a_reminder_with_no_time_of_day_reads_the_start_of_its_day_in_the_persons_zone() {
    // Midnight on the 27th in Los Angeles is 07:00 UTC that day.
    let found = due(&some("2026-10-27"), &no(), &no(), &some("-PT15M")).expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 06:45:00 UTC");
    assert_eq!(found.person, None, "nobody named: the record's creator's");
    let nine = due(&some("2026-10-27"), &no(), &no(), &some("PT9H")).expect("a reminder");
    assert_eq!(
        nine.at.to_string(),
        "2026-10-27 16:00:00 UTC",
        "09:00 local"
    );
}

#[test]
fn a_time_of_day_that_carries_its_zone_reads_no_other_zone() {
    let record = task(
        &some("2026-10-27"),
        &some("2026-10-27T09:00:00Z"),
        &no(),
        &some("-PT15M"),
        false,
    );
    let found = super::due(&record, &|_, _| {
        panic!("a time that carries its zone needs no reading of the person's")
    })
    .expect("a reminder");
    assert_eq!(found.at.to_string(), "2026-10-27 08:45:00 UTC");
}

#[test]
fn a_record_with_no_reminder_has_none() {
    assert_eq!(due(&some("2026-10-27"), &no(), &no(), &no()), None);
    assert_eq!(
        due(&no(), &no(), &no(), &some("-PT15M")),
        None,
        "a reminder of a record with no day never comes"
    );
}

#[test]
fn a_done_record_is_read_as_done_and_reminds_nobody() {
    let done = task(&some("2026-10-27"), &no(), &no(), &some("-PT15M"), true);
    assert!(is_done(&done));
    let open = task(&some("2026-10-27"), &no(), &no(), &some("-PT15M"), false);
    assert!(!is_done(&open), "an open record is not done");
}

#[test]
fn an_item_is_named_by_the_text_its_type_titles_it_with() {
    let record = task(&some("2026-10-27"), &no(), &no(), &some("-PT15M"), false);
    assert_eq!(title(&record), "Call the landlord");
}

//! Repeats and reminders at runtime: a rule expands from the due day or from the day it was
//! done, a wall time survives a DST change in the person's zone, a reminder reads off the due
//! moment, and `std.complete` advances a repeating item in place so two people completing the
//! same task agree on its next day.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::common::*;
    use tilolang_runtime::words::Words;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.repeats "Repeats"

type Item {
    title: Text (title)
    done: Bool = false
    due: Option<Date> = .none
    at: Option<Time> = .none
    repeats: Option<std.Repeat> = .none
    completed: Set<Date>
}

type Probe {
    said: Text = ""
    when: Option<Time> = .none
    day: Option<Date> = .none
}

type Every {
    weekly: std.Repeat = std.Repeat("RRULE:FREQ=WEEKLY;INTERVAL=2")
    lead: std.Reminder = std.Reminder("PT9H")
}

root Item

default view items for Item

view items(i: Item) {
    ui.Text(i.title)
}

action next(probe: Probe, rule: std.Repeat, from: Date, done_on: Date) {
    probe.day = .some(rule.next(from, done_on))
}

action next_at(probe: Probe, rule: std.Repeat, at: Time, day: Date) {
    probe.when = .some(rule.next_at(at, day))
}

action say_repeat(probe: Probe, rule: std.Repeat) {
    probe.said = rule.label()
}

action say_reminder(probe: Probe, reminder: std.Reminder) {
    probe.said = reminder.label()
}

action remind(probe: Probe, reminder: std.Reminder, due: Date, at: Option<Time>) {
    probe.when = .some(reminder.at(due, at))
}

action set_rule(item: Item, rule: std.Repeat) {
    item.repeats = .some(rule)
}

action seed(item: Item, due: Date) {
    item.due = .some(due)
}

action seed_at(item: Item, at: Time) {
    item.at = .some(at)
}

action check(item: Item, done: Bool) {
    let again = std.complete(item, done)
    item.done = done and not again
}
"#;

    struct Workspace {
        rt: Runtime,
        app: App,
        item: ResourceRef,
        probe: ResourceRef,
    }

    impl Workspace {
        fn new() -> Self {
            Self::on(&AmStore::new(), None)
        }

        fn on(store: &AmStore, of: Option<&ResourceRef>) -> Self {
            // Every device runs the same compiled package.
            thread_local! {
                static COMPILED: tilolang::Compiled = App::compile(SOURCE, &[]).compiled;
            }
            let app = App {
                compiled: COMPILED.with(Clone::clone),
            };
            let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let item = of.cloned().unwrap_or_else(|| rt.create("Item").unwrap());
            let probe = rt.create("Probe").unwrap();
            Self {
                rt,
                app,
                item,
                probe,
            }
        }

        fn at(now: &str, zone: i64) -> Self {
            let w = Self::new();
            let clock = Rc::new(RefCell::new(now.to_owned()));
            let reading = Rc::clone(&clock);
            w.rt.set_clock(move || reading.borrow().clone());
            w.rt.set_zone(move || zone);
            w
        }

        fn in_zone(now: &str, name: &str) -> Self {
            let w = Self::at(now, 0);
            let zone = name.to_owned();
            w.rt.set_zone_name(move || Some(zone.clone()));
            w
        }

        fn call(&self, action: &str, args: Vec<Value>) {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|e| panic!("{action}: {e}"));
        }

        fn try_call(&self, action: &str, args: Vec<Value>) -> Result<(), String> {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .map(|_| ())
                .map_err(|e| e.to_string())
        }

        fn next(&self, rule: &str, from: &str, done_on: &str) -> String {
            self.call(
                "next",
                vec![
                    Value::resource(&self.probe),
                    Value::text(rule),
                    Value::text(from),
                    Value::text(done_on),
                ],
            );
            self.read(&self.probe, "day")
        }

        fn next_at(&self, rule: &str, at: &str, day: &str) -> String {
            self.call(
                "next_at",
                vec![
                    Value::resource(&self.probe),
                    Value::text(rule),
                    Value::text(at),
                    Value::text(day),
                ],
            );
            self.read(&self.probe, "when")
        }

        fn said(&self, action: &str, held: &str) -> String {
            self.call(
                action,
                vec![Value::resource(&self.probe), Value::text(held)],
            );
            match self.rt.read(&self.probe, "said").unwrap() {
                Value::Text(t) => t,
                other => panic!("said reads as {other:?}"),
            }
        }

        /// Reads a field and unwraps the text of a text-held value, optional or not.
        fn read(&self, item: &ResourceRef, field: &str) -> String {
            match self.rt.read(item, field).unwrap() {
                Value::Text(t) => t,
                Value::Opt(Some(v)) => match *v {
                    Value::Text(t) => t,
                    other => panic!("{field} reads as {other:?}"),
                },
                other => panic!("{field} reads as {other:?}"),
            }
        }

        fn seed(&self, due: &str) {
            self.call("seed", vec![Value::resource(&self.item), Value::text(due)]);
        }

        fn check(&self, done: bool) {
            self.call(
                "check",
                vec![Value::resource(&self.item), Value::Bool(done)],
            );
        }

        fn done(&self) -> bool {
            match self.rt.read(&self.item, "done").unwrap() {
                Value::Bool(b) => b,
                other => panic!("done reads as {other:?}"),
            }
        }

        fn dates(&self, item: &ResourceRef, field: &str) -> Vec<String> {
            match self.rt.read(item, field).unwrap() {
                Value::List(l) => l
                    .iter()
                    .map(|v| match v {
                        Value::Text(t) => t.clone(),
                        other => panic!("{field} holds {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} reads as {other:?}"),
            }
        }
    }

    #[test]
    fn a_daily_rule_advances_one_day_and_a_weekly_one_a_week() {
        let w = Workspace::new();
        assert_eq!(
            w.next("FREQ=DAILY", "2026-04-01", "2026-04-01"),
            "2026-04-02"
        );
        assert_eq!(
            w.next("FREQ=WEEKLY", "2026-04-01", "2026-04-01"),
            "2026-04-08"
        );
        assert_eq!(
            w.next("FREQ=WEEKLY;INTERVAL=2", "2026-04-01", "2026-04-01"),
            "2026-04-15"
        );
        assert_eq!(
            w.next("FREQ=YEARLY", "2026-04-01", "2026-04-01"),
            "2027-04-01"
        );
    }

    #[test]
    fn a_monthly_rule_on_the_31st_skips_the_months_without_one() {
        let w = Workspace::new();
        assert_eq!(
            w.next("FREQ=MONTHLY", "2026-01-31", "2026-01-31"),
            "2026-03-31"
        );
        assert_eq!(
            w.next("FREQ=MONTHLY", "2026-03-31", "2026-03-31"),
            "2026-05-31"
        );
        // The last day of each month is a day the rule names, not a month-end guess.
        assert_eq!(
            w.next("FREQ=MONTHLY;BYMONTHDAY=-1", "2026-01-31", "2026-01-31"),
            "2026-02-28"
        );
    }

    #[test]
    fn a_weekday_rule_goes_from_friday_to_monday() {
        let w = Workspace::new();
        let weekdays = "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR";
        assert_eq!(w.next(weekdays, "2026-04-02", "2026-04-02"), "2026-04-03");
        assert_eq!(w.next(weekdays, "2026-04-03", "2026-04-03"), "2026-04-06");
    }

    #[test]
    fn a_rule_that_counts_from_done_advances_from_the_completion_day() {
        let w = Workspace::new();
        let from_done = "FREQ=WEEKLY;X-FROM=DONE";
        // Done on a Wednesday, due the Monday before it: the next one is a week from Wednesday.
        assert_eq!(w.next(from_done, "2026-03-30", "2026-04-01"), "2026-04-08");
        assert_eq!(
            w.next("FREQ=DAILY;X-FROM=DONE", "2026-03-01", "2026-04-01"),
            "2026-04-02"
        );
        // The same rule that counts from the due day stays anchored there.
        assert_eq!(
            w.next("FREQ=WEEKLY", "2026-03-30", "2026-04-01"),
            "2026-04-06"
        );
    }

    #[test]
    fn a_rule_that_has_ended_says_so_instead_of_inventing_a_day() {
        let w = Workspace::new();
        let why = w
            .try_call(
                "next",
                vec![
                    Value::resource(&w.probe),
                    Value::text("FREQ=DAILY;UNTIL=20260110"),
                    Value::text("2026-01-10"),
                    Value::text("2026-01-10"),
                ],
            )
            .unwrap_err();
        assert!(why.contains("no occurrence after 2026-01-10"), "{why}");
        assert!(why.contains("UNTIL"), "{why}");
    }

    #[test]
    fn the_next_time_keeps_the_wall_time_across_a_dst_change() {
        // 29 March 2026: Berlin goes from 02:00 CET to 03:00 CEST.
        let w = Workspace::in_zone("2026-03-28T09:00:00Z", "Europe/Berlin");
        // 08:30Z the day before is 09:30 in Berlin; the next day's 09:30 is 07:30Z, not 08:30Z.
        assert_eq!(
            w.next_at("FREQ=DAILY", "2026-03-28T08:30:00Z", "2026-03-29"),
            "2026-03-29T07:30:00Z"
        );
        // And back again in October, when 09:30 is 08:30Z.
        assert_eq!(
            w.next_at("FREQ=DAILY", "2026-10-24T07:30:00Z", "2026-10-25"),
            "2026-10-25T08:30:00Z"
        );
    }

    #[test]
    fn without_a_zone_name_the_offset_stands_in() {
        let w = Workspace::at("2026-01-05T09:00:00Z", 3600);
        assert_eq!(
            w.next_at("FREQ=DAILY", "2026-01-05T08:30:00Z", "2026-01-06"),
            "2026-01-06T08:30:00Z"
        );
    }

    #[test]
    fn a_repeat_reads_in_words_through_the_reader_s_locale() {
        let w = Workspace::new();
        assert_eq!(
            w.said("say_repeat", "FREQ=WEEKLY;INTERVAL=2"),
            "Every 2 weeks"
        );
        assert_eq!(w.said("say_repeat", "FREQ=DAILY"), "Every day");
        assert_eq!(
            w.said("say_repeat", "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
            "Every weekday"
        );
        assert_eq!(
            w.said("say_repeat", "FREQ=DAILY;X-FROM=DONE"),
            "1 day after done"
        );
        assert_eq!(
            w.said("say_repeat", "FREQ=MONTHLY;INTERVAL=3;X-FROM=DONE"),
            "3 months after done"
        );
    }

    #[test]
    fn a_label_asks_the_words_for_a_phrase_and_its_count() {
        type Asked = Vec<(String, Vec<(String, String)>)>;

        #[derive(Clone, Default)]
        struct Recorded {
            asked: Rc<RefCell<Asked>>,
        }

        impl Words for Recorded {
            fn add(&self, _catalogues: &[(String, String)]) {}
            fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
                self.asked
                    .borrow_mut()
                    .push((key.to_owned(), args.to_vec()));
                format!("{source} (asked as {key})")
            }
        }

        let w = Workspace::new();
        let words = Recorded::default();
        w.rt.set_words(words.clone());
        // In another language the phrase reads differently, and the id and count still come
        // through: a host translates them without knowing what a repeat is.
        assert_eq!(
            w.said("say_repeat", "FREQ=WEEKLY;INTERVAL=2"),
            "Every 2 weeks (asked as tilolang-repeat-every-weeks)"
        );
        let asked = words.asked.borrow();
        assert_eq!(
            asked.as_slice(),
            [(
                "tilolang-repeat-every-weeks".to_owned(),
                vec![("count".to_owned(), "2".to_owned())]
            )]
        );
    }

    #[test]
    fn a_reminder_reads_in_words_and_lands_from_the_due_moment() {
        let w = Workspace::in_zone("2026-03-28T09:00:00Z", "Europe/Berlin");
        assert_eq!(w.said("say_reminder", "PT9H"), "9 hours after");
        assert_eq!(w.said("say_reminder", "-PT15M"), "15 minutes before");
        assert_eq!(w.said("say_reminder", "-P1D"), "1 day before");
        assert_eq!(w.said("say_reminder", "P0D"), "When it is due");
        // A task with a time: fifteen minutes before it.
        w.call(
            "remind",
            vec![
                Value::resource(&w.probe),
                Value::text("-PT15M"),
                Value::text("2026-04-01"),
                Value::some(Value::text("2026-04-01T08:00:00Z")),
            ],
        );
        assert_eq!(w.read(&w.probe, "when"), "2026-04-01T07:45:00Z");
        // A task with a day only: 09:00 where the person is, then fifteen minutes before it.
        w.call(
            "remind",
            vec![
                Value::resource(&w.probe),
                Value::text("PT9H"),
                Value::text("2026-04-01"),
                Value::none(),
            ],
        );
        assert_eq!(w.read(&w.probe, "when"), "2026-04-01T07:00:00Z");
        w.call(
            "remind",
            vec![
                Value::resource(&w.probe),
                Value::text("-PT15M"),
                Value::text("2026-04-01"),
                Value::none(),
            ],
        );
        assert_eq!(w.read(&w.probe, "when"), "2026-03-31T21:45:00Z");
    }

    #[test]
    fn completing_a_repeating_task_advances_it_and_records_the_day() {
        let w = Workspace::in_zone("2026-04-01T10:00:00Z", "Europe/Berlin");
        w.call(
            "set_rule",
            vec![Value::resource(&w.item), Value::text("FREQ=DAILY")],
        );
        w.seed("2026-04-01");
        w.call(
            "seed_at",
            vec![
                Value::resource(&w.item),
                Value::text("2026-04-01T08:00:00Z"),
            ],
        );
        w.check(true);
        // The next one is tomorrow, at the same wall time, and the task is open again.
        assert_eq!(w.read(&w.item, "due"), "2026-04-02");
        assert_eq!(w.read(&w.item, "at"), "2026-04-02T08:00:00Z");
        // The task is open again: the next occurrence is what it is about now.
        assert!(!w.done());
        assert_eq!(w.dates(&w.item, "completed"), ["2026-04-01"]);
    }

    #[test]
    fn completing_a_task_that_does_not_repeat_leaves_it_alone() {
        let w = Workspace::at("2026-04-01T10:00:00Z", 0);
        w.seed("2026-04-01");
        w.check(true);
        assert_eq!(w.read(&w.item, "due"), "2026-04-01");
        assert!(w.dates(&w.item, "completed").is_empty());
        assert!(w.done());
        // Un-completing changes nothing either.
        w.check(false);
        assert_eq!(w.read(&w.item, "due"), "2026-04-01");
        assert!(w.dates(&w.item, "completed").is_empty());
    }

    #[test]
    fn two_people_completing_the_same_task_record_that_day_once() {
        let store = AmStore::new();
        let one = Workspace::on(&store, None);
        one.rt.set_clock(|| "2026-04-01T10:00:00Z".to_owned());
        let two = Workspace::on(&store, Some(&one.item));
        two.rt.set_clock(|| "2026-04-01T12:00:00Z".to_owned());
        one.seed("2026-04-01");
        one.call(
            "set_rule",
            vec![Value::resource(&one.item), Value::text("FREQ=DAILY")],
        );
        // Both saw the same open task and both complete it on the same day.
        let action = one.app.action("check");
        let args = || vec![Value::resource(&one.item), Value::Bool(true)];
        one.rt
            .invoke_action(&me(), (action.as_str(), args()))
            .unwrap();
        two.rt
            .invoke_action(&me(), (action.as_str(), args()))
            .unwrap();
        let three = Workspace::on(&store, Some(&one.item));
        // Each completion steps one occurrence on, and the day both people recorded stays one
        // entry: the field is an add-wins set, so the two writes of the same day are one.
        assert_eq!(three.read(&three.item, "due"), "2026-04-03");
        assert_eq!(three.dates(&three.item, "completed"), ["2026-04-01"]);
    }

    #[test]
    fn a_rule_is_stored_as_its_canonical_text() {
        let meta = App::compile(SOURCE, &[]);
        let every = meta
            .meta()
            .types
            .iter()
            .find(|t| t.name == "Every")
            .expect("Every");
        let weekly = every
            .fields
            .iter()
            .find(|f| f.name == "weekly")
            .expect("weekly");
        assert_eq!(
            weekly.default,
            Some(serde_json::json!("FREQ=WEEKLY;INTERVAL=2"))
        );
        let lead = every
            .fields
            .iter()
            .find(|f| f.name == "lead")
            .expect("lead");
        assert_eq!(lead.default, Some(serde_json::json!("PT9H")));
        // What a rule stores reads back as the same rule, so a stored one expands unchanged.
        let w = Workspace::new();
        let stored = w.rt.create("Every").unwrap();
        assert_eq!(w.read(&stored, "weekly"), "FREQ=WEEKLY;INTERVAL=2");
        assert_eq!(w.read(&stored, "lead"), "PT9H");
        assert_eq!(
            w.next("FREQ=WEEKLY;INTERVAL=2", "2026-04-01", "2026-04-01"),
            "2026-04-15"
        );
    }
}

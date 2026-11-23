//! Time as values: a length of time is one of its own, a day and a moment move by it and are
//! apart by it, a day reads as its weekday and parts, a moment as the day and time of day it is
//! for the person — read in their zone as `today` reads the day it is — and words name a day or
//! a moment read against one.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, RuntimeError, SimpleAuthority, UiNode, Value,
    };

    const SOURCE: &str = r#"app so.tilo.time "Time"

type Log {
    at: Time = Time("1970-01-01T00:00:00Z")
    stamp: Date = Date("1970-01-01")
    seconds: Int = 0
    weekday: Text = ""

    search {
        filter "Today" = stamp == today
    }
}

type Board {
    logs: Collection<Log>
}

action seed(b: Board) uses time {
    let l = b.logs.insert(Log(at: 2026-10-29T23:30:00Z))
    // The day the action's own moment falls on: the filter's `today` is the same day.
    l.stamp = now.day()
}

action soon(b: Board) uses time {
    let l = b.logs.insert(Log())
    l.at = now + 2h
    l.stamp = today + 3d
    l.seconds = now.time_of_day().seconds()
}

action stamp(b: Board) uses time {
    let l = b.logs.insert(Log())
    l.stamp = (today + 1w) - 2d
    l.seconds = (now - Time("1970-01-01T00:00:00Z")).seconds()
}

action read(b: Board, text: Text) uses time {
    let l = b.logs.insert(Log())
    l.at = Time.parse(text, near: now) ?? Time("1970-01-01T00:00:00Z")
    l.stamp = Date.parse(text, near: now) ?? Date("1970-01-01")
}

/// `d + span` for a span code holds: a day moves by whole days only, told where it moves.
fn moved(d: Date, span: Duration) -> Date {
    d + span
}

fn double_span(span: Duration) -> Duration {
    span + span
}

fn weekday_name(d: Date) -> Text {
    match d.weekday() {
        .monday => "monday",
        .tuesday => "tuesday",
        .wednesday => "wednesday",
        .thursday => "thursday",
        .friday => "friday",
        .saturday => "saturday",
        .sunday => "sunday",
    }
}

view lengths() {
    text("a {2h}")
    text("b {90m}")
    text("c {1w - 2d}")
    text("d {2h + 30m}")
    text("e {(2h + 30m).seconds()}")
    text("f {-30m}")
    text("g {0s}")
    text("h {2h < 3h}")
    text("i {2h == 120m}")
    text("j {Date("2026-11-01") - Date("2026-10-29")}")
    text("k {Time("2026-10-29T12:00:00Z") - Time("2026-10-29T10:30:00Z")}")
    text("l {Date("2026-10-29") + 3d}")
    text("m {3d + Date("2026-10-29")}")
    text("n {Date("2026-10-29") - 1w}")
    text("o {Time("2026-10-29T12:00:00Z") + 30m}")
}

view edges(b: Board) {
    text("a {Time("2026-10-29T21:59:00Z").day()}")
    text("b {Time("2026-10-29T22:00:00Z").day()}")
    text("c {Time("2026-10-30T05:00:00Z").day()}")
    text("d {Time("2026-10-30T07:00:00Z").day()}")
    for l in b.logs {
        text("e {l.at.day()} {l.stamp}")
    }
    text("f {Time("2026-10-29T22:30:00Z").day()}")
    text("g {Time("2026-07-01T22:30:00Z").day()}")
}

view parts() {
    text("a {weekday_name(Date("2026-10-29"))}")
    text("b {Date("2026-10-29").day()} {Date("2026-10-29").month()} {Date("2026-10-29").year()}")
    text("c {Date("2026-10-29").start_of_week()}")
    text("d {Date("2026-10-26").start_of_week()}")
}

view clocks() {
    text("a {Time("2026-10-29T10:41:00Z").clock()}")
    text("b {Time("2026-10-29T00:05:00Z").clock()}")
    text("c {Time("2026-10-29T14:05:00Z").clock()}")
}

view read_back(b: Board) {
    for l in b.logs {
        text("{l.at} {l.stamp} {l.seconds}")
    }
}

/// A day and a moment named in text (`"{d}"`), and the stored text a machine reads (`.iso()`).
view named() {
    text("a {Date("2026-06-29")}")
    text("b {Date("2025-06-29")}")
    text("c {Time("2026-06-29T18:00:00Z")}")
    text("d {Date("2026-06-29").iso()} {Time("2026-06-29T18:00:00Z").iso()}")
}

view agreed(b: Board) {
    for f in filters(Log).filter(f -> f.toggle) {
        text("on today {b.logs.matching(f).len()}")
    }
}

action too_fine(b: Board) uses time {
    let l = b.logs.insert(Log())
    l.stamp = moved(today, 2h)
}

action fine(b: Board) uses time {
    let l = b.logs.insert(Log())
    l.stamp = moved(today, 2d)
}

action defer(b: Board, span: Duration) uses time {
    let l = b.logs.insert(Log())
    l.at = now + span
    l.seconds = span.seconds()
}

action since_hour(b: Board) -> Duration uses time {
    return now - Time("2026-10-29T21:00:00Z")
}

action extend(b: Board, span: Duration = 45m) -> Duration {
    return span + 15m
}

view span_label(span: Duration) {
    text("{span} ({span.seconds()})")
}
"#;

    const NOW: &str = "2026-10-29T22:10:00Z";

    struct Sheet {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Sheet {
        /// A person whose clock reads `clock` and whose offset is `offset`, in the zone `name`
        /// where the host names one, whose week starts `week_start` days after Monday.
        fn at(clock: &str, offset: i64, name: Option<&str>, week_start: i64) -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let clock = clock.to_owned();
            rt.set_clock(move || clock.clone());
            rt.set_zone(move || offset);
            rt.set_week_start(move || week_start);
            if let Some(name) = name {
                let name = name.to_owned();
                rt.set_zone_name(move || Some(name.clone()));
            }
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            Self { rt, app, board }
        }

        fn in_zone(offset: i64, name: Option<&str>) -> Self {
            Self::at(NOW, offset, name, 0)
        }

        fn call(&self, action: &str, args: Vec<Value>) {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
        }

        fn seed(&self) {
            self.call("seed", vec![Value::resource(&self.board)]);
        }

        fn view(&self, view: &str) -> Vec<String> {
            let opened = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view(view),
                    vec![Value::resource(&self.board)],
                )
                .unwrap();
            texts(opened.tree())
        }

        fn bare(&self, view: &str) -> Vec<String> {
            let opened = self
                .rt
                .open_view(&me(), &self.app.view(view), Vec::new())
                .unwrap();
            texts(opened.tree())
        }
    }

    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    tilo_test_env::scenario! {
        fn days_and_lengths_of_time_move_by_whole_units;

        phase a_length_of_time_reads_in_its_largest_unit_and_moves_days_and_moments;
        {
            let sheet = Sheet::in_zone(0, None);
            assert_eq!(
                sheet.bare("lengths"),
                [
                    "a 2h",
                    "b 90m",
                    "c 5d",
                    "d 150m",
                    "e 9000",
                    "f -30m",
                    "g 0s",
                    "h true",
                    "i true",
                    "j 3d",
                    "k 90m",
                    "l 2026-11-01",
                    "m 2026-11-01",
                    "n 2026-10-22",
                    "o 2026-10-29T12:30:00Z",
                ]
            );
        }

        phase a_day_moves_by_whole_days_only;
        {
            let sheet = Sheet::in_zone(0, None);
            sheet.call("fine", vec![Value::resource(&sheet.board)]);
            assert_eq!(
                sheet.view("read_back"),
                ["1970-01-01T00:00:00Z 2026-10-31 0"]
            );
            // A span code holds can be part of a day: the checker cannot tell it, so the day tells
            // it where it moves.
            let id = sheet.app.action("too_fine");
            let refused = sheet
                .rt
                .invoke_action(&me(), (id.as_str(), vec![Value::resource(&sheet.board)]));
            let said = refused.expect_err("an afternoon cannot move a day");
            assert!(
                said.to_string().contains("whole days"),
                "the reason says what a day moves by: {said}"
            );
        }
    }

    /// The clock a host keeps for the person, in a style of its own: 12-hour here, as a person
    /// who chose it in Settings has.
    struct TwelveHour;

    impl tilolang_runtime::ShownHost for TwelveHour {
        fn shown(&self, _kind: &str, value: &str) -> String {
            value.to_owned()
        }

        fn clock(&self, hour: u32, minute: u32) -> String {
            let half = if hour < 12 { "AM" } else { "PM" };
            let hour = match hour % 12 {
                0 => 12,
                hour => hour,
            };
            format!("{hour}:{minute:02} {half}")
        }
    }

    /// A moment says the time of day it falls at as the person reads a clock: the host's style
    /// where it keeps one, and a 24-hour clock where it names none.
    #[test]
    fn a_moment_reads_the_clock_the_person_reads() {
        let sheet = Sheet::in_zone(0, None);
        assert_eq!(
            sheet.bare("clocks"),
            ["a 10:41", "b 00:05", "c 14:05"],
            "no host keeps a style: a 24-hour clock"
        );
        sheet.rt.set_shown_host(TwelveHour);
        assert_eq!(
            sheet.bare("clocks"),
            ["a 10:41 AM", "b 12:05 AM", "c 2:05 PM"],
            "the clock the host keeps for the person"
        );
    }

    #[test]
    fn an_action_reads_its_own_moment_and_day() {
        let sheet = Sheet::in_zone(0, None);
        sheet.call("soon", vec![Value::resource(&sheet.board)]);
        assert_eq!(
            sheet.view("read_back"),
            ["2026-10-30T00:10:00Z 2026-11-01 79800"]
        );
        // The day is the moment's day: `today` is where `now` falls, not the next day of it.
        let sheet = Sheet::in_zone(2 * 3600, None);
        sheet.call("soon", vec![Value::resource(&sheet.board)]);
        assert_eq!(
            sheet.view("read_back"),
            ["2026-10-30T00:10:00Z 2026-11-02 600"]
        );
        // Two moments are apart by the seconds between them, whatever the day it is then.
        sheet.call("stamp", vec![Value::resource(&sheet.board)]);
        assert_eq!(
            sheet.view("read_back")[1],
            "1970-01-01T00:00:00Z 2026-11-04 1793311800",
            "a week ahead, two days back, then the seconds since the epoch"
        );
    }

    #[test]
    fn a_moment_falls_on_the_day_it_is_for_the_person() {
        // UTC: the late evening is its own day, and midnight is the next one.
        let utc = Sheet::in_zone(0, None);
        utc.seed();
        assert_eq!(
            utc.view("edges"),
            [
                "a 2026-10-29",
                "b 2026-10-29",
                "c 2026-10-30",
                "d 2026-10-30",
                "e 2026-10-29 2026-10-29",
                "f 2026-10-29",
                "g 2026-07-01",
            ]
        );

        // Two hours ahead: 22:00 UTC is already midnight of the next day.
        let east = Sheet::in_zone(2 * 3600, None);
        east.seed();
        assert_eq!(
            east.view("edges"),
            [
                "a 2026-10-29",
                "b 2026-10-30",
                "c 2026-10-30",
                "d 2026-10-30",
                "e 2026-10-30 2026-10-30",
                "f 2026-10-30",
                "g 2026-07-02",
            ]
        );

        // Seven hours behind: the early morning UTC is still the evening before, and midnight of
        // their day is 07:00 UTC.
        let west = Sheet::in_zone(-7 * 3600, None);
        west.seed();
        assert_eq!(
            west.view("edges"),
            [
                "a 2026-10-29",
                "b 2026-10-29",
                "c 2026-10-29",
                "d 2026-10-30",
                "e 2026-10-29 2026-10-29",
                "f 2026-10-29",
                "g 2026-07-01",
            ]
        );

        // A host that names the zone reads the day the moment fell on in it: Berlin was two hours
        // ahead in July, so 22:30 UTC that day was already the next one there, while the hour the
        // host's offset names today would have called it the same evening.
        let berlin = Sheet::in_zone(3600, Some("Europe/Berlin"));
        berlin.seed();
        assert_eq!(berlin.view("edges")[6], "g 2026-07-02");

        // The day an action read with `now.day()` is the day the filter's `today` reads, in each
        // zone: one source for the person's day.
        for (i, sheet) in [&utc, &east, &west, &berlin].into_iter().enumerate() {
            assert_eq!(sheet.view("agreed"), ["on today 1"], "case {i}");
        }
    }

    /// Text a program builds by interpolation names a day or a moment the way the person reads
    /// it, and `.iso()` is the stored text a machine reads.
    #[test]
    fn text_names_a_day_and_a_moment_as_the_person_reads_them() {
        // Nothing names them: a day and a moment keep their stored text.
        let sheet = Sheet::in_zone(0, None);
        assert_eq!(
            sheet.bare("named"),
            [
                "a 2026-06-29",
                "b 2025-06-29",
                "c 2026-06-29T18:00:00Z",
                "d 2026-06-29 2026-06-29T18:00:00Z",
            ],
            "what the formats name, and the same text from `.iso()`"
        );
        // What names them for the person is handed the kind of value and its stored text, and
        // `.iso()` still answers the stored text.
        sheet
            .rt
            .set_sentence_by(|kind, value| format!("<{kind} {value}>"));
        assert_eq!(
            sheet.bare("named"),
            [
                "a <date 2026-06-29>",
                "b <date 2025-06-29>",
                "c <time 2026-06-29T18:00:00Z>",
                "d 2026-06-29 2026-06-29T18:00:00Z",
            ]
        );
    }

    #[test]
    fn the_parts_of_a_day_and_the_week_it_stands_in() {
        // Thursday the 29th of October 2026; its week starts on the Monday of it.
        let monday = Sheet::in_zone(0, None);
        assert_eq!(
            monday.bare("parts"),
            ["a thursday", "b 29 10 2026", "c 2026-10-26", "d 2026-10-26"]
        );
        // A region whose week starts on Sunday reads the same day as in the week that
        // began the Sunday before it.
        let sunday = Sheet::at(NOW, 0, None, 6);
        assert_eq!(
            sunday.bare("parts"),
            ["a thursday", "b 29 10 2026", "c 2026-10-25", "d 2026-10-25"]
        );
    }

    #[test]
    fn words_name_a_day_or_a_moment_read_against_one() {
        // The spring night the clocks go forward in Berlin: tomorrow at nine is an hour nearer
        // than the day before's nine was, and the day begins an hour later in UTC than usual.
        let spring = Sheet::at("2026-03-28T12:00:00Z", 3600, Some("Europe/Berlin"), 0);
        spring.call(
            "read",
            vec![Value::resource(&spring.board), Value::text("tomorrow 9am")],
        );
        assert_eq!(
            spring.view("read_back"),
            ["2026-03-29T07:00:00Z 2026-03-29 0"]
        );
        spring.call(
            "read",
            vec![Value::resource(&spring.board), Value::text("tomorrow")],
        );
        // A text that names a day and no time is the moment that day starts.
        assert_eq!(
            spring.view("read_back")[1],
            "2026-03-28T23:00:00Z 2026-03-29 0"
        );

        // The autumn night they go back: nine is an hour further off.
        let autumn = Sheet::at("2026-10-24T12:00:00Z", 3600, Some("Europe/Berlin"), 0);
        autumn.call(
            "read",
            vec![Value::resource(&autumn.board), Value::text("tomorrow 9am")],
        );
        assert_eq!(
            autumn.view("read_back"),
            ["2026-10-25T08:00:00Z 2026-10-25 0"]
        );

        // A time alone is today's while it is still to come, else tomorrow's; words that name
        // nothing name no day.
        let evening = Sheet::at("2026-10-29T22:10:00Z", 0, None, 0);
        evening.call(
            "read",
            vec![Value::resource(&evening.board), Value::text("9am")],
        );
        assert_eq!(
            evening.view("read_back"),
            ["2026-10-30T09:00:00Z 2026-10-30 0"]
        );
        evening.call(
            "read",
            vec![Value::resource(&evening.board), Value::text("sometime")],
        );
        assert_eq!(
            evening.view("read_back")[1],
            "1970-01-01T00:00:00Z 1970-01-01 0",
            "words no phrase reads name nothing"
        );
    }

    tilo_test_env::scenario! {
        fn a_length_of_time_crosses_the_wasm_boundary;

        phase an_export_takes_a_duration_from_the_host;
        {
            let sheet = Sheet::in_zone(0, None);
            let defer = sheet.app.action("defer");
            let outcome = sheet
                .rt
                .invoke_action(
                    &me(),
                    (defer.as_str(), vec![Value::resource(&sheet.board), Value::Int(5400)]),
                )
                .expect("a duration crosses from the host as whole seconds");
            assert_eq!(outcome, InvokeOutcome::Done(Value::Unit));
            assert_eq!(
                sheet.view("read_back"),
                ["2026-10-29T23:40:00Z 1970-01-01 5400"]
            );

            let opened = sheet
                .rt
                .open_view(&me(), &sheet.app.view("span_label"), vec![Value::Int(5400)])
                .expect("a view export takes a duration from the host");
            assert_eq!(texts(opened.tree()), ["90m (5400)"]);

            for wrong in [Value::Bool(true), Value::text("90m"), Value::Float(90.0)] {
                let err = sheet
                    .rt
                    .invoke_action(
                        &me(),
                        (defer.as_str(), vec![Value::resource(&sheet.board), wrong.clone()]),
                    )
                    .expect_err("a wrong-typed argument for a duration is refused");
                assert!(
                    matches!(err, RuntimeError::BadArgs(_)),
                    "wrong value {wrong:?} refused as BadArgs, got {err:?}"
                );
            }
        }

        phase an_export_returns_a_duration_to_the_host;
        {
            let sheet = Sheet::in_zone(0, None);
            let since_hour = sheet.app.action("since_hour");
            let outcome = sheet
                .rt
                .invoke_action(&me(), (since_hour.as_str(), vec![Value::resource(&sheet.board)]))
                .expect("an action export returns a duration as whole seconds");
            assert_eq!(outcome, InvokeOutcome::Done(Value::Int(4200)));

            let extend = sheet.app.action("extend");
            let extended = sheet
                .rt
                .invoke_action(
                    &me(),
                    (
                        extend.as_str(),
                        vec![Value::resource(&sheet.board), Value::Int(1800)],
                    ),
                )
                .expect("an action export both takes and returns a duration");
            assert_eq!(extended, InvokeOutcome::Done(Value::Int(2700)));

            let default_span = sheet
                .rt
                .default_argument(extend.as_str(), 1, &[Value::resource(&sheet.board)])
                .expect("a duration parameter default unmarshals as whole seconds");
            assert_eq!(default_span, Some(Value::Int(2700)));

            let completed = sheet
                .rt
                .complete_args(extend.as_str(), vec![Some(Value::resource(&sheet.board))])
                .expect("completing a duration default succeeds");
            let from_default = sheet
                .rt
                .invoke_action(&me(), (extend.as_str(), completed))
                .expect("invoking with a completed duration default succeeds");
            assert_eq!(from_default, InvokeOutcome::Done(Value::Int(3600)));
        }

        phase a_cross_package_fn_takes_and_returns_a_duration;
        {
            let sheet = Sheet::in_zone(0, None);
            let caller = App::compile(
                "app so.tilo.time_caller \"Time Caller\"\n\
                 use so.tilo.time as time\n\
                 type Holder { secs: Int = 0 }\n\
                 action run(h: Holder, span: Duration) -> Duration {\n\
                     let doubled = time.double_span(span)\n\
                     h.secs = doubled.seconds()\n\
                     return doubled\n\
                 }\n",
                std::slice::from_ref(sheet.app.meta()),
            );
            caller.load(&sheet.rt);
            let holder = sheet.rt.create("Holder").unwrap();
            let run = caller.action("run");
            let outcome = sheet
                .rt
                .invoke_action(
                    &me(),
                    (run.as_str(), vec![Value::resource(&holder), Value::Int(2700)]),
                )
                .expect("a cross-package fn takes and returns a duration");
            assert_eq!(outcome, InvokeOutcome::Done(Value::Int(5400)));
            assert_eq!(sheet.rt.read(&holder, "secs").unwrap(), Value::Int(5400));
        }

        phase a_program_keeps_a_duration_and_handles_sent_duration_actions;
        {
            use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
            use tilolang_runtime::program::{ProgramOutcome, Session};

            struct AlwaysSent;
            impl Invoker for AlwaysSent {
                fn invoke(&self, _: &Invocation) -> Invoked {
                    Invoked::Sent
                }
            }

            let sheet = Sheet::in_zone(0, None);
            let mut session = Session::new();
            let agent = agent("claude");
            let kept = session.run(&sheet.rt, &agent, "let d = 90m");
            assert!(
                matches!(kept.outcome, ProgramOutcome::Done),
                "keeping a duration succeeds: {kept:?}"
            );
            let shown = session.run(&sheet.rt, &agent, "print(\"{d} {d.seconds()}\")");
            assert!(
                matches!(shown.outcome, ProgramOutcome::Done),
                "passing a kept duration into the next program succeeds: {shown:?}"
            );
            assert_eq!(shown.output, ["90m 5400"]);

            sheet.rt.set_invoker(AlwaysSent);
            let call = format!(
                "let d = time.since_hour(@time.Board:{})\nprint(\"{{d}} {{d.seconds()}}\")",
                sheet.board.id_string()
            );
            let sent = session.run(&sheet.rt, &agent, &call);
            assert!(
                matches!(sent.outcome, ProgramOutcome::Done),
                "a sent action returning Duration produces 0s rather than trapping: {sent:?}"
            );
            assert_eq!(sent.output, ["0s 0"]);
        }

        phase an_int_export_accepts_a_flag_while_a_duration_refuses_it;
        {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let app = App::compile(
                "app so.tilo.flag_int \"Flag Int\"\n\
                 action echo_int(n: Int) -> Int {\n\
                     return n\n\
                 }\n\
                 action echo_duration(span: Duration) -> Duration {\n\
                     return span\n\
                 }\n",
                &[],
            );
            app.load(&rt);
            let echo_int = app.action("echo_int");
            let echo_duration = app.action("echo_duration");

            assert_eq!(
                rt.invoke_action(&me(), (echo_int.as_str(), vec![Value::Bool(true)]))
                    .expect("an Int export reads a bare Bool flag as 1"),
                InvokeOutcome::Done(Value::Int(1))
            );
            assert_eq!(
                rt.invoke_action(&me(), (echo_int.as_str(), vec![Value::Bool(false)]))
                    .expect("an Int export reads a bare Bool false as 0"),
                InvokeOutcome::Done(Value::Int(0))
            );
            assert_eq!(
                rt.invoke_action(
                    &me(),
                    (echo_int.as_str(), vec![Value::some(Value::Bool(true))])
                )
                .expect("an Int export reads a boxed Bool flag as 1"),
                InvokeOutcome::Done(Value::Int(1))
            );

            assert_eq!(
                rt.invoke_action(&me(), (echo_duration.as_str(), vec![Value::Int(5400)]))
                    .expect("a Duration export takes bare whole seconds"),
                InvokeOutcome::Done(Value::Int(5400))
            );
            assert_eq!(
                rt.invoke_action(
                    &me(),
                    (echo_duration.as_str(), vec![Value::some(Value::Int(5400))])
                )
                .expect("a Duration export unboxes boxed whole seconds"),
                InvokeOutcome::Done(Value::Int(5400))
            );
            for wrong in [Value::Bool(true), Value::some(Value::Bool(true))] {
                let err = rt
                    .invoke_action(&me(), (echo_duration.as_str(), vec![wrong.clone()]))
                    .expect_err("a Duration export refuses bare and boxed flags");
                assert!(
                    matches!(err, RuntimeError::BadArgs(_)),
                    "wrong value {wrong:?} refused as BadArgs, got {err:?}"
                );
            }
        }
    }
}

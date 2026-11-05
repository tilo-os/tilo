//! `day_of(t)` is the day a moment falls on for the person, read in their zone as `today` reads the
//! day it is: the evening of one day is the next morning east of them and still the afternoon west
//! of them, a zone that names itself is the one that decides, and a day an action wrote is the day
//! a filter's `today` calls today.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value};

    const SOURCE: &str = r#"app so.tilo.dayof "Day of"

type Log {
    at: Time = Time("1970-01-01T00:00:00Z")
    stamp: Date = Date("1970-01-01")

    search {
        filter "Today" = stamp == today
    }
}

type Board {
    logs: Collection<Log>
}

action seed(b: Board) {
    let l = b.logs.insert(Log(at: 2026-10-29T23:30:00Z))
    // The day the action's own moment falls on: the filter's `today` is the same day.
    l.stamp = day_of(now)
}

view edges(b: Board) {
    text("a {day_of(2026-10-29T21:59:00Z)}")
    text("b {day_of(2026-10-29T22:00:00Z)}")
    text("c {day_of(2026-10-30T05:00:00Z)}")
    text("d {day_of(2026-10-30T07:00:00Z)}")
    for l in b.logs {
        text("e {day_of(l.at)}")
    }
    text("f {day_of(2026-10-29T22:30:00Z)}")
    text("g {day_of(2026-07-01T22:30:00Z)}")
}

view agreed(b: Board) {
    for f in filters(Log).filter(f -> f.toggle) {
        text("on today {b.logs.matching(f).len()}")
    }
}
"#;

    const NOW: &str = "2026-10-29T22:10:00Z";

    struct Days {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Days {
        /// A person whose clock reads `NOW` and whose offset is `offset`, in the zone `name`
        /// where the host names one.
        fn in_zone(offset: i64, name: Option<&str>) -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_clock(|| NOW.to_owned());
            rt.set_zone(move || offset);
            if let Some(name) = name {
                let name = name.to_owned();
                rt.set_zone_name(move || Some(name.clone()));
            }
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            let id = app.action("seed");
            rt.invoke_action(&me(), (id.as_str(), vec![Value::resource(&board)]))
                .unwrap();
            Self { rt, app, board }
        }

        fn edges(&self) -> Vec<String> {
            let view = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("edges"),
                    vec![Value::resource(&self.board)],
                )
                .unwrap();
            texts(view.tree())
        }

        /// What the filter reading `today` makes of the day the action read with `day_of(now)`.
        fn agreed(&self) -> Vec<String> {
            let view = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("agreed"),
                    vec![Value::resource(&self.board)],
                )
                .unwrap();
            texts(view.tree())
        }
    }

    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    #[test]
    fn a_moment_falls_on_the_day_it_is_for_the_person() {
        // UTC: the late evening is its own day, and midnight is the next one.
        let utc = Days::in_zone(0, None);
        assert_eq!(
            utc.edges(),
            [
                "a 2026-10-29",
                "b 2026-10-29",
                "c 2026-10-30",
                "d 2026-10-30",
                "e 2026-10-29",
                "f 2026-10-29",
                "g 2026-07-01",
            ]
        );

        // Two hours ahead: 22:00 UTC is already midnight of the next day.
        let east = Days::in_zone(2 * 3600, None);
        assert_eq!(
            east.edges(),
            [
                "a 2026-10-29",
                "b 2026-10-30",
                "c 2026-10-30",
                "d 2026-10-30",
                "e 2026-10-30",
                "f 2026-10-30",
                "g 2026-07-02",
            ]
        );

        // Seven hours behind: the early morning UTC is still the evening before, and midnight of
        // their day is 07:00 UTC.
        let west = Days::in_zone(-7 * 3600, None);
        assert_eq!(
            west.edges(),
            [
                "a 2026-10-29",
                "b 2026-10-29",
                "c 2026-10-29",
                "d 2026-10-30",
                "e 2026-10-29",
                "f 2026-10-29",
                "g 2026-07-01",
            ]
        );

        // A host that names the zone reads the day the moment fell on in it: Berlin was two hours
        // ahead in July, so 22:30 UTC that day was already the next one there, while the hour the
        // host's offset names today would have called it the same evening.
        let berlin = Days::in_zone(3600, Some("Europe/Berlin"));
        assert_eq!(berlin.edges()[6], "g 2026-07-02");

        // The day an action read with `day_of(now)` is the day the filter's `today` reads, in each
        // zone: one source for the person's day.
        for (i, days) in [&utc, &east, &west, &berlin].into_iter().enumerate() {
            assert_eq!(days.agreed(), ["on today 1"], "case {i}");
        }
    }
}

//! A time of no zone is the person's own time of day: `2026-06-30T09:00` is the instant it names
//! in their zone, so a sample task due at 09:00 shows 9:00 wherever it is seeded, and the same
//! literal in a zone an hour further east is another instant.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value};

    const SOURCE: &str = r#"app so.tilo.clock "Clock"

type Task {
    title: Text
    due: Time = Time("1970-01-01T00:00:00Z")
}

action seed(t: Task) uses time {
    t.due = 2026-06-30T09:00
}

view shown(t: Task) {
    text("{t.due}")
}
"#;

    struct Clock {
        rt: Runtime,
        app: App,
        task: ResourceRef,
    }

    impl Clock {
        fn in_zone(offset: i64, name: Option<&str>) -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_zone(move || offset);
            if let Some(name) = name {
                let name = name.to_owned();
                rt.set_zone_name(move || Some(name.clone()));
            }
            app.load(&rt);
            let task = rt.create("Task").unwrap();
            let id = app.action("seed");
            rt.invoke_action(&me(), (id.as_str(), vec![Value::resource(&task)]))
                .unwrap();
            Self { rt, app, task }
        }

        fn due(&self) -> Vec<String> {
            let view = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("shown"),
                    vec![Value::resource(&self.task)],
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
    fn a_wall_clock_time_is_the_instant_it_names_in_the_persons_zone() {
        // Seven hours behind UTC: 09:00 where they are is 16:00 UTC.
        let west = Clock::in_zone(-7 * 3600, None);
        assert_eq!(west.due(), ["2026-06-30T16:00:00Z"]);
        // Two hours ahead: the same words are another instant.
        let east = Clock::in_zone(2 * 3600, None);
        assert_eq!(east.due(), ["2026-06-30T07:00:00Z"]);
        // A zone that names itself keeps its wall time of day across a change of clocks.
        let berlin = Clock::in_zone(0, Some("Europe/Berlin"));
        assert_eq!(berlin.due(), ["2026-06-30T07:00:00Z"]);
    }
}

//! Clocks (`apps/tl/clocks`): the world clocks, the alarms and the instant each rings, the
//! timers, the stopwatch, and the notice an alarm rings as.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::collections::BTreeMap;
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, UiNode, Value,
    };

    const APP: &str = concat!(
        include_str!("../../../../apps/tl/clocks/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/clocks/scenarios.tl"),
    );

    struct Clocks {
        rt: Runtime,
        app: App,
        clocks: ResourceRef,
    }

    impl Clocks {
        fn new() -> Self {
            let app = App::compile(APP, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let clocks = rt.create_root("Clocks").unwrap();
            Self { rt, app, clocks }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// The case `name` of the app's `Weekday`, as a value: what `toggle_day` takes.
        fn weekday(&self, name: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids.get("Weekday").unwrap().to_owned(),
                variant: ids.get(name).unwrap().to_owned(),
                fields: BTreeMap::new(),
            })
        }

        fn view(&self) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("clocks_page"),
                    vec![Value::resource(&self.clocks)],
                )
                .unwrap()
        }
    }

    /// The stopwatch the clocks at `clocks` hold: the record of its own the field starts with.
    fn stopwatch_of(rt: &Runtime, clocks: &ResourceRef) -> ResourceRef {
        match rt.read(clocks, "stopwatch").unwrap() {
            Value::Ref(r) => (*r.target).clone(),
            other => panic!("the clocks hold a stopwatch, not {other:?}"),
        }
    }

    /// Whether any prop of any node is the text `wanted`: a card's title is a prop, not a text
    /// node.
    fn any_prop_text(tree: &UiNode, wanted: &str) -> bool {
        let here = tree.props.values().any(|value| match value {
            Value::Text(text) => text.contains(wanted),
            _ => false,
        });
        here || tree
            .children
            .iter()
            .any(|child| any_prop_text(child, wanted))
    }

    tilo_test_env::scenario! {
        fn clocks_keep_world_time_alarms_timers_and_the_stopwatch;

        phase a_city_is_added_once_and_removed;
        {
            let t = Clocks::new();
            let tokyo = returned_ref(t.call("add_world_clock", vec![
                Value::resource(&t.clocks),
                Value::text("Tokyo"),
                Value::text("Asia/Tokyo"),
            ]));
            // The same zone again is the clock that is already there.
            let again = returned_ref(t.call("add_world_clock", vec![
                Value::resource(&t.clocks),
                Value::text("Tokyo again"),
                Value::text("Asia/Tokyo"),
            ]));
            assert_eq!(again, tokyo);
            assert_eq!(t.rt.read(&tokyo, "city").unwrap(), Value::text("Tokyo"));
            assert_eq!(t.rt.read(&tokyo, "zone").unwrap(), Value::text("Asia/Tokyo"));
            t.call("remove_world_clock", vec![
                Value::resource(&t.clocks),
                Value::resource(&tokyo),
            ]);
        }

        phase an_alarm_holds_its_time_days_and_next_ring;
        {
            let t = Clocks::new();
            let alarm = returned_ref(t.call("add_alarm", vec![
                Value::resource(&t.clocks),
                Value::text("Wake up"),
                Value::Int(7),
                Value::Int(0),
            ]));
            assert_eq!(t.rt.read(&alarm, "label").unwrap(), Value::text("Wake up"));
            assert_eq!(t.rt.read(&alarm, "hour").unwrap(), Value::Int(7));
            assert_eq!(t.rt.read(&alarm, "minute").unwrap(), Value::Int(0));
            assert_eq!(t.rt.read(&alarm, "enabled").unwrap(), Value::Bool(true));
            assert!(
                matches!(t.rt.read(&alarm, "at").unwrap(), Value::Opt(Some(_))),
                "an alarm that is on names the instant it next rings"
            );
            t.call("toggle_day", vec![Value::resource(&alarm), t.weekday("Weekday.mon")]);
            assert_eq!(
                t.rt.read(&alarm, "days").unwrap(),
                Value::List(vec![t.weekday("Weekday.mon")]),
                "the day it repeats on is the one kept"
            );
            // Off, it names no instant, and on it names one again.
            t.call("set_alarm_enabled", vec![
                Value::resource(&alarm),
                Value::Bool(false),
            ]);
            assert_eq!(t.rt.read(&alarm, "at").unwrap(), Value::Opt(None));
            t.call("set_alarm_enabled", vec![
                Value::resource(&alarm),
                Value::Bool(true),
            ]);
            assert!(matches!(
                t.rt.read(&alarm, "at").unwrap(),
                Value::Opt(Some(_))
            ));
        }

        phase a_snoozed_alarm_rings_again_in_five_minutes;
        {
            let t = Clocks::new();
            let alarm = returned_ref(t.call("add_alarm", vec![
                Value::resource(&t.clocks),
                Value::text("Gym"),
                Value::Int(18),
                Value::Int(30),
            ]));
            t.call("snooze_alarm", vec![Value::resource(&alarm)]);
            assert_eq!(t.rt.read(&alarm, "snoozed").unwrap(), Value::Bool(true));
            assert!(
                matches!(t.rt.read(&alarm, "at").unwrap(), Value::Opt(Some(_))),
                "a snoozed alarm names when it rings again"
            );
        }

        phase a_timer_runs_pauses_and_takes_another_minute;
        {
            let t = Clocks::new();
            let timer = returned_ref(t.call("add_timer", vec![
                Value::resource(&t.clocks),
                Value::text("Tea"),
                Value::Int(300),
            ]));
            assert_eq!(t.rt.read(&timer, "label").unwrap(), Value::text("Tea"));
            assert!(matches!(t.rt.read(&timer, "ends_at").unwrap(), Value::Opt(Some(_))));
            t.call("pause_timer", vec![Value::resource(&timer)]);
            assert_eq!(t.rt.read(&timer, "ends_at").unwrap(), Value::Opt(None));
            assert!(
                matches!(t.rt.read(&timer, "left").unwrap(), Value::Opt(Some(_))),
                "a paused timer names what it has left"
            );
            t.call("add_minute", vec![Value::resource(&timer)]);
            t.call("resume_timer", vec![Value::resource(&timer)]);
            assert_eq!(t.rt.read(&timer, "left").unwrap(), Value::Opt(None));
            assert!(matches!(t.rt.read(&timer, "ends_at").unwrap(), Value::Opt(Some(_))));
            t.call("remove_timer", vec![
                Value::resource(&t.clocks),
                Value::resource(&timer),
            ]);
        }

        phase the_stopwatch_counts_laps_and_resets;
        {
            let t = Clocks::new();
            // The clocks start holding one stopwatch: a record of its own, made with the root.
            let stopwatch = stopwatch_of(&t.rt, &t.clocks);
            t.call("start_stopwatch", vec![Value::resource(&t.clocks)]);
            let since = t.rt.read(&stopwatch, "since").unwrap();
            assert!(matches!(since, Value::Opt(Some(_))), "{since:?}");
            t.call("lap", vec![Value::resource(&t.clocks)]);
            assert!(
                matches!(t.rt.read(&stopwatch, "laps").unwrap(), Value::List(laps) if laps.len() == 1),
                "the lap is one the stopwatch keeps"
            );
            t.call("pause_stopwatch", vec![Value::resource(&t.clocks)]);
            assert_eq!(
                t.rt.read(&stopwatch, "since").unwrap(),
                Value::Opt(None)
            );
            t.call("reset_stopwatch", vec![Value::resource(&t.clocks)]);
            assert_eq!(
                t.rt.read(&stopwatch, "elapsed").unwrap(),
                Value::Int(0)
            );
        }

        phase an_alarm_rings_as_a_notice_the_person_sees_on_the_devices_it_names;
        {
            let t = Clocks::new();
            let alarm = returned_ref(t.call("add_alarm", vec![
                Value::resource(&t.clocks),
                Value::text("Wake up"),
                Value::Int(7),
                Value::Int(0),
            ]));
            t.call("set_alarm_device", vec![
                Value::resource(&alarm),
                Value::text("phone"),
            ]);
            t.call("ring_alarm", vec![Value::resource(&alarm)]);
            // An alarm that rings once turns itself off; the notice it rang as stays.
            assert_eq!(t.rt.read(&alarm, "enabled").unwrap(), Value::Bool(false));
        }

        phase the_window_lists_each_place;
        {
            let t = Clocks::new();
            let mut view = t.view();
            assert!(
                !of_kind(view.tree(), "ui.NavShell").is_empty(),
                "the window is the app's shell"
            );
            t.call("add_timer", vec![
                Value::resource(&t.clocks),
                Value::text("Tea"),
                Value::Int(300),
            ]);
            view.poll_patches().unwrap();
            assert!(any_prop_text(view.tree(), "Tea"), "the timer stands in the window");
            assert!(any_prop_text(view.tree(), "Timers"), "the places stand beside it");
        }
    }

    /// The stopwatch is one record of the clocks' document: the same on both devices, so a lap
    /// one keeps is a lap the other shows.
    #[test]
    fn two_devices_see_the_same_laps() {
        let app = App::compile(APP, &[]);
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let (a, b) = (
            Runtime::new(store_a.clone(), TestAuthority::new()).unwrap(),
            Runtime::new(store_b.clone(), TestAuthority::new()).unwrap(),
        );
        app.load(&a);
        app.load(&b);
        let clocks_a = a.create_root("Clocks").unwrap();
        let clocks_b = b.create_root("Clocks").unwrap();
        store_a.sync_with(&mut store_b).unwrap();

        let call = |rt: &Runtime, action: &str, clocks: &ResourceRef| {
            rt.invoke_action(
                &me(),
                (app.action(action).as_str(), vec![Value::resource(clocks)]),
            )
            .unwrap_or_else(|error| panic!("{action}: {error}"));
        };
        call(&a, "start_stopwatch", &clocks_a);
        call(&a, "lap", &clocks_a);
        store_a.sync_with(&mut store_b).unwrap();

        let watch_a = stopwatch_of(&a, &clocks_a);
        let watch_b = stopwatch_of(&b, &clocks_b);
        assert_eq!(watch_a, watch_b, "one stopwatch on both devices");
        let laps = |rt: &Runtime, watch: &ResourceRef| match rt.read(watch, "laps").unwrap() {
            Value::List(laps) => laps,
            other => panic!("not laps: {other:?}"),
        };
        let (kept_a, kept_b) = (laps(&a, &watch_a), laps(&b, &watch_b));
        assert_eq!(kept_a.len(), 1, "the lap one device kept");
        assert_eq!(kept_a, kept_b, "the other device sees the same lap");
    }
}

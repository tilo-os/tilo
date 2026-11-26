//! An action gated on the clock: the condition that says whether it can run reads the moment
//! the offer is drawn at, and a view still has no clock of its own.

#[cfg(test)]
mod cases {
    #![allow(
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.gated "Gated"

type Door {
    name: Text
}

root doors: Door

default view door for Door

action unlatch(d: Door) uses time {
    require now > Time("2020-01-01T00:00:00Z") "Not yet"
    notice("The door opened")
}

view door(d: Door) {
    stack {
        button(unlatch(d)) as opener
    }
}
"#;

    fn button_state(now: &str) -> (bool, String) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        let at = now.to_owned();
        rt.set_clock(move || at.clone());
        app.load(&rt);
        let root = rt.create_root("Door").expect("the door's root");
        let view = rt
            .open_view(&me(), &app.view("door"), vec![Value::resource(&root)])
            .unwrap();
        let button = of_kind(view.tree(), "ui.Button")[0];
        // A control the guard lets run holds no `enabled: false`: the prop is only written to
        // turn one off.
        let enabled = button.props.get("enabled") != Some(&Value::Bool(false));
        let why = match button.props.get("description") {
            Some(Value::Text(why)) => why.clone(),
            _ => String::new(),
        };
        (enabled, why)
    }

    tilo_test_env::scenario! {
        fn a_clock_gated_action_says_at_the_offer_whether_it_can_run;

        phase a_move_the_clock_has_not_opened_is_offered_disabled_with_its_reason;
        {
            let (enabled, why) = button_state("2019-01-01T00:00:00Z");
            assert!(!enabled, "the moment has not come");
            assert_eq!(why, "Not yet");
        }

        phase a_move_the_clock_has_opened_is_offered_and_runs;
        {
            let (enabled, why) = button_state("2021-01-01T00:00:00Z");
            assert!(enabled, "the moment has come");
            assert_eq!(why, "");
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            rt.set_clock(|| "2021-01-01T00:00:00Z".to_owned());
            app.load(&rt);
            let root = rt.create_root("Door").unwrap();
            let opened = rt.invoke_action(
                &me(),
                (app.action("unlatch").as_str(), vec![Value::resource(&root)]),
            );
            assert!(opened.is_ok(), "{opened:?}");
        }
    }
}

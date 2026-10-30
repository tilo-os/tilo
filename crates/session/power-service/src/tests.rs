//! The policy's decisions, and the published resources a person or a test
//! takes them through. No bus: what logind and `UPower` say is the service
//! crate's tests' business.

use std::time::Duration;

use serde_json::Value;
use tilo_power_mgmt::{PowerAction, PowerCommand, PowerPolicy, PowerSignal};
use tilo_service::{Service as _, Started};

use crate::{BATTERY, PowerLink, SLEEP, SessionPower};

fn quiet() -> PowerPolicy {
    PowerPolicy {
        idle_blank: Some(Duration::from_secs(600)),
        idle_lock: Some(Duration::from_secs(300)),
        idle_suspend: None,
        lock_on_sleep: true,
        lid_close_action: PowerAction::Lock,
        power_key_action: PowerAction::Lock,
        critical_action: PowerAction::Lock,
        ..PowerPolicy::default()
    }
}

fn invoke(service: &mut SessionPower, action: &str, arguments: Value) -> Result<Started, String> {
    let Value::Object(arguments) = arguments else {
        return Err("arguments are an object".into());
    };
    service
        .invoke(SLEEP, action, &arguments)
        .map_err(|error| format!("{error:?}"))
}

#[test]
fn the_reading_is_the_policy_and_what_holds_the_machine_awake() {
    let link = PowerLink::new(quiet());
    assert_eq!(
        link.sleep(),
        crate::Sleep {
            blank_minutes: Some(10),
            lock_minutes: Some(5),
            suspend_minutes: None,
            lock_on_sleep: true,
            lid_close: "lock",
            power_key: "lock",
            held_awake: false,
            holds: Vec::new(),
        }
    );
    let ticket = link.keep_awake("a long download");
    assert!(link.suspend_held());
    let sleep = link.sleep();
    assert!(sleep.held_awake);
    assert_eq!(sleep.holds, vec!["a long download".to_owned()]);
    assert_eq!(link.holds(), vec![(ticket, "a long download".to_owned())]);
    assert!(link.release(ticket));
    assert!(!link.suspend_held());
    assert!(!link.release(ticket), "a ticket is released once");
}

#[test]
fn a_hold_taken_until_a_moment_stops_counting_then() {
    let link = PowerLink::new(quiet());
    let now = std::time::Instant::now();
    let _lease = link.keep_awake_until("a lease", now + Duration::from_secs(60));
    assert!(link.suspend_held());
    assert!(!link.registry().is_inhibited_for_at(
        tilo_power_mgmt::inhibit::INHIBIT_SUSPEND,
        now + Duration::from_secs(61)
    ));
}

#[test]
fn the_power_key_and_the_lid_follow_the_policy() {
    let link = PowerLink::new(quiet());
    assert_eq!(link.power_key(), Some(PowerAction::Lock));
    assert_eq!(link.lid(false), None, "opening the lid does nothing");
    assert_eq!(link.lid(true), Some(PowerAction::Lock));
    assert_eq!(link.act(PowerAction::Lock), "lock");
    assert!(link.bridge().drain_signals().contains(&PowerSignal::Lock));
    assert_eq!(link.act(PowerAction::Suspend), "suspend");
    assert_eq!(link.bridge().drain_commands(), vec![PowerCommand::Suspend]);
    assert_eq!(link.act(PowerAction::Ignore), "ignore");
    assert!(link.bridge().drain_commands().is_empty());
}

#[test]
fn a_new_policy_reaches_the_service_and_the_reading() {
    let link = PowerLink::new(quiet());
    link.set_policy(PowerPolicy {
        idle_suspend: Some(Duration::from_secs(900)),
        power_key_action: PowerAction::Ignore,
        ..quiet()
    });
    assert!(
        link.bridge()
            .drain_commands()
            .contains(&PowerCommand::UpdatePolicy(link.policy()))
    );
    assert_eq!(link.sleep().suspend_minutes, Some(15));
    assert_eq!(link.sleep().power_key, "ignore");
    assert_eq!(link.power_key(), None);
}

#[test]
fn the_published_resource_holds_the_machine_awake_and_releases_it() {
    let link = PowerLink::new(quiet());
    let mut service = SessionPower::without_daemons(link.clone()).expect("service");
    let resources = service.resources();
    let sleep = resources
        .iter()
        .find(|resource| resource.id.as_str() == SLEEP)
        .expect("the sleep resource");
    for action in ["keep-awake", "release", "power-key", "lid"] {
        assert!(
            sleep.actions.iter().any(|spec| spec.id == action),
            "{action} is offered: {:?}",
            sleep
                .actions
                .iter()
                .map(|spec| spec.id.as_str())
                .collect::<Vec<_>>()
        );
    }
    assert!(service.read(SLEEP).is_some());
    assert!(service.read(BATTERY).is_some());
    assert!(service.read("power").is_none(), "another service's");

    let Started::Done(output) = invoke(
        &mut service,
        "keep-awake",
        serde_json::json!({ "reason": "a backup runs" }),
    )
    .expect("a hold") else {
        panic!("a hold answers at once")
    };
    let ticket = output
        .value
        .as_ref()
        .and_then(|value| value.get("ticket"))
        .and_then(Value::as_str)
        .expect("the ticket")
        .to_owned();
    assert!(link.suspend_held());
    let reading = service.read(SLEEP).expect("reading");
    assert_eq!(reading["held_awake"], Value::Bool(true), "{reading}");
    assert_eq!(
        reading["holds"],
        serde_json::json!(["a backup runs"]),
        "{reading}"
    );

    invoke(
        &mut service,
        "release",
        serde_json::json!({ "ticket": ticket }),
    )
    .expect("the release");
    assert!(!link.suspend_held());
    assert!(
        invoke(
            &mut service,
            "release",
            serde_json::json!({ "ticket": ticket })
        )
        .is_err(),
        "a released hold is no hold"
    );
    assert!(
        invoke(
            &mut service,
            "keep-awake",
            serde_json::json!({ "reason": " " })
        )
        .is_err(),
        "a hold says why"
    );
}

#[test]
fn the_power_key_and_the_lid_are_actions_too() {
    let link = PowerLink::new(quiet());
    let mut service = SessionPower::without_daemons(link.clone()).expect("service");
    let Started::Done(key) =
        invoke(&mut service, "power-key", serde_json::json!({})).expect("the power key")
    else {
        panic!("resolves at once")
    };
    assert_eq!(key.value, Some(serde_json::json!({ "action": "lock" })));
    assert!(link.bridge().drain_signals().contains(&PowerSignal::Lock));

    let Started::Done(lid) =
        invoke(&mut service, "lid", serde_json::json!({ "closed": true })).expect("the lid")
    else {
        panic!("resolves at once")
    };
    assert_eq!(lid.value, Some(serde_json::json!({ "action": "lock" })));

    link.set_policy(PowerPolicy {
        lid_close_action: PowerAction::Ignore,
        power_key_action: PowerAction::Ignore,
        ..quiet()
    });
    let Started::Done(none) =
        invoke(&mut service, "lid", serde_json::json!({ "closed": true })).expect("the lid")
    else {
        panic!("resolves at once")
    };
    assert_eq!(none.value, None, "an ignored lid does nothing");
}

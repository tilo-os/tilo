//! A desktop whose device's daemon restarts: the session asks the daemon for a new link for the
//! same seat by itself, the seat's providers are served on it again, and the person's actions are
//! listed and invoked through the daemon's dispatch as before.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use serde_json::json;
use tilo_actions::remote::CatalogueRow;

/// The action the person's Settings offers on the daemon's dispatch: the seat's provider the
/// session serves on its link.
fn settings_offered(rows: &[CatalogueRow]) -> bool {
    rows.iter().any(|row| {
        row.action.as_ref().is_some_and(|action| {
            action.address.resource == "settings/appearance"
                && action.address.action == "set-dark-mode"
        })
    })
}

#[test]
fn a_session_relinks_to_its_daemon_after_it_restarts() {
    let person = crate::common::people::Person::new("Ada");
    let mut device = person.device("laptop");
    device.start();
    // The shell shows the person's workspace once the daemon holds it: what its actions are
    // listed in.
    let workspace = tilo_test_env::wait_for("the shell to show a workspace", || device.showing());
    device.wait_holding(&workspace);
    // The session's actions are the daemon's dispatch, which lists what the seat serves there.
    let until = tilo_test_env::hang_deadline();
    loop {
        if settings_offered(&device.actions()) {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "{} lists the person's settings action before the restart:\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // The daemon goes away: the session keeps running and answering, saying it reconnects.
    device.stop_daemon();
    let windows = device.ask(&json!({"op": "windows"}));
    assert!(
        windows.get("answer").is_some(),
        "the session answers while its daemon is away: {windows}"
    );
    let until = tilo_test_env::hang_deadline();
    loop {
        let link = device.ask(&json!({"op": "daemon_link"}));
        if link
            .pointer("/answer/linked")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "{} says it reconnects while its daemon is away: {link}",
            device.label
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // It comes back over the state it kept; the session asks it for a new link for the same seat,
    // and the seat's providers are served on it again.
    device.restart_daemon();
    let until = tilo_test_env::hang_deadline();
    loop {
        let link = device.ask(&json!({"op": "daemon_link"}));
        if link
            .pointer("/answer/linked")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "{} relinks to its daemon: {link}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    loop {
        if settings_offered(&device.actions()) {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "{} lists the person's settings action again after the restart:\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // And the person's action runs on the new link: the appearance changes.
    let target = device
        .offered("settings/appearance", "set-dark-mode")
        .expect("the settings action is listed again");
    let answer = device.act_wait(&target, &json!({"on": true}));
    assert!(
        answer.get("err").is_none(),
        "the person's action runs on the link they were reconnected on: {answer}\n{}",
        device.diagnostic()
    );
}

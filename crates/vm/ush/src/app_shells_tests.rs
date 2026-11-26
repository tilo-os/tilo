#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use std::sync::Arc;
use std::sync::mpsc;

use tilo_action_services::{CallLanes as _, Take};
use tilo_actions::remote::RemoteActionMessage;
use tilo_actions::remote::shell::ShellOpen;

use super::{AppShells, Out};
use crate::control_line::ControlLine;

/// What the lane answers the app's open with, once it has.
fn answer_to_open(allowed: super::Allowed) -> serde_json::Value {
    // A runtime that is not there: an open that reaches it is unavailable, never refused.
    let ush: &'static ControlLine = Box::leak(Box::new(ControlLine::at(
        std::env::temp_dir().join("no-shell-runtime-here.sock"),
    )));
    let (said, heard) = mpsc::channel();
    let out: Out = Arc::new(move |line| {
        let _ = said.send(line);
    });
    let shells = AppShells::new("workspace", Ok(()), allowed, ush, out);
    let open = RemoteActionMessage::ShellOpenRequest {
        correlation: 7,
        open: ShellOpen {
            target: serde_json::json!({"kind": "host"}),
            cols: 80,
            rows: 24,
            ..ShellOpen::default()
        },
    };
    assert!(
        matches!(shells.take(&open), Take::Silent),
        "it answers later"
    );
    let line = heard
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the open is answered");
    let line: serde_json::Value = serde_json::from_str(&line).unwrap();
    line.pointer("/message/ShellOpenResponse/result")
        .cloned()
        .unwrap_or_else(|| panic!("no open answer: {line}"))
}

/// A workspace other people are members of runs its programs in a VM on a host: an app's open
/// is refused in the words the computer gives, before the shell runtime is asked anything.
#[test]
fn a_shell_the_computer_does_not_run_is_refused_before_the_runtime_is_asked() {
    let words = "this computer runs only its own person's workspace's programs";
    let refused = answer_to_open(Arc::new(move || Err(words.to_owned())));
    assert_eq!(
        refused.pointer("/Err/Refused/reason"),
        Some(&serde_json::Value::from(words)),
        "{refused}"
    );

    let asked = answer_to_open(Arc::new(|| Ok(())));
    assert!(
        asked.pointer("/Err/Unavailable").is_some(),
        "an allowed open goes on to the runtime: {asked}"
    );
}

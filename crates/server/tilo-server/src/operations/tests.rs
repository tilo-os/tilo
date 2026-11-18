//! A stand-in for the root helper proves each operation on the real path:
//! the daemon writes its request where the helper reads it, waits for the
//! answer a stale one is never taken for, and keeps what it did.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tilo_server_protocol::transport::{ServerAnswer, ServerOperation};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::{CONTROL_REQUEST, HISTORY, MAX_LOG_LINES, operate_within, parse_answer, valid_service};

fn own() -> DeviceCert {
    DeviceCert {
        user_public_key: [1; 32],
        device_public_key: [2; 32],
        device_label: "server".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    }
}

/// Answer every request written at `state_root` with `answer`, as the
/// root helper does, until the returned guard is dropped.
fn stand_in_helper(
    state_root: &Path,
    answer: impl Fn(&str, &str) -> String + Send + 'static,
) -> Guard {
    let path = state_root.join(CONTROL_REQUEST);
    let result = state_root.join(super::CONTROL_RESULT);
    let task = tokio::spawn(async move {
        let mut seen = String::new();
        loop {
            if let Ok(request) = std::fs::read_to_string(&path)
                && request != seen
            {
                seen = request.clone();
                let verb = field(&request, "verb");
                let answer = answer(&verb, &request);
                std::fs::write(&result, answer).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    Guard(task)
}

struct Guard(tokio::task::JoinHandle<()>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn field(request: &str, key: &str) -> String {
    request
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_default()
        .to_owned()
}

fn identity_of(request: &str) -> String {
    field(request, "id")
}

#[tokio::test]
async fn every_mutation_reaches_the_helper_and_is_kept() {
    let temp = tempfile::tempdir().unwrap();
    let state_root = temp.path().to_path_buf();
    let _helper = stand_in_helper(&state_root, |verb, request| match verb {
        "logs" => {
            let id = identity_of(request);
            format!("{id} 1\nNov 18 tilo-server: serving\n")
        }
        _ => format!("{} 1\n{verb} done", identity_of(request)),
    });
    let ask = |operation| {
        let state_root = state_root.clone();
        async move {
            operate_within(&state_root, None, &own(), operation, Duration::from_secs(5))
                .await
                .unwrap()
        }
    };
    for (operation, verb) in [
        (
            ServerOperation::Restart {
                service: "tilo-server.service".into(),
            },
            "restart",
        ),
        (ServerOperation::Update, "update"),
        (ServerOperation::PauseUpdates, "pause-updates"),
        (ServerOperation::ResumeUpdates, "resume-updates"),
    ] {
        let result = ask(operation).await;
        assert_eq!(result.operation, verb);
        assert!(result.done, "{result:?}");
        assert_eq!(result.answer, ServerAnswer::Nothing);
    }
    let logs = ask(ServerOperation::Logs {
        service: "tilo-server.service".into(),
        lines: 5,
    })
    .await;
    assert_eq!(logs.operation, "logs");
    assert_eq!(
        logs.answer,
        ServerAnswer::Text("Nov 18 tilo-server: serving\n".into())
    );

    // Rebooting and rolling back change a boot, so the answer is the request
    // and not a wait: what the helper would say would not outlive the machine
    // going down.
    let reboot = ask(ServerOperation::Reboot).await;
    assert!(!reboot.done);
    assert_eq!(reboot.operation, "reboot");
    let rollback = ask(ServerOperation::Rollback).await;
    assert!(!rollback.done);

    // Every one of them is in the history the next `Status` reads back.
    let status = ask(ServerOperation::Status).await;
    let ServerAnswer::Status(status) = status.answer else {
        panic!("status answers with the server");
    };
    let asked = status
        .history
        .iter()
        .map(|entry| entry.operation.as_str())
        .collect::<Vec<_>>();
    for verb in [
        "restart",
        "update",
        "pause-updates",
        "resume-updates",
        "logs",
        "reboot",
        "rollback",
    ] {
        assert!(asked.contains(&verb), "{asked:?} lacks {verb}");
    }
    assert!(state_root.join(HISTORY).exists());
}

#[tokio::test]
async fn a_stale_answer_is_never_taken_for_the_request() {
    let temp = tempfile::tempdir().unwrap();
    let state_root = temp.path().to_path_buf();
    std::fs::write(
        state_root.join(super::CONTROL_RESULT),
        "an-earlier-request 1\nold\n",
    )
    .unwrap();
    let failed = operate_within(
        &state_root,
        None,
        &own(),
        ServerOperation::Restart {
            service: "tilo-server.service".into(),
        },
        Duration::from_millis(300),
    )
    .await;
    assert!(
        failed.unwrap_err().contains("did not answer"),
        "an answer to another request is not this one's"
    );
}

#[tokio::test]
async fn a_helper_that_refuses_answers_with_why() {
    let temp = tempfile::tempdir().unwrap();
    let state_root = temp.path().to_path_buf();
    let _helper = stand_in_helper(&state_root, |_, request| {
        format!("{} 0\nunit not found", identity_of(request))
    });
    let refused = operate_within(
        &state_root,
        None,
        &own(),
        ServerOperation::Restart {
            service: "missing.service".into(),
        },
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(refused.unwrap_err(), "unit not found");
}

#[test]
fn a_service_is_a_unit_name_and_nothing_else() {
    assert_eq!(
        valid_service("tilo-server.service").unwrap(),
        "tilo-server.service"
    );
    assert_eq!(valid_service("tilo-server").unwrap(), "tilo-server.service");
    assert_eq!(
        valid_service("tilo-fs-replica@1.service").unwrap(),
        "tilo-fs-replica@1.service"
    );
    for refused in [
        "../../bin/sh",
        "tilo-server.service --now",
        "$(reboot)",
        "tilo server",
        "",
        "/usr/libexec/tilo-server",
    ] {
        assert!(valid_service(refused).is_err(), "{refused:?} passed");
    }
}

#[test]
fn journal_lines_are_capped() {
    assert_eq!(MAX_LOG_LINES, 500);
    assert_eq!(
        parse_answer("abc 1\nline\nline 2\n"),
        Some(super::ControlAnswer {
            id: "abc".into(),
            ok: true,
            text: "line\nline 2\n".into(),
        })
    );
    assert_eq!(parse_answer("no newline"), None);
    assert_eq!(parse_answer("abc 7\n"), None);
}

#[test]
fn the_status_says_which_way_this_boots_volume_opened() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("tilo-unlock.json");
    assert_eq!(super::opening_at(&path), None, "no note, no claim");
    std::fs::write(
        &path,
        r#"{"method":"sibling-server","at_ns":1700000000000000000}"#,
    )
    .unwrap();
    assert_eq!(
        super::opening_at(&path),
        Some(tilo_unlock_protocol::opened::Opening {
            method: tilo_unlock_protocol::opened::UnlockMethod::SiblingServer,
            at_ns: 1_700_000_000_000_000_000,
        })
    );
    std::fs::write(&path, "not a note").unwrap();
    assert_eq!(
        super::opening_at(&path),
        None,
        "a file that is not one says nothing"
    );
}

#[test]
fn history_keeps_the_newest() {
    let temp = tempfile::tempdir().unwrap();
    let state_root: PathBuf = temp.path().to_path_buf();
    for index in 0..(super::HISTORY_KEPT + 5) {
        super::keep(
            &state_root,
            &tilo_server_protocol::transport::ServerOperationResult {
                operation: format!("op{index}"),
                done: true,
                summary: "did it".into(),
                answer: ServerAnswer::Nothing,
            },
        );
    }
    let kept = super::history(&state_root);
    assert_eq!(kept.len(), super::HISTORY_KEPT);
    assert_eq!(kept[0].operation, format!("op{}", super::HISTORY_KEPT + 4));
}

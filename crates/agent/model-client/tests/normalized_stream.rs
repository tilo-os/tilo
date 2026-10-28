#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "fixture setup and unexpected protocol failures must fail the test"
)]

use agent_provider_config::protocol::{
    Content, Conversation, ErrorKind, Message, ModelResponse, Role, StopReason, StreamEvent,
    StreamState, StreamStatus, ToolCall, Usage,
};
use serde_json::json;
use std::os::fd::AsRawFd;
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::process::{Command, Stdio};
use tilo_model_client::{Frame, Request, Response, read_frame, write_frame};

fn answer(duplicate: bool) -> ModelResponse {
    let call = Content::ToolCall(ToolCall {
        id: "call".into(),
        name: "invoke".into(),
        input: json!({}),
        extra_content: None,
    });
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: if duplicate {
                vec![call.clone(), call]
            } else {
                vec![call]
            },
        },
        stop_reason: StopReason::ToolCalls,
        usage: Usage::default(),
    }
}

fn send(peer: &mut UnixStream, event: Option<&str>, output: &str) {
    write_frame(
        peer,
        &Response {
            ok: true,
            output: Some(output.into()),
            configured: None,
            error: None,
            model_error: None,
            event: event.map(str::to_owned),
        },
    )
    .unwrap();
}

fn child(case: &str) {
    let request = Conversation::new("system", "question").request(vec![], 64);
    let mut state = StreamState::default();
    let mut terminals = 0_usize;
    let error = tilo_model_client::complete_streaming(
        "profile",
        "model",
        request.clone(),
        tilo_test_env::hang_bound(),
        &mut |event| {
            if matches!(
                event,
                StreamEvent::Finished { .. } | StreamEvent::Failed { .. }
            ) {
                terminals = terminals.saturating_add(1);
            }
            state.apply(event);
        },
    )
    .unwrap_err()
    .model_error();
    assert_eq!(
        error.kind,
        if case == "eof" {
            ErrorKind::Truncated
        } else {
            ErrorKind::InvalidResponse
        }
    );
    assert_eq!(terminals, 1);
    assert_eq!(state.text, "partial answer");
    assert!(state.response().is_none());
    assert_eq!(
        state.status,
        if case == "eof" {
            StreamStatus::Truncated
        } else {
            StreamStatus::Failed
        }
    );
    if case != "eof" {
        // Invalid deltas must be drained, never replayed into the next turn.
        let response = tilo_model_client::complete_streaming(
            "profile",
            "model",
            request,
            tilo_test_env::hang_bound(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(response, answer(false));
    }
}

#[test]
fn synchronous_stream_validates_finals_and_drains_poisoned_turns() {
    if let Ok(case) = std::env::var("NORMALIZED_STREAM_CASE") {
        child(&case);
        return;
    }
    for case in ["event", "duplicate", "eof"] {
        let (app, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        peer.set_write_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let fd = app.as_raw_fd();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "synchronous_stream_validates_finals_and_drains_poisoned_turns",
                "--nocapture",
            ])
            .env_clear()
            .env("TILO_SVC_MODEL-GATEWAY_FD", fd.to_string())
            .env("NORMALIZED_STREAM_CASE", case)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: only fcntl runs after fork; the connected descriptor stays
        // owned by the parent until the child has inherited it.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command.spawn().unwrap();
        drop(app);
        let serving = std::thread::spawn(move || {
            let request: Frame<Request> = read_frame(&mut peer).unwrap();
            assert!(matches!(
                request.value,
                Request::Complete { stream: true, .. }
            ));
            send(&mut peer, Some("text_delta"), r#""partial answer""#);
            send(
                &mut peer,
                Some("tool_call_delta"),
                r#"{"index":0,"id":"call","name":"invoke","delta":"{}"}"#,
            );
            send(
                &mut peer,
                Some("tool_call"),
                r#"{"id":"call","name":"invoke","arguments":"{}"}"#,
            );
            if case == "eof" {
                return;
            }
            if case == "event" {
                send(&mut peer, Some("reasoning_delta"), "{}");
                send(&mut peer, Some("text_delta"), r#""not delivered""#);
            }
            send(
                &mut peer,
                None,
                &serde_json::to_string(&answer(case == "duplicate")).unwrap(),
            );
            let request: Frame<Request> = read_frame(&mut peer).unwrap();
            assert!(matches!(
                request.value,
                Request::Complete { stream: true, .. }
            ));
            send(
                &mut peer,
                None,
                &serde_json::to_string(&answer(false)).unwrap(),
            );
        });
        let output = process.wait_with_output().unwrap();
        serving.join().unwrap();
        assert!(
            output.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

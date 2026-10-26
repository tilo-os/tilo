#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use tilo_model_client::{ClientError, Frame, Request, Response, read_frame, write_frame};

fn selection_request() -> Request {
    serde_json::from_value(serde_json::json!({
        "verb": "selection_configured",
        "profile_id": "test-provider",
        "model_id": "test-model",
        "protocol": "open_ai_compatible",
    }))
    .unwrap()
}

fn child_request(case: &str) {
    if matches!(case, "legacy-alias" | "absent" | "invalid") {
        assert!(!tilo_model_client::available());
        assert!(matches!(
            tilo_model_client::request(selection_request()),
            Err(ClientError::Unavailable)
        ));
        #[cfg(feature = "runtime-client")]
        {
            use agent_provider_config::protocol::ErrorKind;
            use tilo_model_client::runtime_client;
            assert!(
                matches!(runtime_client::from_launcher(), Err(error) if error.kind == ErrorKind::Unavailable)
            );
            assert!(
                matches!(runtime_client::provider_admin_from_launcher(), Err(error) if error.kind == ErrorKind::Unavailable)
            );
        }
        return;
    }
    assert!(
        tilo_model_client::available(),
        "the actual launcher-provided connected descriptor must be available"
    );
    // A request must duplicate the descriptor, preserving the inherited
    // capability for subsequent operations in the same managed child.
    for _ in 0..2 {
        let response = tilo_model_client::request(selection_request()).unwrap();
        assert!(response.ok);
        assert_eq!(response.configured, Some(true));
        assert!(response.output.is_none());
        assert!(response.error.is_none());
    }
}

fn run_child(test: &str, case: &str) {
    if let Ok(case) = std::env::var("GATEWAY_DESCRIPTOR_TEST_CASE") {
        child_request(&case);
        return;
    }
    let (app, mut service) = UnixStream::pair().unwrap();
    app.set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    app.set_write_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    service
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    service
        .set_write_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let fd = app.as_raw_fd();
    assert!(fd >= 3);
    // Authored by launcher::sandbox::svc_env_var("model-gateway") and also
    // used by runtime_portal::ServiceClient. Deliberately independent of the
    // gateway client constant: this is the real producer's environment shape.
    let name = if case == "legacy-alias" {
        "TILO_SVC_MODEL_GATEWAY_FD"
    } else {
        "TILO_SVC_MODEL-GATEWAY_FD"
    };
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env_clear()
        .env(name, fd.to_string())
        .env("GATEWAY_DESCRIPTOR_TEST_CASE", case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match case {
        "absent" => {
            command.env_remove(tilo_model_client::SERVICE_FD_ENV);
        }
        "invalid" => {
            command.env(tilo_model_client::SERVICE_FD_ENV, "2");
        }
        _ => {}
    }
    // SAFETY: the child only clears CLOEXEC on its inherited connected socket.
    // The callback invokes only fcntl and does not access parent-thread state.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    drop(app);
    let peer = std::thread::spawn(move || {
        let mut requests = 0;
        while let Ok(frame) = read_frame::<Frame<Request>>(&mut service) {
            assert_eq!(frame.version, 3);
            assert_eq!(
                serde_json::to_value(frame.value).unwrap(),
                serde_json::to_value(selection_request()).unwrap()
            );
            requests += 1;
            write_frame(
                &mut service,
                &Response {
                    ok: true,
                    configured: Some(true),
                    output: None,
                    error: None,
                    model_error: None,
                    event: None,
                },
            )
            .unwrap();
        }
        requests
    });
    let output = child.wait_with_output().unwrap();
    let requests = peer.join().unwrap();
    assert!(
        output.status.success(),
        "isolated client failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests, if case == "launcher" { 2 } else { 0 });
}

#[test]
fn launcher_named_inherited_descriptor_supports_available_and_requests() {
    run_child(
        "launcher_named_inherited_descriptor_supports_available_and_requests",
        "launcher",
    );
}

#[test]
fn missing_launcher_capability_never_discovers_an_ambient_service() {
    run_child(
        "missing_launcher_capability_never_discovers_an_ambient_service",
        "absent",
    );
}

#[test]
fn standard_io_descriptor_is_not_a_launcher_capability() {
    run_child(
        "standard_io_descriptor_is_not_a_launcher_capability",
        "invalid",
    );
}

#[test]
fn unissued_underscore_alias_is_not_a_gateway_capability() {
    run_child(
        "unissued_underscore_alias_is_not_a_gateway_capability",
        "legacy-alias",
    );
}

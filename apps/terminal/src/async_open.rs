//! Hosted opens run away from the UI and retain a cancellable connection.

use crate::target::SessionTarget;
use crate::{HostedProcessServices, OpenError, Requester, TerminalBackend};
use runtime_portal::pending_connection::PendingResponse;
use std::sync::{Arc, Mutex, mpsc};

pub(crate) struct PendingOpen {
    answer: PendingResponse<Result<TerminalBackend, OpenError>>,
    pub requester: Requester,
    pub attach: Option<String>,
}

impl PendingOpen {
    pub fn start(
        first: Option<HostedProcessServices>,
        target: SessionTarget,
        requester: Requester,
        attach: Option<String>,
        workspace: Option<String>,
        waker: Arc<Mutex<ui::WakeHandle>>,
    ) -> Result<Self, OpenError> {
        let worker_attach = attach.clone();
        let answer = PendingResponse::start(
            "tilo-terminal-open",
            move |cancellation| {
                HostedProcessServices::open_session(
                    first,
                    &target,
                    requester,
                    worker_attach.as_deref(),
                    workspace.as_deref(),
                    Some(&cancellation),
                )
            },
            move || crate::inner::request_terminal_wake(&waker),
        )
        .map_err(|e| OpenError::Failed(e.to_string()))?;
        Ok(Self {
            answer,
            requester,
            attach,
        })
    }

    pub fn take_answer(&self) -> Option<Result<TerminalBackend, OpenError>> {
        match self.answer.take_answer() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(OpenError::Failed(
                "session open worker ended without an answer".into(),
            ))),
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixStream;
    use tilo_test_env::hang_bound;
    use ui::widget::Widget;

    /// The persistent-control handshake that precedes every open: read the
    /// client's hello and answer ready. Returns the reader holding any bytes
    /// buffered past it, so the open request is read from the same stream.
    fn accept_hello(server: &UnixStream) -> std::io::BufReader<&UnixStream> {
        let mut reader = std::io::BufReader::new(server);
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        assert!(hello.contains("\"op\":\"hello\""), "{hello}");
        (&*server)
            .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
            .unwrap();
        reader
    }

    /// Answer an open with a PTY and a private session lease; the returned
    /// supervisor end of the lease reads end-of-file once the tab drops it.
    fn grant_session(server: &UnixStream, session: &str) -> UnixStream {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .unwrap();
        let (supervisor_lease, terminal_lease) = UnixStream::pair().unwrap();
        let reply = format!(
            "{{\"result\":\"session\",\"protocol\":2,\"session\":\"{session}\",\"environment\":\"default\"}}\n"
        );
        runtime_portal::fd_transport::send_with_fds(
            server,
            reply.as_bytes(),
            // SAFETY: `pair` owns the master and outlives this call.
            &[
                unsafe { std::os::fd::BorrowedFd::borrow_raw(pair.master.as_raw_fd().unwrap()) },
                std::os::fd::AsFd::as_fd(&terminal_lease),
            ],
        )
        .unwrap();
        supervisor_lease
    }

    fn tab(socket: UnixStream) -> crate::TerminalWidget {
        crate::TerminalWidget::spawn_hosted(HostedProcessServices { ush: Some(socket) }).unwrap()
    }

    fn finish(widget: &mut crate::TerminalWidget, doc: &automerge::AutoCommit) {
        tilo_test_env::wait_until("the open to finish", || {
            widget.update(doc, &automerge::ROOT);
            !matches!(widget.session_state(), crate::sessions::OpenState::Opening)
        });
    }

    /// The `open-session` line a hosted tab shown in `window` sends its
    /// supervisor when it opens, as the supervisor reads it.
    fn open_request_from(window: &ui::hosted_window::HostedWindow) -> serde_json::Value {
        // The window bar then lists the environments off the UI thread over
        // the runtime directory's socket: pin that to an empty directory so
        // the listing never reaches another test's runtime.
        let runtime = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let (client, server) = UnixStream::pair().unwrap();
        let supervisor = std::thread::spawn(move || {
            let mut reader = accept_hello(&server);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            (&server)
                .write_all(b"{\"result\":\"no-runtime\",\"reason\":\"checked\"}\n")
                .unwrap();
            line
        });
        let mut widget = tab(client);
        widget.show_in_window(Some(window.clone()));
        let doc = automerge::AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        finish(&mut widget, &doc);
        tilo_test_env::wait_until("the window bar's listing", || {
            widget.sync(&doc, &automerge::ROOT);
            widget.sessions().directory != crate::sessions::Directory::Unasked
        });
        let request = serde_json::from_str(&supervisor.join().unwrap()).unwrap();
        drop(widget);
        request
    }

    #[test]
    fn an_open_names_the_workspace_the_window_is_in_and_none_when_unknown() {
        let window = ui::hosted_window::HostedWindow::new();
        window.set_workspace(Some("e787c8a3-e6d5-4b00-90fe-ff1af708b979".into()));
        let request = open_request_from(&window);
        assert_eq!(request["op"], "open-session");
        assert_eq!(request["workspace"], "e787c8a3-e6d5-4b00-90fe-ff1af708b979");

        // The window moved: the next open names where it is now.
        window.set_workspace(Some("2b1f6c0e-8d1a-4c5e-9f3b-7a6d5e4c3b2a".into()));
        let request = open_request_from(&window);
        assert_eq!(request["workspace"], "2b1f6c0e-8d1a-4c5e-9f3b-7a6d5e4c3b2a");

        // A window in no workspace, or an empty one, names none.
        for unknown in [None, Some(String::new())] {
            window.set_workspace(unknown);
            let request = open_request_from(&window);
            assert_eq!(request["op"], "open-session");
            assert!(request.get("workspace").is_none(), "{request}");
        }
    }

    #[test]
    fn deferred_host_constructs_without_a_socket_and_reports_missing_ush_after_open() {
        // A runtime directory with no desktop service sockets, held for the
        // open so no other test's runtime appears in it meanwhile.
        let runtime = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let mut widget = crate::TerminalWidget::spawn_hosted_deferred();
        assert_eq!(widget.inspect_session()["lifecycle"], "unopened");
        assert_eq!(widget.inspect_session()["input_available"], false);
        let doc = automerge::AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        finish(&mut widget, &doc);
        assert_eq!(widget.inspect_session()["lifecycle"], "no_runtime");
        assert!(
            widget.inspect_session()["startup_reason"]
                .as_str()
                .unwrap()
                .contains("ush")
        );
        assert_eq!(widget.inspect_session()["input_available"], false);
    }

    #[test]
    fn hosted_open_is_pending_until_reply_and_refuses_input() {
        let (client, server) = UnixStream::pair().unwrap();
        let (received, request) = mpsc::channel();
        let (release, delayed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut reader = accept_hello(&server);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            received.send(line).unwrap();
            delayed.recv_timeout(hang_bound()).unwrap();
            (&server)
                .write_all(b"{\"result\":\"no-runtime\",\"reason\":\"delayed runtime check\"}\n")
                .unwrap();
        });
        let mut widget = tab(client);
        let doc = automerge::AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        assert_eq!(widget.inspect_session()["lifecycle"], "opening");
        assert!(widget.try_write_input(b"must not disappear\r").is_err());
        assert!(
            request
                .recv_timeout(hang_bound())
                .unwrap()
                .contains("open-session")
        );
        release.send(()).unwrap();
        finish(&mut widget, &doc);
        assert_eq!(widget.inspect_session()["lifecycle"], "no_runtime");
        assert_eq!(
            widget.inspect_session()["startup_reason"],
            "delayed runtime check"
        );
        worker.join().unwrap();
    }

    #[test]
    fn closing_pending_tab_disconnects_supervisor_before_reply() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.set_read_timeout(Some(hang_bound())).unwrap();
        let mut widget = tab(client);
        widget.sync(&automerge::AutoCommit::new(), &automerge::ROOT);
        let mut reader = accept_hello(&server);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        assert!(request.contains("open-session"));
        drop(widget);
        assert_eq!(
            server.read(&mut [0]).unwrap(),
            0,
            "cancel ends the lifetime capability without waiting for readiness"
        );
    }

    #[test]
    fn repeated_open_cancels_old_attempt_and_never_attaches_its_reply() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.set_read_timeout(Some(hang_bound())).unwrap();
        let waker = Arc::new(Mutex::new(ui::WakeHandle::new()));
        let mut sessions =
            crate::sessions::Sessions::hosted(HostedProcessServices { ush: Some(client) });
        let target = SessionTarget::default_environment();
        assert!(
            sessions
                .begin_open(&target, Requester::Person, None, Arc::clone(&waker))
                .is_none()
        );
        let mut reader = accept_hello(&server);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        // Agent host refusal is deterministic and starts no replacement child.
        assert!(matches!(
            sessions.begin_open(&SessionTarget::Host, Requester::Agent, None, waker),
            Some(Err(OpenError::Refused(_)))
        ));
        assert_eq!(server.read(&mut [0]).unwrap(), 0);
        assert!(
            sessions.take_open_answer().is_none(),
            "old receiver was discarded"
        );
    }

    #[test]
    fn accepted_open_keeps_lifetime_until_tab_closes_even_with_reader_clone() {
        let (client, server) = UnixStream::pair().unwrap();
        server.set_read_timeout(Some(hang_bound())).unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = accept_hello(&server);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut lease = grant_session(&server, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            lease.set_read_timeout(Some(hang_bound())).unwrap();
            assert_eq!(
                lease.read(&mut [0]).unwrap(),
                0,
                "closing the accepted tab must end the guest lifetime despite the PTY reader's socket clone"
            );
        });
        let mut widget = tab(client);
        let doc = automerge::AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        finish(&mut widget, &doc);
        assert_eq!(widget.inspect_session()["lifecycle"], "open");
        assert_eq!(widget.inspect_session()["input_available"], true);
        drop(widget);
        worker.join().unwrap();
    }

    #[test]
    fn repeated_successful_opens_replace_and_disconnect_previous_sessions() {
        let (mut widget, _writes, _output) = crate::tests::build_test_widget_io();
        let doc = automerge::AutoCommit::new();
        let mut supervisors = Vec::new();
        for _ in 0..3 {
            let (client, server) = UnixStream::pair().unwrap();
            supervisors.push(std::thread::spawn(move || {
                let mut reader = accept_hello(&server);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut lease = grant_session(&server, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                lease.set_read_timeout(Some(hang_bound())).unwrap();
                assert_eq!(
                    lease.read(&mut [0]).unwrap(),
                    0,
                    "every replaced or closed session must disconnect"
                );
            }));
            // Use the existing test seam to lend the next private connection,
            // without touching the process-global runtime directory.
            widget.set_sessions(crate::sessions::Sessions::hosted(HostedProcessServices {
                ush: Some(client),
            }));
            widget
                .switch_target(&SessionTarget::default_environment(), Requester::Person)
                .unwrap();
            finish(&mut widget, &doc);
            assert_eq!(widget.inspect_session()["lifecycle"], "open");
            assert_eq!(widget.inspect_session()["input_available"], true);
        }
        drop(widget);
        for supervisor in supervisors {
            supervisor.join().unwrap();
        }
    }

    #[test]
    fn a_queued_success_is_discarded_when_the_attempt_is_cancelled() {
        let (client, server) = UnixStream::pair().unwrap();
        let (sent, handed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut reader = accept_hello(&server);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut lease = grant_session(&server, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            lease.set_read_timeout(Some(hang_bound())).unwrap();
            sent.send(()).unwrap();
            assert_eq!(lease.read(&mut [0]).unwrap(), 0);
        });
        let pending = PendingOpen::start(
            Some(HostedProcessServices { ush: Some(client) }),
            SessionTarget::default_environment(),
            Requester::Person,
            None,
            None,
            Arc::new(Mutex::new(ui::WakeHandle::new())),
        )
        .unwrap();
        handed.recv_timeout(hang_bound()).unwrap();
        // Keep the result queued, with its descriptors received but unaccepted.
        // Worker completion pins that race without consuming the UI's answer.
        tilo_test_env::wait_until("the answer to be queued", || pending.answer.is_finished());
        drop(pending);
        worker.join().unwrap();
    }

    #[test]
    fn reply_failure_finishes_as_failed_without_a_fallback_shell() {
        for reply in [
            b"not json\n".as_slice(),
            b"{\"result\":\"denied\",\"reason\":\"refused\"}\n".as_slice(),
        ] {
            let (client, server) = UnixStream::pair().unwrap();
            let bytes = reply.to_vec();
            let worker = std::thread::spawn(move || {
                let mut reader = accept_hello(&server);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                (&server).write_all(&bytes).unwrap();
            });
            let mut widget = tab(client);
            let doc = automerge::AutoCommit::new();
            widget.sync(&doc, &automerge::ROOT);
            finish(&mut widget, &doc);
            assert_eq!(widget.inspect_session()["lifecycle"], "failed");
            assert_eq!(widget.inspect_session()["input_available"], false);
            worker.join().unwrap();
        }
    }
}

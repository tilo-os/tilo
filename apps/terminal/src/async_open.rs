//! A hosted open owns its connection until the UI accepts the answer. Dropping
//! an attempt shuts it down, so the supervisor cleans up even a late child.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, mpsc};

use crate::target::SessionTarget;
use crate::{HostedProcessServices, OpenError, Requester, TerminalBackend};

#[derive(Default)]
struct Lifetime {
    cancelled: bool,
    socket: Option<UnixStream>,
}

pub(crate) struct PendingOpen {
    lifetime: Arc<Mutex<Lifetime>>,
    answer: mpsc::Receiver<Result<TerminalBackend, OpenError>>,
    pub requester: Requester,
    pub attach: Option<String>,
}

impl PendingOpen {
    pub fn start(
        first: Option<HostedProcessServices>,
        target: SessionTarget,
        requester: Requester,
        attach: Option<String>,
        waker: Arc<Mutex<ui::WakeHandle>>,
    ) -> Result<Self, OpenError> {
        let lifetime = Arc::new(Mutex::new(Lifetime::default()));
        let worker_lifetime = Arc::clone(&lifetime);
        let (send, answer) = mpsc::channel();
        let worker_attach = attach.clone();
        std::thread::Builder::new()
            .name("tilo-terminal-open".into())
            .spawn(move || {
                let result = (|| {
                    let socket = first
                        .and_then(|services| services.ush)
                        .or_else(|| HostedProcessServices::default().ush)
                        .ok_or_else(|| {
                            OpenError::NoRuntime(
                                "the required ush supervisor is not running".into(),
                            )
                        })?;
                    {
                        let mut guard = worker_lifetime
                            .lock()
                            .map_err(|e| OpenError::Failed(e.to_string()))?;
                        if guard.cancelled {
                            return Err(OpenError::Failed("session open cancelled".into()));
                        }
                        guard.socket = Some(
                            socket
                                .try_clone()
                                .map_err(|e| OpenError::Failed(e.to_string()))?,
                        );
                    }
                    crate::inner::ush_backend_on(
                        &socket,
                        &[],
                        &target,
                        requester,
                        worker_attach.as_deref(),
                    )
                })();
                // Sending into a dropped receiver drops any returned PTY and
                // lifetime capability. A replaced attempt cannot attach here.
                let _ = send.send(result);
                crate::inner::request_terminal_wake(&waker);
            })
            .map_err(|e| OpenError::Failed(e.to_string()))?;
        Ok(Self {
            lifetime,
            answer,
            requester,
            attach,
        })
    }

    pub fn take_answer(&self) -> Option<Result<TerminalBackend, OpenError>> {
        let result = match self.answer.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(OpenError::Failed(
                "session open worker ended without an answer".into(),
            )),
        };
        // Transfer lifetime to the backend before dropping this attempt.
        if let Ok(mut guard) = self.lifetime.lock() {
            guard.socket = None;
        }
        Some(result)
    }
}

impl Drop for PendingOpen {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.lifetime.lock() {
            guard.cancelled = true;
            if let Some(socket) = guard.socket.take() {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::time::{Duration, Instant};
    use ui::widget::Widget;

    fn tab(socket: UnixStream) -> crate::TerminalWidget {
        crate::TerminalWidget::spawn_hosted(HostedProcessServices { ush: Some(socket) }).unwrap()
    }

    fn finish(widget: &mut crate::TerminalWidget, doc: &automerge::AutoCommit) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(widget.session_state(), crate::sessions::OpenState::Opening) {
            assert!(Instant::now() < deadline, "open must finish");
            widget.update(doc, &automerge::ROOT);
            std::thread::yield_now();
        }
    }

    #[test]
    fn deferred_host_constructs_without_a_socket_and_reports_missing_ush_after_open() {
        // The ordinary test environment has no desktop service sockets.
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
            let mut line = String::new();
            std::io::BufReader::new(&server)
                .read_line(&mut line)
                .unwrap();
            received.send(line).unwrap();
            delayed.recv_timeout(Duration::from_secs(5)).unwrap();
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
                .recv_timeout(Duration::from_secs(5))
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
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut widget = tab(client);
        widget.sync(&automerge::AutoCommit::new(), &automerge::ROOT);
        let mut request = String::new();
        std::io::BufReader::new(&server)
            .read_line(&mut request)
            .unwrap();
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
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let waker = Arc::new(Mutex::new(ui::WakeHandle::new()));
        let mut sessions =
            crate::sessions::Sessions::hosted(HostedProcessServices { ush: Some(client) });
        let target = SessionTarget::default_environment();
        assert!(
            sessions
                .begin_open(&target, Requester::Person, None, Arc::clone(&waker))
                .is_none()
        );
        let mut request = String::new();
        std::io::BufReader::new(&server)
            .read_line(&mut request)
            .unwrap();
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
        use std::os::fd::AsRawFd;
        let (client, mut server) = UnixStream::pair().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut request = String::new();
            std::io::BufReader::new(&server)
                .read_line(&mut request)
                .unwrap();
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize::default())
                .unwrap();
            runtime_portal::fd_transport::send_with_fd(
                server.as_raw_fd(),
                b"{\"result\":\"session\",\"session\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"environment\":\"default\"}\n",
                pair.master.as_raw_fd().unwrap(),
            ).unwrap();
            assert_eq!(
                server.read(&mut [0]).unwrap(),
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
        use std::os::fd::AsRawFd;
        let (mut widget, _writes, _output) = crate::tests::build_test_widget_io();
        let doc = automerge::AutoCommit::new();
        let mut supervisors = Vec::new();
        for _ in 0..3 {
            let (client, mut server) = UnixStream::pair().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            supervisors.push(std::thread::spawn(move || {
                let mut request = String::new();
                std::io::BufReader::new(&server).read_line(&mut request).unwrap();
                let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize::default()).unwrap();
                runtime_portal::fd_transport::send_with_fd(
                    server.as_raw_fd(),
                    b"{\"result\":\"session\",\"session\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"environment\":\"default\"}\n",
                    pair.master.as_raw_fd().unwrap(),
                ).unwrap();
                assert_eq!(server.read(&mut [0]).unwrap(), 0, "every replaced or closed session must disconnect");
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
        use std::os::fd::AsRawFd;
        let (client, mut server) = UnixStream::pair().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (sent, handed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut request = String::new();
            std::io::BufReader::new(&server)
                .read_line(&mut request)
                .unwrap();
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize::default())
                .unwrap();
            runtime_portal::fd_transport::send_with_fd(
                server.as_raw_fd(),
                b"{\"result\":\"session\",\"session\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"environment\":\"default\"}\n",
                pair.master.as_raw_fd().unwrap(),
            ).unwrap();
            sent.send(()).unwrap();
            assert_eq!(server.read(&mut [0]).unwrap(), 0);
        });
        let pending = PendingOpen::start(
            Some(HostedProcessServices { ush: Some(client) }),
            SessionTarget::default_environment(),
            Requester::Person,
            None,
            Arc::new(Mutex::new(ui::WakeHandle::new())),
        )
        .unwrap();
        handed.recv_timeout(Duration::from_secs(5)).unwrap();
        // Receive the queued result without the UI's lifetime transfer. This
        // pins the race after descriptor receipt, rather than relying on scheduling.
        let backend = pending
            .answer
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        drop(pending);
        drop(backend);
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
                let mut request = String::new();
                std::io::BufReader::new(&server)
                    .read_line(&mut request)
                    .unwrap();
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

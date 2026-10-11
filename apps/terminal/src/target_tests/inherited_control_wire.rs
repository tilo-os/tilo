//! Native standalone sessions over one real inherited connection, never scripted opens.
use super::{App, OpenState, Requester, SessionTarget, Sessions, TerminalWidget, on_tab, recorded};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

struct ServiceChild {
    lease: UnixStream,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl ServiceChild {
    fn assert_cancelled(&mut self) {
        self.lease
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(
            self.lease.read(&mut byte).unwrap(),
            0,
            "reader clones must not keep the old session alive"
        );
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for ServiceChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_json(reader: &mut impl BufRead) -> serde_json::Value {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0);
    serde_json::from_str(&line).unwrap()
}

fn serve_native_sessions(mut control: UnixStream) {
    control
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(control.try_clone().unwrap());
    let mut previous: Option<ServiceChild> = None;
    for index in 0..13 {
        let kind = if index % 2 == 0 {
            "environment"
        } else {
            "host"
        };
        if let Some(mut previous) = previous.take() {
            previous.assert_cancelled();
        }
        let hello = read_json(&mut reader);
        assert_eq!(hello["op"], "hello");
        assert_eq!(hello["protocol"], 2);
        control
            .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
            .unwrap();
        let request = read_json(&mut reader);
        assert_eq!(request["op"], "open-session");
        assert_eq!(request["target"]["kind"], kind);
        assert_eq!(request["requested_by"], "person");
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pair
            .slave
            .spawn_command(portable_pty::CommandBuilder::new("cat"))
            .unwrap();
        let (supervisor, terminal) = UnixStream::pair().unwrap();
        let response = serde_json::json!({
            "result": "session", "protocol": 2,
            "session": format!("{index:032x}"),
            "environment": if kind == "host" { "host" } else { "default" }
        });
        let mut bytes = serde_json::to_vec(&response).unwrap();
        bytes.push(b'\n');
        runtime_portal::fd_transport::send_with_fds(
            control.as_raw_fd(),
            &bytes,
            &[pair.master.as_raw_fd().unwrap(), terminal.as_raw_fd()],
        )
        .unwrap();
        drop(pair);
        drop(terminal);
        previous = Some(ServiceChild {
            lease: supervisor,
            child,
        });
    }
    previous.as_mut().unwrap().assert_cancelled();
}

#[test]
#[serial_test::serial]
fn native_mounted_terminal_switches_default_host_default_on_one_inherited_connection() {
    let (terminal, supervisor) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || serve_native_sessions(supervisor));
    let mut environment = tilo_test_env::ScopedEnv::new();
    // Ownership transfers exactly once through the actual standalone service path.
    environment.set(
        crate::inner::USH_SERVICE_FD_ENV,
        terminal.into_raw_fd().to_string(),
    );
    let mut app = App::new().unwrap();
    app.with_surface(|terminal: &mut TerminalWidget| terminal.set_sessions(Sessions::standalone()))
        .unwrap();
    app.settle();
    assert_eq!(
        on_tab(&mut app, |terminal| terminal.session_state().clone()),
        OpenState::Open
    );
    assert_eq!(recorded(&app), Some(SessionTarget::default()));
    {
        let _agent = ui::invocation_scope::InvokerScope::enter(
            ui::tilo_actions::ActionInvoker::actor("agent:fixture"),
        );
        assert!(
            on_tab(&mut app, |terminal| terminal
                .switch_target(&SessionTarget::Host, Requester::current()))
            .is_err()
        );
    }
    assert_eq!(recorded(&app), Some(SessionTarget::default()));
    for index in 1..13 {
        let argument = if index % 2 == 0 {
            "environment"
        } else {
            "host"
        };
        on_tab(&mut app, |terminal| {
            terminal.perform(crate::resource::TerminalEffect::OpenIn {
                target: argument.into(),
            })
        })
        .unwrap();
        assert_eq!(
            on_tab(&mut app, |terminal| terminal.session_state().clone()),
            OpenState::Open
        );
        let inspection = app
            .effect(&crate::resource::TerminalEffect::InspectSession)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(inspection["lifecycle"], "open");
        assert_eq!(inspection["input_available"], true);
    }
    assert_eq!(recorded(&app), Some(SessionTarget::default()));
    drop(app);
    peer.join().unwrap();
    drop(environment);
}

#[test]
fn an_old_supervisor_is_refused_before_any_session_open_or_local_fallback() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || {
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(server.try_clone().unwrap());
        assert_eq!(read_json(&mut reader)["op"], "hello");
        server
            .write_all(b"{\"result\":\"denied\",\"reason\":\"unknown operation\"}\n")
            .unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(
            reader.read(&mut byte).unwrap(),
            0,
            "no OpenSession follows unsupported negotiation"
        );
    });
    let answer = crate::inner::ush_backend_on(
        &client,
        &[],
        &SessionTarget::default(),
        Requester::Person,
        None,
    );
    assert!(matches!(answer, Err(crate::OpenError::Unavailable(_))));
    drop(client);
    peer.join().unwrap();
}

#[test]
fn an_invalid_session_reply_closes_both_received_rights() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || {
        let mut reader = BufReader::new(server.try_clone().unwrap());
        assert_eq!(read_json(&mut reader)["op"], "hello");
        server
            .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
            .unwrap();
        assert_eq!(read_json(&mut reader)["op"], "open-session");
        let (mut master_peer, master) = UnixStream::pair().unwrap();
        let (mut lease_peer, lease) = UnixStream::pair().unwrap();
        runtime_portal::fd_transport::send_with_fds(
            server.as_raw_fd(),
            b"{\"result\":\"session\",\"protocol\":1}\n",
            &[master.as_raw_fd(), lease.as_raw_fd()],
        )
        .unwrap();
        drop(master);
        drop(lease);
        for peer in [&mut master_peer, &mut lease_peer] {
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            assert_eq!(peer.read(&mut [0u8; 1]).unwrap(), 0);
        }
    });
    assert!(matches!(
        crate::inner::ush_backend_on(
            &client,
            &[],
            &SessionTarget::default(),
            Requester::Person,
            None
        ),
        Err(crate::OpenError::Failed(_))
    ));
    drop(client);
    peer.join().unwrap();
}

//! A session whose device's daemon answers slowly goes on drawing and answering: its loop never
//! waits for the daemon, so a frame, an app's request and a tool's question are answered while
//! the daemon is busy.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tilo_desktop_model::{Layout, Workspace, WorkspaceId};

use crate::daemon_fixture::TestDaemon;

/// The workspace the person works in.
const STUDIO: &str = "8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a";

/// Whether the daemon holds back what it answers, and the condition that says it stopped.
type Load = Arc<(Mutex<bool>, Condvar)>;

fn set(load: &Load, busy: bool) {
    *load.0.lock().unwrap() = busy;
    load.1.notify_all();
}

/// The session's link to `daemon`, carried through a relay that holds back what the daemon
/// answers while `load` says it is busy: the daemon under load, as the session sees it.
fn busy_link(daemon: &TestDaemon, load: &Load) -> std::os::fd::OwnedFd {
    let (session, relay) = UnixStream::pair().expect("a link");
    let to_daemon = UnixStream::from(daemon.shell_link());
    let (mut asks, mut asked) = (
        relay.try_clone().expect("the relay"),
        to_daemon.try_clone().unwrap(),
    );
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut asks, &mut asked);
    });
    let (mut answers, mut answered, load) = (to_daemon, relay, Arc::clone(load));
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let Ok(read) = answers.read(&mut buffer) else {
                return;
            };
            if read == 0 {
                return;
            }
            {
                let (busy, eased) = &*load;
                let mut held = busy.lock().unwrap();
                while *held {
                    held = eased.wait(held).unwrap();
                }
            }
            if answered
                .write_all(buffer.get(..read).unwrap_or_default())
                .is_err()
            {
                return;
            }
        }
    });
    session.into()
}

/// The daemon is busy and answers nothing; the person changes their shell meanwhile, and a tool
/// asks the session which windows it shows. Every pass ends at once, and the question is
/// answered while the daemon still says nothing.
#[test]
fn a_session_answers_while_its_daemon_is_busy() {
    let home = tilo_test_env::TestHome::new().expect("a home");
    let daemon = TestDaemon::holding(&[STUDIO]);
    let person = hex::encode(daemon.certificate().user_public_key);
    let load: Load = Arc::new((Mutex::new(false), Condvar::new()));
    let control = home.runtime().join("test-control.sock");
    let mut session = crate::Session::headless(
        crate::Options {
            person: person.clone(),
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(home.state().to_path_buf()),
            shell_link: Some(busy_link(&daemon, &load)),
            test_control: Some(control.clone()),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session");
    let mut studio = Workspace::new(
        WorkspaceId::parse(STUDIO).expect("a workspace id"),
        "Studio",
    );
    studio.policy.set_owner(person);
    session.handle().replace_tree(Layout {
        active: studio.id(),
        workspaces: vec![studio.clone()],
        ..Layout::default()
    });
    for _ in 0..20 {
        session.step(Duration::from_millis(10)).expect("a turn");
    }

    // From here the daemon answers nothing; a pass that waited for it would wait as long as the
    // test lets it, and the time shows.
    set(&load, true);
    let (ease, eased) = std::sync::mpsc::channel::<()>();
    let easing = {
        let load = Arc::clone(&load);
        std::thread::spawn(move || {
            let _ = eased.recv_timeout(Duration::from_secs(20));
            set(&load, false);
        })
    };

    // The person makes the workspace their own: a strip of it, which the layout writes.
    let mut changed = studio;
    changed.strips = vec![tilo_desktop_model::Strip::new(
        tilo_desktop_model::StripId(1),
        "Main",
    )];
    session.handle().replace_tree(Layout {
        active: changed.id(),
        workspaces: vec![changed],
        ..Layout::default()
    });

    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stream = UnixStream::connect(&control).expect("the test-control socket");
        stream
            .write_all(b"{\"op\":\"windows\"}\n")
            .expect("the socket takes the question");
        let mut line = String::new();
        let read = BufReader::new(stream).read_line(&mut line);
        let _ = answer.send(read.map(|_| line));
    });
    let asked = Instant::now();
    let mut longest = Duration::ZERO;
    let line = loop {
        let pass = Instant::now();
        session.step(Duration::from_millis(10)).expect("a turn");
        longest = longest.max(pass.elapsed());
        if let Ok(line) = answered.try_recv() {
            break line.expect("the session answers");
        }
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "the session did not answer while its daemon was busy (its longest pass took \
             {longest:?})"
        );
    };
    assert!(
        longest < Duration::from_secs(1),
        "a pass waited {longest:?} for a busy daemon"
    );
    let answer: serde_json::Value = serde_json::from_str(&line).expect("a JSON answer");
    assert!(
        answer.pointer("/answer/windows").is_some(),
        "it answered which windows it shows: {answer}"
    );

    drop(ease);
    easing.join().expect("the daemon eases");
}

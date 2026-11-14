//! The native endpoint at its limits, through its own accept loop and a real
//! socket: how many connections it serves at once and what the next one is
//! told, how many requests of one connection work at once and what the next
//! one waits for, and how many work or wait before one is refused.
//!
//! Each bound is reached exactly, the one request or connection past it is
//! read for what it is told, and room is then made the way a client makes
//! it: a request answers, a caller cancels, a connection closes.
use super::{Endpoint, MAX_CONNECTIONS, Status};
use crate::mcp::{MAX_ADMITTED, MAX_WORKING, McpServer};
use crate::mcp_test_hosts::{GatedHost, long_invoke};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use tilo_actions::InvocationId;

/// An endpoint accepting on a socket of its own, over a host whose
/// invocations end when the test says so. The accept loop runs for as long
/// as the test process does, as a daemon's does.
fn endpoint() -> (tempfile::TempDir, PathBuf, GatedHost) {
    let host = GatedHost::default();
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("native.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let serving = host.clone();
    let endpoint = Endpoint {
        session_factory: Arc::new(move || {
            Ok::<_, std::io::Error>(McpServer::new(
                serving.clone(),
                "workspace".into(),
                "owner".into(),
            ))
        }),
        status: Status {
            version: 1,
            pid: std::process::id(),
            workspace: "workspace".into(),
        },
        joiner: None,
        search: Arc::new(tilo_os_search::OsSearch::new(
            Arc::new(host.clone()),
            tilo_actions::remote::Caller::local_person("owner").in_workspace("workspace"),
        )),
        media: None,
        remote: None,
    };
    std::thread::spawn(move || endpoint.accept(&listener));
    (directory, socket, host)
}

/// One client's connection to the endpoint: requests go out a line each, and
/// replies are read a line each, never waited on for longer than a hang.
struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    /// Replies read while another was looked for, by their request's id.
    read: BTreeMap<u64, Value>,
}

impl Client {
    fn connect(socket: &std::path::Path) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Self {
            stream,
            reader,
            read: BTreeMap::new(),
        }
    }

    /// Open an MCP session on the connection, as a client does first.
    fn initialized(socket: &std::path::Path) -> Self {
        let mut client = Self::connect(socket);
        client.send(
            1,
            "initialize",
            &json!({"protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"limits","version":"1"}}),
        );
        assert!(
            client.reply_to(1)["result"].is_object(),
            "the session opens"
        );
        client
    }

    fn send(&self, id: u64, method: &str, params: &Value) {
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        writeln!(&self.stream, "{request}").unwrap();
    }

    fn call(&self, id: u64, tool: &str, arguments: &Value) {
        self.send(
            id,
            "tools/call",
            &json!({"name": tool, "arguments": arguments}),
        );
    }

    /// The next line the endpoint wrote, or `None` once it closed the
    /// connection. A connection that says nothing within a hang fails the
    /// test: nobody is left waiting on one.
    fn line(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&line).expect("each line is one whole JSON reply")),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("the endpoint neither answered nor closed the connection: {error}")
            }
            // Closed while this side still had something unread by it.
            Err(_) => None,
        }
    }

    /// The reply to the request `id`, whichever replies come before it.
    fn reply_to(&mut self, id: u64) -> Value {
        loop {
            if let Some(reply) = self.read.remove(&id) {
                return reply;
            }
            let reply = self.line().expect("a reply before the connection closes");
            let to = reply["id"].as_u64().expect("a reply names its request");
            self.read.insert(to, reply);
        }
    }
}

/// The state a `tilo_invoke` or `tilo_cancel` answered with.
fn state(reply: &Value) -> &str {
    reply["result"]["structuredContent"]["update"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("no state in {reply}"))
}

/// One invocation a test starts: its id at the host and the id of the
/// request that started it.
#[derive(Clone, Copy)]
struct Started {
    invocation: InvocationId,
    request: u64,
}

/// Send a `tilo_invoke` for each of `count` new invocations, none waiting on
/// another's answer.
fn invoke_many(client: &Client, count: usize) -> Vec<Started> {
    (0..count)
        .map(|place| {
            let started = Started {
                invocation: InvocationId::new(),
                request: 100_u64.saturating_add(u64::try_from(place).unwrap()),
            };
            client.call(
                started.request,
                "tilo_invoke",
                &long_invoke(started.invocation),
            );
            started
        })
        .collect()
}

/// The request that started `invocation`.
fn request_of(started: &[Started], invocation: InvocationId) -> u64 {
    started
        .iter()
        .find(|started| started.invocation == invocation)
        .expect("an invocation this test started")
        .request
}

/// The endpoint serves [`MAX_CONNECTIONS`] connections at once, each
/// answered. The next one is told, before anything is read of it, that the
/// endpoint has no room, that nothing it asked ran and when to connect
/// again, and is then closed: it is neither dropped without a word nor left
/// waiting. When one of the served connections closes, a client that
/// connects again is served.
#[test]
fn the_endpoint_serves_its_connections_at_once_and_tells_the_next_there_is_no_room() {
    let (_directory, socket, _host) = endpoint();
    let status = |client: &mut Client| -> Option<Value> {
        // A connection that is not served may be closed before this is
        // written; what it was told is read all the same.
        let _ = writeln!(&client.stream, r#"{{"native":"status"}}"#);
        client.line()
    };
    let mut served = Vec::new();
    for place in 0..MAX_CONNECTIONS {
        let mut client = Client::connect(&socket);
        let answer = status(&mut client).expect("a served connection is answered");
        assert_eq!(
            answer["pid"],
            std::process::id(),
            "connection {place} is served: {answer}"
        );
        served.push(client);
    }

    let mut next = Client::connect(&socket);
    let refused = status(&mut next).expect("one past the bound is told why it is not served");
    assert_eq!(refused["error"]["code"], -32000, "{refused}");
    assert!(
        refused["id"].is_null(),
        "no request of it was read: {refused}"
    );
    let told = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        told.contains(&format!("{MAX_CONNECTIONS} connections"))
            && told.contains("nothing it asked ran")
            && told.contains("connect again once one of them has closed"),
        "it says what is full, that nothing ran and when to come back: {told}"
    );
    assert_eq!(next.line(), None, "and its connection is closed");
    // The ones being served are not disturbed by it.
    let still = status(&mut served[0]).expect("a served connection still answers");
    assert_eq!(still["pid"], std::process::id(), "{still}");

    // One connection ends: a client that connects again is served.
    drop(served.pop());
    let retried = tilo_test_env::wait_for("a connection to be served once one closed", || {
        let mut client = Client::connect(&socket);
        let answer = status(&mut client)?;
        (answer["pid"] == std::process::id()).then_some(client)
    });
    drop(retried);
}

/// One connection's requests work at once up to [`MAX_WORKING`]: that many
/// invocations reach the host while none has ended, and two more wait for a
/// turn without being refused, while a request that needs no turn answers
/// beside them. When one that works answers, one that waited takes its turn;
/// when the caller cancels another, the last one that waited takes that
/// turn. Every request is answered as its own.
#[test]
fn a_connection_works_its_requests_at_once_up_to_the_bound_and_the_next_take_freed_turns() {
    let (_directory, socket, host) = endpoint();
    let mut client = Client::initialized(&socket);
    let started = invoke_many(&client, MAX_WORKING + 2);
    tilo_test_env::wait_until(
        "as many invocations as work at once to reach the host",
        || host.invoked().len() >= MAX_WORKING,
    );
    // The connection read every request before this one, which takes no
    // turn and so answers while all the turns are taken.
    client.send(2, "ping", &json!({}));
    assert!(client.reply_to(2)["result"].is_object());
    let working = host.invoked();
    assert_eq!(working.len(), MAX_WORKING, "two requests wait for a turn");

    host.finish(working[0]);
    let finished = client.reply_to(request_of(&started, working[0]));
    assert_eq!(state(&finished), "succeeded", "{finished}");
    tilo_test_env::wait_until("a waiting request to take the turn an answer freed", || {
        host.invoked().len() == MAX_WORKING + 1
    });

    client.call(
        3,
        "tilo_cancel",
        &json!({"invocation_id": working[1].0.to_string()}),
    );
    assert_eq!(state(&client.reply_to(3)), "cancelled");
    let cancelled = client.reply_to(request_of(&started, working[1]));
    assert_eq!(state(&cancelled), "cancelled", "{cancelled}");
    tilo_test_env::wait_until(
        "the last waiting request to take the turn a cancel freed",
        || host.invoked().len() == MAX_WORKING + 2,
    );

    let reached = host.invoked();
    for one in &started {
        assert_eq!(
            reached.iter().filter(|id| **id == one.invocation).count(),
            1,
            "each invocation reached the host once"
        );
        if one.invocation == working[0] || one.invocation == working[1] {
            continue;
        }
        host.finish(one.invocation);
        let reply = client.reply_to(one.request);
        assert_eq!(state(&reply), "succeeded", "{reply}");
    }
    client.stream.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(client.line(), None, "no reply is left over");
}

/// One connection may have [`MAX_ADMITTED`] requests working or waiting. The
/// next is refused at once, while every one of those is still in flight: it
/// is told how many are, that it did not run and when to send it again, and
/// asking again while nothing changed is refused the same way. Once one of
/// the requests in flight has answered, the same request sent again is
/// taken, waits its turn and runs once.
#[test]
fn a_connection_refuses_the_request_past_those_it_admits_and_takes_it_once_one_answered() {
    let (_directory, socket, host) = endpoint();
    let mut client = Client::initialized(&socket);
    let started = invoke_many(&client, MAX_ADMITTED);
    tilo_test_env::wait_until(
        "as many invocations as work at once to reach the host",
        || host.invoked().len() >= MAX_WORKING,
    );
    let extra = InvocationId::new();
    for request in [900, 901] {
        client.call(request, "tilo_invoke", &long_invoke(extra));
        let refused = client.reply_to(request);
        assert_eq!(refused["error"]["code"], -32000, "{refused}");
        let told = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(
            told.contains(&format!("{MAX_ADMITTED} requests working or waiting"))
                && told.contains("was not run")
                && told.contains("send it again once one of them has answered"),
            "it says what is full, that it did not run and when to ask again: {told}"
        );
    }
    assert!(
        client.read.is_empty(),
        "nothing in flight had answered when it was refused: {:?}",
        client.read.keys()
    );
    assert_eq!(host.invoked().len(), MAX_WORKING);
    assert!(
        !host.invoked().contains(&extra),
        "a refused request reached no host"
    );

    // One in flight answers, and the request sent again is taken.
    let first = host.invoked()[0];
    host.finish(first);
    let finished = client.reply_to(request_of(&started, first));
    assert_eq!(state(&finished), "succeeded", "{finished}");
    client.call(902, "tilo_invoke", &long_invoke(extra));

    // Everything in flight ends as it reaches the host.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finishing = {
        let (host, stop) = (host.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                for invocation in host.invoked() {
                    host.finish(invocation);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let retried = client.reply_to(902);
    assert_eq!(state(&retried), "succeeded", "{retried}");
    for one in &started {
        if one.invocation == first {
            continue;
        }
        let reply = client.reply_to(one.request);
        assert_eq!(state(&reply), "succeeded", "{reply}");
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    finishing.join().unwrap();
    let reached = host.invoked();
    assert_eq!(reached.len(), MAX_ADMITTED + 1);
    assert_eq!(
        reached.iter().filter(|id| **id == extra).count(),
        1,
        "the request that was refused ran once, when it was sent again"
    );
    client.stream.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(client.line(), None, "no reply is left over");
}

use std::io::BufReader;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tilo_vm_protocol::{Client, PROTOCOL_VERSION};

use super::during;
use crate::builderlayer::{BEGIN, Packed, Stream};
use crate::remote::Remote;

// Exercise the real Client request/response envelopes over owned child pipes.
// The counter is returned through HelloResult, not a test-only renewal callback.
fn transport(mode: &str) -> Remote {
    let server = r"
import json, sys, time
count = 0
for line in sys.stdin:
    request = json.loads(line)
    assert request['method'] == 'hello'
    count += 1
    if sys.argv[2] == 'stall' and count > 1:
        time.sleep(60)
    protocol = int(sys.argv[1]) + (1 if sys.argv[2] == 'mismatch' and count > 1 else 0)
    result = {'protocol': protocol, 'daemon_version': str(count),
              'root': '/fixture', 'arch': 'x86_64',
              'awake': {'held': sys.argv[2] != 'unheld',
                        'hold_until': 9999999999,
                        'problem': 'fixture inhibitor unavailable' if sys.argv[2] == 'unheld' else None}}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
";
    let mut child = Command::new("python3")
        .args(["-u", "-c", server, &PROTOCOL_VERSION.to_string(), mode])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the owned protocol fixture");
    let mut client = Client::new(
        BufReader::new(child.stdout.take().expect("fixture stdout")),
        child.stdin.take().expect("fixture stdin"),
    );
    let hello = client
        .hello(env!("CARGO_PKG_VERSION"))
        .expect("initial hello");
    Remote {
        child,
        client: Some(client),
        hello,
        peer: false,
    }
}

#[test]
fn quiet_builder_renews_through_real_hello_and_keeps_framed_header() {
    let mut remote = transport("held");
    let packed = Packed {
        component: "shell".into(),
        ..Packed::default()
    };
    let header = format!(
        "{BEGIN}{}",
        serde_json::to_string(&packed).expect("header JSON")
    );
    let mut builder = Command::new("python3")
        .args([
            "-u",
            "-c",
            "import sys,time; time.sleep(.25); print(sys.argv[1], flush=True)",
            &header,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the quiet builder");
    let mut stream = Stream::new(BufReader::new(
        builder.stdout.take().expect("builder stdout"),
    ));
    let (result, renewal) = during(
        Duration::from_millis(20),
        || remote.renew_sleep_lease(Duration::from_secs(1)),
        || stream.header(&mut std::io::sink()),
    )
    .expect("start renewal");
    assert_eq!(result.expect("real framed header"), packed);
    renewal.expect("the reported inhibitor is held");
    assert!(builder.wait().expect("reap quiet builder").success());
    assert!(
        remote
            .hello
            .daemon_version
            .parse::<u64>()
            .expect("hello count")
            > 1
    );
    assert_eq!(
        remote.hello.awake.as_ref().expect("lease").hold_until,
        Some(9_999_999_999)
    );
}

#[test]
fn completed_build_stops_renewal_without_waiting_for_interval() {
    let mut remote = transport("held");
    let started = Instant::now();
    let (value, renewal) = during(
        Duration::from_secs(60),
        || remote.renew_sleep_lease(Duration::from_secs(1)),
        || 42,
    )
    .expect("start renewal");
    assert_eq!(value, 42);
    renewal.expect("clean stop");
    assert_eq!(remote.hello.daemon_version, "1");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn missing_inhibitor_is_reported_after_the_owned_build_completes() {
    let mut remote = transport("unheld");
    let (value, renewal) = during(
        Duration::from_millis(10),
        || remote.renew_sleep_lease(Duration::from_secs(1)),
        || {
            std::thread::sleep(Duration::from_millis(100));
            42
        },
    )
    .expect("start renewal");
    assert_eq!(value, 42);
    assert!(
        renewal
            .expect_err("unheld lease must fail")
            .to_string()
            .contains("fixture inhibitor unavailable")
    );
}

#[test]
fn stalled_hello_times_out_and_reaps_only_its_owned_transport() {
    let mut remote = transport("stall");
    let started = Instant::now();
    let error = remote
        .renew_sleep_lease(Duration::from_millis(50))
        .expect_err("stalled renewal must fail");
    assert!(error.to_string().contains("did not complete its handshake"));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        remote
            .child
            .try_wait()
            .expect("owned child status")
            .is_some()
    );
}

#[test]
fn renewal_preserves_protocol_version_validation() {
    let mut remote = transport("mismatch");
    let error = remote
        .renew_sleep_lease(Duration::from_secs(1))
        .expect_err("a changed daemon protocol must fail");
    assert!(error.to_string().contains("speaks protocol"));
    assert_eq!(remote.hello.daemon_version, "1");
}

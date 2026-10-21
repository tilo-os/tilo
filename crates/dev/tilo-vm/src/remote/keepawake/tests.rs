use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use tilo_vm_protocol::{Client, PROTOCOL_VERSION};

use super::during;
use crate::builderlayer::{BEGIN, Packed, Stream};
use crate::remote::Remote;

// Exercise the real Client request/response envelopes over owned child pipes.
// The counter is returned through HelloResult, not a test-only renewal callback.
// The fixture creates `renewing` once a renewal's hello has arrived, before
// it answers, so a build can wait for a renewal under way instead of a time.
fn transport(mode: &str, renewing: &Path) -> Remote {
    let server = r"
import json, sys, threading
count = 0
for line in sys.stdin:
    request = json.loads(line)
    assert request['method'] == 'hello'
    count += 1
    if count > 1:
        open(sys.argv[3], 'w').close()
    if sys.argv[2] == 'stall' and count > 1:
        threading.Event().wait()
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
        .arg(renewing)
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

/// Where the fixture marks a renewal under way, and the directory holding it.
fn renewal_marker() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("marker directory");
    let marker = directory.path().join("renewing");
    (directory, marker)
}

#[test]
fn quiet_builder_renews_through_real_hello_and_keeps_framed_header() {
    let (_directory, renewing) = renewal_marker();
    let mut remote = transport("held", &renewing);
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
            "import os,sys,time\nwhile not os.path.exists(sys.argv[2]): time.sleep(.01)\nprint(sys.argv[1], flush=True)",
            &header,
        ])
        .arg(&renewing)
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the quiet builder");
    let mut stream = Stream::new(BufReader::new(
        builder.stdout.take().expect("builder stdout"),
    ));
    let (result, renewal) = during(
        Duration::from_millis(20),
        || remote.renew_sleep_lease(tilo_test_env::hang_bound()),
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
    let (_directory, renewing) = renewal_marker();
    let mut remote = transport("held", &renewing);
    // An interval that never runs out: the build ends the renewal or nothing does.
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ended = during(
            Duration::MAX,
            || remote.renew_sleep_lease(tilo_test_env::hang_bound()),
            || 42,
        )
        .map(|(value, renewal)| (value, renewal.is_ok()))
        .map_err(|error| error.to_string());
        let _ = done.send((ended, remote.hello.daemon_version.clone()));
    });
    let (ended, daemon_version) = tilo_test_env::recv("the renewal to stop with the build", &ended);
    assert_eq!(ended.expect("start renewal"), (42, true), "a clean stop");
    assert_eq!(daemon_version, "1");
    assert!(!renewing.exists(), "no renewal ran");
}

#[test]
fn missing_inhibitor_is_reported_after_the_owned_build_completes() {
    let (_directory, renewing) = renewal_marker();
    let mut remote = transport("unheld", &renewing);
    let (value, renewal) = during(
        Duration::from_millis(10),
        || remote.renew_sleep_lease(tilo_test_env::hang_bound()),
        || {
            tilo_test_env::wait_until("a renewal under way", || renewing.exists());
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
    let (_directory, renewing) = renewal_marker();
    let mut remote = transport("stall", &renewing);
    // The fixture never answers the renewal: only the handshake's own
    // timeout ends the call.
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let error = remote
            .renew_sleep_lease(Duration::from_millis(50))
            .map_err(|error| error.to_string());
        let _ = done.send((error, remote));
    });
    let (error, mut remote) = tilo_test_env::recv("the stalled renewal to time out", &ended);
    let error = error.expect_err("stalled renewal must fail");
    assert!(error.contains("did not complete its handshake"));
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
    let (_directory, renewing) = renewal_marker();
    let mut remote = transport("mismatch", &renewing);
    let error = remote
        .renew_sleep_lease(tilo_test_env::hang_bound())
        .expect_err("a changed daemon protocol must fail");
    assert!(error.to_string().contains("speaks protocol"));
    assert_eq!(remote.hello.daemon_version, "1");
}

#![cfg(feature = "test-support")]

use qos_session_peer::SessionOwner;
use std::{
    fs,
    io::Write,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn status(origin: &str, deadline: Instant) -> serde_json::Value {
    tokio::time::timeout_at(deadline.into(), async {
        let mut stream = tokio::net::TcpStream::connect(origin).await.unwrap();
        stream
            .write_all(format!("GET /status HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        let response = String::from_utf8(bytes).unwrap();
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    })
    .await
    .expect("status exchange exceeded its polling deadline")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_preserves_precommitted_local_document_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    drop(SessionOwner::initialize(&state).unwrap());
    let mut host = Host(
        Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
            .arg("run")
            .arg(&state)
            .arg(&ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    // Queue before connecting so the first actual Writer commit is held after
    // its durable barrier and before the SDK's exact receipt handoff.
    writeln!(
        host.0.stdin.as_mut().unwrap(),
        "qualification hold so.tilo.word-processor tab:0000000000000001"
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let metadata: serde_json::Value = loop {
        if let Ok(bytes) = fs::read(&ready) {
            break serde_json::from_slice(&bytes).unwrap();
        }
        assert!(Instant::now() < deadline, "host never wrote ready metadata");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let origin = metadata["fixture_origin"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap();
    let held = loop {
        let current = status(origin, deadline).await;
        assert!(current["action_error"].is_null(), "{current}");
        assert_eq!(current["qualification"]["expired"], false, "{current}");
        if current["qualification"]["held"] == true {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "Writer signature never held: {current}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        held["qualification"]["signing"]["pending_exact_receipts"]
            .as_u64()
            .unwrap()
            > 0,
        "the original exact committed receipt must still be in custody: {held}"
    );
    let subscription = held["native_subscription_generation"].as_u64().unwrap();
    // These ordered commands release the already committed reply during the
    // replacement connection, without requesting the document mutation again.
    host.0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"reconnect\nqualification release\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = status(origin, deadline).await;
        assert!(
            current["action_error"].is_null(),
            "remote replacement must not reject an exact local receipt: {current}"
        );
        assert_eq!(current["qualification"]["expired"], false, "{current}");
        if current["native_subscription_generation"].as_u64().unwrap() > subscription
            && current["native_peer"] == "subscribed"
            && current["actions"] == "ready"
            && current["qualification"]["held"] == false
            && current["qualification"]["signing"]["pending_exact_receipts"] == 0
            && current["qualification"]["signing"]["outstanding_handoffs"] == 0
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "exact receipt did not settle after reconnect: {current}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "shutdown").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let exit = loop {
        if let Some(exit) = host.0.try_wait().unwrap() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "host did not finish graceful shutdown"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(exit.success(), "settled workers failed graceful close");
    let reopened = SessionOwner::open(&state).unwrap();
    assert!(
        reopened
            .peer()
            .document("so.tilo.word-processor", "tab:0000000000000001")
            .is_some()
    );
    assert!(
        !state.join("actions/recovery.json").exists(),
        "settled receipts left unresolved custody"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_flush_keeps_status_and_terminal_controls_alive() {
    use std::io::BufRead;

    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    drop(SessionOwner::initialize(&state).unwrap());
    let mut host = Host(
        Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
            .arg("run")
            .arg(&state)
            .arg(&ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let metadata: serde_json::Value = loop {
        if let Ok(bytes) = fs::read(&ready) {
            break serde_json::from_slice(&bytes).unwrap();
        }
        assert!(Instant::now() < deadline, "host never wrote ready metadata");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let origin = metadata["fixture_origin"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap();
    let before = loop {
        let current = status(origin, deadline).await;
        if current["actions"] == "ready"
            && current["qualification"]["signing"]["outstanding_handoffs"] == 0
        {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "startup did not settle: {current}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let saved = state.join("session.saved");
    fs::rename(state.join("session.bin"), &saved).unwrap();
    fs::create_dir(state.join("session.bin")).unwrap();
    writeln!(host.0.stdin.as_mut().unwrap(), "flush").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let terminal = loop {
        let current = status(origin, deadline).await;
        if current["actions"] == "unavailable" && current["native_peer"] == "disconnected" {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "failed flush did not withdraw authority: {current}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(terminal["action_error"].is_string(), "{terminal}");
    assert_eq!(terminal["generation"], before["generation"]);
    // Repairing the pathname does not clear this owner's uncertainty latch.
    fs::remove_dir(state.join("session.bin")).unwrap();
    fs::rename(&saved, state.join("session.bin")).unwrap();
    let retained = fs::read(state.join("session.bin")).unwrap();
    let export = temp.path().join("must-not-export");
    let deadline = Instant::now() + Duration::from_secs(5);
    writeln!(
        host.0.stdin.as_mut().unwrap(),
        "reconnect\nflush\nexport {}\nstatus",
        export.display()
    )
    .unwrap();
    let stdout = host.0.stdout.take().unwrap();
    let line = tokio::time::timeout_at(
        deadline.into(),
        tokio::task::spawn_blocking(move || {
            std::io::BufReader::new(stdout)
                .lines()
                .next()
                .expect("control process exited")
                .unwrap()
        }),
    )
    .await
    .expect("terminal controls did not finish")
    .unwrap();
    let console: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(console["generation"], before["generation"]);
    assert_eq!(console["native_peer"], "disconnected");
    let after = status(origin, deadline).await;
    assert_eq!(after["action_error"], terminal["action_error"]);
    assert_eq!(
        after["native_subscription_generation"],
        terminal["native_subscription_generation"]
    );
    assert_eq!(after["actions"], "unavailable");
    assert_eq!(fs::read(state.join("session.bin")).unwrap(), retained);
    assert!(
        !export.exists(),
        "terminal owner exported apparently healthy custody"
    );
    drop(host);
    assert_eq!(
        SessionOwner::open(&state).unwrap().status().generation,
        before["generation"].as_u64().unwrap()
    );
}

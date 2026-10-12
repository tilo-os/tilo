//! Exercise the executable against the production JSON-line Unix socket protocol.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "Socket fixtures and result assertions fail the test immediately on malformed data"
)]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::var_os("TMPDIR").map_or_else(|| PathBuf::from("/var/tmp"), PathBuf::from);
        let path = root.join(format!(
            "history-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str], steps: Vec<(Value, Value)>) -> Output {
        let socket = self.0.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for (expected, response) in steps {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let actual: Value = serde_json::from_str(&line).unwrap();
                // Wait budgets are deadlines, not identities.
                for (key, value) in expected.as_object().unwrap() {
                    assert_eq!(&actual[key], value, "wire field {key}: {actual}");
                }
                writeln!(stream, "{response}").unwrap();
            }
        });
        let output = Command::new(env!("CARGO_BIN_EXE_tilo-control"))
            .arg("--socket")
            .arg(&socket)
            .args(args)
            .output()
            .unwrap();
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        output
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn row(operation: &str, id: &str) -> Value {
    json!({"provider":"app-actions", "id":id, "operation":operation, "application":"so.tilo.notes", "room":"room-A", "record":"record-A", "resource":"root"})
}
fn listing(rows: Vec<Value>) -> (Value, Value) {
    (
        json!({"op":"actions_enumerate", "query":"", "include_programmatic":true}),
        json!({"answer":{"actions":Value::Array(rows),"complete":true,"truncated":false}}),
    )
}
fn receipt(state: &str, value: Value) -> Value {
    let mut response = json!({"answer":{"receipt":{"broker_invocation_id":42,"update":{"state":state,"output":{"value":null},"error":{"code":"stale_review","message":"Stale restore review: the live document changed"}}}}});
    *response
        .pointer_mut("/answer/receipt/update/output/value")
        .unwrap() = value;
    response
}
fn output(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn historical_read_diff_and_paged_exact_discovery_return_native_results() {
    let fixture = Fixture::new();
    for (verb, args, native) in [
        (
            "read",
            json!({"version":"saved","item":"note-7"}),
            json!({"heads":["d".repeat(64)],"content":{"title":"before"}}),
        ),
        (
            "diff",
            json!({"from":"saved","to":"now"}),
            json!({"paths":["/title"],"from_heads":["e".repeat(64)]}),
        ),
        (
            "log",
            json!({"last":2}),
            json!({"versions":[{"id":"f".repeat(64),"author":"collaborator"}]}),
        ),
    ] {
        let operation = if verb == "log" { "history" } else { verb };
        let arg_text = args.to_string();
        let result = fixture.run(&["history",verb,"--provider","app-actions","--id","second-page","--args-json",&arg_text],vec![
            (json!({"op":"actions_enumerate"}),json!({"answer":{"actions":[row(operation,"other")],"complete":true,"next_cursor":"opaque-cursor","truncated":true}})),
            (json!({"op":"actions_enumerate","cursor":"opaque-cursor"}),json!({"answer":{"actions":[row(operation,"second-page")],"complete":true,"truncated":false}})),
            (json!({"op":"actions_invoke","id":"second-page","args":args}),receipt("succeeded",native.clone())),
        ]);
        assert_eq!(output(&result, 0)["result"], native);
    }
}

#[test]
fn pending_completion_keeps_job_identity_and_nonzero_exit() {
    let fixture = Fixture::new();
    let response = receipt("running", Value::Null);
    let steps = (0..65)
        .map(|_| {
            (
                json!({"op":"actions_status","invocation_id":42}),
                response.clone(),
            )
        })
        .collect();
    let result = fixture.run(&["history", "status", "--invocation-id", "42"], steps);
    let result = output(&result, 3);
    assert_eq!(result["answer"]["receipt"]["broker_invocation_id"], 42);
    assert_eq!(result["answer"]["receipt"]["update"]["state"], "running");
}

const TARGET: &[&str] = &[
    "--application",
    "so.tilo.notes",
    "--room",
    "room-A",
    "--record",
    "record-A",
    "--resource",
    "root",
];

#[test]
fn discovery_refuses_ambiguity_missing_and_incomplete_targets() {
    let fixture = Fixture::new();
    let mut args = vec!["history", "log"];
    args.extend(TARGET);
    let result = fixture.run(
        &args,
        vec![listing(vec![row("history", "one"), row("history", "two")])],
    );
    assert_eq!(output(&result, 1)["err"]["code"], "ambiguous_target");
    let result = fixture.run(
        &[
            "history",
            "log",
            "--provider",
            "app-actions",
            "--id",
            "absent",
        ],
        vec![listing(vec![row("history", "one")])],
    );
    assert_eq!(output(&result, 1)["err"]["code"], "missing_target");
    let result = fixture.run(
        &args,
        vec![(
            json!({"op":"actions_enumerate"}),
            json!({"answer":{"complete":false,"actions":[row("history","one")]}}),
        )],
    );
    assert_eq!(output(&result, 1)["err"]["code"], "discovery_pending");
    let bad = Command::new(env!("CARGO_BIN_EXE_tilo-control"))
        .args(["history", "log", "--application", "so.tilo.notes"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn review_roundtrip_binds_target_and_preserves_full_heads_and_stale_error() {
    let fixture = Fixture::new();
    let review = json!({"schema":1,"target_heads":["a".repeat(64)],"live_heads":["b".repeat(64)],"paths":["/title"]});
    let native = json!({"review":review,"diff":{"paths":["/title"]}});
    let mut args = vec!["history", "review"];
    args.extend(TARGET);
    args.extend(["--args-json", r#"{"version":"saved","paths":["/title"]}"#]);
    let result = fixture.run(&args, vec![listing(vec![row("review-restore","opaque-review")]), (json!({"op":"actions_invoke","provider":"app-actions","id":"opaque-review","args":{"version":"saved","paths":["/title"]}}),receipt("succeeded",native.clone()))]);
    let file = output(&result, 0);
    assert_eq!(file["result"], native);
    let path = fixture.0.join("review.json");
    std::fs::write(&path, &result.stdout).unwrap();
    let path = path.to_str().unwrap();
    let result = fixture.run(
        &[
            "history",
            "restore",
            "--provider",
            "app-actions",
            "--id",
            "opaque-restore",
            "--review-file",
            path,
        ],
        vec![
            listing(vec![row("restore-reviewed", "opaque-restore")]),
            (
                json!({"op":"actions_invoke","id":"opaque-restore","args":{"review":review}}),
                receipt("running", Value::Null),
            ),
            (
                json!({"op":"actions_status","invocation_id":42}),
                receipt("failed", Value::Null),
            ),
        ],
    );
    let result = output(&result, 1);
    assert_eq!(
        result["response"]["answer"]["receipt"]["update"]["error"]["code"],
        "stale_review"
    );
    assert!(
        result["response"]["answer"]["receipt"]["update"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("live document changed")
    );
    let mut foreign = row("restore-reviewed", "foreign");
    foreign["record"] = json!("another-record");
    let result = fixture.run(
        &[
            "history",
            "restore",
            "--provider",
            "app-actions",
            "--id",
            "foreign",
            "--review-file",
            path,
        ],
        vec![listing(vec![foreign])],
    );
    assert_eq!(output(&result, 1)["err"]["code"], "review_target_mismatch");
}

#[test]
fn permission_denial_and_terminal_status_remain_machine_readable() {
    let fixture = Fixture::new();
    let denied = json!({"err":{"code":"permission_denied","message":"role cannot checkpoint","details":{"role":"viewer"}}});
    let result = fixture.run(
        &[
            "history",
            "checkpoint",
            "--provider",
            "app-actions",
            "--id",
            "checkpoint",
            "--args-json",
            r#"{"name":"saved"}"#,
        ],
        vec![
            listing(vec![row("checkpoint", "checkpoint")]),
            (
                json!({"op":"actions_invoke","args":{"name":"saved"}}),
                denied.clone(),
            ),
        ],
    );
    assert_eq!(output(&result, 1), denied);
    for (state, code) in [("failed", 1), ("cancelled", 1), ("succeeded", 0)] {
        let response = receipt(state, json!({"heads":["c".repeat(64)]}));
        let result = fixture.run(
            &["history", "status", "--invocation-id", "42"],
            vec![(
                json!({"op":"actions_status","invocation_id":42}),
                response.clone(),
            )],
        );
        assert_eq!(output(&result, code), response);
    }
}

#[test]
fn strict_arguments_and_unchecked_restore_are_never_invoked() {
    for args in [
        vec![
            "history",
            "restore",
            "--provider",
            "app-actions",
            "--id",
            "restore",
            "--args-json",
            r#"{"version":"old"}"#,
        ],
        vec![
            "history",
            "diff",
            "--provider",
            "app-actions",
            "--id",
            "diff",
            "--args-json",
            r#"{"from":"old","extra":true}"#,
        ],
        vec![
            "history",
            "log",
            "--provider",
            "app-actions",
            "--id",
            "log",
            "--args-json",
            r#"{"last":0}"#,
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tilo-control"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let fixture = Fixture::new();
    let path = fixture.0.join("review.json");
    std::fs::write(&path, b"{}").unwrap();
    let result = fixture.run(
        &[
            "history",
            "restore",
            "--provider",
            "app-actions",
            "--id",
            "unchecked",
            "--review-file",
            path.to_str().unwrap(),
        ],
        vec![listing(vec![row("restore", "unchecked")])],
    );
    assert_eq!(output(&result, 1)["err"]["code"], "missing_target");
}

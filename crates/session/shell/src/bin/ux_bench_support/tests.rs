#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test assertions and native protocol fixture"
)]
use super::*;

#[test]
fn bench_input_accepts_only_bounded_visible_commands() {
    // Phase `accepts_only_bounded_visible_input`.
    {
        for raw in [
            r#"{"op":"observe"}"#,
            r#"{"op":"click","x":12,"y":34}"#,
            r#"{"op":"context_click","x":12,"y":34}"#,
            r#"{"op":"key","key":"a","modifiers":["ctrl"]}"#,
            r#"{"op":"type","text":"Evidence"}"#,
            r#"{"op":"wheel","x":12,"y":34,"dx":0,"dy":300}"#,
            r#"{"op":"finish"}"#,
        ] {
            assert!(validate_command(raw, [1280, 800]).is_ok(), "{raw}");
        }
    }
    // Phase `rejects_malformed_nonfinite_outside_and_semantic_commands`.
    {
        for raw in [
            "null",
            "garbage",
            r#"{"op":"click","x":1e999,"y":0}"#,
            r#"{"op":"click","x":-1,"y":0}"#,
            r#"{"op":"click","x":1280,"y":0}"#,
            r#"{"op":"click","x":0,"y":800}"#,
            r#"{"op":"context_click","x":1280,"y":10}"#,
            r#"{"op":"observe","extra":true}"#,
            r#"{"op":"click_named","target":"Files"}"#,
            r#"{"op":"key","key":"Frob","modifiers":[]}"#,
            r#"{"op":"key","key":"a","modifiers":["unknown"]}"#,
            r#"{"op":"wheel","x":10,"y":10,"dx":0,"dy":10001}"#,
        ] {
            assert!(validate_command(raw, [1280, 800]).is_err(), "{raw}");
        }
        let long = serde_json::json!({"op":"type","text":"x".repeat(4097)}).to_string();
        assert!(validate_command(&long, [1280, 800]).is_err());
    }
}

// Removing the interactive branch, rebuilding each request, or retaining a chord
// modifier must break this actual-shell persistence test.
#[test]
#[ignore = "requires cargo build -p shell --bin ux-bench first; renders native fixture"]
fn native_persistent_click_key_rename_and_finish() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let out = root
        .join("target/ux-evidence")
        .join(format!("host-test-{}", std::process::id()));
    let binary = std::env::var_os("UX_BENCH_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/debug/ux-bench"));
    let mut child = Command::new(binary)
        .args(["--interactive", "--scenario"])
        .arg(root.join("scripts/ux-bench/workspace-settings.json"))
        .arg("--out")
        .arg(&out)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    fn receive(output: &mut impl BufRead) -> serde_json::Value {
        let mut line = String::new();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "host exited before response"
        );
        let value: serde_json::Value =
            serde_json::from_str(&line).expect("stdout must contain only JSONL");
        for field in ["screenshot", "inspection"] {
            let path = std::path::Path::new(value[field].as_str().unwrap());
            assert!(path.is_absolute() && path.is_file());
        }
        value
    }
    fn send(
        input: &mut impl Write,
        output: &mut impl BufRead,
        command: serde_json::Value,
    ) -> serde_json::Value {
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
        receive(output)
    }
    fn bounds(value: &serde_json::Value, name: &str) -> Option<[f64; 4]> {
        if value.get("name").and_then(serde_json::Value::as_str) == Some(name)
            || value
                .get("semantic_key")
                .and_then(serde_json::Value::as_str)
                == Some(name)
        {
            if let Some(b) = value.get("bounds") {
                return Some([
                    b["x0"].as_f64()?,
                    b["y0"].as_f64()?,
                    b["x1"].as_f64()?,
                    b["y1"].as_f64()?,
                ]);
            }
        }
        match value {
            serde_json::Value::Object(map) => map.values().find_map(|v| bounds(v, name)),
            serde_json::Value::Array(list) => list.iter().find_map(|v| bounds(v, name)),
            _ => None,
        }
    }
    fn click(
        input: &mut impl Write,
        output: &mut impl BufRead,
        frame: &serde_json::Value,
        name: &str,
    ) -> serde_json::Value {
        let inspection: serde_json::Value =
            serde_json::from_slice(&std::fs::read(frame["inspection"].as_str().unwrap()).unwrap())
                .unwrap();
        let b = bounds(&inspection, name).unwrap_or_else(|| panic!("missing bounds for {name}"));
        send(
            input,
            output,
            serde_json::json!({"op":"click","x":(b[0]+b[2])/2.,"y":(b[1]+b[3])/2.}),
        )
    }
    let initial = receive(&mut output);
    assert_eq!(initial["sequence"], 0);
    let invalid = send(
        &mut input,
        &mut output,
        serde_json::json!({"op":"click","x":-1,"y":0}),
    );
    assert_eq!(invalid["ok"], false);
    assert_eq!(invalid["counts"]["clicks"], 0);
    // The workspace name opens its menu; the settings are one row in it.
    let menu = click(
        &mut input,
        &mut output,
        &initial,
        "topbar:workspace-settings",
    );
    let settings = click(&mut input, &mut output, &menu, "Workspace settings…");
    let _ = click(&mut input, &mut output, &settings, "Workspace name");
    send(
        &mut input,
        &mut output,
        serde_json::json!({"op":"key","key":"a","modifiers":["ctrl"]}),
    );
    send(
        &mut input,
        &mut output,
        serde_json::json!({"op":"type","text":"Evidence"}),
    );
    send(
        &mut input,
        &mut output,
        serde_json::json!({"op":"key","key":"Enter","modifiers":[]}),
    );
    let last = send(&mut input, &mut output, serde_json::json!({"op":"finish"}));
    assert_eq!(last["ok"], true);
    assert_eq!(last["state"]["active_workspace"], "Evidence");
    assert_eq!(
        last["persisted_workspace_names"],
        serde_json::json!(["Evidence"])
    );
    assert_eq!(last["counts"]["clicks"], 3);

    assert!(child.wait().unwrap().success());
}

/// Native regression: selecting a result must close paint and accessibility together.
#[test]
#[ignore = "requires a built ux-bench binary; renders native fixture"]
fn dismissed_palette_semantics_match_observation_after_bounded_frames() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let out = root
        .join("target/ux-evidence")
        .join(format!("palette-semantics-repro-{}", std::process::id()));
    let binary = std::env::var_os("UX_BENCH_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/debug/ux-bench"));
    let mut child = Command::new(binary)
        .args(["--interactive", "--scenario"])
        .arg(root.join("scripts/ux-bench/open-files.json"))
        .arg("--out")
        .arg(&out)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    fn receive(output: &mut impl BufRead) -> Value {
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    fn send(input: &mut impl Write, output: &mut impl BufRead, action: Value) -> Value {
        writeln!(input, "{action}").unwrap();
        input.flush().unwrap();
        receive(output)
    }
    fn open_palette_nodes(value: &Value) -> usize {
        let own = usize::from(
            value.get("name").and_then(Value::as_str) == Some("Command palette")
                && value["state"]["hidden"] == false,
        );
        own + match value {
            Value::Object(map) => map.values().map(open_palette_nodes).sum(),
            Value::Array(list) => list.iter().map(open_palette_nodes).sum(),
            _ => 0,
        }
    }
    receive(&mut output);
    send(
        &mut input,
        &mut output,
        json!({"op":"key","key":"Space","modifiers":["meta"]}),
    );
    send(&mut input, &mut output, json!({"op":"type","text":"Files"}));
    let selected = send(
        &mut input,
        &mut output,
        json!({"op":"key","key":"Enter","modifiers":[]}),
    );
    let mut samples = vec![selected];
    for _ in 0..3 {
        samples.push(send(&mut input, &mut output, json!({"op":"observe"})));
    }
    send(&mut input, &mut output, json!({"op":"finish"}));
    assert!(child.wait().unwrap().success());
    let mut counts = vec![];
    for frame in &samples {
        assert_eq!(frame["state"]["tab_titles"], json!(["Files"]));
        let inspection: Value =
            serde_json::from_slice(&std::fs::read(frame["inspection"].as_str().unwrap()).unwrap())
                .unwrap();
        counts.push(open_palette_nodes(&inspection));
    }
    eprintln!(
        "bounded frame palette semantic counts: {counts:?}; evidence: {}",
        out.display()
    );
    assert_eq!(
        counts,
        vec![0, 0, 0, 0],
        "dismissed palette must not remain accessibility-visible after real frame updates"
    );
}

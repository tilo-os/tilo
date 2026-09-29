#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
//! `--json` keeps stdout to exactly one JSON document, whatever the command
//! and its child processes print.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn tilo_app(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tilo-app"))
        .args(args)
        .current_dir(dir)
        .env_remove("TILO_UI_PATH")
        .output()
        .expect("run tilo-app")
}

fn document(output: &Output) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout");
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must be one JSON line, got {stdout:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect("stdout is JSON")
}

#[test]
fn new_dry_run_lists_every_planned_file_in_both_modes() {
    let dir = tempfile::tempdir().unwrap();
    let human = tilo_app(dir.path(), &["new", "notes", "--dry-run"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    for file in [
        "Cargo.toml",
        "src/lib.rs",
        "tilo-app.toml",
        "locales/en-US.ftl",
    ] {
        assert!(human.contains(&format!("\n  {file}")), "{human}");
    }

    let output = tilo_app(dir.path(), &["new", "notes", "--dry-run", "--json"]);
    assert!(output.status.success());
    let report = document(&output);
    assert_eq!(report["ok"], true);
    assert_eq!(report["command"], "new");
    assert_eq!(report["result"]["dry_run"], true);
    let files = report["result"]["files"].as_array().unwrap();
    assert!(files.iter().any(|file| file == "Cargo.toml"), "{report}");
    assert!(
        Path::new(report["result"]["app_dir"].as_str().unwrap()).is_absolute(),
        "{report}"
    );
    assert!(
        !dir.path().join("notes").exists(),
        "a dry run writes nothing"
    );
}

#[test]
fn templates_are_listed_in_both_modes() {
    let dir = tempfile::tempdir().unwrap();
    let human = tilo_app(dir.path(), &["new", "--list-templates"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("starter"), "{human}");
    assert!(human.contains("(default)"), "{human}");

    let output = tilo_app(dir.path(), &["new", "--list-templates", "--json"]);
    assert!(output.status.success());
    let report = document(&output);
    assert_eq!(report["ok"], true);
    let names: Vec<_> = report["result"]["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|template| template["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["starter", "document", "collection", "graph"]);
}

#[test]
fn examples_are_listed_in_both_modes() {
    let dir = tempfile::tempdir().unwrap();
    let human = tilo_app(dir.path(), &["examples"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("shared-checklist"), "{human}");
    assert!(human.contains("--from-example"), "{human}");

    let output = tilo_app(dir.path(), &["examples", "--json"]);
    assert!(output.status.success());
    let report = document(&output);
    assert_eq!(report["ok"], true);
    let examples = report["result"]["examples"].as_array().unwrap();
    assert_eq!(examples[0]["name"], "counter");
    assert_eq!(examples[0]["path"], "examples/apps/counter");
    assert!(examples.iter().all(|example| example["web"].is_boolean()));

    let refused = tilo_app(
        dir.path(),
        &[
            "new",
            "notes",
            "--from-example",
            "no-such-example",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    let report = document(&refused);
    assert!(
        report["error"].as_str().unwrap().contains("counter"),
        "the error names the examples there are: {report}"
    );
}

#[test]
fn json_never_prompts_and_reports_failures_as_documents() {
    let dir = tempfile::tempdir().unwrap();
    let output = tilo_app(dir.path(), &["new", "--json"]);
    assert!(!output.status.success());
    let report = document(&output);
    assert_eq!(report["ok"], false);
    assert!(
        report["error"].as_str().unwrap().contains("name"),
        "{report}"
    );

    let output = tilo_app(dir.path(), &["--json", "no-such-command"]);
    assert_eq!(output.status.code(), Some(2));
    let report = document(&output);
    assert_eq!(report["ok"], false);
    assert!(
        report["usage"].as_str().unwrap().contains("Usage"),
        "{report}"
    );
}

#[test]
fn child_process_output_moves_to_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("main.json");
    let init = tilo_app(
        dir.path(),
        &["ui", "init", graph.to_str().unwrap(), "--json"],
    );
    assert_eq!(document(&init)["result"]["action"], "create");

    // Installed by a separate process: a sibling test thread forking while we
    // hold a writable descriptor on the script would make its exec ETXTBSY.
    let draft = dir.path().join("noisy-builder.draft");
    std::fs::write(&draft, "#!/bin/sh\necho builder-noise-on-stdout\n").unwrap();
    let builder = dir.path().join("noisy-builder");
    let installed = Command::new("install")
        .arg("-m")
        .arg("755")
        .arg(&draft)
        .arg(&builder)
        .status()
        .unwrap();
    assert!(installed.success());
    let output = tilo_app(
        dir.path(),
        &[
            "ui",
            "edit",
            graph.to_str().unwrap(),
            "--builder",
            builder.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let report = document(&output);
    assert_eq!(report["result"]["action"], "edit");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("builder-noise-on-stdout"),
        "child output stays visible on stderr"
    );
}

#[test]
fn a_failed_verdict_still_carries_its_report() {
    let dir = tempfile::tempdir().unwrap();
    let created = tilo_app(dir.path(), &["new", "notes", "--json"]);
    assert_eq!(document(&created)["ok"], true);
    std::fs::remove_file(dir.path().join("notes/src/main.rs")).unwrap();

    let output = tilo_app(dir.path(), &["doctor", "notes", "--json"]);
    assert!(!output.status.success());
    let report = document(&output);
    assert_eq!(report["ok"], false);
    let checks = report["result"]["checks"]
        .as_array()
        .expect("doctor checks");
    assert!(
        checks.iter().any(|check| check["status"] == "fail"),
        "{report}"
    );
}

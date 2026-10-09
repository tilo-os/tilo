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
fn guides_work_offline_outside_an_app_and_emit_only_the_requested_task() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tilo-app"))
            .args(args)
            .current_dir(dir.path())
            .env("PATH", dir.path())
            .env("CARGO", dir.path().join("missing-cargo"))
            .env_remove("TILO_UI_PATH")
            .output()
            .unwrap()
    };
    let index = run(&["guide", "--json"]);
    assert!(index.status.success(), "{index:?}");
    let index = document(&index);
    let topics = index["result"]["topics"].as_array().unwrap();
    assert_eq!(topics.len(), 7);
    for topic in topics {
        let id = topic["id"].as_str().unwrap();
        let human = run(&["guide", id]);
        assert!(human.status.success(), "{human:?}");
        let human = String::from_utf8(human.stdout).unwrap();
        let output = run(&["guide", id, "--json"]);
        assert!(output.status.success(), "{output:?}");
        assert!(
            output.stderr.is_empty(),
            "guide text is not duplicated to stderr"
        );
        let guide = document(&output);
        assert_eq!(guide["command"], "guide");
        assert_eq!(guide["result"]["topic"], id);
        assert_eq!(guide["result"]["format"], "markdown");
        assert_eq!(guide["result"]["content"], human);
        assert!(human.starts_with("# "));
        assert!(human.contains("tilo-app "));
        assert!(human.len() < 3500, "one task should stay small: {id}");
    }
    for alias in ["undo", "history", "sync"] {
        let output = run(&["guide", alias, "--json"]);
        assert!(output.status.success(), "{alias}: {output:?}");
        assert!(output.stderr.is_empty());
        let guide = document(&output);
        assert_eq!(guide["result"]["topic"], "collaborate");
        assert_eq!(
            guide["result"]["source"],
            "docs/developers/tasks/collaborate.md"
        );
        let content = guide["result"]["content"].as_str().unwrap();
        assert!(content.contains("--from-example shared-checklist"));
        assert!(content.contains("undo_takes_back_only_my_own_actions"));
        assert!(content.contains("host integration tests"));
    }
    for alias in ["network", "http", "fetch", "metered", "offline"] {
        let output = run(&["guide", alias, "--json"]);
        assert!(output.status.success(), "{alias}: {output:?}");
        let guide = document(&output);
        assert_eq!(guide["result"]["topic"], "network");
        assert_eq!(
            guide["result"]["source"],
            "docs/developers/tasks/network.md"
        );
        let content = guide["result"]["content"].as_str().unwrap();
        assert!(content.contains("tilo-app add capability network.http"));
        assert!(content.contains("error_for_status"));
        assert!(content.contains("permission grant"));
        assert!(content.contains("lossless hints"));
        assert!(content.contains("false answer stays unknown"));
    }
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    let invalid = run(&["guide", "unknown", "--json"]);
    assert!(!invalid.status.success());
    let error = document(&invalid);
    assert_eq!(error["ok"], false);
    let message = error["usage"].as_str().unwrap();
    for topic in [
        "start",
        "actions",
        "collaborate",
        "permissions",
        "network",
        "verify",
        "publish",
    ] {
        assert!(
            message.contains(topic),
            "usage error suggests {topic}: {error}"
        );
    }
}

#[test]
fn guides_permissions_aliases_are_offline_and_explain_live_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tilo-app"))
            .args(args)
            .current_dir(dir.path())
            .env("PATH", dir.path())
            .env("CARGO", dir.path().join("missing-cargo"))
            .env_remove("TILO_UI_PATH")
            .output()
            .unwrap()
    };
    for alias in ["permissions", "roles", "access"] {
        let output = run(&["guide", alias, "--json"]);
        assert!(output.status.success(), "{alias}: {output:?}");
        assert!(output.stderr.is_empty());
        let guide = document(&output);
        assert_eq!(guide["result"]["topic"], "permissions");
        assert_eq!(
            guide["result"]["source"],
            "docs/developers/tasks/permissions.md"
        );
        let content = guide["result"]["content"].as_str().unwrap();
        assert!(content.contains("ActionKind::Outside"));
        assert!(content.contains("set_role"));
        assert!(content.contains("invoke"));
        assert!(content.contains("outside the replicated resource"));
        assert!(content.contains("host integration"));
    }
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn github_ci_reports_both_workflows_without_extra_json_output() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tilo-app.toml"), "").unwrap();
    let args = ["add", "ci", "--forge", "github", "--dry-run"];
    let human = tilo_app(dir.path(), &args);
    assert!(human.status.success(), "{human:?}");
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("tilo.yml"));
    assert!(text.contains("tilo-build.yml"));
    let mut args = args.to_vec();
    args.push("--json");
    let output = tilo_app(dir.path(), &args);
    assert!(output.status.success(), "{output:?}");
    let report = document(&output);
    assert_eq!(report["ok"], true);
    assert_eq!(report["result"]["written"], false);
    for field in ["path", "reusable_path"] {
        assert!(Path::new(report["result"][field].as_str().unwrap()).is_absolute());
    }
    assert!(!dir.path().join(".github").exists());
    let output = tilo_app(dir.path(), &["add", "ci", "--forge", "github", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let report = document(&output);
    assert_eq!(report["result"]["written"], true);
    for field in ["path", "reusable_path"] {
        assert!(Path::new(report["result"][field].as_str().unwrap()).is_file());
    }
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
fn focused_examples_show_one_example_offline_outside_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-app"))
        .args(["examples", "shared-checklist", "--json"])
        .current_dir(dir.path())
        .env("PATH", dir.path())
        .env("CARGO", dir.path().join("missing-cargo"))
        .env_remove("TILO_UI_PATH")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stderr.is_empty(),
        "metadata is not duplicated to stderr"
    );
    let result = document(&output);
    assert_eq!(result["command"], "examples");
    assert_eq!(result["result"]["example"]["name"], "shared-checklist");
    assert!(result["result"]["examples"].is_null());
    assert!(
        result["result"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file == "tests/app.rs")
    );
    assert_eq!(
        result["result"]["create_command"],
        "tilo-app new NAME --from-example shared-checklist"
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn focused_examples_emit_exact_embedded_tests_once_in_each_mode() {
    let dir = tempfile::tempdir().unwrap();
    let args = ["examples", "shared-checklist", "--file", "tests/app.rs"];
    let human = tilo_app(dir.path(), &args);
    assert!(human.status.success(), "{human:?}");
    let expected = include_str!("../../../../examples/apps/shared-checklist/tests/app.rs");
    assert_eq!(human.stdout, expected.as_bytes());
    let mut args = args.to_vec();
    args.push("--json");
    let output = tilo_app(dir.path(), &args);
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stderr.is_empty(),
        "source is not duplicated to stderr"
    );
    let result = document(&output);
    assert_eq!(result["result"]["file"], "tests/app.rs");
    assert_eq!(result["result"]["content"], expected);
    assert_eq!(result["result"]["example"]["name"], "shared-checklist");
}

#[test]
fn focused_examples_refuse_unknown_files_without_reading_the_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("outside.txt"), "outside example fixture").unwrap();
    for path in ["outside.txt", "../../Cargo.toml", "/etc/passwd"] {
        let output = tilo_app(
            dir.path(),
            &["examples", "shared-checklist", "--file", path, "--json"],
        );
        assert!(!output.status.success());
        let report = document(&output);
        assert_eq!(report["ok"], false);
        assert_eq!(report["command"], "examples");
        let error = report["error"].as_str().unwrap();
        assert!(error.contains("choose one of:"));
        assert!(error.contains("src/lib.rs"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("outside example fixture"));
    }
    let missing_name = tilo_app(
        dir.path(),
        &["examples", "--file", "tests/app.rs", "--json"],
    );
    assert!(!missing_name.status.success());
    assert_eq!(document(&missing_name)["ok"], false);
    let unknown = tilo_app(dir.path(), &["examples", "unknown", "--json"]);
    assert!(!unknown.status.success());
    assert!(
        document(&unknown)["error"]
            .as_str()
            .unwrap()
            .contains("shared-checklist")
    );
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
    assert!(
        output.stderr.is_empty(),
        "example list is not duplicated to stderr"
    );
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

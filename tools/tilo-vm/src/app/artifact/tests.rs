use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_device_protocol::{RpcError, code};
use tilo_test_env::SpawnUnderEnvLock as _;

use super::*;
use crate::remote::fixture::Device;

/// An `x86_64` device that imports and verifies what it is given and installs what it swaps,
/// with `case` breaking one of those steps.
fn device(case: &'static str, layer_hash: &str) -> Device {
    let layer_hash = layer_hash.to_owned();
    let provenance = Arc::new(Mutex::new(Value::Null));
    let installed = Arc::new(Mutex::new(serde_json::Map::new()));
    Device::of_arch("x86_64", move |action, params| match action {
        "layer-import" => {
            *provenance.lock().unwrap() = params["provenance"].clone();
            let hash = if case == "import-hash" {
                "f".repeat(64)
            } else {
                layer_hash.clone()
            };
            Ok(json!({ "hash": hash, "existed": false }))
        }
        "layer-verify" => {
            let mut provenance = provenance.lock().unwrap().clone();
            if case == "provenance" {
                provenance["source_commit"] = json!("e".repeat(40));
            }
            Ok(json!({ "hash": layer_hash, "provenance": provenance }))
        }
        "swap"
            if matches!(
                case,
                "signature" | "publisher" | "runtime" | "incomplete" | "activation"
            ) =>
        {
            Err(RpcError {
                code: code::INVALID,
                message: format!("cohort verification: {case}"),
                hint: "supply a valid cohort".into(),
                see: None,
            })
        }
        "swap" => {
            let layer = if case == "account-layer" {
                json!("f".repeat(64))
            } else {
                params["layer"].clone()
            };
            installed
                .lock()
                .unwrap()
                .insert(params["component"].as_str().unwrap().to_owned(), layer);
            Ok(
                json!({ "account": params["account"], "generation": 1, "changed": false, "policy": "no_restart", "actions": [] }),
            )
        }
        "account-show" => Ok(json!({
            "account": {
                "name": params["name"], "kind": "headless", "owner": { "branch": "test" },
                "user": "test", "generation": 1, "generations": [0, 1],
                "components": installed.lock().unwrap().clone(), "session": "active", "seat": null,
            },
            "files": [],
        })),
        other => panic!("an artifact swap asks for no {other}"),
    })
}

#[test]
fn imported_cohort_uses_verified_import_and_addressed_account_readback() {
    for case in [
        "valid",
        "import-hash",
        "provenance",
        "signature",
        "publisher",
        "runtime",
        "incomplete",
        "activation",
        "account-layer",
    ] {
        let (_dir, request) = crate::artifact::tests::fixture();
        let mut prepared = prepare(&request).unwrap();
        let layer = prepared
            .layers
            .iter_mut()
            .find(|l| l.receipt.component == "apps+runtime")
            .unwrap();
        let device = device(case, &layer.receipt.layer_hash);
        let mut remote = Remote::connect("artifact-test", &device.machine, &device.route).unwrap();
        let calls = || -> Vec<String> { device.asked().into_iter().map(|a| a.action).collect() };
        let imported = import(&mut remote, layer);
        if matches!(case, "import-hash" | "provenance") {
            assert!(imported.is_err());
            assert!(!calls().contains(&"swap".to_owned()));
            continue;
        }
        assert_eq!(imported.unwrap(), layer.receipt.layer_hash);
        let activated = activate(&mut remote, "review", layer, None);
        if case == "valid" {
            assert!(activated.is_ok(), "{activated:?}");
        } else {
            assert!(activated.is_err(), "{case} was acknowledged as activated");
        }
        let calls = calls();
        assert_eq!(calls[..3], ["layer-import", "layer-verify", "swap"]);
        if matches!(
            case,
            "signature" | "publisher" | "runtime" | "incomplete" | "activation"
        ) {
            assert_eq!(calls.len(), 3, "failed apply must stop immediately");
        } else {
            assert_eq!(calls.last().map(String::as_str), Some("account-show"));
        }
    }
}

#[test]
fn consumer_checkout_accepts_exact_head_without_its_object_and_refuses_another_commit() {
    let dir = tempfile::tempdir().unwrap();
    let clone = dir.path().join("clone");
    let checkout = dir.path().join("src");
    let git = |cwd: &std::path::Path, args: &[&str]| -> String {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output_locked()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    std::fs::create_dir(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    git(&clone, &["commit", "-q", "--allow-empty", "-m", "producer"]);
    let producer = git(&clone, &["rev-parse", "HEAD"]);
    git(&clone, &["commit", "-q", "--allow-empty", "-m", "later"]);
    let later = git(&clone, &["rev-parse", "HEAD"]);
    git(
        &clone,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            checkout.to_str().unwrap(),
            &producer,
        ],
    );
    let worktree = Worktree::detect(&checkout).unwrap();
    consumer_checkout(&worktree, &producer).unwrap();

    let refused = consumer_checkout(&worktree, &later).unwrap_err().render();
    assert!(
        refused.contains("differs from requested exact source")
            && refused.contains(&producer)
            && refused.contains(&later),
        "{refused}"
    );

    let (shard, rest) = producer.split_at(2);
    std::fs::remove_file(clone.join(".git/objects").join(shard).join(rest)).unwrap();
    assert!(
        worktree.rev_parse("HEAD").is_none(),
        "the fixture must make the commit object unreadable"
    );
    consumer_checkout(&worktree, &producer).unwrap();
    assert!(consumer_checkout(&worktree, &later).is_err());
}

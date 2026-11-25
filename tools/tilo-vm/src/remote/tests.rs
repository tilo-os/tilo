use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_device_protocol::{LayerVerify, RpcError, TreeManifest, code};

use super::Remote;
use super::fixture::Device;
use crate::error::CliError;

fn layer(hash: &str) -> Value {
    json!({ "hash": hash, "provenance": { "component": "tilo-session" } })
}

tilo_test_env::scenario! {
    fn a_device_is_driven_through_this_computers_daemon;

    phase a_step_reaches_the_device_in_its_workspace_and_answers_typed;
    {
        let device = Device::serving(|action, argument| match action {
            "layer-verify" => Ok(layer(argument["hash"].as_str().unwrap())),
            other => panic!("unexpected step {other}"),
        });
        let mut remote = Remote::connect("dev", &device.machine, &device.route).unwrap();
        let verified = remote.call(LayerVerify { hash: "ab".repeat(32) });
        let asked = device.asked();
        assert_eq!(asked.len(), 1, "{verified:?}");
        assert_eq!(asked[0].action, "layer-verify");
        assert_eq!(asked[0].resource, format!("devices/{}/vm", device.route.device));
        assert_eq!(asked[0].argument, json!({ "hash": "ab".repeat(32) }));
        assert_eq!(
            asked[0].workspace.as_deref(),
            Some(device.route.workspace.as_str()),
            "asked in the route's workspace"
        );
        assert_eq!(verified.unwrap().hash, "ab".repeat(32));
    }

    phase a_failed_step_keeps_the_devices_code_and_hint;
    {
        let device = Device::serving(|_, _| {
            Err(RpcError {
                code: code::NOT_FOUND,
                message: "no layer ab".into(),
                hint: "tilo-vm layers".into(),
                see: None,
            })
        });
        let mut remote = Remote::connect("dev", &device.machine, &device.route).unwrap();
        match remote.call(LayerVerify { hash: "ab".into() }) {
            Err(CliError::Remote(error)) => {
                assert_eq!((error.code, error.message.as_str()), (code::NOT_FOUND, "no layer ab"));
                assert_eq!(error.hint, "tilo-vm layers");
            }
            other => panic!("expected the device's failure, got {other:?}"),
        }
    }

    phase bytes_go_once_and_a_tree_lands_by_its_manifest;
    {
        let synced = Arc::new(std::sync::Mutex::new(None));
        let seen = Arc::clone(&synced);
        let device = Device::serving(move |action, argument| {
            assert_eq!(action, "tree-sync");
            *seen.lock().unwrap() = Some(argument.clone());
            Ok(json!({ "written": 2, "removed": 0 }))
        });
        let mut remote = Remote::connect("dev", &device.machine, &device.route).unwrap();
        let big = vec![7_u8; 3 * tilo_actions::remote::MAX_CONTENT_FRAME_BYTES + 5];
        let hash = remote.put(&big).unwrap();
        assert_eq!(device.content.get(&hash).as_deref(), Some(big.as_slice()));
        assert_eq!(remote.put(&big).unwrap(), hash, "the same bytes are named the same");
        assert_eq!(device.content.len(), 1, "and are not stored twice");

        let tree = tempfile::tempdir().unwrap();
        std::fs::create_dir(tree.path().join("src")).unwrap();
        std::fs::write(tree.path().join("src/lib.rs"), b"pub fn a() {}\n").unwrap();
        std::fs::write(tree.path().join("Cargo.toml"), b"[package]\n").unwrap();
        let files = ["Cargo.toml".into(), "src/lib.rs".into()];
        remote.sync_tree("wt", tree.path(), &files).unwrap();
        let argument = synced.lock().unwrap().clone().unwrap();
        assert_eq!(argument["worktree"], "wt");
        let manifest: TreeManifest = serde_json::from_slice(
            &device.content.get(argument["manifest"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let listed: Vec<&str> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(listed, ["Cargo.toml", "src/lib.rs"]);
        for entry in &manifest.entries {
            let bytes = device.content.get(&entry.hash).expect("each file is on the device");
            assert_eq!(bytes, std::fs::read(tree.path().join(&entry.path)).unwrap());
        }
        let held = device.content.len();
        remote.sync_tree("wt", tree.path(), &files).unwrap();
        assert_eq!(device.content.len(), held, "an unchanged tree sends nothing new");
    }

    phase the_sleep_lease_is_renewed_while_work_runs_and_a_lost_lease_is_reported;
    {
        let renewals = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&renewals);
        let device = Device::serving(move |action, _| {
            assert_eq!(action, "awake");
            let held = counted.fetch_add(1, Ordering::SeqCst) < 2;
            Ok(json!({ "held": held, "problem": (!held).then_some("no inhibitor") }))
        });
        let remote = Remote::connect("dev", &device.machine, &device.route).unwrap();
        let (built, renewed) = remote
            .keep_awake_every(Duration::from_millis(20), || {
                while renewals.load(Ordering::SeqCst) < 2 {
                    std::thread::yield_now();
                }
                "built"
            })
            .unwrap();
        assert_eq!(built, "built");
        renewed.unwrap();
        let ((), lost) = remote
            .keep_awake_every(Duration::from_millis(20), || {
                while renewals.load(Ordering::SeqCst) < 3 {
                    std::thread::yield_now();
                }
            })
            .unwrap();
        assert!(lost.unwrap_err().to_string().contains("no inhibitor"));
    }
}

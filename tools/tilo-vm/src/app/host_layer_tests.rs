use super::*;
use crate::remote::fixture::Device;

fn connect(device: &Device) -> Remote {
    Remote::connect("test", &device.machine, &device.route).unwrap()
}

fn actions(device: &Device) -> Vec<String> {
    device
        .asked()
        .into_iter()
        .map(|asked| asked.action)
        .collect()
}

fn import_case(cached: bool, component: &'static str, corrupt: bool) {
    let dir = tempfile::tempdir().unwrap();
    let staging = build::Staging::new(&Worktree {
        root: dir.path().into(),
        branch: None,
    })
    .unwrap();
    std::fs::create_dir_all(staging.tree().join("usr/bin")).unwrap();
    std::fs::write(staging.tree().join("usr/bin/shell"), b"actual binary bytes").unwrap();
    let expected = tilo_device_protocol::layer_hash::tree_hash(&staging.tree()).unwrap();
    let listed = if cached {
        expected.clone()
    } else {
        {
            std::fs::write(staging.tree().join("usr/bin/shell"), b"older binary bytes").unwrap();
            let previous = tilo_device_protocol::layer_hash::tree_hash(&staging.tree()).unwrap();
            std::fs::write(staging.tree().join("usr/bin/shell"), b"actual binary bytes").unwrap();
            previous
        }
    };
    let returned = if corrupt {
        "f".repeat(64)
    } else {
        expected.clone()
    };
    let device = Device::serving(move |action, _| match action {
        "layer-list" => Ok(json!([{ "hash": listed, "provenance": { "component": component } }])),
        "layer-import" => Ok(json!({ "hash": returned, "existed": false })),
        other => panic!("a host layer import asks for no {other}"),
    });
    let mut remote = connect(&device);
    let mut app = App {
        json: true,
        local: false,
        target: None,
        worktree: None,
        registry_path: dir.path().join("vms.toml"),
        runner: SystemRunner { cwd: None },
        receipt: Receipt::new(dir.path(), vec![]),
        warnings: Vec::new(),
        machine: None,
    };
    let result = app.import_host_layer(
        &mut remote,
        &staging,
        tilo_device_protocol::Provenance::local("shell"),
    );
    let reused = cached && component == "shell";
    if reused {
        assert_eq!(result.unwrap(), expected);
        assert_eq!(actions(&device), ["layer-list"]);
        assert_eq!(device.content.len(), 0);
        assert!(
            !staging.archive().exists(),
            "a cache hit never packs the layer"
        );
    } else {
        assert_eq!(device.content.len(), 1, "the packed layer was sent");
        assert_eq!(
            actions(&device).last().map(String::as_str),
            Some("layer-import")
        );
        assert!(staging.archive().exists());
        if corrupt {
            assert!(result.is_err(), "a mismatched import cannot reach swap");
        } else {
            assert_eq!(result.unwrap(), expected);
        }
    }
}

tilo_test_env::scenario! {
    fn host_built_layers_and_apps_reach_the_daemon_whole_or_not_at_all;

    phase host_layer_cache_skips_upload_only_for_identical_component_content;
    {
        import_case(true, "shell", false);
        import_case(false, "shell", false);
        import_case(true, "other", false);
        import_case(false, "shell", true);
    }

    phase host_app_build_reconnects_before_upload_and_preserves_refusal;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App {
            json: true,
            local: false,
            target: None,
            worktree: None,
            registry_path: dir.path().join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir.path(), vec![]),
            warnings: Vec::new(),
            machine: None,
        };
        let before = refusing_device();
        let after = refusing_device();
        let mut remote = connect(&before);
        let result = app.install_host_app_package(
            &mut remote,
            "fixture-refused",
            |_runner| {
                let package = dir.path().join("app.tapp");
                std::fs::write(&package, b"owned package transport fixture").unwrap();
                Ok(package)
            },
            |_app| Ok(connect(&after)),
        );
        let error = result.expect_err("daemon refusal must not become installation success");
        assert!(
            error.to_string().contains("fixture account refused"),
            "{error}"
        );
        assert!(before.asked().is_empty() && before.content.len() == 0);
        let package = blake3::hash(b"owned package transport fixture").to_hex().to_string();
        assert!(after.content.get(&package).is_some());
        assert_eq!(actions(&after), ["app-install"]);
        assert_eq!(
            after.asked()[0].argument,
            json!({ "account": "fixture-refused", "source": { "content": { "hash": package } } })
        );
        let receipt_path = app
            .receipt
            .finish(Err(&json!({ "message": error.to_string() })))
            .unwrap();
        let receipt: Value = serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
        assert_eq!(receipt["status"], "failure");
        let phases: Vec<_> = receipt["phases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|phase| phase["name"].as_str().unwrap())
            .collect();
        assert_eq!(phases, ["build", "transfer", "install"]);
    }

    phase host_app_build_and_reconnect_failures_never_upload;
    {
        for build_fails in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = App {
                json: true,
                local: false,
                target: None,
                worktree: None,
                registry_path: dir.path().join("vms.toml"),
                runner: SystemRunner { cwd: None },
                receipt: Receipt::new(dir.path(), vec![]),
                warnings: Vec::new(),
                machine: None,
            };
            let device = refusing_device();
            let mut remote = connect(&device);
            let result = app.install_host_app_package(
                &mut remote,
                "fixture-refused",
                |_runner| {
                    if build_fails {
                        return Err(CliError::InvalidInput("build refused".into()));
                    }
                    let package = dir.path().join("app.tapp");
                    std::fs::write(&package, b"owned package transport fixture").unwrap();
                    Ok(package)
                },
                |_app| {
                    assert!(!build_fails, "failed builds must not reconnect");
                    Err(CliError::InvalidInput("connection refused".into()))
                },
            );
            let error = result.unwrap_err().to_string();
            assert!(error.contains(if build_fails {
                "build refused"
            } else {
                "connection refused"
            }));
            assert!(device.asked().is_empty());
            assert_eq!(device.content.len(), 0);
            assert!(!app.receipt.summary().contains("transfer"));
            assert!(!app.receipt.summary().contains("install"));
        }
    }

    phase metadata_failure_refuses_host_and_remote_builds_before_admission_or_apply;
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "this is not a Cargo manifest",
        )
        .unwrap();
        let device = refusing_device();
        let mut remote = connect(&device);
        let worktree = Worktree {
            root: dir.path().into(),
            branch: None,
        };
        let mut app = App {
            json: true,
            local: false,
            target: None,
            worktree: Some(worktree),
            registry_path: dir.path().join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir.path(), vec![]),
            warnings: Vec::new(),
            machine: None,
        };
        let vm = Resolved {
            name: "test".into(),
            entry: VmEntry::default(),
            source: Source::Argument,
        };
        let component = Manifest::parse("[[component]]\nname='shell'\npackages=['shell']\n")
            .unwrap()
            .components
            .remove("shell")
            .unwrap();
        for (attempt, remote_builder) in [false, true].into_iter().enumerate() {
            let result = if remote_builder {
                app.build_layer_remote(
                    &mut remote,
                    &vm,
                    &component,
                    &["must-not-start-builder".into()],
                )
            } else {
                app.build_layer(&mut remote, &vm, &component)
            };
            let CliError::CommandFailed {
                command, stderr, ..
            } = result.unwrap_err()
            else {
                panic!("the original Cargo metadata failure must remain observable");
            };
            assert_eq!(
                command,
                "cargo metadata --locked --no-deps --format-version 1"
            );
            assert!(stderr.contains("Cargo.toml"), "{stderr}");
            assert!(device.asked().is_empty());
            let phases = app.receipt.summary();
            assert_eq!(phases.split(", ").count(), attempt + 1);
            assert!(
                phases
                    .split(", ")
                    .all(|phase| phase.starts_with("metadata ")),
                "{phases}"
            );
            assert!(
                !dir.path().join("target").exists(),
                "no build or staging may begin"
            );
        }
    }
}

/// A device that refuses whatever it is asked to install.
fn refusing_device() -> Device {
    Device::serving(|_, _| {
        Err(tilo_device_protocol::RpcError {
            code: tilo_device_protocol::code::NOT_FOUND,
            message: "fixture account refused".into(),
            hint: "no install occurred".into(),
            see: None,
        })
    })
}

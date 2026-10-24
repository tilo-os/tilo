use super::*;
use crate::apply::session_unit;

#[test]
fn network_wake_requires_the_normal_protocol_handshake() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::default()),
        None,
        BaseInfo::default(),
    );
    let request = Request::NetworkWake(tilo_vm_protocol::NetworkWake {
        profile: "--invalid".into(),
        enabled: Some(true),
    });
    assert!(matches!(
        daemon.handle(request.clone()),
        Err(DaemonError::HelloRequired)
    ));
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let error = daemon.handle(request).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid NetworkManager profile UUID")
    );
}

#[test]
fn slot_show_reads_imported_provenance_and_keeps_it_after_failed_activation() {
    let root = tempfile::tempdir().unwrap();
    let payload = tempfile::tempdir().unwrap();
    fs::create_dir_all(payload.path().join("usr/bin")).unwrap();
    let binary = payload.path().join("usr/bin/shell");
    fs::write(&binary, "baseline").unwrap();
    let mut original = Provenance::local("shell");
    original.source_commit = Some("ab".repeat(20));
    // Built for the machine the daemon runs on, as a real layer is.
    original.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
    original.cargo_lock_hash = Some("cd".repeat(32));
    let store = LayerStore::new(root.path());
    let (baseline, _) = store
        .import_path(payload.path(), &original, &mut |_| Ok(()))
        .unwrap();
    fs::write(&binary, "candidate").unwrap();
    let mut candidate = original.clone();
    candidate.source_commit = Some("ef".repeat(20));
    let (next, _) = store
        .import_path(payload.path(), &candidate, &mut |_| Ok(()))
        .unwrap();
    let mut apply = RecordingApply::default();
    apply.readiness_replies.extend([
        Ok(()), // fresh slot
        Ok(()), // baseline
        Err("candidate failed readiness".into()),
        Ok(()), // recovered baseline
    ]);
    let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
    daemon
        .create("proof", SlotKind::Headless, Owner::default(), &[], false)
        .unwrap();
    assert!(daemon.show("proof").unwrap().provenance.is_empty());
    let info = Some(ComponentInfo::new(Scope::Session, vec![]));
    daemon
        .swap("proof", "shell", &baseline, info.clone())
        .unwrap();
    let before = daemon.show("proof").unwrap();
    assert_eq!(before.provenance["shell"], original);
    assert!(
        before
            .files
            .iter()
            .any(|file| file.path == "/usr/bin/shell")
    );
    let error = daemon.swap("proof", "shell", &next, info).unwrap_err();
    assert!(error.to_string().contains("restored generation 1"));
    let recovered = daemon.show("proof").unwrap();
    assert_eq!(recovered.slot.components["shell"], baseline);
    assert_eq!(recovered.provenance, before.provenance);
    // Old imports without metadata remain observable as unknown, rather
    // than inventing provenance from the component name or swap request.
    fs::remove_file(
        root.path()
            .join("layers")
            .join(&baseline)
            .join("provenance.json"),
    )
    .unwrap();
    let unknown = daemon.show("proof").unwrap();
    assert!(unknown.provenance.is_empty());
    assert_eq!(unknown.slot.components["shell"], baseline);
    assert!(!unknown.files.is_empty());
    let mut legacy = serde_json::to_value(&unknown).unwrap();
    legacy.as_object_mut().unwrap().remove("provenance");
    let legacy: SlotShowResult = serde_json::from_value(legacy).unwrap();
    assert!(legacy.provenance.is_empty());
}

#[test]
fn failed_swap_restores_generation_and_reports_failed_recovery() {
    for (slot, recovery) in [
        (DEVICE_SLOT, Ok(())),
        (DEVICE_SLOT, Err("old desktop also failed".to_owned())),
        ("review", Ok(())),
        ("review", Err("old desktop also failed".to_owned())),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut apply = RecordingApply::with_login("owner");
        apply.mirror = Some(mirror.clone());
        if slot != DEVICE_SLOT {
            apply.readiness_replies.push_back(Ok(()));
        }
        apply.readiness_replies.extend([
            Ok(()),
            Err("new desktop failed".to_owned()),
            recovery.clone(),
        ]);
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        if slot != DEVICE_SLOT {
            daemon
                .create(slot, SlotKind::Headless, Owner::default(), &[], false)
                .unwrap();
        }
        // Start with a real prior generation, then exercise the production
        // swap path: preparation, activation, readiness and recovery.
        let hash = "ab".repeat(32);
        fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
        let info = Some(ComponentInfo::new(Scope::Session, vec![]));
        assert_eq!(
            daemon
                .swap(slot, "shell", &hash, info.clone())
                .unwrap()
                .generation,
            1
        );
        mirror.lock().unwrap().clear();
        let next = "cd".repeat(32);
        fs::create_dir_all(root.path().join("layers").join(&next).join("usr/bin")).unwrap();
        let error = daemon.swap(slot, "shell", &next, info).unwrap_err();
        let detail = error.to_string();
        assert!(detail.contains("new desktop failed"), "{detail}");
        let record = daemon.load_slot(slot).unwrap();
        assert_eq!(
            record.current, 1,
            "a failed candidate never becomes current"
        );
        if slot == DEVICE_SLOT {
            assert_eq!(record.device.as_ref().unwrap().account, "owner");
        } else {
            assert_eq!(record.kind, SlotKind::Headless);
        }
        let calls = mirror.lock().unwrap().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("wait_session_ready "))
                .count(),
            2
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("reset_desktop") || c.contains("remove_user"))
        );
        if recovery.is_ok() {
            assert!(detail.contains("restored generation 1"), "{detail}");
            assert_eq!(daemon.slots.pending(slot), None);
        } else {
            assert!(
                detail.contains("recovery to generation 1 also failed"),
                "{detail}"
            );
            assert!(detail.contains("old desktop also failed"), "{detail}");
            assert_eq!(daemon.slots.pending(slot), Some(1));
        }
    }
}

const TOOLKIT: &str = "2026.07+native.0123456789abcdef0123456789abcdef";

fn user_package(id: &str, runtime: &str) -> package_store::registry::InstalledPackage {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "payload_root": format!("/home/tester/.local/share/tilo-apps/pkgs/{id}"),
        "manifest_path": format!("/home/tester/.local/share/tilo-apps/pkgs/{id}.pkg/manifest.toml"),
        "runtime": runtime,
    }))
    .unwrap()
}

/// Roll the owner's laptop back to the bare image after it took an
/// apps+runtime cohort shipping `TOOLKIT` and then a shell. Returns the
/// rollback's outcome, the calls the refusal check left behind, and
/// the generation current afterwards.
fn roll_cohort_back_to_image(
    packages: Vec<package_store::registry::InstalledPackage>,
    image_runtimes: &[&str],
) -> (Result<ApplyResult, DaemonError>, usize, usize, u64) {
    let root = tempfile::tempdir().unwrap();
    let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut apply = RecordingApply::with_login("tester");
    apply.mirror = Some(mirror.clone());
    apply.user_packages = packages;
    apply.image_runtimes = image_runtimes.iter().map(|v| (*v).to_owned()).collect();
    let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let first = "01".repeat(32);
    let next = "02".repeat(32);
    let layers = root.path().join("layers");
    fs::create_dir_all(
        layers
            .join(&first)
            .join("usr/lib/tilo-runtime")
            .join(TOOLKIT),
    )
    .unwrap();
    fs::create_dir_all(layers.join(&next).join("usr/bin")).unwrap();
    let session = Some(ComponentInfo::new(Scope::Session, Vec::new()));
    let cohort = daemon
        .swap(DEVICE_SLOT, COHORT_COMPONENT, &first, session.clone())
        .unwrap();
    daemon.swap(DEVICE_SLOT, "shell", &next, session).unwrap();
    // Rolling back a component the cohort does not carry is never checked.
    daemon
        .rollback(DEVICE_SLOT, Some(cohort.generation))
        .unwrap();
    let before = mirror.lock().unwrap().len();
    let result = daemon.rollback(DEVICE_SLOT, Some(0));
    let after = mirror.lock().unwrap().len();
    let current = daemon.load_slot(DEVICE_SLOT).unwrap().current;
    (result, before, after, current)
}

#[test]
fn cohort_rollback_without_user_packages_is_allowed() {
    let (result, _, _, current) = roll_cohort_back_to_image(Vec::new(), &[]);
    assert_eq!(result.unwrap().generation, 0);
    assert_eq!(current, 0);
}

#[test]
fn cohort_rollback_ignores_packages_that_need_no_app_toolkit() {
    // A runtime package (it may not declare a runtime) and an app that
    // names none, as `tilo-install` records them.
    let (result, _, _, current) = roll_cohort_back_to_image(
        vec![
            user_package("so.tilo.runtime.chromium", ""),
            user_package("org.example.static", ""),
        ],
        &[],
    );
    assert_eq!(result.unwrap().generation, 0);
    assert_eq!(current, 0);
}

#[test]
fn cohort_rollback_refuses_to_strand_a_package_on_the_removed_toolkit() {
    let runtime = format!("tilo-runtime/{TOOLKIT}");
    let (result, before, after, current) = roll_cohort_back_to_image(
        vec![
            user_package("so.tilo.runtime.chromium", ""),
            user_package("org.example.thin", &runtime),
            user_package("org.example.elsewhere", "tilo-runtime/2025.01+native.ffff"),
        ],
        &[],
    );
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains(&format!("org.example.thin needs {runtime}")),
        "{error}"
    );
    assert!(
        error.contains("tilo app remove org.example.thin"),
        "{error}"
    );
    assert!(!error.contains("chromium"), "{error}");
    assert!(!error.contains("org.example.elsewhere"), "{error}");
    assert_eq!(after, before, "refusal must precede any layer side effect");
    assert_ne!(current, 0);
}

#[test]
fn cohort_rollback_allows_a_package_whose_toolkit_the_image_ships() {
    let (result, _, _, current) = roll_cohort_back_to_image(
        vec![user_package(
            "org.example.thin",
            &format!("tilo-runtime/{TOOLKIT}"),
        )],
        &[TOOLKIT],
    );
    assert_eq!(result.unwrap().generation, 0);
    assert_eq!(current, 0);
}

#[test]
fn only_a_single_tilo_runtime_directory_names_a_native_toolkit() {
    assert_eq!(
        native_toolkit(&format!("tilo-runtime/{TOOLKIT}")),
        Some(TOOLKIT)
    );
    for runtime in [
        "",
        "tilo-runtime/",
        "tilo-runtime/..",
        "tilo-runtime/a/b",
        "other/1.0",
    ] {
        assert_eq!(native_toolkit(runtime), None, "{runtime}");
    }
}

/// Every connection's hello takes the keep-awake lease; `awake` holds it
/// longer, reports it, and releases it, and the next hello takes it again.
#[test]
fn every_connection_keeps_the_machine_awake_until_released() {
    // The lease is read against the clock it was taken by.
    tilo_clock::freeze_wall(tilo_clock::wall());
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::default()),
        None,
        BaseInfo::default(),
    );
    let hello = |daemon: &mut Daemon| -> HelloResult {
        serde_json::from_value(
            daemon
                .handle(Request::Hello(tilo_vm_protocol::Hello {
                    protocol: PROTOCOL_VERSION,
                    client_version: String::new(),
                }))
                .unwrap(),
        )
        .unwrap()
    };
    let awake = |daemon: &mut Daemon, seconds, off| -> AwakeLease {
        serde_json::from_value(
            daemon
                .handle(Request::Awake(tilo_vm_protocol::Awake { seconds, off }))
                .unwrap(),
        )
        .unwrap()
    };
    let now = tilo_clock::unix_now();
    let lease = hello(&mut daemon).awake.unwrap();
    assert!(lease.held, "{lease:?}");
    assert_eq!(lease.problem, None);
    let until = lease.until.unwrap();
    assert_eq!(until, now + 1800);
    assert!(root.path().join(awake::LEASE_FILE).is_file());

    let held = awake(&mut daemon, Some(7200), false);
    assert!(held.held);
    assert!(held.until.unwrap() >= now + 7200);
    assert_eq!(held.hold_until, held.until);
    // Reporting changes nothing; a new connection keeps the hold.
    assert_eq!(awake(&mut daemon, None, false).until, held.until);
    assert_eq!(hello(&mut daemon).awake.unwrap().until, held.until);

    let off = awake(&mut daemon, None, true);
    assert_eq!(off, AwakeLease::default());
    assert!(hello(&mut daemon).awake.unwrap().held);
}

/// Requests on a connection renew the lease as its hello did, but never
/// shorten an explicit hold.
#[test]
fn requests_renew_an_expired_lease_and_keep_a_hold() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::default()),
        None,
        BaseInfo::default(),
    );
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let now = tilo_clock::unix_now();
    let expired = awake::Lease {
        idle_until: now.saturating_sub(1),
        hold_until: None,
    };
    fs::write(daemon.lease_path(), serde_json::to_vec(&expired).unwrap()).unwrap();
    daemon
        .handle(Request::SlotList(tilo_vm_protocol::SlotList {}))
        .unwrap();
    let renewed = awake::read(&daemon.lease_path()).unwrap();
    assert!(renewed.active(now), "{renewed:?}");

    daemon
        .handle(Request::Awake(tilo_vm_protocol::Awake {
            seconds: Some(7200),
            off: false,
        }))
        .unwrap();
    let held = fs::read(daemon.lease_path()).unwrap();
    daemon
        .handle(Request::SlotList(tilo_vm_protocol::SlotList {}))
        .unwrap();
    assert_eq!(fs::read(daemon.lease_path()).unwrap(), held);
}

/// The laptop on 2026-10-22: a shell layer from main ships its
/// one-version-per-interface declaration over an image whose app host
/// reads lists of versions. The swap is refused before anything is
/// activated, naming the image's components to swap with it; the desktop
/// keeps running on the generation it had.
#[test]
fn a_swap_that_leaves_the_app_host_unable_to_read_the_declaration_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::with_login("tester")),
        None,
        BaseInfo::default(),
    );
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let device = daemon.load_slot(DEVICE_SLOT).unwrap();
    daemon.slots.save(&device).unwrap();
    let before = device.current;
    fs::write(
        root.path().join("image.json"),
        serde_json::to_vec(&serde_json::json!({
            "image": "composefs:1c9ab8",
            "recorded_at": "2026-10-19T19:32:21Z",
            "interfaces": tilo_vm_protocol::Declaration::parse(
                "[interfaces]\nsystem = [1]\napp-host = [1]\nshell-ui = [1]\n"
            ),
        }))
        .unwrap(),
    )
    .unwrap();
    let hash = "ca".repeat(32);
    let layer = root.path().join("layers").join(&hash);
    fs::create_dir_all(layer.join("usr/share/tilo")).unwrap();
    let declaration = "[interfaces]\nsystem = 1\nshell-ui = 11\nruntime = 2\n";
    fs::write(
        layer.join("usr/share/tilo/system-interfaces.toml"),
        declaration,
    )
    .unwrap();
    let mut provenance = Provenance::local("shell");
    provenance.interfaces = tilo_vm_protocol::system_interfaces(declaration);
    fs::write(
        layer.join("provenance.json"),
        serde_json::to_vec(&provenance).unwrap(),
    )
    .unwrap();

    let refused = daemon
        .handle(Request::Swap(tilo_vm_protocol::Swap {
            slot: DEVICE_SLOT.into(),
            component: "shell".into(),
            layer: hash,
            info: Some(ComponentInfo::new(Scope::Session, Vec::new())),
        }))
        .unwrap_err()
        .to_string();

    assert!(refused.contains("app-host"), "{refused}");
    assert!(refused.contains("ush"), "{refused}");
    assert!(refused.contains("tilo-vm swap userspace"), "{refused}");
    assert_eq!(
        daemon.load_slot(DEVICE_SLOT).unwrap().current,
        before,
        "the device keeps the generation it ran"
    );
}

/// tilo-fs is a runtime service apps reach, swapped VM-wide: an fs
/// layer serving another runtime version than the declaration the
/// device reads is refused before anything restarts, and one from the
/// same tree goes in.
#[test]
fn an_fs_swap_serving_another_runtime_version_than_the_device_reads_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::with_login("tester")),
        None,
        BaseInfo::default(),
    );
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let device = daemon.load_slot(DEVICE_SLOT).unwrap();
    daemon.slots.save(&device).unwrap();
    let declared = "[interfaces]\nsystem = 1\nshell-ui = 11\nruntime = 2\n";
    fs::write(
        root.path().join("image.json"),
        serde_json::to_vec(&serde_json::json!({
            "image": "composefs:1c9ab8",
            "recorded_at": "2026-10-22T08:00:00Z",
            "interfaces": tilo_vm_protocol::Declaration::parse(declared),
        }))
        .unwrap(),
    )
    .unwrap();
    // The device slot runs a shell from the image's tree, so it reads
    // runtime 2 whatever happens VM-wide.
    let shell = "5e".repeat(32);
    let layer = root.path().join("layers").join(&shell);
    fs::create_dir_all(layer.join("usr/bin")).unwrap();
    let mut provenance = Provenance::local("shell");
    provenance.interfaces = tilo_vm_protocol::system_interfaces(declared);
    fs::write(
        layer.join("provenance.json"),
        serde_json::to_vec(&provenance).unwrap(),
    )
    .unwrap();
    daemon
        .handle(Request::Swap(tilo_vm_protocol::Swap {
            slot: DEVICE_SLOT.into(),
            component: "shell".into(),
            layer: shell,
            info: Some(ComponentInfo::new(Scope::Session, Vec::new())),
        }))
        .unwrap();
    let fs_layer = |n: u8, runtime: u32| {
        let hash = format!("{n:02x}").repeat(32);
        let layer = root.path().join("layers").join(&hash);
        fs::create_dir_all(layer.join("usr/bin")).unwrap();
        let mut provenance = Provenance::local("fs");
        provenance.interfaces = tilo_vm_protocol::system_interfaces(&format!(
            "[interfaces]\nsystem = 1\nshell-ui = 11\nruntime = {runtime}\n"
        ));
        fs::write(
            layer.join("provenance.json"),
            serde_json::to_vec(&provenance).unwrap(),
        )
        .unwrap();
        hash
    };
    let swap_fs = |daemon: &mut Daemon, layer: String| {
        daemon.handle(Request::Swap(tilo_vm_protocol::Swap {
            slot: VM_SLOT.into(),
            component: "fs".into(),
            layer,
            info: Some(ComponentInfo::new(Scope::Vm, vec!["fs.service".into()])),
        }))
    };
    let vm_before = daemon.load_slot(VM_SLOT).unwrap().current;

    let refused = swap_fs(&mut daemon, fs_layer(0xf3, 3))
        .unwrap_err()
        .to_string();

    assert!(refused.contains("slot 'device'"), "{refused}");
    assert!(refused.contains("runtime 3"), "{refused}");
    assert_eq!(daemon.load_slot(VM_SLOT).unwrap().current, vm_before);
    swap_fs(&mut daemon, fs_layer(0xf2, 2)).expect("fs from the same tree goes in");
}

#[test]
fn vm_component_refresh_restarts_declared_live_owner_user_services() {
    let root = tempfile::tempdir().unwrap();
    let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut apply = RecordingApply::with_login("tester");
    apply.mirror = Some(mirror.clone());
    let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let device = daemon.load_slot(DEVICE_SLOT).unwrap();
    daemon.slots.save(&device).unwrap();
    let hash = "cd".repeat(32);
    fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
    let mut info = ComponentInfo::new(Scope::Vm, vec!["fs.service".into()]);
    info.user_units = vec!["tilo-fs-cloud.service".into()];
    let result: ApplyResult = serde_json::from_value(
        daemon
            .handle(Request::Swap(tilo_vm_protocol::Swap {
                slot: VM_SLOT.into(),
                component: "fs".into(),
                layer: hash,
                info: Some(info),
            }))
            .unwrap(),
    )
    .unwrap();
    let user_service = Action::RestartUserUnit {
        user: "tester".into(),
        unit: "tilo-fs-cloud.service".into(),
    };
    let desktop = Action::RestartDesktop {
        user: "tester".into(),
    };
    let user_ix = result
        .actions
        .iter()
        .position(|action| action == &user_service)
        .unwrap();
    let desktop_ix = result
        .actions
        .iter()
        .position(|action| action == &desktop)
        .unwrap();
    assert!(
        user_ix < desktop_ix,
        "owner services must observe the new binary before the desktop restarts"
    );
    let calls = mirror.lock().unwrap().clone();
    assert!(calls.iter().any(|call| call == "try-restart fs.service"));
    assert!(
        calls
            .iter()
            .any(|call| call == "restart_user_unit tester tilo-fs-cloud.service")
    );
    assert!(
        !calls.iter().any(|call| call == "restart user@1000.service"),
        "the owner login remains intact"
    );
}

#[test]
fn a_vm_component_outside_sessions_leaves_desktops_running() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::with_login("tester")),
        None,
        BaseInfo::default(),
    );
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let device = daemon.load_slot(DEVICE_SLOT).unwrap();
    daemon.slots.save(&device).unwrap();
    let mut swap = |component: &str, n: u8, outside_sessions: bool| -> ApplyResult {
        let hash = format!("{n:02x}").repeat(32);
        fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
        let info = ComponentInfo {
            outside_sessions,
            ..ComponentInfo::new(Scope::Vm, Vec::new())
        };
        serde_json::from_value(
            daemon
                .handle(Request::Swap(tilo_vm_protocol::Swap {
                    slot: VM_SLOT.into(),
                    component: component.into(),
                    layer: hash,
                    info: Some(info),
                }))
                .unwrap(),
        )
        .unwrap()
    };
    let restarted = Action::RestartDesktop {
        user: "tester".into(),
    };
    let vmd = swap("vmd", 1, true);
    assert_eq!(vmd.policy, Policy::VmRefresh);
    assert!(
        vmd.actions.contains(&Action::SysextRefresh),
        "{:?}",
        vmd.actions
    );
    assert!(!vmd.actions.contains(&restarted), "{:?}", vmd.actions);
    let fs = swap("fs", 2, false);
    assert!(fs.actions.contains(&restarted), "{:?}", fs.actions);
}

/// A laptop's `device` slot (the owner's greetd login) and the `vm` slot
/// merge one system-wide layer set: neither drops the other's layers, and
/// every change restarts the owner's desktop in place.
#[test]
fn a_laptops_device_slot_shares_the_system_wide_layers_with_the_vm_slot() {
    let root = tempfile::tempdir().unwrap();
    let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut apply = RecordingApply::with_login("tester");
    apply.mirror = Some(mirror.clone());
    let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let layer = |n: u8| {
        let hash = format!("{n:02x}").repeat(32);
        fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
        hash
    };
    let mut apply = |request: Request| -> ApplyResult {
        serde_json::from_value(daemon.handle(request).unwrap()).unwrap()
    };
    let swap = |slot: &str, component: &str, layer: &str, scope: Scope| {
        Request::Swap(tilo_vm_protocol::Swap {
            slot: slot.into(),
            component: component.into(),
            layer: layer.into(),
            info: Some(ComponentInfo::new(scope, Vec::new())),
        })
    };
    let restarted = Action::RestartDesktop {
        user: "tester".into(),
    };

    let shell = apply(swap(DEVICE_SLOT, "shell", &layer(1), Scope::Session));
    assert_eq!(shell.policy, Policy::SessionRestart);
    assert!(
        shell.actions.contains(&Action::SysextRefresh),
        "{:?}",
        shell.actions
    );
    assert_eq!(shell.actions.last(), Some(&restarted));
    let fs = apply(swap(VM_SLOT, "fs", &layer(2), Scope::Vm));
    assert!(fs.actions.contains(&restarted), "{:?}", fs.actions);
    let back = apply(Request::Rollback(tilo_vm_protocol::Rollback {
        slot: DEVICE_SLOT.into(),
        to: Some(0),
    }));
    assert_eq!(back.generation, 0);

    let calls = mirror.lock().unwrap().clone();
    let linked: Vec<&str> = calls
        .iter()
        .filter(|c| c.starts_with("link_sysext"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        linked,
        ["link_sysext 1", "link_sysext 2", "link_sysext 1"],
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("write_dropin") || c.starts_with("stop ")),
        "the owner's login is never stopped: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("restart_desktop"))
            .count(),
        3
    );
}

#[test]
fn creating_with_outputs_applies_the_layout_every_time() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(
        root.path(),
        Box::new(RecordingApply::default()),
        None,
        BaseInfo::default(),
    );
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let create = || {
        Request::SlotCreate(tilo_vm_protocol::SlotCreate {
            name: "a".into(),
            kind: SlotKind::Headful,
            owner: Owner::default(),
            outputs: tilo_vm_protocol::display::parse_outputs("2560x1600@2,1920x1080").unwrap(),
            identity: false,
        })
    };
    for _ in 0..2 {
        let result: SlotCreateResult =
            serde_json::from_value(daemon.handle(create()).unwrap()).unwrap();
        assert_eq!(result.slot.outputs.len(), 2);
        assert!(
            result.actions.contains(&Action::SetOutputs {
                outputs: "2560x1600@2,1920x1080@1".into()
            }),
            "{:?}",
            result.actions
        );
    }
}

use crate::apply::RecordingApply;

#[test]
fn slot_create_identity_mints_in_the_session_and_restarts_it_fresh() {
    use tilo_vm_protocol::ControlResult;
    let reply = |exit_code, stdout: &str, stderr: &str| ControlResult {
        exit_code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    };
    let mut apply = RecordingApply::default();
    apply.control_replies.extend([
        reply(2, "", "tilo-control: connect: No such file or directory"),
        reply(
            0,
            r#"{"answer":{"created":true,"display_name":"Throwaway test identity","throwaway":true}}"#,
            "",
        ),
        reply(
            0,
            r#"{"answer":{"created":false,"display_name":"Throwaway test identity","throwaway":true}}"#,
            "",
        ),
        reply(1, "", "error [identity_exists]: this user already has an identity"),
    ]);
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    let create = |identity| {
        Request::SlotCreate(tilo_vm_protocol::SlotCreate {
            name: "a".into(),
            kind: SlotKind::Headless,
            owner: Owner::default(),
            outputs: Vec::new(),
            identity,
        })
    };
    let session = session_unit("a");
    // Phase `a_new_throwaway_restarts_the_session_with_fresh_desktop_state`.
    {
        let result: SlotCreateResult =
            serde_json::from_value(daemon.handle(create(true)).unwrap()).unwrap();
        let identity = result.identity.expect("the identity answer");
        assert!(identity.created && identity.throwaway);
        let minted = result
            .actions
            .iter()
            .position(|action| action == &Action::ProvisionTestIdentity { created: true })
            .expect("the identity is minted");
        // After minting: the session stops, loses its layout, starts.
        let after: Vec<&Action> = result.actions[minted..]
            .iter()
            .filter(|action| {
                matches!(action, Action::StopUnit { unit } | Action::StartUnit { unit } if *unit == session)
                    || **action == Action::ResetDesktopState
            })
            .collect();
        assert_eq!(
            after.first(),
            Some(&&Action::StopUnit {
                unit: session.clone()
            }),
            "{:?}",
            result.actions
        );
        assert_eq!(after.get(1), Some(&&Action::ResetDesktopState));
        assert_eq!(
            after.last(),
            Some(&&Action::StartUnit {
                unit: session.clone()
            })
        );
    }
    // Phase `a_kept_throwaway_keeps_the_running_session`.
    {
        let result: SlotCreateResult =
            serde_json::from_value(daemon.handle(create(true)).unwrap()).unwrap();
        assert_eq!(
            result.identity.map(|identity| identity.created),
            Some(false)
        );
        assert!(!result.actions.contains(&Action::ResetDesktopState));
        assert!(!result.actions.contains(&Action::StopUnit { unit: session }));
    }
    // Phase `a_refused_identity_fails_the_create`.
    {
        let error = daemon.handle(create(true)).unwrap_err();
        assert!(matches!(error, DaemonError::TestIdentity { .. }), "{error}");
        assert!(error.to_string().contains("identity_exists"), "{error}");
    }
    // Phase `a_plain_create_leaves_identity_alone`.
    {
        let result: SlotCreateResult =
            serde_json::from_value(daemon.handle(create(false)).unwrap()).unwrap();
        assert_eq!(result.identity, None);
        assert!(result.actions.is_empty(), "{:?}", result.actions);
    }
}

#[test]
fn candidate_slot_view_preserves_identity_before_generation_commit() {
    let root = tempfile::tempdir().unwrap();
    let slots = SlotStore::new(root.path());
    let layers = LayerStore::new(root.path());
    let mut apply = RecordingApply::default();
    let (mut proof, _) = slots
        .create("proof", SlotKind::Headless, Owner::default(), &[])
        .unwrap();
    let own_hash = "03".repeat(32);
    let own = layers.path(&own_hash);
    fs::create_dir_all(own.join("usr")).unwrap();
    let (_, generation) = slots
        .prepare_swap(
            &proof,
            "shell",
            &own_hash,
            ComponentInfo::new(Scope::Session, vec![]),
        )
        .unwrap()
        .unwrap();
    slots.set_current(&mut proof, generation.number).unwrap();
    let candidate = layers.path(&"04".repeat(32));
    fs::create_dir_all(candidate.join("usr")).unwrap();
    let before = slots.load(VM_SLOT).unwrap().current;
    prepare_slot_session(
        &slots,
        &layers,
        &mut apply,
        &proof,
        Some(std::slice::from_ref(&candidate)),
    )
    .unwrap();
    assert_eq!(
        apply.prepared_session_layers,
        [("proof".into(), vec![candidate, own])]
    );
    assert_eq!(
        slots.load(VM_SLOT).unwrap().current,
        before,
        "candidate view preparation must not commit a generation before readiness"
    );
    for unit in ["user@60001.service", "tilo-vm-session@proof.service"] {
        assert!(apply.dropins[&(unit.into(), apply::LAYERS_DROPIN.into())].contains("Environment=\"TILO_IDENTITY_ROOT=/home/tds-proof/.local/share/tilo/test-identity\"\n"));
    }
}

#[test]
fn restored_slot_views_preserve_global_layers_and_refuse_missing_layers() {
    let root = tempfile::tempdir().unwrap();
    let slots = SlotStore::new(root.path());
    let layers = LayerStore::new(root.path());
    let locks = Locks::new(root.path());
    let mut apply = RecordingApply::with_login("owner");
    let mut expected = Vec::new();
    for (slot, n) in [(VM_SLOT, 1u8), (DEVICE_SLOT, 2), ("proof", 3)] {
        let mut record = if slot == VM_SLOT {
            slots.load(slot).unwrap()
        } else if slot == DEVICE_SLOT {
            slots.load_with(slot, apply.device.clone()).unwrap()
        } else {
            slots
                .create(slot, SlotKind::Headless, Owner::default(), &[])
                .unwrap()
                .0
        };
        slots.save(&record).unwrap();
        let hash = format!("{n:02x}").repeat(32);
        let path = layers.path(&hash);
        fs::create_dir_all(path.join("usr")).unwrap();
        expected.push(path);
        let (_, generation) = slots
            .prepare_swap(
                &record,
                "component",
                &hash,
                ComponentInfo::new(Scope::Session, Vec::new()),
            )
            .unwrap()
            .unwrap();
        slots.set_current(&mut record, generation.number).unwrap();
    }
    assert!(restore_slots(&slots, &layers, &locks, &mut apply).is_empty());
    assert_eq!(
        apply.prepared_session_layers,
        [("proof".into(), expected.clone())]
    );
    for unit in ["user@60001.service", "tilo-vm-session@proof.service"] {
        assert!(apply.dropins[&(unit.into(), apply::LAYERS_DROPIN.into())].contains(
            "Environment=\"TILO_IDENTITY_ROOT=/home/tds-proof/.local/share/tilo/test-identity\"\n"
        ));
    }
    let reload = apply
        .calls
        .iter()
        .position(|call| call == "reload")
        .unwrap();
    let start = apply
        .calls
        .iter()
        .position(|call| call == &format!("start {}", session_unit("proof")))
        .unwrap();
    assert!(reload < start);
    let held = locks.slot("proof").unwrap();
    let mut busy = RecordingApply::with_login("owner");
    let issues = restore_slots(&slots, &layers, &locks, &mut busy);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].slot, "proof");
    assert!(busy.prepared_session_layers.is_empty());
    assert!(!busy.calls.iter().any(|call| call.starts_with("start ")));
    drop(held);
    let held = locks.vm_exclusive().unwrap();
    let issues = restore_slots(&slots, &layers, &locks, &mut busy);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].slot, VM_SLOT);
    assert!(busy.prepared_session_layers.is_empty());
    drop(held);
    fs::remove_dir_all(&expected[2]).unwrap();
    let mut missing = RecordingApply::with_login("owner");
    let issues = restore_slots(&slots, &layers, &locks, &mut missing);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].slot, "proof");
    assert!(!missing.calls.iter().any(|call| call.starts_with("start ")));
}

#[test]
fn a_board_session_running_before_restore_restarts_onto_its_view() {
    let root = tempfile::tempdir().unwrap();
    let slots = SlotStore::new(root.path());
    let layers = LayerStore::new(root.path());
    let locks = Locks::new(root.path());
    let mut apply = RecordingApply::with_device("tester");
    let mut record = slots.load_with(DEVICE_SLOT, apply.device.clone()).unwrap();
    slots.save(&record).unwrap();
    let hash = "07".repeat(32);
    fs::create_dir_all(layers.path(&hash).join("usr")).unwrap();
    let (_, generation) = slots
        .prepare_swap(
            &record,
            "shell",
            &hash,
            ComponentInfo::new(Scope::Session, Vec::new()),
        )
        .unwrap()
        .unwrap();
    slots.set_current(&mut record, generation.number).unwrap();
    let unit = "tilo-board-headless-session@tester.service";
    // Enabled at boot, it started before the restore.
    apply.start_unit(unit).unwrap();
    let issues = restore_slots(&slots, &layers, &locks, &mut apply);
    assert!(issues.is_empty(), "{issues:?}");
    let after_reload = |call: &str| {
        let reload = apply.calls.iter().position(|c| c == "reload").unwrap();
        apply.calls.iter().skip(reload).any(|c| c == call)
    };
    assert!(after_reload(&format!("stop {unit}")));
    assert!(after_reload("restart user@1000.service"));
    assert!(after_reload(&format!("start {unit}")));
}

/// gc deletes only what nobody holds: it never touches the device
/// slot's current generation or a generation with a held layer, nor the
/// held layer itself, however few generations it is asked to keep.
#[test]
fn gc_never_touches_the_device_slots_current_or_held_generation() {
    let root = tempfile::tempdir().unwrap();
    let slots = SlotStore::new(root.path());
    let layers = LayerStore::new(root.path());
    let apply = RecordingApply::with_login("owner");
    let mut record = slots.load_with(DEVICE_SLOT, apply.device.clone()).unwrap();
    slots.save(&record).unwrap();
    let mut hashes = Vec::new();
    for n in 1..=4u8 {
        let hash = format!("{n:02x}").repeat(32);
        fs::create_dir_all(layers.path(&hash).join("usr")).unwrap();
        let (_, generation) = slots
            .prepare_swap(
                &record,
                &format!("component-{n}"),
                &hash,
                ComponentInfo::new(Scope::Session, Vec::new()),
            )
            .unwrap()
            .unwrap();
        slots.set_current(&mut record, generation.number).unwrap();
        hashes.push(hash);
    }
    // Back on generation 1 (component-1 only); generation 2 added the
    // held layer, generations 3 and 4 build on it.
    slots.set_current(&mut record, 1).unwrap();
    let held = serde_json::json!([{
        "layer": hashes[1], "component": "component-2", "reason": "speaks shell-ui 9"
    }]);
    fs::write(root.path().join("held-layers.json"), held.to_string()).unwrap();
    let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
    let result = daemon
        .gc(&tilo_vm_protocol::Gc {
            keep: 0,
            dry_run: false,
            keep_targets: None,
            free_goal_bytes: None,
            min_age_seconds: Some(0),
        })
        .unwrap();
    // Every generation from 2 on names the held layer: all are kept.
    assert_eq!(slots.generations(DEVICE_SLOT), vec![1, 2, 3, 4]);
    assert!(result.removed_generations.is_empty(), "{result:?}");
    for hash in &hashes {
        assert!(layers.require(hash).is_ok(), "{hash} kept");
    }
}

/// The device slot comes up on its current generation at every boot:
/// the restore links the `vm` and `device` slots' layers and refreshes
/// `/usr` when the links `systemd-sysext` merged at boot were not that
/// set, and leaves a set that already was merged alone.
#[test]
fn the_device_slot_re_merges_its_layers_at_boot() {
    let root = tempfile::tempdir().unwrap();
    let slots = SlotStore::new(root.path());
    let layers = LayerStore::new(root.path());
    let mut apply = RecordingApply::with_login("owner");
    let mut expected = Vec::new();
    for (slot, n) in [(VM_SLOT, 1u8), (DEVICE_SLOT, 2)] {
        let mut record = if slot == VM_SLOT {
            slots.load(slot).unwrap()
        } else {
            slots.load_with(slot, apply.device.clone()).unwrap()
        };
        slots.save(&record).unwrap();
        let hash = format!("{n:02x}").repeat(32);
        let path = layers.path(&hash);
        fs::create_dir_all(path.join("usr")).unwrap();
        expected.push(path);
        let (_, generation) = slots
            .prepare_swap(
                &record,
                "component",
                &hash,
                ComponentInfo::new(Scope::Session, Vec::new()),
            )
            .unwrap()
            .unwrap();
        slots.set_current(&mut record, generation.number).unwrap();
    }

    // The boot merged no links: they are put back and /usr refreshed.
    merge_system_layers(&slots, &layers, &mut apply).unwrap();
    assert_eq!(apply.sysext, expected);
    assert_eq!(apply.calls, ["link_sysext 2", "sysext_refresh"]);

    // Already merged: nothing is refreshed.
    apply.calls.clear();
    merge_system_layers(&slots, &layers, &mut apply).unwrap();
    assert_eq!(apply.calls, ["link_sysext 2"]);

    // The daemon's boot restore does the same.
    let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut watched = RecordingApply::with_login("owner");
    watched.mirror = Some(mirror.clone());
    let mut daemon = Daemon::new(root.path(), Box::new(watched), None, BaseInfo::default());
    assert!(daemon.restore(Some("composefs:one")).is_empty());
    let calls = mirror.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("link_sysext") || c.as_str() == "sysext_refresh")
            .collect::<Vec<_>>(),
        ["link_sysext 2", "sysext_refresh"],
        "a fresh boot's links are relinked and merged"
    );

    // A layer the device's generation names is gone: reported, links
    // left as they were.
    fs::remove_dir_all(&expected[1]).unwrap();
    apply.calls.clear();
    assert!(merge_system_layers(&slots, &layers, &mut apply).is_err());
    assert!(apply.calls.is_empty(), "{:?}", apply.calls);
}

#[test]
fn daemon_sessions_and_seats_are_restored_and_required_for_exec() {
    // Phase `exec_needs_a_user_slot_with_a_running_session`.
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(
            root.path(),
            Box::new(RecordingApply::default()),
            None,
            BaseInfo::default(),
        );
        SlotStore::new(root.path())
            .create("a", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        let run = |daemon: &mut Daemon, slot: &str| {
            daemon
                .exec(slot, &["true".into()], Duration::from_secs(5), 1024)
                .unwrap_err()
        };
        // The recording backend has no session process.
        let stopped = run(&mut daemon, "a");
        assert!(
            matches!(stopped, DaemonError::SessionStopped(_)),
            "{stopped}"
        );
        assert!(stopped.hint().contains("tilo-vm slot restart --slot a"));
        let vm = run(&mut daemon, VM_SLOT);
        assert!(vm.hint().contains("tilo-vm vm ssh"), "{}", vm.hint());
        assert!(matches!(
            run(&mut daemon, "nope"),
            DaemonError::SlotNotFound(_)
        ));
    }
    // Phase `headful_seats_are_recreated_at_start`.
    {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let locks = Locks::new(root.path());
        for (name, kind) in [
            ("a", SlotKind::Headful),
            ("b", SlotKind::Headless),
            ("c", SlotKind::Headful),
        ] {
            let (mut record, _) = slots.create(name, kind, Owner::default(), &[]).unwrap();
            if kind == SlotKind::Headful {
                record.seat = Some(format!("seat-{name}"));
                slots.save(&record).unwrap();
            }
        }
        let mut apply = RecordingApply::default();
        let busy = locks.slot("c").unwrap();
        assert!(reconcile_seats(&slots, &locks, &mut apply).is_empty());
        assert_eq!(apply.calls, ["allocate_seat a"]);
        drop(busy);
    }
    // Phase `restore_brings_back_seats_then_sessions`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(tmp.path());
        let locks = Locks::new(tmp.path());
        for (name, kind) in [("a", SlotKind::Headful), ("b", SlotKind::Headless)] {
            let (mut record, _) = slots.create(name, kind, Owner::default(), &[]).unwrap();
            if kind == SlotKind::Headful {
                record.seat = Some(format!("seat-{name}"));
                slots.save(&record).unwrap();
            }
        }
        let mut apply = RecordingApply::default();
        assert!(restore_slots(&slots, &LayerStore::new(tmp.path()), &locks, &mut apply).is_empty());
        let calls = &apply.calls;
        let seat = calls.iter().position(|c| c == "allocate_seat a").unwrap();
        let session = calls
            .iter()
            .position(|c| c == &format!("start {}", session_unit("a")))
            .unwrap();
        assert!(seat < session, "{calls:?}");
        assert!(
            calls.contains(&format!("start {}", session_unit("b"))),
            "{calls:?}"
        );
    }
    // Phase `installed_line_names_the_app`.
    {
        assert_eq!(
            parse_installed("tilo-install: WARN self-signed\ninstalled: so.tilo.notes 0.1.0\n"),
            (Some("so.tilo.notes".into()), Some("0.1.0".into()))
        );
        assert_eq!(parse_installed("nothing"), (None, None));
    }
}

#[test]
fn abi_checks_refuse_foreign_builds_and_compare_targets_and_locks() {
    // Phase `abi_refuses_foreign_arch_and_builder`.
    {
        let base = BaseInfo {
            image_digest: Some("sha256:base".into()),
            builder_image_digest: Some("sha256:builder".into()),
            toolchain: Some("1.97.1".into()),
            ..BaseInfo::default()
        };
        let mut layer = Provenance::local("shell");
        layer.target = Some("aarch64-unknown-linux-gnu".into());
        assert!(matches!(
            abi_check(&layer, &base, "x86_64"),
            Err(DaemonError::AbiMismatch { .. })
        ));
        layer.target = Some("x86_64-unknown-linux-gnu".into());
        assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
        layer.builder_image_digest = Some("sha256:other".into());
        assert!(matches!(
            abi_check(&layer, &base, "x86_64"),
            Err(DaemonError::AbiMismatch { .. })
        ));
        layer.builder_image_digest = None;
        layer.base_image_digest = Some("sha256:older".into());
        layer.toolchain = Some("1.96.0".into());
        assert_eq!(abi_check(&layer, &base, "x86_64").unwrap().len(), 2);
        // Nothing known about the VM: nothing to compare.
        assert!(
            abi_check(&layer, &BaseInfo::default(), "x86_64")
                .unwrap()
                .is_empty()
        );
    }
    // Phase `abi_compares_the_dev_images_target_and_cargo_lock`.
    {
        let base = dev_base();
        let mut layer = Provenance::local("shell");
        layer.target = Some("x86_64-unknown-linux-gnu".into());
        layer.cargo_lock_hash = base.cargo_lock_hash.clone();
        layer.toolchain = base.toolchain.clone();
        // A layer from some builder image, onto an image that is its own
        // builder: nothing to compare, so neither a refusal nor a warning.
        layer.builder_image_digest = Some("sha256:ci-builder".into());
        assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
        layer.builder = Some("host".into());
        assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());

        layer.target = Some("x86_64-unknown-linux-musl".into());
        let error = abi_check(&layer, &base, "x86_64").unwrap_err();
        assert_eq!(
            error.to_string(),
            "layer target is x86_64-unknown-linux-musl but the VM expects x86_64-unknown-linux-gnu"
        );
        layer.target = Some("x86_64-unknown-linux-gnu".into());

        layer.cargo_lock_hash = Some("fedcba9876543210fedc".into());
        // A statically linked component only warns.
        let warnings = abi_check(&layer, &base, "x86_64").unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("Cargo.lock"), "{warnings:?}");
        // Thin apps share libruntime_dylib.so with the image: refused.
        layer.cargo_profile = Some("thin-app".into());
        let error = abi_check(&layer, &base, "x86_64").unwrap_err();
        assert_eq!(
            error.to_string(),
            "layer Cargo.lock is fedcba987654 but the VM expects 0123456789ab"
        );
        let rpc = error.to_rpc();
        assert!(rpc.hint.contains("Cargo.lock"), "{}", rpc.hint);
        assert!(rpc.hint.contains("tilo-vm vm upgrade"), "{}", rpc.hint);
        assert_eq!(rpc.see.as_deref(), Some("tilo-vm guide swapping"));

        // A CI layer records no Cargo.lock: nothing to compare.
        layer.cargo_lock_hash = None;
        assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
    }
}

/// base.json as a dev image writes it: no builder image, but a target
/// triple and a Cargo.lock hash.
fn dev_base() -> BaseInfo {
    BaseInfo {
        image_digest: Some("sha256:base".into()),
        builder_image_digest: None,
        toolchain: Some("rustc 1.97.1".into()),
        target: Some("x86_64-unknown-linux-gnu".into()),
        cargo_lock_hash: Some("0123456789abcdef0123".into()),
    }
}

//! Actual request transitions: management-only material cannot disturb owner apps.
use super::*;
use crate::apply::RecordingApply;
use crate::slots::DeviceSession;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn layer(root: &Path, byte: u8) -> String {
    let hash = format!("{byte:02x}").repeat(32);
    let path = root.join("layers").join(&hash);
    fs::create_dir_all(path.join("usr/libexec")).unwrap();
    fs::create_dir_all(path.join("usr/bin")).unwrap();
    let binary = path.join("usr/libexec/tilo-vmd");
    fs::write(&binary, format!("management daemon {byte}")).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    symlink("../libexec/tilo-vmd", path.join("usr/bin/tilo-vmd")).unwrap();
    hash
}

fn daemon(root: &Path, apply: RecordingApply) -> Daemon {
    let mut daemon = Daemon::new(root, Box::new(apply), None, BaseInfo::default());
    daemon
        .handle(Request::Hello(tilo_vm_protocol::Hello {
            protocol: PROTOCOL_VERSION,
            client_version: String::new(),
        }))
        .unwrap();
    daemon
}

fn swap(daemon: &mut Daemon, hash: String, info: ComponentInfo) -> ApplyResult {
    answer(
        daemon,
        Request::Swap(tilo_vm_protocol::Swap {
            slot: VM_SLOT.into(),
            component: "vmd".into(),
            layer: hash,
            info: Some(info),
        }),
    )
}

fn answer(daemon: &mut Daemon, request: Request) -> ApplyResult {
    serde_json::from_value(daemon.handle(request).unwrap()).unwrap()
}

fn restarted(result: &ApplyResult) -> bool {
    result
        .actions
        .iter()
        .any(|action| matches!(action, Action::RestartDesktop { .. }))
}

#[test]
fn vmd_only_swap_preserves_the_live_owner_desktop() {
    let root = tempfile::tempdir().unwrap();
    let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut apply = RecordingApply::with_login("quickuser");
    apply.mirror = Some(mirror.clone());
    let mut daemon = daemon(root.path(), apply);
    let device = daemon.load_slot(DEVICE_SLOT).unwrap();
    daemon.slots.save(&device).unwrap();
    let before = device.session().unwrap().identity();
    // First addition and replacement both use the actual request activation.
    for byte in [0xbc, 0xbd] {
        let result = swap(
            &mut daemon,
            layer(root.path(), byte),
            ComponentInfo::new(Scope::Vm, Vec::new()),
        );
        assert!(result.changed);
        assert_eq!(result.policy, Policy::VmRefresh);
        assert!(result.actions.contains(&Action::SysextRefresh));
        assert!(!restarted(&result));
        assert_eq!(
            daemon.load_slot(VM_SLOT).unwrap().current,
            result.generation
        );
        assert_eq!(
            daemon
                .load_slot(DEVICE_SLOT)
                .unwrap()
                .session()
                .unwrap()
                .identity(),
            before
        );
        let calls = mirror.lock().unwrap();
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("restart_desktop")
                    || call == "restart user@1000.service")
        );
        drop(calls);
    }
    let explicit = answer(
        &mut daemon,
        Request::SlotRestart(tilo_vm_protocol::SlotRestart {
            name: VM_SLOT.into(),
        }),
    );
    assert!(
        restarted(&explicit),
        "an explicit VM refresh retains its normal contract"
    );
    let removal = answer(
        &mut daemon,
        Request::Rollback(tilo_vm_protocol::Rollback {
            slot: VM_SLOT.into(),
            to: Some(0),
        }),
    );
    assert!(removal.changed && removal.actions.contains(&Action::SysextRefresh));
    assert!(!restarted(&removal));
    assert_eq!(
        daemon
            .load_slot(DEVICE_SLOT)
            .unwrap()
            .session()
            .unwrap()
            .identity(),
        before
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unexpected {
    File,
    SystemUnit,
    UserUnit,
    Link,
    Mode,
}

#[test]
fn management_unknown_material_or_metadata_retains_normal_refresh() {
    for case in [
        Unexpected::File,
        Unexpected::SystemUnit,
        Unexpected::UserUnit,
        Unexpected::Link,
        Unexpected::Mode,
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = daemon(root.path(), RecordingApply::with_login("quickuser"));
        let device = daemon.load_slot(DEVICE_SLOT).unwrap();
        daemon.slots.save(&device).unwrap();
        let hash = layer(root.path(), 0xbe);
        let path = root.path().join("layers").join(&hash);
        let mut info = ComponentInfo::new(Scope::Vm, Vec::new());
        match case {
            Unexpected::File => fs::write(path.join("usr/bin/shell"), "owner component").unwrap(),
            Unexpected::SystemUnit => info.units.push("fs.service".into()),
            Unexpected::UserUnit => info.user_units.push("tilo-fs-cloud.service".into()),
            Unexpected::Link => {
                fs::remove_file(path.join("usr/bin/tilo-vmd")).unwrap();
                symlink("../libexec/shell", path.join("usr/bin/tilo-vmd")).unwrap();
            }
            Unexpected::Mode => fs::set_permissions(
                path.join("usr/libexec/tilo-vmd"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap(),
        }
        let result = swap(&mut daemon, hash, info);
        assert!(
            restarted(&result),
            "case {case:?} must retain conservative activation"
        );
        if case == Unexpected::SystemUnit {
            assert!(result.actions.contains(&Action::RestartUnit {
                unit: "fs.service".into()
            }));
        }
        if case == Unexpected::UserUnit {
            assert!(result.actions.contains(&Action::RestartUserUnit {
                user: "quickuser".into(),
                unit: "tilo-fs-cloud.service".into()
            }));
        }
    }
}

#[test]
fn management_refresh_keeps_logged_out_and_stand_in_sessions_untouched() {
    for logged_out in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut apply = if logged_out {
            RecordingApply::with_login("quickuser")
        } else {
            RecordingApply::default()
        };
        if logged_out {
            apply.device = apply.device.take().map(DeviceSession::logged_out);
        }
        let mut daemon = daemon(root.path(), apply);
        if logged_out {
            let device = daemon.load_slot(DEVICE_SLOT).unwrap();
            assert!(device.session().unwrap().logged_out());
            daemon.slots.save(&device).unwrap();
        } else {
            daemon
                .create("agent", SlotKind::Headless, Owner::default(), &[], false)
                .unwrap();
        }
        let result = swap(
            &mut daemon,
            layer(root.path(), 0xbf),
            ComponentInfo::new(Scope::Vm, Vec::new()),
        );
        assert!(result.actions.contains(&Action::SysextRefresh));
        assert!(!result.actions.iter().any(|action| matches!(
            action,
            Action::RestartDesktop { .. }
                | Action::RestartUnit { .. }
                | Action::StopUnit { .. }
                | Action::StartUnit { .. }
        )));
    }
}

#[test]
fn management_mixed_rollback_refreshes_other_changed_components() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = daemon(root.path(), RecordingApply::with_login("quickuser"));
    let device = daemon.load_slot(DEVICE_SLOT).unwrap();
    daemon.slots.save(&device).unwrap();
    swap(
        &mut daemon,
        layer(root.path(), 0xb1),
        ComponentInfo::new(Scope::Vm, Vec::new()),
    );
    let fs_layer = layer(root.path(), 0xb2);
    answer(
        &mut daemon,
        Request::Swap(tilo_vm_protocol::Swap {
            slot: VM_SLOT.into(),
            component: "fs".into(),
            layer: fs_layer,
            info: Some(ComponentInfo::new(Scope::Vm, vec!["fs.service".into()])),
        }),
    );
    let rollback = answer(
        &mut daemon,
        Request::Rollback(tilo_vm_protocol::Rollback {
            slot: VM_SLOT.into(),
            to: Some(0),
        }),
    );
    assert!(restarted(&rollback));
    assert!(rollback.actions.contains(&Action::RestartUnit {
        unit: "fs.service".into()
    }));
}

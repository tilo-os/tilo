use std::net::{Ipv4Addr, TcpListener};

use clap::Parser as _;

use super::*;
use crate::host::HostKind;
use crate::host::tests::{LIBVIRT, linux};

fn app(dir: &Path) -> App {
    App {
        json: false,
        target: None,
        worktree: None,
        registry_path: dir.join("vms.toml"),
        runner: SystemRunner { cwd: None },
        receipt: Receipt::new(dir, vec![]),
        warnings: Vec::new(),
        peer: None,
    }
}

fn args(dir: &Path) -> CreateArgs {
    let Command::Vm(VmCommand::Create(mut args)) =
        crate::cli::Cli::try_parse_from(["tilo-vm", "vm", "create", "new"])
            .unwrap()
            .command
            .unwrap()
    else {
        panic!("create command expected");
    };
    args.image = Some(dir.join("base.qcow2").display().to_string());
    args.disk = Some(dir.join("new/disk.qcow2").display().to_string());
    args
}

#[test]
fn preflight_refuses_an_occupied_explicit_port_before_creating_vm_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut args = args(dir.path());
    args.ssh_port = Some(port);
    // No image: a missing port preflight would try the remaining creation inputs.
    args.image = None;
    let error = app
        .preflight_create(&linux(HostKind::Dnf, LIBVIRT), &args, "x86_64")
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains(&format!("--ssh-port {port} is busy")),
        "{error}"
    );
    assert!(!app.registry_path.exists());
    assert!(!dir.path().join("new").exists());
    assert!(!dir.path().join("keys").exists());
}

#[test]
fn normal_preflight_skips_busy_default_and_registered_stopped_vm_ports() {
    use std::io::ErrorKind;

    let default_listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 2222)) {
        Ok(listener) => Some(listener),
        Err(error) if error.kind() == ErrorKind::AddrInUse => None,
        Err(error) => panic!("cannot establish occupied default port: {error}"),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    std::fs::write(
        &app.registry_path,
        "min_free_gb = 0\n[vms.stopped]\nhypervisor = 'qemu'\nssh_port = 2223\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("base.qcow2"), b"QFI\xfb\0\0\0\x03").unwrap();
    let plan = app
        .preflight_create(&linux(HostKind::Dnf, LIBVIRT), &args(dir.path()), "x86_64")
        .unwrap();
    let selected = plan.ssh_port.unwrap();
    assert!(
        selected >= 2224,
        "selected occupied or registered port {selected}"
    );
    assert_eq!(plan.choice.adapter, "libvirt");
    assert!(
        plan.describe()
            .contains(&format!("host TCP port {selected}"))
    );
    assert!(!dir.path().join("new").exists());
    assert!(!dir.path().join("keys").exists());
    drop(default_listener);
}

#[test]
fn create_an_existing_vm_is_a_no_op_even_when_its_port_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let entry = VmEntry {
        hypervisor: "libvirt".into(),
        ssh_port: Some(port),
        ..VmEntry::default()
    };
    let mut registry = Registry::default();
    registry.vms.insert("new".into(), entry);
    registry.save(&app.registry_path).unwrap();
    let before = std::fs::read(&app.registry_path).unwrap();
    let mut args = args(dir.path());
    args.ssh_port = Some(port);
    let outcome = app.vm_create(args).unwrap();
    assert_eq!(outcome.data["created"], false);
    assert_eq!(std::fs::read(&app.registry_path).unwrap(), before);
    assert!(!dir.path().join("keys").exists());
    assert!(!dir.path().join("new").exists());
}

//! A real hosted VM: a command wakes it, its output comes back, it is
//! snapshotted when idle, the next command restores it, and every second
//! it ran is metered. A host being taken back saves its running VM, and
//! another host restores it from the same state.
//!
//! It needs KVM and a guest, so it is ignored by default. Run it on a host
//! with `/dev/kvm` and Firecracker:
//!
//! ```text
//! TILO_HOSTED_FIRECRACKER=/path/firecracker \
//! TILO_HOSTED_KERNEL=/path/vmlinux \
//! TILO_HOSTED_INITRAMFS=/path/guest.cpio.gz \
//! TILO_LEGACY_BIN=/path/tilo-legacy \
//! hosted_boot --ignored --nocapture
//! ```
//!
//! The kernel needs virtio-mmio, vsock and FUSE built in; the initramfs
//! starts `tilo-legacy-agent` as root with an ordinary user `tilo`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "a test")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use legacy_agent_protocol::client::{self, LaunchIo};
use legacy_agent_protocol::link::AgentLink;
use legacy_agent_protocol::{AGENT_PORT, Display, Launch, Stdio};
use legacy_vm::hosted::control::{ShareRequest, VmRequest};
use legacy_vm::hosted::firecracker::Machine;
use legacy_vm::hosted::meter::{RunEnd, UsageKind};
use legacy_vm::hosted::service::{Service, ServiceConfig};
use legacy_vm::hosted::size::VmSize;
use legacy_vm::hosted::vm::{Settings, system_clock};

fn env_path(name: &str) -> PathBuf {
    let path = std::env::var_os(name).map(PathBuf::from);
    assert!(path.is_some(), "set {name}: see this file's docs");
    path.unwrap_or_default()
}

fn config(scratch: &std::path::Path, run: &str) -> ServiceConfig {
    ServiceConfig {
        state: scratch.join("state"),
        run: scratch.join(run),
        plans: scratch.join("plans.json"),
        settings: Settings {
            machine: Machine {
                firecracker: env_path("TILO_HOSTED_FIRECRACKER"),
                kernel: env_path("TILO_HOSTED_KERNEL"),
                initramfs: Some(env_path("TILO_HOSTED_INITRAMFS")),
                cmdline: Machine::DEFAULT_CMDLINE.into(),
                base_disk: None,
            },
            idle_suspend: Duration::from_secs(2),
            self_exe: std::env::var_os("TILO_LEGACY_BIN").map_or_else(
                || PathBuf::from(env!("CARGO_BIN_EXE_tilo-legacy")),
                PathBuf::from,
            ),
            jail: None,
            relay: None,
        },
    }
}

fn run(link: &AgentLink, script: &str, cwd: &str) -> (i32, String, String) {
    let stream = link.connect(AGENT_PORT).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = client::launch(
        stream,
        &Launch {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: Some(cwd.into()),
            env: Vec::new(),
            stdio: Stdio::Relay,
            display: Display::None,
        },
        LaunchIo {
            stdin: None,
            stdout: &mut out,
            stderr: &mut err,
            started: &mut |_| {},
        },
        |_| {},
    )
    .unwrap();
    (
        status.shell_code(),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

fn wait_for_phase(service: &Service, phase: &str, patience: Duration) -> Duration {
    let started = Instant::now();
    loop {
        service.tick();
        let status = service.status();
        if status.first().is_some_and(|vm| vm.phase == phase) {
            return started.elapsed();
        }
        assert!(started.elapsed() < patience, "never {phase}: {status:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "boots a VM: needs KVM, Firecracker, a guest kernel and initramfs"]
#[allow(clippy::too_many_lines, reason = "one scenario, phase by phase")]
fn a_command_wakes_the_vm_which_sleeps_when_idle_and_is_metered() {
    let scratch = tempfile::Builder::new().prefix("hb").tempdir().unwrap();
    let share = scratch.path().join("ws/files");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(share.join("in.txt"), "from the host\n").unwrap();
    let config = config(scratch.path(), "run");
    let service = Arc::new(Service::open(config.clone(), system_clock()).unwrap());
    let request = VmRequest {
        workspace: "ws1".into(),
        environment: "default".into(),
        account: "acct-1".into(),
        size: VmSize::Small,
    };
    let shares = [ShareRequest {
        path: share.clone(),
        read_only: false,
    }];
    let cwd = share.display().to_string();
    let wall = Instant::now();

    // phase: the first command boots the VM, sees the share and writes to it
    let (link, booted) = service.ensure(&request, &shares).unwrap();
    let booted = booted.expect("the first command boots the VM");
    eprintln!("cold boot to agent: {booted} ms");
    let (code, out, err) = run(
        &link,
        "uname -s; cat in.txt; echo built > out.txt; ls",
        &cwd,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.starts_with("Linux\nfrom the host\n"), "{out}");
    assert!(out.contains("out.txt"), "{out}");
    assert_eq!(
        std::fs::read_to_string(share.join("out.txt")).unwrap(),
        "built\n"
    );

    // phase: the next command finds it awake
    let (_, again) = service.ensure(&request, &shares).unwrap();
    assert_eq!(again, None, "an awake VM is not woken again");

    // phase: idle, it is snapshotted and its process ends
    let slept_after = wait_for_phase(&service, "Suspended", Duration::from_secs(60));
    let first_run_wall = wall.elapsed();
    eprintln!("suspended {slept_after:?} after the last command");
    let status = service.status();
    let snapshot_bytes = status.first().and_then(|vm| vm.snapshot_bytes).unwrap();
    eprintln!("snapshot on disk: {} MB", snapshot_bytes / 1_000_000);
    assert!(
        snapshot_bytes < VmSize::Small.memory_mib() * 1024 * 1024 / 2,
        "the snapshot holds what the guest touched, not its RAM"
    );

    // phase: the host changes a file while the VM sleeps; the next command restores it and sees the change
    std::fs::write(share.join("in.txt"), "changed while asleep\n").unwrap();
    let (link, restored) = service.ensure(&request, &shares).unwrap();
    let restored = restored.expect("a command restores the sleeping VM");
    eprintln!("restore to agent: {restored} ms");
    let (code, out, _) = run(&link, "cat in.txt out.txt", &cwd);
    assert_eq!(code, 0);
    assert_eq!(out, "changed while asleep\nbuilt\n");

    // phase: a second idle suspend merges only what changed
    wait_for_phase(&service, "Suspended", Duration::from_secs(60));
    let (link, restored_again) = service.ensure(&request, &shares).unwrap();
    eprintln!(
        "second restore to agent: {} ms",
        restored_again.expect("restored again")
    );
    assert_eq!(run(&link, "cat out.txt", &cwd).1, "built\n");

    // phase: stopping ends the run and drops the snapshot
    service.stop("ws1", "default").unwrap();
    let total_wall = wall.elapsed();

    // phase: every run and the held snapshot are in the account's ledger
    let records = config.ledger().records("acct-1").unwrap();
    for record in &records {
        eprintln!(
            "{:?} {:?} {} ms → {} s",
            record.kind, record.ended, record.millis, record.seconds
        );
    }
    let runs: Vec<_> = records
        .iter()
        .filter(|record| record.kind == UsageKind::Running)
        .collect();
    assert_eq!(
        runs.iter().map(|record| record.ended).collect::<Vec<_>>(),
        [
            Some(RunEnd::Suspended),
            Some(RunEnd::Suspended),
            Some(RunEnd::Stopped)
        ]
    );
    let millis: u64 = runs.iter().map(|record| record.millis).sum();
    let seconds: u64 = runs.iter().map(|record| record.seconds).sum();
    assert_eq!(
        seconds,
        millis / 1000,
        "billed seconds are the running time, carried across runs"
    );
    let first = runs.first().unwrap();
    assert!(
        u128::from(first.millis) <= first_run_wall.as_millis(),
        "the first run is not longer than the wall clock around it"
    );
    assert!(
        u128::from(millis) <= total_wall.as_millis(),
        "never more than the wall clock"
    );
    assert!(
        total_wall.as_millis().saturating_sub(u128::from(millis)) < 10_000,
        "the time it slept is the only time not billed"
    );
    assert!(
        records
            .iter()
            .any(|record| record.kind == UsageKind::Snapshot && record.snapshot_bytes.is_some()),
        "the held snapshot is counted"
    );
    assert!(
        records
            .iter()
            .all(|record| record.identity.account == "acct-1"
                && record.identity.vm == "ws1/default"
                && record.identity.size == VmSize::Small)
    );
}

#[test]
#[ignore = "needs KVM, Firecracker and a guest: see the file's docs"]
fn a_host_taken_back_saves_its_vm_and_the_next_host_restores_it() {
    let scratch = tempfile::Builder::new().prefix("he").tempdir().unwrap();
    let share = scratch.path().join("ws/files");
    std::fs::create_dir_all(&share).unwrap();
    let cwd = share.display().to_string();
    let shares = [ShareRequest {
        path: share,
        read_only: false,
    }];
    let request = VmRequest {
        workspace: "ws1".into(),
        environment: "default".into(),
        account: "acct-1".into(),
        size: VmSize::Small,
    };
    let uptime = |link: &AgentLink| -> f64 {
        let (code, out, err) = run(link, "cut -d' ' -f1 /proc/uptime", &cwd);
        assert_eq!(code, 0, "stderr: {err}");
        out.trim().parse().unwrap()
    };

    // phase: a running VM with something only its memory holds
    let first = Service::open(config(scratch.path(), "run-a"), system_clock()).unwrap();
    let (link, booted) = first.ensure(&request, &shares).unwrap();
    assert!(booted.is_some(), "the first command boots the VM");
    let (code, _, err) = run(&link, "echo kept > /dev/shm/only-in-memory", &cwd);
    assert_eq!(code, 0, "stderr: {err}");
    let before = uptime(&link);

    // phase: the notice comes; every VM is saved within it, and nothing more wakes
    let saving = Instant::now();
    first.evict();
    let saved_in = saving.elapsed();
    eprintln!("saved on the notice in {saved_in:?}");
    assert!(
        saved_in < Duration::from_secs(20),
        "well within Azure's 30 s"
    );
    assert_eq!(
        first.status().first().map(|vm| vm.phase.as_str()),
        Some("Suspended")
    );
    let refused = first.ensure(&request, &shares).map(|_| ()).unwrap_err();
    assert!(
        refused.to_string().contains("moving to another host"),
        "{refused}"
    );
    drop(first);

    // phase: another host, the same state: the next command restores the VM as it was
    let next = Service::open(config(scratch.path(), "run-b"), system_clock()).unwrap();
    let (link, restored) = next.ensure(&request, &shares).unwrap();
    eprintln!(
        "restored on the next host in {} ms",
        restored.expect("restored")
    );
    let (_, kept, _) = run(&link, "cat /dev/shm/only-in-memory", &cwd);
    assert_eq!(
        kept, "kept\n",
        "the guest's memory came back, not a fresh boot"
    );
    assert!(
        uptime(&link) >= before,
        "the guest kept running from where it was"
    );
    next.stop("ws1", "default").unwrap();
}

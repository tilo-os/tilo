//! A real hosted VM in its jail: it runs as its slot's own uid in its own
//! cgroup, fetches from the internet, is refused the cloud's metadata
//! endpoint and the host's network, gives an interactive program its keys
//! on a terminal, and leaves nothing behind when it stops.
//!
//! It needs KVM, Firecracker and its jailer, pasta, and the privileged
//! helper installed as a host installs it, so it is ignored by default. On
//! a host with `/dev/kvm`, as the service's uid:
//!
//! ```text
//! # root: the helper may become root, and only the service's group runs it
//! install -o root -g SERVICE -m 0750 tilo-hosted-jail /opt/jail/tilo-hosted-jail
//! setcap cap_setuid,cap_setgid+ep /opt/jail/tilo-hosted-jail
//! # root: a root-owned configuration in a root-owned directory
//! install -o root -m 0644 hosted-jail.json /opt/jail/conf/hosted-jail.json
//!
//! TILO_HOSTED_FIRECRACKER=… TILO_HOSTED_KERNEL=/guest/vmlinux \
//! TILO_HOSTED_ROOTFS=/guest/rootfs.ext4   # or TILO_HOSTED_INITRAMFS=…
//! TILO_HOSTED_JAIL_HELPER=/opt/jail/tilo-hosted-jail \
//! TILO_HOSTED_JAIL_CONFIG=/opt/jail/conf/hosted-jail.json \
//! TILO_HOSTED_HOST_ADDRESS=<an address of this host> \
//! TILO_LEGACY_BIN=… hosted_jailed_boot --ignored --nocapture
//! ```
//!
//! The guest is the published root image (`curl` fetches a crate over
//! HTTPS) or a busybox initramfs (`wget` fetches a package over HTTP).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test"
)]

use std::io::Read as _;
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use legacy_agent_protocol::client::{self, LaunchIo};
use legacy_agent_protocol::link::AgentLink;
use legacy_agent_protocol::{AGENT_PORT, Display, Launch, Stdio, TerminalSize};
use legacy_vm::hosted::control::VmRequest;
use legacy_vm::hosted::firecracker::Machine;
use legacy_vm::hosted::jail::{Helper, Jail};
use legacy_vm::hosted::service::{Service, ServiceConfig};
use legacy_vm::hosted::size::VmSize;
use legacy_vm::hosted::vm::{Settings, system_clock};

fn env_path(name: &str) -> PathBuf {
    let path = std::env::var_os(name).map(PathBuf::from);
    assert!(path.is_some(), "set {name}: see this file's docs");
    path.unwrap_or_default()
}

fn run(link: &AgentLink, script: &str) -> (i32, String, String) {
    let stream = link.connect(AGENT_PORT).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = client::launch(
        stream,
        &Launch {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: None,
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

/// The running Firecracker's pid and slot, from the pid file the service
/// keeps in the VM's run directory.
fn firecracker(run: &Path) -> (u32, u32) {
    for entry in std::fs::read_dir(run).unwrap().flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path().join("firecracker.pid")) {
            let mut words = text.split_whitespace().map(|word| word.parse().unwrap());
            return (
                words.next().unwrap(),
                words.next().expect("a jailed VM's slot"),
            );
        }
    }
    panic!("no running VM under {}", run.display());
}

/// The real uid and gid of `pid`.
fn ids(pid: u32) -> (u32, u32) {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let field = |name: &str| -> u32 {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .parse()
            .unwrap()
    };
    (field("Uid:"), field("Gid:"))
}

fn wait_for_phase(service: &Service, phase: &str) {
    let started = std::time::Instant::now();
    loop {
        service.tick();
        if service.status().first().is_some_and(|vm| vm.phase == phase) {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "never {phase}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether any process runs as `uid`.
fn runs_as(uid: u32) -> bool {
    std::fs::read_dir("/proc").unwrap().flatten().any(|entry| {
        entry.file_name().to_string_lossy().parse::<u32>().is_ok()
            && entry.metadata().is_ok_and(|meta| meta.uid() == uid)
    })
}

#[test]
#[ignore = "boots a jailed VM: needs KVM, Firecracker's jailer, pasta and the installed helper"]
#[allow(clippy::too_many_lines, reason = "one scenario, phase by phase")]
fn a_jailed_vm_reaches_the_internet_and_nothing_of_the_hosts() {
    let scratch = tempfile::Builder::new().prefix("hj").tempdir().unwrap();
    let helper = Helper::open(
        env_path("TILO_HOSTED_JAIL_HELPER"),
        env_path("TILO_HOSTED_JAIL_CONFIG"),
    )
    .unwrap();
    let jail_config = helper.config.clone();
    assert!(
        jail_config.network.is_some(),
        "the jails give VMs a network"
    );
    let rootfs = std::env::var_os("TILO_HOSTED_ROOTFS").map(PathBuf::from);
    let config = ServiceConfig {
        state: scratch.path().join("state"),
        run: scratch.path().join("run"),
        plans: scratch.path().join("plans.json"),
        settings: Settings {
            machine: Machine {
                firecracker: env_path("TILO_HOSTED_FIRECRACKER"),
                kernel: env_path("TILO_HOSTED_KERNEL"),
                initramfs: rootfs.is_none().then(|| env_path("TILO_HOSTED_INITRAMFS")),
                cmdline: Machine::DEFAULT_CMDLINE.into(),
                base_disk: rootfs,
            },
            idle_suspend: Duration::from_secs(3),
            self_exe: env_path("TILO_LEGACY_BIN"),
            jail: Some(Arc::new(Jail::new(helper))),
        },
    };
    let service = Arc::new(Service::open(config.clone(), system_clock()).unwrap());
    let request = VmRequest {
        workspace: "ws1".into(),
        environment: "default".into(),
        account: "acct-1".into(),
        size: VmSize::Small,
    };

    // phase: the first command boots the VM in a jail of its slot's own
    let (link, booted) = service.ensure(&request, &[]).unwrap();
    eprintln!("jailed cold boot to agent: {} ms", booted.unwrap());
    let (pid, slot) = firecracker(&config.run);
    let vm_uid = jail_config.vm_uid(slot);
    assert_eq!(
        ids(pid),
        (vm_uid, vm_uid),
        "Firecracker runs as its slot's uid"
    );
    assert_ne!(vm_uid, rustix::process::getuid().as_raw());
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        cgroup
            .trim_end()
            .ends_with(&format!("/{}/vm-{slot}", jail_config.cgroup)),
        "in its own cgroup: {cgroup}"
    );
    assert!(
        std::fs::read_link(format!("/proc/{pid}/root")).is_err(),
        "its root is not the service's to look into"
    );
    let (code, out, err) = run(&link, "uname -s; id -u");
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Linux\n"), "{out}");

    // phase: the VM fetches from the internet
    let fetch = "if command -v curl >/dev/null; then \
           curl -fsS --max-time 60 -o /tmp/fetched \
             https://static.crates.io/crates/itoa/itoa-1.0.11.crate; \
         else \
           wget -q -T 60 -O /tmp/fetched \
             http://deb.debian.org/debian/dists/trixie/main/binary-amd64/Packages.xz; \
         fi && wc -c < /tmp/fetched";
    let (code, out, err) = run(&link, fetch);
    assert_eq!(code, 0, "the VM reaches the internet: {err}");
    let bytes: u64 = out.trim().parse().unwrap();
    assert!(bytes > 1000, "fetched {bytes} bytes");
    eprintln!("fetched {bytes} bytes from the internet");

    // phase: the metadata endpoint, the host and the gateway are refused
    let host = std::env::var("TILO_HOSTED_HOST_ADDRESS").expect("TILO_HOSTED_HOST_ADDRESS");
    let listener = TcpListener::bind(("0.0.0.0", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    for url in [
        "http://169.254.169.254/latest/meta-data/".to_string(),
        "http://168.63.129.16/machine?comp=goalstate".to_string(),
        format!("http://{host}:{port}/"),
        format!("http://10.0.2.2:{port}/"),
    ] {
        let script = format!(
            "if command -v curl >/dev/null; then curl -s --max-time 5 -o /dev/null {url}; \
             else wget -q -T 5 -O /dev/null {url}; fi"
        );
        let (code, _, _) = run(&link, &script);
        assert_ne!(code, 0, "{url} must be unreachable from the VM");
    }
    assert!(
        listener.accept().is_err(),
        "nothing from the VM reached the host's listener"
    );

    // phase: an interactive program gets its keys on a terminal
    let stream = link.connect(AGENT_PORT).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let typed = Arc::new(AtomicBool::new(false));
    let typing = Arc::clone(&typed);
    let status = client::launch(
        stream,
        &Launch {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "tty; stty size; printf 'name? '; read name; echo \"hi $name\"".into(),
            ],
            cwd: None,
            env: Vec::new(),
            stdio: Stdio::Terminal(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            display: Display::None,
        },
        LaunchIo {
            stdin: None,
            stdout: &mut out,
            stderr: &mut err,
            started: &mut |_| {},
        },
        move |signals| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                signals.input(b"agent\r").unwrap();
                typing.store(true, Ordering::SeqCst);
            });
        },
    )
    .unwrap();
    let out = String::from_utf8_lossy(&out);
    assert_eq!(status.shell_code(), 0, "{out}");
    assert!(typed.load(Ordering::SeqCst));
    assert!(out.contains("/dev/pts/"), "on a terminal: {out}");
    assert!(out.contains("40 120"), "of the session's size: {out}");
    assert!(out.contains("hi agent"), "the keys arrived: {out}");

    // phase: idle, it is snapshotted and its slot released; restored, it is jailed and online again
    wait_for_phase(&service, "Suspended");
    assert!(!runs_as(vm_uid), "a sleeping VM holds no process");
    assert!(!jail_config.jail_dir(slot).exists(), "nor its jail");
    let (link, restored) = service.ensure(&request, &[]).unwrap();
    eprintln!(
        "jailed restore to agent: {} ms",
        restored.expect("restored")
    );
    let (pid, slot) = firecracker(&config.run);
    let vm_uid = jail_config.vm_uid(slot);
    assert_eq!(ids(pid).0, vm_uid, "the restored VM is jailed too");
    let (code, out, err) = run(&link, "wc -c < /tmp/fetched");
    assert_eq!((code, out.trim()), (0, bytes.to_string().as_str()), "{err}");
    let (code, _, err) = run(&link, fetch);
    assert_eq!(code, 0, "the restored VM reaches the internet: {err}");

    // phase: stopped, the slot's uids run nothing and its files are the service's again
    service.stop("ws1", "default").unwrap();
    assert!(!runs_as(vm_uid), "nothing runs as the VM's uid");
    assert!(
        !runs_as(jail_config.net_uid(slot)),
        "nothing runs as its network's uid"
    );
    assert!(!jail_config.jail_dir(slot).exists(), "the jail is gone");
    let disk = config.state.join("vms/ws1/default/vmm/disk.raw");
    assert_eq!(
        std::fs::metadata(&disk).unwrap().uid(),
        rustix::process::getuid().as_raw(),
        "the VM's disk is the service's again"
    );
    let mut leftover = String::new();
    let _ = std::fs::File::open(
        jail_config
            .cgroup_dir()
            .join(format!("vm-{slot}/cgroup.procs")),
    )
    .map(|mut procs| procs.read_to_string(&mut leftover));
    assert!(leftover.is_empty(), "its cgroup is gone");
}

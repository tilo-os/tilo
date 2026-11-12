//! A hosted workspace's shell on a host that runs real VMs: a command that
//! needs a Linux program runs in the workspace's jailed VM, metered, and an
//! account with no card on file is refused it, typed, in the words an agent
//! is given.
//!
//! It boots VMs, so it is ignored by default. On a host with `/dev/kvm`,
//! with the hosted-VM service's machine and jail set up as for
//! `legacy-vm`'s `hosted_jailed_boot` (see its docs; the jail is used when
//! `TILO_HOSTED_JAIL_HELPER` is set):
//!
//! ```text
//! TILO_HOSTED_FIRECRACKER=… TILO_HOSTED_KERNEL=… TILO_HOSTED_INITRAMFS=… \
//! TILO_LEGACY_BIN=… it workspace_shell_boot --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::firecracker::Machine;
use legacy_vm::hosted::jail::{Helper, Jail};
use legacy_vm::hosted::plan::{Plan, Plans, Sells};
use legacy_vm::hosted::service::{Service, ServiceConfig};
use legacy_vm::hosted::size::VmSize;
use legacy_vm::hosted::vm::{Settings, system_clock};
use tilo_actions::remote::host_account::{NextStepKind, Reason};
use ush::hosted_vm::HostedTarget;
use ush::workspace_shell::WorkspaceShells;

fn env_path(name: &str) -> PathBuf {
    let path = std::env::var_os(name).map(PathBuf::from);
    assert!(path.is_some(), "set {name}: see this file's docs");
    path.unwrap_or_default()
}

fn text(output: &[u8]) -> String {
    String::from_utf8_lossy(output).into_owned()
}

struct Down(Arc<Service>);

impl Drop for Down {
    fn drop(&mut self) {
        let _ = self.0.stop("ws-1", "default");
    }
}

#[test]
#[ignore = "boots a VM: needs KVM, Firecracker and a guest"]
#[allow(clippy::too_many_lines, reason = "one scenario, phase by phase")]
fn a_workspace_shell_runs_programs_in_its_vm_and_refuses_an_account_without_a_card() {
    let scratch = tempfile::Builder::new().prefix("ws").tempdir().unwrap();
    // A host that sells its VMs: the account with a card is named, the
    // other is not.
    let plans = scratch.path().join("plans.json");
    let sold = Plans {
        accounts: [("acct-card".to_owned(), Plan::default())].into(),
        sells: Some(Sells {
            host: "Tilo Test".into(),
            plans: Vec::new(),
            add_payment_method: "https://billing.example/card".into(),
            manage_billing: None,
            raise_limit: None,
            browser_free_actions: None,
        }),
        ..Plans::default()
    };
    std::fs::write(&plans, serde_json::to_vec(&sold).unwrap()).unwrap();
    let jail = std::env::var_os("TILO_HOSTED_JAIL_HELPER").map(|helper| {
        Arc::new(Jail::new(
            Helper::open(PathBuf::from(helper), env_path("TILO_HOSTED_JAIL_CONFIG")).unwrap(),
        ))
    });
    let config = ServiceConfig {
        state: scratch.path().join("state"),
        run: scratch.path().join("run"),
        plans: plans.clone(),
        settings: Settings {
            machine: Machine {
                firecracker: env_path("TILO_HOSTED_FIRECRACKER"),
                kernel: env_path("TILO_HOSTED_KERNEL"),
                initramfs: Some(env_path("TILO_HOSTED_INITRAMFS")),
                cmdline: Machine::DEFAULT_CMDLINE.into(),
                base_disk: None,
            },
            idle_suspend: Duration::from_secs(2),
            self_exe: env_path("TILO_LEGACY_BIN"),
            jail,
            relay: None,
            templates: None,
        },
    };
    let socket = config.socket();
    std::fs::create_dir_all(&config.run).unwrap();
    let service = Arc::new(Service::open(config, system_clock()).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    // A failed assertion still shuts the VM down: nothing outlives the test.
    let _down = Down(Arc::clone(&service));
    let serving = {
        let (service, stop) = (Arc::clone(&service), Arc::clone(&stop));
        std::thread::spawn(move || service.serve(&stop, None))
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the service never listened"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let target = |account: &str| HostedTarget {
        socket: socket.clone(),
        account: account.into(),
        size: VmSize::Small,
        plans: Some(plans.clone()),
    };

    // With a card: ush's own commands run on the host, the Linux program in
    // the workspace's VM, its output back in the run.
    let shells = WorkspaceShells::new(scratch.path().join("shells"), Some(target("acct-card")));
    let ran = shells
        .run(
            "ws-1",
            "s1",
            "echo on the host && uname -s && sh -c 'echo in the vm'",
            true,
        )
        .unwrap();
    eprintln!("{}", text(&ran.output));
    assert_eq!(ran.exit, 0, "{}", text(&ran.output));
    assert_eq!(text(&ran.output), "on the host\nLinux\nin the vm\n");
    assert!(ran.refusal.is_none());

    // A build in the VM writes into the workspace's files, which the host
    // sees: the VM works on the same files as ush.
    let built = shells
        .run(
            "ws-1",
            "s1",
            "mkdir -p out && sh -c 'echo built > out/artifact' && cat out/artifact",
            true,
        )
        .unwrap();
    assert_eq!(built.exit, 0, "{}", text(&built.output));
    assert!(
        text(&built.output).ends_with("\nbuilt\n"),
        "{}",
        text(&built.output)
    );
    assert_eq!(
        std::fs::read_to_string(shells.files("ws-1").join("out/artifact")).unwrap(),
        "built\n"
    );

    // The VM's time is metered for the account: its run reaches the ledger
    // when the service suspends the VM, once it is idle.
    let usage = || {
        let reply = control::ask(
            &socket,
            &Request::Usage {
                account: "acct-card".into(),
                from_ms: 0,
                to_ms: u64::MAX,
            },
            Duration::from_secs(5),
        )
        .unwrap();
        match reply {
            Some(Reply::Usage { records, .. }) if !records.is_empty() => Some(records),
            _ => None,
        }
    };
    let metered = (0..60).find_map(|_| {
        std::thread::sleep(Duration::from_millis(500));
        usage()
    });
    let records = metered.expect("the VM's run was metered");
    assert!(
        records
            .iter()
            .all(|record| record.identity.account == "acct-card"),
        "{records:?}"
    );
    eprintln!("metered: {records:?}");

    // Without a card: the agent is refused at once, typed, and told what to
    // ask of the person; nothing ran.
    let shells = WorkspaceShells::new(scratch.path().join("shells-2"), Some(target("acct-none")));
    let ran = shells.run("ws-2", "s1", "uname -s", true).unwrap();
    assert_eq!(ran.exit, 126);
    let refusal = ran.refusal.expect("a typed refusal");
    assert_eq!(refusal.reason, Reason::NoPaymentMethod);
    assert_eq!(
        refusal
            .step(NextStepKind::AddPaymentMethod)
            .and_then(|step| step.url.as_deref()),
        Some("https://billing.example/card")
    );
    let said = text(&ran.output);
    assert!(said.contains("Ask the person to add a card"), "{said}");

    stop.store(true, Ordering::SeqCst);
    serving.join().unwrap().unwrap();
}

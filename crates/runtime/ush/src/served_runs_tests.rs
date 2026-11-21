//! A program served through a workspace's machine: the hosted-VM service admits
//! it, the guest's agent launches the real `tilo-linux-vm-agent serve-run`, and
//! the program's frames cross the launch's relayed streams both ways.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use linux_vm_protocol::link::AgentLink;
use linux_vm_protocol::{AGENT_PORT, GuestMessage, HostMessage, client};

use super::wire::{FromRun, ToRun};
use super::*;
use crate::hosted_vm::HostedTarget;
use crate::hosted_vm::testing::{fake_service, fake_vm};

/// Says where it started and with what, then repeats what it is told.
const REPEATS: &str = r#"#!/bin/bash
echo "started in $PWD with $GREETING"
while IFS= read -r line; do echo "heard $line"; done
echo "input ended"
"#;

/// What an agent program from before it served programs does when it is started
/// as one: it tries to be the agent, and ends.
const OLD_AGENT: &str = r"#!/bin/sh
echo 'tilo-linux-vm-agent: listen on vsock port 7110: Address already in use' >&2
exit 1
";

/// A host's shells whose workspaces' machines are a stand-in for the
/// hosted-VM service, in front of the real guest agent started in this process.
fn shells(dir: &Path) -> (WorkspaceShells, PathBuf) {
    let vm = fake_vm(dir);
    let target = HostedTarget {
        socket: fake_service(dir, vm.clone()),
        account: "acct-1".into(),
        size: linux_vm::hosted::size::VmSize::default(),
        plans: None,
    };
    (WorkspaceShells::new(dir.join("state"), Some(target)), vm)
}

fn serving(program: &Path, agent_program: &Path) -> ServeRun {
    ServeRun {
        argv: vec![program.display().to_string()],
        env: vec![("GREETING".into(), "hello".into())],
        directory: PathBuf::new(),
        endpoints: Vec::new(),
        stop_grace: Duration::from_secs(20),
        agent_program: agent_program.display().to_string(),
        secrets: Vec::new(),
    }
}

/// How many programs the machine's agent counts as running: what keeps the
/// machine awake.
fn running(vm: &Path) -> u32 {
    let link = AgentLink::Hybrid {
        socket: vm.to_path_buf(),
    };
    let mut stream = link.connect(AGENT_PORT).unwrap();
    match client::request(&mut stream, &HostMessage::Health).unwrap() {
        GuestMessage::Health(health) => health.running,
        other => panic!("expected the machine's health, got {other:?}"),
    }
}

fn frame(run: &ServedRun) -> FromRun {
    match tilo_test_env::wait_for("the run to say something", || {
        run.hear(Duration::from_millis(100))
    }) {
        Heard::Frame(frame) => frame,
        Heard::Over(over) => panic!("the run was over: {over:?}"),
    }
}

/// What the run's program prints next.
fn printed(run: &ServedRun) -> String {
    match frame(run) {
        FromRun::Out(bytes) => String::from_utf8(bytes).unwrap(),
        other => panic!("expected the program to print, got {other:?}"),
    }
}

fn over(run: &ServedRun) -> Over {
    tilo_test_env::wait_for("the run to be over", || {
        match run.hear(Duration::from_millis(100)) {
            Some(Heard::Over(over)) => Some(over),
            _ => None,
        }
    })
}

tilo_test_env::scenario! {
    fn a_run_in_a_workspaces_machine_is_held_by_its_host_for_as_long_as_it_lasts;

    phase its_frames_cross_the_machine_both_ways_and_the_machine_counts_it_as_running;
    {
        let dir = tempfile::tempdir().unwrap();
        let (shells, vm) = shells(dir.path());
        let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
        let agent = tilo_test_env::sibling_binary("linux-vm-agent", "tilo-linux-vm-agent");
        let run = shells.serve_run("ws1", None, serving(program.path(), &agent)).unwrap();
        assert_eq!(
            frame(&run),
            FromRun::Ready { version: wire::VERSION, endpoints: Vec::new() }
        );
        assert!(matches!(frame(&run), FromRun::Started { .. }));
        assert_eq!(
            printed(&run),
            format!("started in {} with hello\n", shells.files("ws1").display()),
            "it starts in the workspace's files, with the variables it was given"
        );
        assert!(run.tell(&ToRun::In(b"one\n".to_vec())));
        assert_eq!(printed(&run), "heard one\n");
        // A program that lives is a program the machine counts: it is not put to sleep under it,
        // not after the thirty seconds an idle machine is given and not after the six hours a
        // command used to be followed for. The machine's own lifecycle decides, on its own clock.
        assert_eq!(running(&vm), 1);
        let mut life = linux_vm::lifecycle::Lifecycle::new(30);
        life.wake().unwrap();
        life.agent_ready();
        for seconds in [0, 31, 6 * 60 * 60 + 1, 3 * 24 * 60 * 60] {
            assert_eq!(
                life.activity(running(&vm), seconds),
                None,
                "a machine with a run in it stays awake at {seconds} s"
            );
        }
        assert!(run.tell(&ToRun::In(b"two\n".to_vec())));
        assert_eq!(printed(&run), "heard two\n");
        assert!(run.tell(&ToRun::CloseIn));
        assert_eq!(printed(&run), "input ended\n");
        assert!(matches!(frame(&run), FromRun::Exited(status) if status.code == Some(0)));
        let over = over(&run);
        assert_eq!((over.exit, over.refusal), (0, None));
        assert_eq!(running(&vm), 0, "and once it ended the machine may sleep");
        let ended = 3 * 24 * 60 * 60 + 60;
        assert_eq!(life.activity(running(&vm), ended), None, "its idle time begins");
        assert!(
            life.activity(running(&vm), ended + 30).is_some(),
            "thirty seconds after the run ended the machine is put to sleep"
        );
        assert!(!run.tell(&ToRun::In(b"late\n".to_vec())), "a run that is over is told nothing");
    }

    phase its_secrets_reach_the_program_in_its_variables_over_any_of_the_same_name;
    {
        let dir = tempfile::tempdir().unwrap();
        let (shells, _vm) = shells(dir.path());
        let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
        let agent = tilo_test_env::sibling_binary("linux-vm-agent", "tilo-linux-vm-agent");
        let mut start = serving(program.path(), &agent);
        start.secrets = vec![VmSecret {
            variable: "GREETING".into(),
            value: "the-vaults-own".into(),
        }];
        let served = shells.serve_run("ws1", None, start).unwrap();
        assert!(matches!(frame(&served), FromRun::Ready { .. }));
        assert!(matches!(frame(&served), FromRun::Started { .. }));
        assert_eq!(
            printed(&served),
            format!("started in {} with the-vaults-own\n", shells.files("ws1").display()),
        );
        served.leave();
        over(&served);
    }

    phase a_host_that_lets_go_of_a_run_ends_its_program;
    {
        let dir = tempfile::tempdir().unwrap();
        let (shells, vm) = shells(dir.path());
        let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
        let agent = tilo_test_env::sibling_binary("linux-vm-agent", "tilo-linux-vm-agent");
        let run = shells.serve_run("ws1", None, serving(program.path(), &agent)).unwrap();
        assert!(matches!(frame(&run), FromRun::Ready { .. }));
        assert!(matches!(frame(&run), FromRun::Started { .. }));
        printed(&run);
        run.leave();
        assert_eq!(over(&run).exit, 143, "its program was asked to end, and did");
        assert_eq!(running(&vm), 0);
    }

    phase a_machine_whose_agent_serves_no_runs_ends_in_its_own_words;
    {
        let dir = tempfile::tempdir().unwrap();
        let (shells, _vm) = shells(dir.path());
        let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
        let old = tilo_test_env::script(dir.path().join("old-agent"), OLD_AGENT).unwrap();
        let run = shells.serve_run("ws1", None, serving(program.path(), old.path())).unwrap();
        let over = over(&run);
        assert_eq!(over.exit, 1);
        assert!(over.words.contains("listen on vsock port 7110"), "{}", over.words);
    }

    phase a_host_without_machines_and_a_folder_outside_the_files_are_refused;
    {
        let dir = tempfile::tempdir().unwrap();
        let none = WorkspaceShells::new(dir.path().join("state"), None);
        let nowhere = Path::new("/nonexistent");
        let refused = none.serve_run("ws1", None, serving(nowhere, nowhere)).err().unwrap();
        assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
        let (shells, _vm) = shells(dir.path());
        let mut outside = serving(nowhere, nowhere);
        outside.directory = PathBuf::from("../ws2/files");
        let refused = shells.serve_run("ws1", None, outside).err().unwrap();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    }
}

tilo_test_env::scenario! {
    fn a_run_whose_handle_is_dropped_is_ended_and_leaves_the_machine_idle;

    phase its_program_is_ended_without_being_asked;
    {
        let dir = tempfile::tempdir().unwrap();
        let (shells, vm) = shells(dir.path());
        let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
        let agent = tilo_test_env::sibling_binary("linux-vm-agent", "tilo-linux-vm-agent");
        let run = shells.serve_run("ws1", None, serving(program.path(), &agent)).unwrap();
        assert!(matches!(frame(&run), FromRun::Ready { .. }));
        let FromRun::Started { pid } = frame(&run) else {
            panic!("the program starts");
        };
        printed(&run);
        assert_eq!(running(&vm), 1);
        // Nobody holds the run any more: it has no host, so it does not go on.
        drop(run);
        tilo_test_env::exited("the program of a run whose handle was dropped", pid);
        tilo_test_env::wait_until("the machine to count nothing as running", || {
            running(&vm) == 0
        });
    }
}

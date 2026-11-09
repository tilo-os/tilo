use super::*;
use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::size::VmSize;
use tilo_actions::remote::host_account::{NextStep, NextStepKind, Reason};

fn text(run: &ShellRun) -> String {
    String::from_utf8_lossy(&run.output).into_owned()
}

#[test]
fn the_shells_own_commands_run_on_the_host_and_a_session_keeps_its_place() {
    let state = tempfile::tempdir().unwrap();
    let shells = WorkspaceShells::new(state.path(), None);
    let files = shells.files("ws-1");

    let echoed = shells.run("ws-1", "s1", "echo hello", true).unwrap();
    assert_eq!((echoed.exit, text(&echoed).as_str()), (0, "hello\n"));
    assert_eq!(echoed.cwd, files.to_string_lossy());
    assert!(echoed.refusal.is_none() && !echoed.truncated);

    // A redirect is the environment's write, on the workspace's directory.
    let wrote = shells
        .run(
            "ws-1",
            "s1",
            "mkdir src && echo 'fn main() {}' > src/main.rs",
            true,
        )
        .unwrap();
    assert_eq!(wrote.exit, 0, "{}", text(&wrote));
    assert_eq!(
        std::fs::read_to_string(files.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );

    let listed = shells.run("ws-1", "s1", "cd src && ls", true).unwrap();
    assert_eq!(listed.exit, 0, "{}", text(&listed));
    assert!(text(&listed).contains("main.rs"), "{}", text(&listed));
    // The session stays where the last command left it; another starts at
    // the workspace's directory.
    let here = shells.run("ws-1", "s1", "pwd", true).unwrap();
    assert_eq!(text(&here).trim(), files.join("src").to_string_lossy());
    let other = shells.run("ws-1", "s2", "pwd", true).unwrap();
    assert_eq!(text(&other).trim(), files.to_string_lossy());

    // Nothing outside the workspace's directory is the shell's to read.
    let outside = shells.run("ws-1", "s1", "cat /etc/hostname", true).unwrap();
    assert_ne!(outside.exit, 0);
}

#[test]
fn a_program_needs_a_vm_the_host_does_not_run() {
    let state = tempfile::tempdir().unwrap();
    let shells = WorkspaceShells::new(state.path(), None);
    let ran = shells.run("ws-1", "s1", "cargo --version", true).unwrap();
    assert_ne!(ran.exit, 0);
    assert!(ran.refusal.is_none());
    assert!(!text(&ran).is_empty());
}

#[test]
fn a_vm_the_account_may_not_have_is_refused_to_the_agent_typed() {
    let state = tempfile::tempdir().unwrap();
    let socket = state.path().join("hosted.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let refusal = Refusal {
        reason: Reason::NoPaymentMethod,
        message: "Linux programs run in a VM, and this account has no card on file.".into(),
        next: vec![NextStep {
            kind: NextStepKind::AddPaymentMethod,
            url: Some("https://tilo.sh/billing".into()),
        }],
    };
    let answered = refusal.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request: Request = control::FRAMING.read_value(&mut stream).unwrap().unwrap();
        let Request::Ensure { vm, shares } = request else {
            panic!("an ensure");
        };
        assert_eq!(
            (vm.workspace.as_str(), vm.account.as_str()),
            ("ws-1", "acct-1")
        );
        assert_eq!(shares.len(), 1);
        control::FRAMING
            .write_value(&mut stream, &Reply::Refused(answered))
            .unwrap();
    });
    let shells = WorkspaceShells::new(
        state.path().join("shells"),
        Some(HostedTarget {
            socket,
            account: "acct-1".into(),
            size: VmSize::default(),
            plans: None,
        }),
    );
    let ran = shells.run("ws-1", "s1", "cargo --version", true).unwrap();
    server.join().unwrap();
    assert_eq!(ran.exit, 126);
    assert_eq!(ran.refusal, Some(refusal));
    let said = text(&ran);
    assert!(
        said.contains("no card on file") && said.contains("Ask the person to add a card"),
        "{said}"
    );
}

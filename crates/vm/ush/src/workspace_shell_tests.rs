use super::*;
use linux_vm::hosted::control::{self, Reply, Request};
use linux_vm::hosted::size::VmSize;
use tilo_actions::remote::server_account::{NextStep, NextStepKind, Reason};

fn text(run: &ShellRun) -> String {
    String::from_utf8_lossy(&run.output).into_owned()
}

tilo_test_env::scenario! {
    /// A hosted workspace's shell on the host.
    fn a_workspace_shell_runs_its_own_commands_and_asks_for_a_vm_for_the_rest;

    phase the_shells_own_commands_run_on_the_host_and_a_session_keeps_its_place;
    {
        let state = tempfile::tempdir().unwrap();
        let shells = WorkspaceShells::new(state.path(), None);
        let files = shells.files("ws-1");

        let echoed = shells.run("ws-1", "s1", "echo hello", true).unwrap();
        assert_eq!((echoed.exit, text(&echoed).as_str()), (0, "hello\n"));
        assert_eq!(echoed.cwd, files.to_string_lossy());
        assert!(echoed.refusal.is_none() && !echoed.truncated);

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
        let here = shells.run("ws-1", "s1", "pwd", true).unwrap();
        assert_eq!(text(&here).trim(), files.join("src").to_string_lossy());
        let other = shells.run("ws-1", "s2", "pwd", true).unwrap();
        assert_eq!(text(&other).trim(), files.to_string_lossy());

        let outside = shells.run("ws-1", "s1", "cat /etc/hostname", true).unwrap();
        assert_ne!(outside.exit, 0);
    }

    phase a_program_needs_a_vm_the_host_does_not_run;
    {
        let state = tempfile::tempdir().unwrap();
        let shells = WorkspaceShells::new(state.path(), None);
        let ran = shells.run("ws-1", "s1", "cargo --version", true).unwrap();
        assert_ne!(ran.exit, 0);
        assert!(ran.refusal.is_none());
        assert!(!text(&ran).is_empty());
    }

    phase a_vm_the_account_may_not_have_is_refused_to_the_agent_typed;
    {
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
}

#[test]
fn a_server_on_its_own_machine_starts_programs_in_the_workspace_files() {
    let state = tempfile::tempdir().unwrap();
    let shells = WorkspaceShells::new(state.path(), None).on_machine(true);
    let ended = shells
        .run("w1", "s1", "sh -c 'echo made > here.txt; pwd'", true)
        .unwrap();
    assert_eq!(ended.exit, 0, "{}", String::from_utf8_lossy(&ended.output));
    let files = shells.files("w1");
    assert_eq!(
        std::fs::read_to_string(files.join("here.txt")).unwrap(),
        "made\n"
    );
    assert!(String::from_utf8_lossy(&ended.output).contains(&*files.to_string_lossy()));
}

#[test]
fn a_shell_on_its_own_machine_reaches_its_files_outside_the_workspace() {
    let state = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("brief.md"), "the plan\n").unwrap();
    let place = elsewhere.path().to_string_lossy().into_owned();
    let on_machine = WorkspaceShells::new(state.path(), None).on_machine(true);
    let read = on_machine
        .run("w1", "s1", &format!("cd {place} && cat brief.md"), true)
        .unwrap();
    assert_eq!(read.exit, 0, "{}", text(&read));
    assert!(text(&read).contains("the plan"), "{}", text(&read));
    // The directory carries over to the next command, whose program starts there.
    let here = on_machine.run("w1", "s1", "sh -c pwd", true).unwrap();
    assert!(text(&here).contains(&place), "{}", text(&here));

    let confined = WorkspaceShells::new(state.path(), None);
    let refused = confined
        .run("w2", "s1", &format!("cd {place} && cat brief.md"), true)
        .unwrap();
    assert_ne!(refused.exit, 0, "{}", text(&refused));
    assert!(!text(&refused).contains("the plan"), "{}", text(&refused));
}

#[test]
fn a_server_without_programs_starts_none() {
    let state = tempfile::tempdir().unwrap();
    let shells = WorkspaceShells::new(state.path(), None);
    let ended = shells
        .run("w1", "s1", "sh -c 'echo no > no'", true)
        .unwrap();
    assert_ne!(ended.exit, 0);
    assert!(!shells.files("w1").join("no").exists());
}

/// A run's secrets reach the programs it starts in the workspace's VM, and nothing else: not
/// the shell's own variables, not the run's answer, not the session's next run.
#[test]
fn a_runs_secrets_reach_its_programs_in_the_vm_and_nothing_else() {
    use crate::hosted_vm::testing::{fake_service, fake_vm};
    const VALUE: &str = "sk-ant-oat01-the-persons-own-token"; // placeholder, not a credential
    let state = tempfile::tempdir().unwrap();
    let socket = fake_service(state.path(), fake_vm(state.path()));
    let shells = WorkspaceShells::new(
        state.path().join("shells"),
        Some(HostedTarget {
            socket,
            account: "acct-1".into(),
            size: VmSize::default(),
            plans: None,
        }),
    );
    let secret = VmSecret {
        variable: "GITHUB_TOKEN".into(),
        value: VALUE.into(),
    };
    let with = RunWith {
        secrets: std::slice::from_ref(&secret),
        ..RunWith::default()
    };
    let given = shells
        .run_for(
            "ws-1",
            "s1",
            &format!("GITHUB_TOKEN=typed sh -c 'test \"$GITHUB_TOKEN\" = {VALUE} && echo given'"),
            true,
            with,
        )
        .unwrap();
    assert_eq!(given.exit, 0, "{}", text(&given));
    assert_eq!(text(&given), "given\n");

    let shell_sees = shells
        .run_for("ws-1", "s1", "echo \"[$GITHUB_TOKEN]\"", true, with)
        .unwrap();
    assert_eq!(text(&shell_sees), "[]\n", "the shell itself never holds it");

    let next = shells
        .run("ws-1", "s1", "sh -c 'echo \"[$GITHUB_TOKEN]\"'", true)
        .unwrap();
    assert_eq!(next.exit, 0, "{}", text(&next));
    assert_eq!(
        text(&next),
        "[]\n",
        "a run without it starts its programs without it"
    );

    // The first command names the value only to compare it; what each run printed never holds it.
    for ran in [&given, &shell_sees, &next] {
        assert!(!text(ran).contains(VALUE), "{}", text(ran));
    }
}

tilo_test_env::scenario! {
    /// A workspace's shell on a host that runs VMs, with the real guest agent standing in for the
    /// machine's.
    fn a_commands_programs_commit_as_the_actor_who_asked_for_it;

    phase the_identity_rides_the_vms_launch_and_the_shell_keeps_none;
    {
        let (shells, state) = hosted(VmSize::default());
        let ran = shells
            .run_for(
                "ws-1",
                "s1",
                "sh -c 'echo \"$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL>\"'",
                true,
                with(Some(&ada())),
            )
            .unwrap();
        assert_eq!(ran.exit, 0, "{}", text(&ran));
        assert_eq!(text(&ran).trim(), "Ada Lovelace <ada@eu1.tilo.sh>");
        // The committer signs as the author, and the shell carries no identity into the next
        // command line: whoever runs one next is not recorded as Ada.
        let next = shells
            .run_for(
                "ws-1",
                "s1",
                "sh -c 'echo \"${GIT_AUTHOR_NAME-unset} ${GIT_COMMITTER_NAME-unset} \
                 ${GIT_AUTHOR_EMAIL-unset} ${GIT_COMMITTER_EMAIL-unset}\"'",
                true,
                with(None),
            )
            .unwrap();
        assert_eq!(text(&next).trim(), "unset unset unset unset");
        drop(state);
    }

    phase two_actors_asking_in_one_shell_each_commit_as_themselves;
    {
        let (shells, state) = hosted(VmSize::default());
        let who = "sh -c 'echo \"$GIT_AUTHOR_NAME <$GIT_COMMITTER_EMAIL>\"'";
        let first = shells
            .run_for("ws-1", "s1", who, true, with(Some(&ada())))
            .unwrap();
        assert_eq!(text(&first).trim(), "Ada Lovelace <ada@eu1.tilo.sh>");
        let scout = Person {
            name: "Scout".into(),
            address: "scout@eu1.tilo.sh".into(),
        };
        let second = shells
            .run_for("ws-1", "s1", who, true, with(Some(&scout)))
            .unwrap();
        assert_eq!(text(&second).trim(), "Scout <scout@eu1.tilo.sh>");
        // The actor who asked first is still themselves when they ask again.
        let again = shells
            .run_for("ws-1", "s1", who, true, with(Some(&ada())))
            .unwrap();
        assert_eq!(text(&again).trim(), "Ada Lovelace <ada@eu1.tilo.sh>");
        drop(state);
    }

    phase an_actor_who_set_their_own_identity_keeps_it_after_the_run;
    {
        let (shells, state) = hosted(VmSize::default());
        let set = shells
            .run_for(
                "ws-1",
                "s1",
                "export GIT_AUTHOR_NAME=mine",
                true,
                with(Some(&ada())),
            )
            .unwrap();
        assert_eq!(set.exit, 0, "{}", text(&set));
        let kept = shells
            .run_for(
                "ws-1",
                "s1",
                "sh -c 'echo \"$GIT_AUTHOR_NAME\"'",
                true,
                with(None),
            )
            .unwrap();
        assert_eq!(text(&kept).trim(), "mine");
        drop(state);
    }
}

/// Ada, as the workspace names her and where it is reached.
fn ada() -> Person {
    Person {
        name: "Ada Lovelace".into(),
        address: "ada@eu1.tilo.sh".into(),
    }
}

/// A hosted workspace's shells in front of the real guest agent, run in this process as the
/// test's own user, and the directory that keeps both.
fn hosted(size: VmSize) -> (WorkspaceShells, tempfile::TempDir) {
    let state = tempfile::tempdir().unwrap();
    let vm = crate::hosted_vm::testing::fake_vm(state.path());
    let target = HostedTarget {
        socket: crate::hosted_vm::testing::fake_service(state.path(), vm),
        account: "acct-1".into(),
        size,
        plans: None,
    };
    (
        WorkspaceShells::new(state.path().join("shells"), Some(target)),
        state,
    )
}

/// One command line asked of the workspace's shell as `person` (nobody, when `None`).
fn with(person: Option<&Person>) -> RunWith<'_> {
    RunWith {
        person,
        ..RunWith::default()
    }
}

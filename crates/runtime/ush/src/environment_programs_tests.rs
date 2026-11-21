//! A served program through the member that takes a workspace's work: the member's own
//! dispatcher, the workspace's machine a stand-in for the hosted-VM service in front of the
//! real guest agent, and the real agent program launched in it in its serving mode.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a test"
)]

use std::path::Path;
use std::time::Duration;

use tilo_actions::remote::environment_programs::{
    self, Frames, ProgramEnded, ServeProgram, TellProgram,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::{FromProgram, ToProgram, framed, unframed};
use crate::environment_commands::EnvironmentCommands;
use crate::hosted_vm::testing::{fake_service, fake_vm};
use crate::workspace_shell::WorkspaceShells;

const OPERATOR: [u8; 32] = [7; 32];

/// Says where it started, then repeats what it is told.
const REPEATS: &str = r#"#!/bin/bash
echo "started in $PWD with $GREETING"
while IFS= read -r line; do echo "heard $line"; done
echo "input ended"
"#;

/// A member whose environment names no account: every program is paid for by the account
/// its server names, in the machine the member's own hosted-VM service gives it.
fn member(dir: &Path) -> EnvironmentCommands {
    let socket = fake_service(dir, fake_vm(dir));
    let files = dir.join("replicas");
    EnvironmentCommands::new(
        WorkspaceShells::new(dir.join("shells"), None)
            .with_files(move |workspace| files.join(workspace))
            .with_browsers_on(Some(socket)),
        OPERATOR,
    )
}

/// The workspace's server: a device of the member's own operator, asking as `workspace`.
fn server(workspace: &str) -> Caller {
    Caller::authenticated(OPERATOR, [1; 32]).in_workspace(workspace)
}

fn answered<T>(pending: tilo_actions::remote::Pending<T>) -> T {
    match pending.wait_timeout(Duration::from_secs(5)) {
        Ok(Some(value)) => value,
        _ => panic!("no answer"),
    }
}

fn asking(target: RemoteActionTarget, asked: &impl serde::Serialize) -> InvokeRequest {
    let serde_json::Value::Object(map) = serde_json::to_value(asked).unwrap() else {
        panic!("what a member is asked is an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(map),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A start of `program`, launched by the agent program at `agent`.
fn serving(program: &Path, agent: &Path) -> InvokeRequest {
    asking(
        environment_programs::serve_target(),
        &ServeProgram {
            argv: vec![program.display().to_string()],
            env: vec![("GREETING".into(), "hello".into())],
            directory: String::new(),
            endpoints: Vec::new(),
            stop_grace_ms: 20_000,
            agent_program: agent.display().to_string(),
            account: "acct-1".into(),
            size: "small".into(),
            secrets: Vec::new(),
        },
    )
}

/// `lines` for the program's input, the first numbered `first`, with `taken` of its own
/// frames said to be read.
fn telling(program: InvocationId, first: u64, lines: &[&str], taken: u64) -> InvokeRequest {
    asking(
        environment_programs::tell_target(),
        &TellProgram {
            program,
            first,
            frames: lines
                .iter()
                .map(|line| framed(&ToProgram::In(line.as_bytes().to_vec())).unwrap())
                .collect(),
            taken,
        },
    )
}

/// What a server keeps of a program it follows: how many frames it took, and what the
/// program printed in them.
#[derive(Default)]
struct Followed {
    taken: u64,
    printed: String,
    ended: Option<ProgramEnded>,
}

impl Followed {
    fn take(&mut self, frames: &Frames) {
        for (number, text) in (frames.first..).zip(&frames.frames) {
            if number == self.taken {
                if let Some(FromProgram::Out(bytes)) = unframed::<FromProgram>(text) {
                    self.printed.push_str(&String::from_utf8_lossy(&bytes));
                }
                self.taken += 1;
            }
        }
    }

    /// Look at the program once, as its server does.
    fn look(&mut self, member: &EnvironmentCommands, caller: &Caller, id: InvocationId) {
        if let Some(change) = member.next_change(caller, &id) {
            answered(change);
        }
        match member.observe(caller, &id).unwrap().update {
            InvocationUpdate::Item { value, .. } => {
                self.take(&serde_json::from_value(value).unwrap());
            }
            InvocationUpdate::Succeeded { output, .. } => {
                let ended: ProgramEnded = serde_json::from_value(output.value.unwrap()).unwrap();
                self.take(&ended.last);
                self.ended = Some(ended);
            }
            InvocationUpdate::Accepted { .. } | InvocationUpdate::Progress { .. } => {}
            other => panic!("the program was not held: {other:?}"),
        }
    }

    /// Look until the program printed `words`. One that ended after printing them is as good as
    /// one still running: the words are what the test waits for, and a program kept only while
    /// its server asks ends by itself as soon as that is up.
    fn until_printed(
        &mut self,
        member: &EnvironmentCommands,
        caller: &Caller,
        id: InvocationId,
        words: &str,
    ) {
        tilo_test_env::wait_for("the program to print", || {
            self.look(member, caller, id);
            let printed = self.printed.contains(words);
            assert!(
                printed || self.ended.is_none(),
                "it ended before printing {words:?}: {:?}",
                self.ended
            );
            printed.then_some(())
        });
    }

    /// Look until the program is over.
    fn until_over(
        &mut self,
        member: &EnvironmentCommands,
        caller: &Caller,
        id: InvocationId,
    ) -> ProgramEnded {
        tilo_test_env::wait_for("the program to be over", || {
            self.look(member, caller, id);
            self.ended.clone()
        })
    }
}

#[test]
fn a_program_is_held_for_its_workspaces_server_and_told_each_frame_once() {
    let dir = tempfile::tempdir().unwrap();
    let member = member(dir.path());
    let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
    let agent = tilo_test_env::sibling_binary("linux-vm-agent", "tilo-linux-vm-agent");
    let ours = server("ws-1");
    let start = serving(program.path(), &agent);
    let id = start.invocation;
    answered(member.invoke(&ours, start)).unwrap();

    // What it sends is kept until its server says it took it: nothing is lost between looks.
    let mut followed = Followed::default();
    followed.until_printed(&member, &ours, id, "with hello\n");
    assert!(
        followed.printed.starts_with("started in "),
        "{}",
        followed.printed
    );

    // What its server sends is handed to it in order, and a frame sent twice once.
    answered(member.invoke(&ours, telling(id, 0, &["one\n"], followed.taken))).unwrap();
    followed.until_printed(&member, &ours, id, "heard one\n");
    answered(member.invoke(&ours, telling(id, 0, &["one\n", "two\n"], followed.taken))).unwrap();
    followed.until_printed(&member, &ours, id, "heard two\n");
    assert_eq!(
        followed.printed.matches("heard one\n").count(),
        1,
        "{}",
        followed.printed
    );
    let gap = answered(member.invoke(&ours, telling(id, 9, &["late\n"], followed.taken)));
    assert!(
        matches!(gap, Err(ActionCallError::InvalidRequest { .. })),
        "frames whose predecessors never came are not handed on: {gap:?}"
    );

    // Nobody who asks as another workspace tells it, sees it or ends it, and a device of
    // another person starts and tells nothing here.
    let theirs = server("ws-2");
    let told = answered(member.invoke(&theirs, telling(id, 2, &["stranger\n"], 0)));
    assert!(
        matches!(told, Err(ActionCallError::UnknownInvocation)),
        "{told:?}"
    );
    assert!(member.observe(&theirs, &id).is_none());
    assert!(matches!(
        answered(member.cancel(&theirs, &id)),
        Err(ActionCallError::UnknownInvocation)
    ));
    let stranger = Caller::authenticated([9; 32], [1; 32]).in_workspace("ws-1");
    for request in [
        serving(program.path(), &agent),
        telling(id, 2, &["stranger\n"], 0),
    ] {
        let refused = answered(member.invoke(&stranger, request));
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{refused:?}"
        );
    }

    // Its server's cancel ends it at once, and nothing a stranger sent ever reached it.
    answered(member.invoke(&ours, telling(id, 2, &["three\n"], followed.taken))).unwrap();
    followed.until_printed(&member, &ours, id, "heard three\n");
    answered(member.cancel(&ours, &id)).unwrap();
    followed.until_over(&member, &ours, id);
    assert!(
        !followed.printed.contains("stranger"),
        "{}",
        followed.printed
    );
    let late = answered(member.invoke(&ours, telling(id, 3, &["after\n"], followed.taken)));
    assert!(
        late.is_ok(),
        "a program that is over takes nothing: {late:?}"
    );
}

#[test]
fn a_program_whose_server_no_longer_asks_after_it_is_ended() {
    let dir = tempfile::tempdir().unwrap();
    let member = member(dir.path()).leaving_programs_after(Duration::from_millis(500));
    let program = tilo_test_env::script(dir.path().join("repeats"), REPEATS).unwrap();
    let agent = tilo_test_env::sibling_binary("linux-vm-agent", "tilo-linux-vm-agent");
    let ours = server("ws-1");
    let start = serving(program.path(), &agent);
    let id = start.invocation;
    answered(member.invoke(&ours, start)).unwrap();
    let mut followed = Followed::default();
    followed.until_printed(&member, &ours, id, "with hello\n");

    // Nobody looks at it for longer than it is kept: the member ends it by itself, though
    // nobody asked it to and its input never ended.
    std::thread::sleep(Duration::from_secs(3));
    followed.until_over(&member, &ours, id);
}

#[test]
fn a_member_without_machines_says_so_as_the_programs_end() {
    let dir = tempfile::tempdir().unwrap();
    let member = EnvironmentCommands::new(
        WorkspaceShells::new(dir.path().join("shells"), None),
        OPERATOR,
    );
    let ours = server("ws-1");
    let start = serving(Path::new("/bin/true"), Path::new("/bin/true"));
    let id = start.invocation;
    answered(member.invoke(&ours, start)).unwrap();
    let failed = tilo_test_env::wait_for("the start to be answered", || {
        match member.observe(&ours, &id).unwrap().update {
            InvocationUpdate::Failed { error, .. } => Some(error),
            _ => None,
        }
    });
    assert!(failed.message.contains("no Linux machines"), "{failed:?}");
}

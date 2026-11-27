//! A secret of the workspace's vault reaching the programs its Linux machine starts, and nothing
//! else: the person keeps a token in the vault, names it in the workspace's machine secrets under
//! a variable, and the next command their agent starts in the machine hands its program it. A
//! program typed at a terminal in the machine is asked about as the agent it is, once the person
//! allows it.

use std::fmt::Write as _;

use serde_json::json;

use crate::language::agent_fixture::{act, open_terminal_allowed};
use crate::language::host;

/// What the person keeps in the vault: nothing but a program in the machine may read it.
const TOKEN: &str = "placeholder-token-for-a-program-in-the-machine";
const VARIABLE: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// The kind of the records naming an item for the machine, as Terminal keeps them.
const MACHINE_SECRET: &str = "tl.so.tilo.terminal.MachineSecret";

/// What the workspace's records naming items for its machine hold, each as text: what the
/// machine reads when a program starts.
async fn named_for_the_machine(host: &host::Host) -> String {
    host.daemon()
        .workspaces()
        .store_found(
            host.workspace(),
            collab_types::store::Query::of(MACHINE_SECRET, 16),
        )
        .await
        .expect("the workspace's records read")
        .into_iter()
        .fold(String::new(), |mut named, (_, held)| {
            let _ = write!(named, "{held:?}");
            named
        })
}

/// The person keeps the token in their vault from a device of theirs, as their page does, and
/// answers with its reference.
async fn keep(host: &host::Host) -> String {
    let workspace = host.workspace().to_owned();
    let laptop = host.member("Ada's laptop", &workspace).await;
    let kept = laptop
        .call(
            tilo_workspace_records::vault::PROVIDER,
            tilo_actions::ActionAddress::new(
                tilo_workspace_records::vault::RESOURCE,
                tilo_workspace_records::vault::ADD,
            ),
            &json!({"kind": "key", "belongsTo": ["https://api.anthropic.com"], "secret": TOKEN}),
        )
        .await
        .expect("the vault keeps the token");
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = kept else {
        panic!("the person's vault keeps the token: {kept:?}");
    };
    output.value.unwrap_or_default()["reference"]
        .as_str()
        .expect("a reference")
        .to_owned()
}

/// The person's agent runs a command in the workspace's machine: its program starts with the
/// token the person named for the machine, and the answer, its arguments and the machine secrets
/// document never carries it.
#[test]
fn a_vault_item_named_for_the_machine_reaches_its_programs_and_nothing_else() {
    host::run_with(&["terminal"], |host| async move {
        let reference = keep(host.as_ref()).await;
        let agent = host.agent();
        // Before anything is named, the machine's programs get nothing.
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "claude --version"}),
        )
        .await;
        assert!(
            !failed && ran.contains("ran claude --version"),
            "the daemon starts the workspace's machine for a program: {ran}"
        );

        // The person names the item for the machine, and the next program gets it.
        let (named, failed) = act(
            &agent,
            "terminal#add_secret",
            json!({"variable": VARIABLE, "item": reference}),
        )
        .await;
        assert!(
            !failed,
            "the person names the item for the machine: {named}"
        );
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "claude --version"}),
        )
        .await;
        assert!(
            !failed && ran.contains("ran claude --version"),
            "the next program runs with it: {ran}"
        );
        assert!(!ran.contains(TOKEN), "the answer never carries it: {ran}");
        // What the machine reads when a program starts names the item, never its secret.
        let document = named_for_the_machine(host.as_ref()).await;
        assert!(
            document.contains(&reference),
            "the document the machine reads names the item: {document}"
        );
        assert!(
            !document.contains(TOKEN),
            "the document names the item, never its secret: {document}"
        );

        // Removed, it no longer arrives with the next command.
        let (removed, failed) = act(
            &agent,
            "terminal#remove_secret",
            json!({"variable": VARIABLE}),
        )
        .await;
        assert!(!failed, "the person removes the item: {removed}");
        assert!(
            !named_for_the_machine(host.as_ref())
                .await
                .contains(&reference),
            "nothing names the item for the machine any more"
        );
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "claude --version"}),
        )
        .await;
        assert!(!failed, "the command runs without it: {ran}");
    });
}

/// A program in the workspace's machine works there as its own agent once the person allows it:
/// asked about the agent it would be, allowed, its work is kept as that agent's.
#[test]
fn a_program_in_the_workspaces_machine_works_there_as_its_own_agent_once_allowed() {
    host::run_with(&["terminal"], |host| async move {
        // The workspace's machine is what takes the person's Terminal command and answers the
        // program that connects there; the daemon serves the machine and the questions its
        // programs are put to the person.
        let agent = host.agent();
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "echo hello from the machine"}),
        )
        .await;
        assert!(
            !failed && ran.contains("hello from the machine"),
            "a program runs in the workspace's machine: {ran}"
        );
        // A terminal a program types into reaches beyond the workspace's records: its person is
        // asked first, and once they allow it the program connects at the machine's socket.
        open_terminal_allowed(
            host.as_ref(),
            &agent,
            "3f6b8c0e-0000-4000-8000-00000000000a",
            json!({"cols": 100, "rows": 30}),
        )
        .await;
    });
}

/// A member of somebody else's, entered in the workspace as an editor, asks this person's vault
/// over the device link: the daemon refuses them, saying what is not theirs, rather than
/// pretending the vault is not there for anyone.
#[test]
fn another_members_device_is_refused_this_persons_vault() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let (theirs, _) = host.other_person("Grace's laptop", &workspace).await;
        let refused = theirs
            .call(
                tilo_workspace_records::vault::PROVIDER,
                tilo_actions::ActionAddress::new(
                    tilo_workspace_records::vault::RESOURCE,
                    tilo_workspace_records::vault::ADD,
                ),
                &json!({"kind": "key", "belongsTo": ["https://api.anthropic.com"], "secret": TOKEN}),
            )
            .await
            .expect_err("another person's device reaches no item of this person's vault");
        assert!(
            !matches!(
                refused,
                tilo_actions::remote::ActionCallError::UnknownAction
            ),
            "the refusal says what is theirs to ask, not that the vault is not there: {refused:?}"
        );
    });
}

/// An agent this person's own root certified asks their vault over the device link: the vault
/// holds the agent to its own rules, which grant an agent nothing by itself.
#[test]
fn an_agent_on_the_persons_device_is_held_to_the_vaults_own_rules() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = agent_member(host.as_ref(), &workspace, "Ada's agent").await;
        let refused = agent
            .call(
                tilo_workspace_records::vault::PROVIDER,
                tilo_actions::ActionAddress::new(
                    tilo_workspace_records::vault::RESOURCE,
                    tilo_workspace_records::vault::ADD,
                ),
                &json!({"kind": "key", "belongsTo": ["https://api.anthropic.com"], "secret": TOKEN}),
            )
            .await
            .expect_err("an agent's grant covers no vault item of its own");
        assert!(
            !matches!(
                refused,
                tilo_actions::remote::ActionCallError::UnknownAction
            ),
            "the vault decides the agent's call, rather than knowing nothing of it: {refused:?}"
        );
    });
}

/// An agent's key of this person's, entered in `workspace` and asking the daemon over the device
/// link as one of their devices does: an agent of theirs on a device of theirs, never a device.
async fn agent_member(host: &host::Host, workspace: &str, label: &str) -> host::Member {
    let (key, cert) = host.certify(
        label,
        tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::AllWorkspaces,
        ),
    );
    host.daemon()
        .workspaces()
        .enter(workspace, (&cert, &key))
        .await
        .expect("the person's agent is entered in their workspace");
    host.dial(key, workspace).await
}

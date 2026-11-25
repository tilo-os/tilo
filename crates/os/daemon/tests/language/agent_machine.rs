//! A secret of the workspace's vault reaching the programs its Linux machine starts, and nothing
//! else: the person keeps a token in the vault, names it in the workspace's machine secrets under
//! a variable, and the next command their agent starts in the machine hands its program it. A
//! program typed at a terminal in the machine is asked about as the agent it is, once the person
//! allows it.

use serde_json::json;

use crate::language::agent_fixture::{act, said};
use crate::language::host;

/// What the person keeps in the vault: nothing but a program in the machine may read it.
const TOKEN: &str = "placeholder-token-for-a-program-in-the-machine";
const VARIABLE: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// The workspace's machine secrets record, as a member's app addresses it.
const DOCUMENT: &str = "derived:terminal.machine-secrets@tab:0000000000000000";

fn secrets_record() -> String {
    format!(
        "{}:{}:{}",
        tilo_actions::remote::RESOURCE_PROVIDER,
        "so.tilo.terminal",
        DOCUMENT
    )
}

/// The person keeps the token in their vault from a device of theirs, as their page does, and
/// answers with its reference.
async fn keep(host: &host::Host) -> String {
    let workspace = host.workspace().to_owned();
    let laptop = host.member("Ada's laptop", &workspace).await;
    let kept = laptop
        .call(
            tilo_actions::remote::person_vault::PROVIDER,
            tilo_actions::ActionAddress::new(
                tilo_actions::remote::person_vault::RESOURCE,
                tilo_actions::remote::person_vault::ADD,
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
    host::run(|host| async move {
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
            "terminal.machine-secrets#add-secret",
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
        let saved = host
            .daemon()
            .workspaces()
            .resource_document(host.workspace(), "so.tilo.terminal", DOCUMENT)
            .await
            .unwrap()
            .expect("the machine secrets document");
        let document = String::from_utf8_lossy(&saved);
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
            "terminal.machine-secrets#remove-secret",
            json!({"variable": VARIABLE}),
        )
        .await;
        assert!(!failed, "the person removes the item: {removed}");
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "claude --version"}),
        )
        .await;
        assert!(!failed, "the command runs without it: {ran}");
    });
}

/// A member names a vault item for the machine before any window made the record: the record is
/// founded, its declared verbs are offered, and the document the machine's secrets read names the
/// item.
#[test]
fn a_member_names_a_vault_item_before_any_window_made_the_record() {
    host::run(|host| async move {
        let reference = keep(host.as_ref()).await;
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        // The record's own listing offers what it declares, its `add-secret` among them.
        let named = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "invitation.list"),
                &json!({}),
            )
            .await;
        assert!(
            matches!(named, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "a member of the workspace is answered: {named:?}"
        );
        let provider = secrets_record();
        // The member invokes the record's declared verb, as `tilo do` does.
        let (added, failed) = said(
            &laptop
                .call(
                    &provider,
                    tilo_actions::ActionAddress::new("terminal.machine-secrets", "add-secret"),
                    &json!({"variable": VARIABLE, "item": &reference}),
                )
                .await
                .map_or_else(
                    |error| json!({"error": format!("{error:?}")}),
                    |_| json!({"ok": true}),
                ),
        );
        assert!(
            !added.contains("error") && !failed,
            "a member names the item before any window made the record: {added}"
        );
        let saved = host
            .daemon()
            .workspaces()
            .resource_document(&workspace, "so.tilo.terminal", DOCUMENT)
            .await
            .unwrap()
            .expect("the machine secrets document");
        let document = String::from_utf8_lossy(&saved);
        assert!(
            document.contains(&reference),
            "the document the machine reads names the item: {document}"
        );
    });
}

/// A program in the workspace's machine works there as its own agent once the person allows it:
/// asked about the agent it would be, allowed, its work is kept as that agent's.
#[test]
fn a_program_in_the_workspaces_machine_works_there_as_its_own_agent_once_allowed() {
    host::run(|host| async move {
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
        let (started, failed) = act(
            &agent,
            "terminal#open-terminal",
            json!({"cols": 100, "rows": 30}),
        )
        .await;
        assert!(
            !failed && started.contains("stream"),
            "a program in the machine connects at its machine's agent socket: {started}"
        );
    });
}

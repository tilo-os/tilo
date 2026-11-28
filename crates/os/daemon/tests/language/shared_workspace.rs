//! A workspace other people are members of runs its programs in a VM on a host, never on the
//! computer of one of them: the device that keeps the workspace refuses, in words, and nothing
//! runs.

use serde_json::json;
use tilo_actions::{ActionAddress, InvocationUpdate};
use ui::resource::DocumentId;

use crate::language::host;

/// What the refusal says: the computer runs only its own person's workspace's programs.
const WORDS: &str = "only its own person's workspace's programs";

#[test]
fn a_shared_workspaces_programs_stay_off_a_members_computer() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("laptop", &workspace).await;
        // Someone else is a member of the workspace.
        let (_grace, _) = host.other_person("Grace", &workspace).await;
        let record = DocumentId::window_default(0x927e_0000_0000_0005);
        let provider = ui::apps::TERMINAL;
        let session =
            tilo_actions::remote::record_path("tl.so.tilo.terminal.Session", record.route());

        let opened = laptop
            .call(
                provider,
                ActionAddress::new(session.as_str(), "open-terminal"),
                &json!({"cols": 100, "rows": 30}),
            )
            .await;
        let Err(tilo_actions::remote::ActionCallError::Refused { reason }) = opened else {
            panic!("a shared workspace's terminal is refused: {opened:?}");
        };
        assert!(reason.contains(WORDS), "{reason}");

        let ran = laptop
            .call(
                provider,
                ActionAddress::new(session.as_str(), "run"),
                &json!({"command": "echo hello"}),
            )
            .await;
        match ran {
            Ok(InvocationUpdate::Failed { error, .. }) => {
                assert!(error.message.contains(WORDS), "{error:?}");
            }
            other => panic!("a shared workspace's program is refused: {other:?}"),
        }
        // The shells the Terminal app opens on its own connection ask the same of the device,
        // on each open.
        let shells = host
            .daemon()
            .workspaces()
            .runs_programs_here(&workspace)
            .await;
        assert!(
            shells.as_ref().is_err_and(|reason| reason.contains(WORDS)),
            "a shared workspace's shells are refused: {shells:?}"
        );
        // Nothing was made: the record was not founded and no command recorded.
        assert!(
            host.daemon()
                .workspaces()
                .resource_document(&workspace, ui::apps::TERMINAL, record.route())
                .await
                .unwrap()
                .is_none(),
            "the refused session was not founded"
        );
    });
}

/// What a computer is to its person (its settings, the tickets that let a device in) stays
/// theirs: another member's device, which the computer serves the workspace's actions, is
/// refused it, in words that say whose it is.
#[test]
fn a_members_device_is_refused_what_is_the_computers_own() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let (grace, _) = host.other_person("Grace", &workspace).await;
        for (resource, action, arguments) in [
            ("devices", "issue", json!({})),
            (
                "settings",
                "set",
                json!({"name": "keymap", "value": "dvorak"}),
            ),
        ] {
            let asked = grace
                .call(
                    tilo_device_identity::login::PROVIDER,
                    ActionAddress::new(resource, action),
                    &arguments,
                )
                .await;
            let Err(tilo_actions::remote::ActionCallError::Refused { reason }) = &asked else {
                panic!("{resource}#{action} is refused to another member's device: {asked:?}");
            };
            assert!(
                reason.contains("only its person's own devices reach it"),
                "{resource}#{action}: {reason}"
            );
        }
    });
}

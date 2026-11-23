//! A workspace other people are members of runs its programs in a VM on a host, never on the
//! computer of one of them: the device that keeps the workspace refuses, in words, and nothing
//! runs.

use serde_json::json;
use tilo_actions::{ActionAddress, InvocationUpdate, remote::RESOURCE_PROVIDER};
use ui::resource::DocumentId;

use crate::host;

/// What the refusal says: the computer runs only its own person's workspace's programs.
const WORDS: &str = "only its own person's workspace's programs";

#[test]
fn a_shared_workspaces_programs_stay_off_a_members_computer() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("laptop", &workspace).await;
        // Someone else is a member of the workspace.
        let _grace = host.other_person("Grace", &workspace).await;
        let record = DocumentId::window_default(0x927e_0000_0000_0005);
        let provider = format!(
            "{RESOURCE_PROVIDER}:{}:{}",
            ui::apps::TERMINAL,
            record.route()
        );

        let opened = laptop
            .call(
                &provider,
                ActionAddress::new("terminal.session", "open-terminal"),
                &json!({"cols": 100, "rows": 30}),
            )
            .await;
        let Err(tilo_actions::remote::ActionCallError::Refused { reason }) = opened else {
            panic!("a shared workspace's terminal is refused: {opened:?}");
        };
        assert!(reason.contains(WORDS), "{reason}");

        let ran = laptop
            .call(
                &provider,
                ActionAddress::new("terminal.session", "run"),
                &json!({"command": "echo hello"}),
            )
            .await;
        match ran {
            Ok(InvocationUpdate::Failed { error, .. }) => {
                assert!(error.message.contains(WORDS), "{error:?}");
            }
            other => panic!("a shared workspace's program is refused: {other:?}"),
        }
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

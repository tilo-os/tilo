//! A Terminal window on a fresh tab of the workspace the daemon governs gets its shell.
//!
//! The window shows its tab's default session record, which nothing wrote yet, and asks the
//! workspace's host to open that session's shell on a terminal: the shell lives with the
//! workspace. The host founds the record at that id, with the genesis every device derives for
//! it, so what the device writes to it later merges, and opens the shell. A host that serves no
//! workspace's shells refuses the open, which is what a device's daemon does until it runs them.

use serde_json::json;
use tilo_actions::remote::RESOURCE_PROVIDER;
use tilo_actions::{ActionAddress, InvocationUpdate};
use ui::resource::DocumentId;

use crate::language::host;

const APP: &str = ui::apps::TERMINAL;

/// The root changes of a record's saved document: its genesis.
fn genesis_of(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    let mut doc = automerge::AutoCommit::load(bytes).unwrap();
    let mut roots: Vec<_> = doc
        .get_changes(&[])
        .into_iter()
        .filter(|change| change.deps().is_empty())
        .map(|change| change.hash())
        .collect();
    roots.sort();
    roots
}

/// A device of the person's opens a fresh tab's Terminal session: the workspace's host opens
/// its shell and founds the tab's record, so what the device writes there later merges.
#[test]
fn a_members_terminal_on_a_fresh_tab_gets_its_shell() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("laptop", &workspace).await;

        // Nothing wrote the tab's session record yet.
        let record = DocumentId::window_default(0x927e_0000_0000_0003);
        assert!(
            host.daemon()
                .workspaces()
                .resource_document(&workspace, APP, record.route())
                .await
                .unwrap()
                .is_none(),
            "the tab's session is not written yet"
        );

        let provider = format!("{RESOURCE_PROVIDER}:{APP}:{}", record.route());
        let opened = laptop
            .call(
                &provider,
                // What Terminal's session offers a device to open its shell on a terminal.
                ActionAddress::new("terminal.session", "open-terminal"),
                &json!({"cols": 100, "rows": 30}),
            )
            .await
            .unwrap_or_else(|refused| panic!("the host opens the fresh tab's shell: {refused:?}"));
        let InvocationUpdate::Succeeded { output, .. } = &opened else {
            panic!("the host opens the fresh tab's shell: {opened:?}");
        };
        let value = output.value.clone().unwrap_or_default();
        let is_id = |name: &str| {
            value[name]
                .as_str()
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        assert!(
            is_id("stream") && is_id("session"),
            "a stream and a session: {value}"
        );

        // The host founded the record as every device's first window of it does: what the
        // device writes to it later merges with what the host holds.
        let held = host
            .daemon()
            .workspaces()
            .resource_document(&workspace, APP, record.route())
            .await
            .unwrap()
            .expect("the host founded the tab's session");
        assert!(
            !genesis_of(&held).is_empty(),
            "the tab's session record has one genesis"
        );
    });
}

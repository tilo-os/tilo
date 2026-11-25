//! A member names a vault item for the workspace's machine before any window made the record: the
//! daemon starts Terminal with no window on that record, the app opens or founds it there, runs
//! the record's own `add-secret`, and what the machine reads names the item, never its secret.

use serde_json::json;
use tilo_actions::{ActionAddress, InvocationUpdate, remote::RESOURCE_PROVIDER};

use crate::language::host;

/// The workspace's machine secrets record, as a member's app addresses it.
const DOCUMENT: &str = "derived:terminal.machine-secrets@tab:0000000000000000";
/// A vault item's reference: what the person keeps, named and never carried.
const REFERENCE: &str = "vault.item:0123456789abcdef0123456789abcdef";
const VARIABLE: &str = "CLAUDE_CODE_OAUTH_TOKEN";

#[test]
fn a_member_names_a_vault_item_before_any_window_made_the_record() {
    host::run_with_packages(&[], &[("terminal", "terminal")], |host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let provider = format!("{RESOURCE_PROVIDER}:{}:{DOCUMENT}", ui::apps::TERMINAL);
        let named = laptop
            .call(
                &provider,
                ActionAddress::new("terminal.machine-secrets", "add-secret"),
                &json!({"variable": VARIABLE, "item": REFERENCE}),
            )
            .await
            .unwrap_or_else(|refused| panic!("the record's own verb is offered: {refused:?}"));
        assert!(
            matches!(named, InvocationUpdate::Succeeded { .. }),
            "a member names the item before any window made the record: {named:?}"
        );
        let saved = host
            .daemon()
            .workspaces()
            .resource_document(&workspace, ui::apps::TERMINAL, DOCUMENT)
            .await
            .unwrap()
            .expect("the machine secrets document");
        let document = String::from_utf8_lossy(&saved);
        assert!(
            document.contains(REFERENCE),
            "the document the machine reads names the item: {document}"
        );
        assert!(
            document.contains(VARIABLE),
            "under the variable the person named: {document}"
        );

        // Removed, the machine no longer reads it.
        let removed = laptop
            .call(
                &provider,
                ActionAddress::new("terminal.machine-secrets", "remove-secret"),
                &json!({"variable": VARIABLE}),
            )
            .await
            .unwrap_or_else(|refused| panic!("the record's remove is offered: {refused:?}"));
        assert!(
            matches!(removed, InvocationUpdate::Succeeded { .. }),
            "a member takes the item back: {removed:?}"
        );
        let saved = host
            .daemon()
            .workspaces()
            .resource_document(&workspace, ui::apps::TERMINAL, DOCUMENT)
            .await
            .unwrap()
            .expect("the machine secrets document");
        assert!(
            !String::from_utf8_lossy(&saved).contains(VARIABLE),
            "the document no longer names it"
        );
    });
}

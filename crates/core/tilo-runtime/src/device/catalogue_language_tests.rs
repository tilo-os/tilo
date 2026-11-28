//! A workspace's rows are named in the language its caller reads: the language a person's
//! request declares for them, which their window, command and agent pass on.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed unwrap or assertion in a test is its assertion"
)]

use std::sync::Arc;

use tilo_actions::remote::{
    ActionProvider as _, Caller, CataloguePage, CatalogueQuery, MAX_CATALOGUE_PAGE,
    WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::{CREATE_WORKSPACE, HOST_ACTIONS_PROVIDER};
use tilo_test_env::hang_bound;

use super::services::{Daemon, tests::isolated_root};
use crate::RuntimeServices as _;
use crate::device::workspaces::block_on_owned;

const TEST: &str =
    "device::catalogue_language_tests::a_listing_names_a_workspaces_rows_in_the_persons_language";

/// A workspace's rows are named in the language its caller declares: what its providers build
/// while they answer — its own verbs, the work it asks of its servers, and the operations of the
/// host keeping it — reads German in German and English in English.
#[test]
fn a_listing_names_a_workspaces_rows_in_the_persons_language() {
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).expect("the daemon starts"));
    let workspace = daemon.workspace().expect("this computer's workspace");
    let (tree, caller) = daemon
        .connection_tree_in(Some(&workspace))
        .expect("the person's own connection");
    // The workspace's own verbs, as its own connection lists them.
    let governance = |language: Option<&str>| {
        let query = CatalogueQuery {
            provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.to_owned()),
            language: language.map(str::to_owned),
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        };
        tree.query(&caller, query)
            .wait_timeout(hang_bound())
            .ok()
            .flatten()
            .expect("the daemon answered")
            .expect("the listing is answered")
    };
    let german = governance(Some("de-DE"));
    says(&german, "rename", "Den Arbeitsbereich umbenennen");
    says(&german, "call", "Einen Anruf moderieren");
    let english = governance(Some("en-US"));
    says(&english, "rename", "Rename the workspace");
    says(&english, "call", "Moderate a call");
    // The operations of the host keeping the workspace, as the daemon's workspace service lists
    // them: rows it builds while it answers too, and German as well.
    let cert = daemon.workspaces().device_cert().clone();
    let asking = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(&workspace);
    let id = WorkspaceId::parse(&workspace).expect("a workspace");
    let service = Arc::clone(&daemon);
    let host = block_on_owned(async move {
        service
            .workspaces()
            .catalogue(
                Some(id),
                &asking,
                Some(&cert),
                &CatalogueQuery {
                    provider: Some(HOST_ACTIONS_PROVIDER.to_owned()),
                    language: Some("de-DE".to_owned()),
                    max_results: MAX_CATALOGUE_PAGE,
                    ..CatalogueQuery::default()
                },
            )
            .await
    });
    says(&host, CREATE_WORKSPACE, "Einen Arbeitsbereich erstellen");
}

/// Says what the listing names the action `id`, and that it is `want`: what a failure reads.
fn says(page: &CataloguePage, id: &str, want: &str) {
    let listed: Vec<String> = page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .map(|action| format!("{}={:?}", action.address.action, action.action_label))
        .collect();
    let named = page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == id)
        .map(|action| action.action_label.clone());
    println!("{id} reads {named:?}, {want:?} wanted; the listing: {listed:?}");
    assert_eq!(named.as_deref(), Some(want));
}

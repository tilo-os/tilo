//! Page actions on records nobody has open, answered by the host.
//!
//! The host runs `tilo_server::agents::start`, whose app action server now
//! also answers the workspace's records ([`tilo_agent_session::WorkspaceActions`]).
//! A browser session creates a Sheets and a Slides record and closes them;
//! no agent exists. The page then lists the workspace's catalogue, runs a record's
//! action through the host, retries it under the same id, reuses the id for
//! another request, is asked to confirm a destructive action, and is refused
//! the cancel of one that has finished. Every change the host makes arrives
//! as an ordinary durable change authored by the host device.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

use tilo_server_protocol::WorkspaceId;

#[cfg(test)]
#[path = "workspace_actions/agent_mcp.rs"]
mod agent_mcp;
#[cfg(test)]
#[path = "workspace_actions/agent_member.rs"]
mod agent_member;
#[cfg(test)]
#[path = "workspace_actions/agent_views.rs"]
mod agent_views;
#[cfg(test)]
#[path = "workspace_actions/api.rs"]
mod api;
#[cfg(test)]
#[path = "workspace_actions/api_agent.rs"]
mod api_agent;
#[cfg(test)]
#[path = "workspace_actions/api_apps.rs"]
mod api_apps;
#[cfg(test)]
#[path = "workspace_actions/api_asks.rs"]
mod api_asks;
#[cfg(test)]
#[path = "workspace_actions/api_beside_import.rs"]
mod api_beside_import;
#[cfg(test)]
#[path = "workspace_actions/api_invites.rs"]
mod api_invites;
#[cfg(test)]
#[path = "workspace_actions/api_key_mail.rs"]
mod api_key_mail;
#[cfg(test)]
#[path = "workspace_actions/api_layout.rs"]
mod api_layout;
#[cfg(test)]
#[path = "workspace_actions/api_links.rs"]
mod api_links;
#[cfg(test)]
#[path = "workspace_actions/api_model_asks.rs"]
mod api_model_asks;
#[path = "workspace_actions/api_on_server.rs"]
pub mod api_on_server;
#[cfg(test)]
#[path = "workspace_actions/api_programs.rs"]
mod api_programs;
#[cfg(test)]
#[path = "workspace_actions/api_questions.rs"]
mod api_questions;
#[cfg(test)]
#[path = "workspace_actions/api_runs.rs"]
mod api_runs;
#[cfg(test)]
#[path = "workspace_actions/api_terminal_restart.rs"]
mod api_terminal_restart;
#[cfg(test)]
#[path = "workspace_actions/api_unattended.rs"]
mod api_unattended;
#[cfg(test)]
#[path = "workspace_actions/api_v1.rs"]
mod api_v1;
#[cfg(test)]
#[path = "workspace_actions/branch_files.rs"]
mod branch_files;
#[cfg(test)]
#[path = "workspace_actions/computer_agent.rs"]
mod computer_agent;
#[cfg(test)]
#[path = "workspace_actions/computer_restart.rs"]
mod computer_restart;
#[cfg(test)]
#[path = "workspace_actions/hosted_machine_agents.rs"]
mod hosted_machine_agents;
#[cfg(test)]
#[path = "workspace_actions/key.rs"]
mod key;
#[cfg(test)]
#[path = "workspace_actions/owner_signs_governance.rs"]
mod owner_signs_governance;

#[cfg(test)]
#[path = "workspace_actions/bulk_import.rs"]
mod bulk_import;
#[cfg(test)]
#[path = "workspace_actions/chain_parts_restart.rs"]
mod chain_parts_restart;
#[cfg(test)]
#[path = "workspace_actions/chat_after_join.rs"]
mod chat_after_join;
#[cfg(test)]
#[path = "workspace_actions/chat_group.rs"]
mod chat_group;
#[cfg(test)]
#[path = "workspace_actions/chat_restart.rs"]
mod chat_restart;
#[cfg(test)]
#[path = "workspace_actions/delete_workspace.rs"]
mod delete_workspace;
#[cfg(test)]
#[path = "workspace_actions/device_issue_create.rs"]
mod device_issue_create;
#[cfg(test)]
#[path = "workspace_actions/device_writes.rs"]
mod device_writes;
#[cfg(test)]
#[path = "workspace_actions/encrypted_resource.rs"]
mod encrypted_resource;
#[cfg(test)]
#[path = "workspace_actions/feedback_journey.rs"]
mod feedback_journey;
#[cfg(test)]
#[path = "workspace_actions/files_founded_page.rs"]
mod files_founded_page;
#[cfg(test)]
#[path = "workspace_actions/files_put_by_key.rs"]
mod files_put_by_key;
#[cfg(test)]
#[path = "workspace_actions/files_window_view.rs"]
mod files_window_view;
#[cfg(test)]
#[path = "workspace_actions/flow_customer_workspaces.rs"]
mod flow_customer_workspaces;
#[cfg(test)]
#[path = "workspace_actions/governed_agent.rs"]
mod governed_agent;
#[cfg(test)]
#[path = "workspace_actions/governed_sealed.rs"]
mod governed_sealed;
#[cfg(test)]
#[path = "workspace_actions/hosted_agent_questions.rs"]
mod hosted_agent_questions;
#[cfg(test)]
#[path = "workspace_actions/hosted_agent_sessions.rs"]
mod hosted_agent_sessions;
#[cfg(test)]
#[path = "workspace_actions/hosted_agent_terminal.rs"]
mod hosted_agent_terminal;
#[cfg(test)]
#[path = "workspace_actions/hosted_agent_turns.rs"]
mod hosted_agent_turns;
#[cfg(test)]
#[path = "workspace_actions/hosted_browser.rs"]
mod hosted_browser;
#[cfg(test)]
#[path = "workspace_actions/hosted_consent_notification.rs"]
mod hosted_consent_notification;
#[cfg(test)]
#[path = "workspace_actions/hosted_documents.rs"]
pub mod hosted_documents;
#[cfg(test)]
#[path = "workspace_actions/hosted_import_open.rs"]
mod hosted_import_open;
#[cfg(test)]
#[path = "workspace_actions/hosted_mail_account_migrate.rs"]
mod hosted_mail_account_migrate;
#[cfg(test)]
#[path = "workspace_actions/hosted_mail_turn.rs"]
mod hosted_mail_turn;
#[cfg(test)]
#[path = "workspace_actions/hosted_page_stream.rs"]
mod hosted_page_stream;
#[cfg(test)]
#[path = "workspace_actions/hosted_secret_use.rs"]
mod hosted_secret_use;
#[cfg(test)]
#[path = "workspace_actions/hosted_settings_first_use.rs"]
mod hosted_settings_first_use;
#[cfg(test)]
#[path = "workspace_actions/hosted_settings_from_page.rs"]
mod hosted_settings_from_page;
#[cfg(test)]
#[path = "workspace_actions/hosted_terminal_fresh_tab.rs"]
mod hosted_terminal_fresh_tab;
#[cfg(test)]
#[path = "workspace_actions/hosted_web_call.rs"]
mod hosted_web_call;
#[cfg(test)]
#[path = "workspace_actions/import_rate.rs"]
mod import_rate;
#[cfg(test)]
#[path = "workspace_actions/joining.rs"]
mod joining;
#[cfg(test)]
#[path = "workspace_actions/machine_secrets.rs"]
mod machine_secrets;
#[cfg(test)]
#[path = "workspace_actions/mcp_issue_by_id.rs"]
mod mcp_issue_by_id;
#[cfg(test)]
#[path = "workspace_actions/mcp_note_create.rs"]
mod mcp_note_create;
#[cfg(test)]
#[path = "workspace_actions/member_own.rs"]
mod member_own;
#[cfg(test)]
#[path = "workspace_actions/member_roles.rs"]
mod member_roles;
#[cfg(test)]
#[path = "workspace_actions/one_workspace_list.rs"]
mod one_workspace_list;
#[cfg(test)]
#[path = "workspace_actions/own_agent.rs"]
mod own_agent;
#[cfg(test)]
#[path = "workspace_actions/packaged_model.rs"]
mod packaged_model;
#[cfg(test)]
#[path = "workspace_actions/person_before_agent.rs"]
mod person_before_agent;
#[cfg(test)]
#[path = "workspace_actions/put_files.rs"]
mod put_files;
#[cfg(test)]
#[path = "workspace_actions/refresh_reads.rs"]
mod refresh_reads;
#[path = "workspace_actions/resource_support.rs"]
mod resource_support;
#[cfg(test)]
#[path = "workspace_actions/share_link.rs"]
mod share_link;
#[cfg(test)]
#[path = "workspace_actions/slow_read.rs"]
mod slow_read;
#[cfg(test)]
#[path = "workspace_actions/throughput.rs"]
mod throughput;
#[cfg(test)]
#[path = "workspace_actions/tracker_first_page.rs"]
mod tracker_first_page;
#[cfg(test)]
#[path = "workspace_actions/tracker_listing.rs"]
mod tracker_listing;
#[cfg(test)]
#[path = "workspace_actions/update_reads_older_writes.rs"]
mod update_reads_older_writes;
#[cfg(test)]
#[path = "workspace_actions/workspace_per_customer.rs"]
mod workspace_per_customer;

// Workspace actions hold each record in a confined app worker: this binary,
// started again.

#[cfg(test)]
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;
use tilo_session_client::Outbox;

use ed25519_dalek::SigningKey;
use tilo_actions::InvocationId;
use tilo_actions::api::ActionAddress;
#[cfg(test)]
use tilo_actions::remote::CatalogueQuery;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
#[cfg(test)]
use tilo_actions::{
    ConfirmationPolicy,
    remote::{ActionCallError, Confirmation, ReceiptStatus, RemoteActionMessage},
};
#[cfg(test)]
use tilo_agent_session::resource_provider;
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::harness::hosted::{Hosted, Options};
use crate::harness::page::Page;
#[cfg(test)]
use crate::support::WORKSPACE;
use crate::support::{Device, enrol, ready, start_peer};
use tilo_actions::MemberId;

const SHEETS: &str = "so.tilo.sheets";
const EBOOK: &str = "so.tilo.ebook";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_existing_native_chat_reference_discovers_local_views_over_the_governed_https_host() {
    use tilo_actions::remote::ViewResourceQuery;
    let (identity_key, _) = tilo_user_identity::UserIdentity::generate("Resource fixture", 1);
    let host = enrol(
        &identity_key,
        "temporary-node",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let browser_device = enrol(
        &identity_key,
        "temporary-browser",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&identity_key, &host, state.path()).await;
    let workspace = hosted
        .server
        .workspaces()
        .unwrap()
        .create("Native resource fixture")
        .await
        .unwrap();
    let mut browser =
        Browser::open_workspace(&hosted, &browser_device, &host, &workspace.workspace).await;
    let mut source = crate::common::open_in(
        &workspace.workspace,
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    crate::common::add_channel(&mut doc, "ada", "general", None);
    let general = crate::common::channel(&doc, chat::model::ChannelKind::Text);
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    // Messages are kept in the parts of the chat's messages, posted through
    // the chain the chat publishes beside its record.
    let message = resource_support::on_messages(
        &mut source,
        "post",
        &serde_json::json!({"channel": general.id, "text": "Hello é, shared natively"}),
    );
    let (id, kind) = resource_support::published_message(&source, &message);
    assert_eq!(
        kind, "chat.messages.messages",
        "an item of the part keeping it"
    );
    let part = chat::parts::part(0);
    let attached = serde_json::json!({"app": "so.tilo.chat", "record": part.as_str(),
        "id": id, "kind": kind, "label": "Native selection",
        "span": {"start": 6, "end": 7, "unit": "Chars"}});
    let shared = resource_support::on_messages(
        &mut source,
        "share-resource",
        &serde_json::json!({"channel": general.id, "attached": attached, "text": "Discuss this resource"}),
    );
    let shared = source
        .read_resource(&part, chat::messages::read_part)
        .unwrap()
        .messages
        .iter()
        .find(|(message, _)| message.as_str() == shared)
        .map(|(_, message)| message.clone())
        .unwrap();
    let shared_at = shared
        .attachments
        .iter()
        .find_map(tilo_reference::Attached::at)
        .expect("the record it shared");
    assert_eq!(serde_json::to_value(shared_at).unwrap(), attached);
    let record = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        document: source.wire_document().into(),
    };
    browser
        .peer
        .write(record, &source.take_mutation().unwrap().unwrap())
        .unwrap();
    // The message's own record is the part, published on its own route.
    let route = resource_support::part_route(&workspace.workspace, &part);
    for (changed, mutation) in source.take_document_mutations().unwrap() {
        assert_eq!(changed, part);
        browser.peer.write(route.clone(), &mutation).unwrap();
    }
    let provider = resource_provider(&route);
    let policy = ui::permissions::PermissionContext::member_of(
        &workspace.workspace,
        &MemberId::person("ada"),
        ui::permissions::Role::Owner,
    );
    let source_views = source
        .read_resource(&part, |doc| {
            crate::common::catalogue().resource_views(
                "so.tilo.chat",
                doc,
                &ViewResourceQuery {
                    id: id.clone(),
                    kind: kind.clone(),
                },
                &policy,
            )
        })
        .unwrap();
    assert!(
        !source_views.is_empty(),
        "the native source publishes its exact local view"
    );
    let query = CatalogueQuery {
        provider: Some(provider.clone()),
        view_resource: Some(ViewResourceQuery {
            id: id.clone(),
            kind: kind.clone(),
        }),
        max_results: 256,
        ..CatalogueQuery::default()
    };
    let deadline = tilo_test_env::hang_deadline();
    let page = loop {
        let result = browser.catalogue(query.clone()).await;
        if let Ok(page) = &result
            && !page.rows.is_empty()
        {
            break page.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the governed host never discovered the existing native reference: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(page.rows.iter().all(|row| {
        row.resource.id == id
            && row.resource.kind == kind
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.kind == tilo_actions::OperationKind::View)
    }));
    // The app filter travels with the query and the host narrows the record's
    // own views by it: its own app keeps every row, another app is answered
    // empty rather than refused, and a query naming no app is unchanged.
    let matching = browser
        .catalogue(CatalogueQuery {
            app_id: Some("so.tilo.chat".into()),
            ..query.clone()
        })
        .await
        .unwrap();
    assert_eq!(
        matching.rows, page.rows,
        "the record's own app keeps its views"
    );
    assert_eq!(
        matching
            .locator
            .as_ref()
            .map(|locator| locator.provider.as_str()),
        Some(provider.as_str()),
        "a matching app is told where to browse the views"
    );
    let elsewhere = browser
        .catalogue(CatalogueQuery {
            app_id: Some("so.tilo.notes".into()),
            ..query.clone()
        })
        .await
        .unwrap();
    assert!(
        elsewhere.rows.is_empty(),
        "another app's rows cannot be this record's views"
    );
    // Where the record opens is the caller's to know once they may read it, views or not.
    assert_eq!(
        elsewhere
            .locator
            .as_ref()
            .map(|locator| locator.provider.as_str()),
        Some(provider.as_str()),
        "an answer with no views still names where the record opens"
    );
    let unscoped = browser.catalogue(query.clone()).await.unwrap();
    assert_eq!(
        unscoped.rows, page.rows,
        "a query naming no app is unchanged"
    );
    let locator = page.locator.unwrap();
    assert_eq!(locator.origin, hosted.web.origin);
    assert_eq!(locator.workspace, workspace.workspace);
    assert_eq!(
        locator.host_device,
        hex::encode(host.cert.device_public_key)
    );
    assert_eq!(locator.provider, provider);
    for wanted in [
        ViewResourceQuery {
            id: ui::tilo_resource::items::item_resource_id::<chat::parts::ChatMessages>(
                "messages",
                &ui::tilo_resource::Id::from_raw("missing"),
            ),
            kind: kind.clone(),
        },
        ViewResourceQuery {
            id: id.clone(),
            kind: "notes.block".into(),
        },
    ] {
        let empty = browser
            .catalogue(CatalogueQuery {
                view_resource: Some(wanted),
                ..query.clone()
            })
            .await
            .unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(
            empty.locator.map(|locator| locator.provider),
            Some(provider.clone()),
            "the record opens whether or not it holds the views asked for"
        );
    }
    assert!(
        browser
            .catalogue(CatalogueQuery {
                provider: Some("record:so.tilo.chat:missing-source".into()),
                ..query.clone()
            })
            .await
            .is_err(),
        "a missing exact source must not create a record, keys or a locator"
    );
    let other_workspace = hosted
        .server
        .workspaces()
        .unwrap()
        .create("Isolated resource fixture")
        .await
        .unwrap();
    let mut other =
        Browser::open_workspace(&hosted, &browser_device, &host, &other_workspace.workspace).await;
    assert!(
        other.catalogue(query.clone()).await.is_err(),
        "an exact reference from another workspace must not produce views or a locator"
    );
    other.peer.close();
    let ordinary = browser
        .catalogue(CatalogueQuery {
            provider: Some(provider),
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    // An ordinary listing runs the part's verbs (published by the chain,
    // routed to the part they change) and the record's, never its views.
    assert!(
        ordinary
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .all(|action| action.kind != tilo_actions::OperationKind::View)
    );
    // The production catalogue worker must discard already-read metadata when
    // the authority changes before delivery, even though the source was valid.
    let bytes = source
        .read_resource(&part, |doc| doc.clone().save())
        .unwrap();
    let caller = tilo_actions::remote::Caller::authenticated(
        browser_device.cert.user_public_key,
        browser_device.cert.device_public_key,
    );
    for revoke_on_read in [false, true] {
        let link = Arc::new(DeliveryReadFixture {
            workspaces: hosted.server.workspaces().unwrap().clone(),
            member: tilo_agent_session::workspace_actions::HostDevice {
                endpoint: tilo_agent_session::ServerEndpoint::iroh(
                    hosted.server.node_addr(),
                    host.cert.clone(),
                ),
                key: host.key.clone(),
                runtime: tokio::runtime::Handle::current(),
                keys: None,
            },
            caller: caller.clone(),
            route: route.clone(),
            bytes: bytes.clone(),
            revoke_on_read,
            revoked: std::sync::atomic::AtomicBool::new(false),
            locator_reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
        actions.attach(link.clone()).unwrap();
        let answer = actions
            .serve(
                workspace.workspace_id,
                &caller,
                RemoteActionMessage::CatalogueRequest {
                    correlation: 1,
                    query: query.clone(),
                },
            )
            .unwrap();
        let response = answer
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .unwrap();
        let RemoteActionMessage::CatalogueResponse { result, .. } = response else {
            panic!("expected catalogue response")
        };
        if revoke_on_read {
            assert_eq!(result, Err(ActionCallError::UnknownAction));
            assert_eq!(
                link.locator_reads.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "revoked metadata must never reach locator issuance"
            );
        } else {
            let page = result.unwrap();
            assert!(
                !page.rows.is_empty(),
                "the same production worker accepts current Read"
            );
            assert!(page.locator.is_some());
        }
        assert_eq!(link.bytes, bytes, "view discovery cannot mutate its source");
        actions.shutdown();
    }
    browser.peer.close();
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}

/// Deterministically changes authority after a real Chat snapshot is read.
struct DeliveryReadFixture {
    /// The host's workspaces, which hold the documents this service reads.
    workspaces: tilo_server::workspaces::Workspaces,
    member: tilo_agent_session::workspace_actions::HostDevice,
    caller: tilo_actions::remote::Caller,
    route: DocumentRoute,
    bytes: Vec<u8>,
    revoke_on_read: bool,
    revoked: std::sync::atomic::AtomicBool,
    locator_reads: std::sync::atomic::AtomicUsize,
}

impl tilo_agent_session::workspace_actions::WorkspaceDocumentsLink for DeliveryReadFixture {
    fn documents(&self) -> Option<Arc<dyn tilo_server::workspace_documents::DeviceDocuments>> {
        Some(Arc::new(
            tilo_server::device_documents::ServiceDocuments::of(self.workspaces.clone()),
        ))
    }

    fn writes(&self) -> Option<Arc<dyn tilo_server::workspace_documents::DeviceWrites>> {
        Some(Arc::new(tilo_server::device_documents::ServiceWrites::of(
            self.workspaces.clone(),
        )))
    }

    fn user_public_key(&self) -> [u8; 32] {
        self.member.endpoint.server_cert.user_public_key
    }
    fn own_device(&self) -> tilo_agent_session::workspace_actions::HostDevice {
        self.member.clone()
    }
    fn view_read_policy(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        app: &str,
        record: &str,
    ) -> Option<tilo_agent_session::workspace_actions::ViewReadPolicy> {
        if self.revoked.load(std::sync::atomic::Ordering::SeqCst)
            || caller != &self.caller
            || self.route.workspace_id().ok() != Some(workspace)
            || app != self.route.app_id
            || record != self.route.document
        {
            return None;
        }
        Some(tilo_agent_session::workspace_actions::ViewReadPolicy {
            workspace: self.route.cluster_id.clone(),
            access: ui::permissions::MemberAccess::person(
                ui::permissions::policy::WorkspacePolicy::new(
                    &self.route.cluster_id,
                    hex::encode(self.user_public_key()),
                ),
                hex::encode(self.user_public_key()),
            ),
            sandbox: Vec::new(),
            unix_time: 1,
            agent_bound: Vec::new(),
        })
    }
    fn view_document(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        source: &DocumentRoute,
    ) -> Result<tilo_agent_session::workspace_actions::ViewDocument, String> {
        if source != &self.route
            || self
                .view_read_policy(workspace, caller, &source.app_id, &source.document)
                .is_none()
        {
            return Err("read refused".into());
        }
        self.revoked
            .store(self.revoke_on_read, std::sync::atomic::Ordering::SeqCst);
        Ok(tilo_agent_session::workspace_actions::ViewDocument {
            bytes: self.bytes.clone(),
            revision: "fixture-source".into(),
        })
    }
    fn view_locator(
        &self,
        workspace: &str,
        provider: &str,
    ) -> Option<tilo_actions::remote::ResourceViewLocator> {
        self.locator_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(tilo_actions::remote::ResourceViewLocator {
            origin: "https://fixture.tilo.invalid".into(),
            web_entry: Some("https://fixture.tilo.invalid/qos-runtime/".into()),
            workspace: workspace.into(),
            host_device: hex::encode(self.member.endpoint.server_cert.device_public_key),
            provider: provider.into(),
        })
    }
}

/// The host with its agent supervisor, offering every app and no page.
async fn start(root: &SigningKey, host: &Device, state: &Path) -> Hosted {
    start_browser(root, host, state, None).await
}

/// The host serving the page at `page`, when it serves one.
async fn start_browser(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    page: Option<&str>,
) -> Hosted {
    start_lending(root, host, state, page, None).await
}

/// A host lending its workspaces' reporters `feedback_reviewer`.
async fn start_lending(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    page: Option<&str>,
    feedback_reviewer: Option<Arc<dyn tilo_agent_session::FeedbackReviewer>>,
) -> Hosted {
    let options = Options {
        feedback_reviewer,
        ..Options::offering(crate::common::catalogue)
    };
    let options = match page {
        Some(origin) => options.serving(origin),
        None => options,
    };
    crate::harness::hosted::start(root, host, state, options).await
}

/// A page speaking the remote-action vocabulary to the host.
pub use crate::harness::page::Page as Browser;

/// What a page does with the records it holds: it creates them whole, and
/// mounts them from the changes it received.
impl Page {
    /// Create `app`'s record the way a page does, then close it: nobody has
    /// it open afterwards.
    fn create(&mut self, app: &str) -> DocumentRoute {
        self.create_before_publish(app, |_| {})
    }

    fn create_before_publish(
        &mut self,
        app: &str,
        before_publish: impl FnOnce(&DocumentRoute),
    ) -> DocumentRoute {
        let mut page = crate::common::open_in(
            &self.cluster_id,
            app,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: self.cluster_id.clone(),
            document: page.wire_document().to_owned(),
        };
        let mutation = page.take_mutation().unwrap().unwrap();
        before_publish(&route);
        self.peer.write(route.clone(), &mutation).unwrap();
        // The host never echoes a device's own change back to it.
        let bytes = match mutation {
            collab_types::DocumentMutation::Snapshot(bytes)
            | collab_types::DocumentMutation::Incremental(bytes) => bytes,
        };
        self.documents
            .entry(route.clone())
            .or_default()
            .push(([0; 32], bytes));
        route
    }

    /// The record at `route` as this page has received it.
    fn mount(&mut self, route: &DocumentRoute) -> Replica {
        self.pump();
        let changes = self.documents.get(route).cloned().unwrap_or_default();
        crate::common::open_in(
            &self.cluster_id,
            &route.app_id,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::Existing {
                record: route_document(&route.document),
                changes: changes.into_iter().map(|(_, bytes)| bytes).collect(),
            },
        )
        .unwrap()
    }
}

/// The record a route names.
pub fn route_document(record: &str) -> ui::resource::DocumentId {
    if record.is_empty() {
        ui::resource::DocumentId::package_primary()
    } else {
        ui::resource::DocumentId::parse(record).unwrap()
    }
}

fn sheet_names(app: &Replica) -> Vec<String> {
    app.read(|doc| {
        <sheets::Workbook as ui::tilo_resource::Resource>::read(doc)
            .map(|workbook| {
                workbook
                    .sheets
                    .values()
                    .map(|sheet| sheet.name.0.clone())
                    .collect()
            })
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// The argument of Sheets' New sheet: the sheet's name.
#[cfg(test)]
/// Run one more action on `record` and wait until its sheet reaches the
/// browser, returning the record's sheet names then. The host writes a
/// record's changes in order, so anything it wrote before has arrived too:
/// what is missing now was never written.
async fn after_a_marker(
    browser: &mut Browser,
    record: &DocumentRoute,
    new_sheet: &ActionAddress,
    marker: &str,
) -> Vec<String> {
    // A save still landing is answered accepted; it is followed until it ends, as a page does.
    let receipt = browser
        .invoke_settled(request(
            &resource_provider(record),
            new_sheet.clone(),
            sheet_named(marker),
        ))
        .await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        let names = sheet_names(&browser.mount(record));
        if names.iter().any(|name| name == marker) {
            return names;
        }
        assert!(
            Instant::now() < deadline,
            "the marker's change never arrived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn sheet_named(name: &str) -> InvokeArgument {
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), name.into());
    InvokeArgument::Json(arguments)
}

fn request(provider: &str, address: ActionAddress, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address,
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_listed_themes_action_is_one_the_host_can_run() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let host_key = host.cert.device_public_key;
    let themes = browser.create("so.tilo.themes");
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let provider = resource_provider(&themes);
    let page = browser
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    // The catalogue offers only what the host runs: the record's document
    // operations, and the effects that answer (an export kept as content),
    // which it runs in the record's own view as a window would. Views move
    // a window and stay with the app where it is open. A listed operation
    // may fail, need confirmation or other arguments, but is never unknown.
    let listed: Vec<_> = page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .collect();
    assert!(!listed.is_empty(), "a themes record offers its operations");
    assert!(
        listed.iter().all(|action| {
            action.kind == tilo_actions::OperationKind::Document
                || (action.kind == tilo_actions::OperationKind::Effect
                    && tilo_agent_session::apps::offered_to_agents(action))
        }),
        "the record catalogue must offer only what the host runs: {:?}",
        listed
            .iter()
            .map(|action| &action.address)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        listed
            .iter()
            .filter(|action| action.address.action == "new-theme")
            .count(),
        1,
        "creating a theme within the open Studio is one canonical document action"
    );
    for action in page.rows.iter().filter_map(|row| row.action.as_ref()) {
        let mut arguments = serde_json::Map::new();
        for param in action.params.iter().filter(|param| param.required) {
            arguments.insert(param.name.to_string(), "Dusk".into());
        }
        let receipt = browser
            .invoke(InvokeRequest {
                on_behalf_of: None,
                confirmation: Some(Confirmation::Confirmed),
                ..request(
                    &provider,
                    action.address.clone(),
                    InvokeArgument::Json(arguments),
                )
            })
            .await;
        assert_ne!(
            receipt.status,
            ReceiptStatus::Rejected {
                error: ActionCallError::UnknownAction
            },
            "{:?} is listed but unknown",
            action.address
        );
    }
    let retained = hosted
        .server
        .workspace_documents()
        .retained(crate::support::workspace())
        .unwrap();
    let (readable, _) = crate::common::member_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
        &host_key,
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_acts_on_records_nobody_has_open_through_the_host() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let host_key = host.cert.device_public_key;

    let sheets = browser.create(SHEETS);
    let books = browser.create(EBOOK);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let sheets_provider = resource_provider(&sheets);
    let books_provider = resource_provider(&books);

    // 1. The catalogue: every record of the workspace is a provider, and with no
    // provider named the recent records' actions are listed with the workspace's
    // "open an app" actions.
    let page = browser.catalogue(CatalogueQuery::default()).await.unwrap();
    let providers: Vec<&str> = page.providers.iter().map(|p| p.provider.as_str()).collect();
    assert!(
        providers.contains(&sheets_provider.as_str()),
        "{providers:?}"
    );
    assert!(
        providers.contains(&books_provider.as_str()),
        "{providers:?}"
    );
    assert!(
        providers.contains(&format!("workspace:{WORKSPACE}").as_str()),
        "{providers:?}"
    );
    let sheets_page = browser
        .catalogue(CatalogueQuery {
            provider: Some(sheets_provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    let new_sheet = sheets_page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "new-sheet")
        .expect("Sheets offers New sheet")
        .address
        .clone();

    // 2. Invoke on a record nobody has open: the host mounts it, runs the
    // action and commits the change as its own.
    let add = request(&sheets_provider, new_sheet.clone(), sheet_named("Budget"));
    let receipt = browser.invoke(add.clone()).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        let by_host = browser
            .documents
            .get(&sheets)
            .is_some_and(|changes| changes.iter().any(|(author, _)| *author == host_key));
        if by_host {
            let app = browser.mount(&sheets);
            let names = sheet_names(&app);
            if names.iter().filter(|name| *name == "Budget").count() == 1 {
                break;
            }
        }
        assert!(Instant::now() < deadline, "the host's change never arrived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The host signed the change as the one who ran it; the change keeps the invocation as
    // the page signed it, which a member reading it checks against Ada's root.
    let kept = {
        let mut doc = automerge::AutoCommit::new();
        for (_, bytes) in &browser.documents[&sheets] {
            let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
        }
        doc.get_changes(&[])
            .iter()
            .filter_map(|change| change.message())
            .filter_map(tilo_actions::ActionContext::from_change_message)
            .find(|context| context.source_invocation == Some(add.invocation))
            .expect("the host's change names the invocation it ran")
    };
    let workspace = browser.workspace.to_string();
    let revocations = tilo_user_identity::RevocationList::default();
    let recorded = |context: &tilo_actions::ActionContext| {
        tilo_server_protocol::invocation::verify_recorded(context, &workspace, &revocations)
    };
    let signer = recorded(&kept)
        .unwrap()
        .expect("the change keeps the invocation its caller signed");
    assert_eq!(
        signer.device_public_key,
        browser_device.cert.device_public_key
    );
    assert_eq!(
        kept.signed.as_ref().unwrap().argument,
        Some(sheet_named("Budget"))
    );
    let mut reargued = kept.clone();
    reargued.signed.as_mut().unwrap().argument = Some(sheet_named("Other"));
    assert_eq!(
        recorded(&reargued),
        Err(tilo_server_protocol::invocation::InvocationError::Mismatch(
            tilo_actions::remote::signed_invocation::StatementMismatch::Arguments
        ))
    );
    let mut misnamed = kept.clone();
    misnamed.invoker = Some(tilo_actions::ActionInvoker::person(hex::encode([9; 32])));
    assert_eq!(
        recorded(&misnamed),
        Err(tilo_server_protocol::invocation::InvocationError::NotInvoker)
    );
    let mut moved = kept.clone();
    moved.source_invocation = Some(InvocationId::new());
    assert!(recorded(&moved).is_err(), "another change's invocation");

    // 3. Idempotency: the same request under the same id reports the
    // original and runs nothing; another request under it is a conflict.
    let retry = browser.invoke(add.clone()).await;
    assert_eq!(retry, receipt);
    let mut other = add.clone();
    other.argument = sheet_named("Other");
    let conflict = browser.invoke(other).await;
    assert_eq!(
        conflict.status,
        ReceiptStatus::Rejected {
            error: ActionCallError::InvocationConflict
        }
    );
    let names = after_a_marker(&mut browser, &sheets, &new_sheet, "After the retry").await;
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "Budget")
            .count(),
        1,
        "a retry never runs the action twice: {names:?}"
    );
    assert!(!names.contains(&"Other".to_owned()));

    // 4. Confirmation: an action Undo cannot take back is refused without
    // the caller's explicit confirmation, and nothing ran; confirmed, it
    // runs. Removing a book asks first: Undo puts the row back but not the
    // device's access to the book's file.
    let books_page = browser
        .catalogue(CatalogueQuery {
            provider: Some(books_provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    let destructive = books_page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "remove-from-library")
        .expect("Books offers Remove from library")
        .clone();
    assert_eq!(
        destructive.contract.confirmation,
        ConfirmationPolicy::Confirm,
        "{destructive:?}"
    );
    let destructive = destructive.address;
    let unconfirmed = request(
        &books_provider,
        destructive.clone(),
        InvokeArgument::Json(
            serde_json::json!({"book": "book:dusk"})
                .as_object()
                .unwrap()
                .clone(),
        ),
    );
    let refused = browser.invoke(unconfirmed.clone()).await;
    assert_eq!(
        refused.status,
        ReceiptStatus::Rejected {
            error: ActionCallError::confirmation_required()
        }
    );
    let confirmed = InvokeRequest {
        on_behalf_of: None,
        confirmation: Some(Confirmation::Confirmed),
        ..unconfirmed
    };
    let ran = browser.invoke(confirmed).await;
    assert!(
        !matches!(
            ran.status,
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { .. }
            }
        ),
        "a refused invocation is forgotten, so the confirmed retry under its id is admitted: {ran:?}"
    );

    // 5. Cancel: a finished invocation cannot be cancelled. An invocation
    // waits in the host's queue only while the host waits for the keys of what
    // it reads; a workspace hands its one key scope over before every invoke,
    // so nothing here queues behind another.
    browser.send(RemoteActionMessage::Cancel {
        invocation: add.invocation,
    });
    let finished = browser
        .answer("cancel finished", |answer| match answer {
            RemoteActionMessage::CancelResponse { invocation, result }
                if *invocation == add.invocation =>
            {
                Some(result.clone())
            }
            _ => None,
        })
        .await;
    assert!(matches!(
        finished,
        Err(ActionCallError::NotCancellable { .. })
    ));

    // 6. The workspace's "open an app" action creates a record through the host.
    let opened = browser
        .invoke_settled(request(
            &format!("workspace:{WORKSPACE}"),
            ActionAddress::new(format!("app.{SHEETS}"), "open"),
            InvokeArgument::None,
        ))
        .await;
    let ReceiptStatus::Completed { output } = opened.status else {
        panic!("{opened:?}");
    };
    assert!(
        output.value.unwrap()["provider"]
            .as_str()
            .unwrap()
            .starts_with("record:so.tilo.sheets:")
    );

    let retained = hosted
        .server
        .workspace_documents()
        .retained(crate::support::workspace())
        .unwrap();
    let (readable, _) = crate::common::member_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
        &host_key,
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_request_carrying_an_appearance_is_refused_and_runs_nothing() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    let sheets = browser.create(SHEETS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let provider = resource_provider(&sheets);
    let new_sheet = browser
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap()
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "new-sheet")
        .expect("Sheets offers New sheet")
        .address
        .clone();

    let mut posing = request(&provider, new_sheet.clone(), sheet_named("Budget"));
    posing.appearance = Some(tilo_actions::Appearance::new("Owner", None, None).unwrap());
    let receipt = browser.invoke(posing).await;
    assert!(
        matches!(
            receipt.status,
            ReceiptStatus::Rejected {
                error: ActionCallError::Refused { .. }
            }
        ),
        "{receipt:?}"
    );
    let names = after_a_marker(&mut browser, &sheets, &new_sheet, "After the refusal").await;
    assert!(!names.contains(&"Budget".to_owned()));
}

pub use crate::harness::page::BARRIER_BOUND;

/// The host's answer to a page's barrier: whether every write it asked after is saved. A barrier
/// the host never answers fails here by name within [`BARRIER_BOUND`], instead of holding a
/// runtime worker on a blocking receive until the test runner kills the whole test.
#[track_caller]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the answer is the host's verdict on the writes; a barrier never answered is the test's failure"
)]
pub fn saved(barrier: &tilo_session_client::Receipt<Result<(), String>>) -> Result<(), String> {
    match barrier.recv_timeout(tilo_test_env::hang_bound().min(BARRIER_BOUND)) {
        Ok(answer) => answer,
        Err(tilo_session_client::RecvTimeoutError::Timeout) => panic!(
            "timed out after {BARRIER_BOUND:?} waiting for the host to say the page's writes are \
             saved: a barrier that is never answered holds a write the host never took"
        ),
        Err(tilo_session_client::RecvTimeoutError::Disconnected) => {
            panic!("the page's session ended before the host answered its barrier")
        }
    }
}

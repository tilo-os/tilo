//! An open record read through the record API ([`tilo_actions::remote::read_resources`]), as
//! every reader reads one: its content as plain data, whatever app keeps it, with the records
//! its app keeps beside it (a chat's messages, in the chain's parts). A conversation is read so,
//! not through an app's own read.

use std::collections::HashMap;

use tilo_actions::ActionOutput;
use tilo_actions::api::{ActionAddress, ActionDescriptor};
use tilo_actions::remote::{Caller, HeldCopies, ResourceAddress, ResourceAsk};
use tilo_agent::surface;
use tilo_peer_state::document_frame::DocumentRoute;

use crate::worker::ConfinedReplica;

/// The action that reads an open record through the API, beside its `open`
/// ([`surface::open_document_address`]).
const READ_DOCUMENT: &str = "read-record";

/// The address of the API's read of `route`.
fn address(route: &DocumentRoute) -> ActionAddress {
    let open = surface::open_document_address(&route.app_id, &route.document);
    ActionAddress::new(open.resource, READ_DOCUMENT)
}

/// What the open record `route` offers: its app's operations, and the API's read of it, a read
/// that runs while a plan is made.
pub(super) fn offered(route: &DocumentRoute, replica: &ConfinedReplica) -> Vec<ActionDescriptor> {
    let mut offered = replica.survey();
    offered.push(descriptor(route));
    offered
}

/// The API's read of `route`.
fn descriptor(route: &DocumentRoute) -> ActionDescriptor {
    let contract = tilo_actions::ActionContract {
        kind: ui::permissions::policy::ActionKind::Read,
        confirmation: tilo_actions::ConfirmationPolicy::None,
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        },
        ..tilo_actions::ActionContract::default()
    };
    let app = &route.app_id;
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: address(route),
        resource_kind: surface::APPLICATION_KIND.to_owned(),
        resource_label: format!("{app} record {}", route.document),
        action_label: format!("read the {app} record as data"),
        keywords: vec!["messages".into(), "conversation".into()],
        icon: None,
        description: Some(
            "The record as plain data with every record its app keeps beside it: a chat's \
             channels and people, and its messages (each naming its channel, author, time and \
             text) in the parts beside it. How a conversation is read."
                .into(),
        ),
        locus: tilo_actions::ExecutionLocus::ServerPreferred,
        kind: tilo_actions::OperationKind::View,
        contract,
        params: Vec::new(),
        input: tilo_actions::InputSpec::None,
        view: None,
    }
}

/// Whether `address` is the API's read of `route` ([`descriptor`]).
pub(super) fn reads(route: &DocumentRoute, address: &ActionAddress) -> bool {
    *address == self::address(route)
}

/// `replica`'s record of `route`, and every record held beside it, through the API, as
/// `agent` reads them.
pub(super) fn read(
    route: &DocumentRoute,
    replica: &ConfinedReplica,
    agent: &str,
) -> Result<ActionOutput, String> {
    let mut held = HashMap::new();
    let mut asks = Vec::new();
    for record in std::iter::once(replica.document_id().clone()).chain(replica.resources_beside()) {
        let Some(bytes) = replica.read_document_history(&record, automerge::Automerge::save) else {
            continue;
        };
        asks.push(ResourceAsk {
            address: ResourceAddress {
                app: route.app_id.clone(),
                record: record.route().to_owned(),
            },
            joined: false,
        });
        held.insert(record.route().to_owned(), bytes);
    }
    Ok(ActionOutput {
        value: Some(HeldCopies(held).readable(&Caller::local_agent(agent), &asks)?),
        resources: Vec::new(),
    })
}

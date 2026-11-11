//! An open record read through the record door ([`tilo_actions::remote::read_records`]), as
//! every reader reads one: its content as plain data, whatever app keeps it, with the records
//! its app keeps beside it (a chat's messages, in the chain's parts). A conversation is read so,
//! not through an app's own read.

use std::collections::HashMap;

use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_actions::remote::{
    Caller, RecordAddress, RecordAsk, RecordCopy, RecordReads, RecordWake, read_records,
};
use tilo_agent::surface;
use tilo_peer_state::document_frame::DocumentRoute;

use crate::worker::ConfinedReplica;

/// The action that reads an open record through the door, beside its `open`
/// ([`surface::open_record_address`]).
const READ_RECORD: &str = "read-record";

/// The address of the door's read of `route`.
fn address(route: &DocumentRoute) -> ActionAddress {
    let open = surface::open_record_address(&route.app_id, &route.record);
    ActionAddress::new(open.resource, READ_RECORD)
}

/// What the open record `route` offers: its app's operations, and the door's read of it, a read
/// that runs while a plan is made.
pub(super) fn offered(route: &DocumentRoute, replica: &ConfinedReplica) -> Vec<ActionDescriptor> {
    let mut offered = replica.survey();
    offered.push(descriptor(route));
    offered
}

/// The door's read of `route`.
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
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: address(route),
        resource_kind: surface::APPLICATION_KIND.to_owned(),
        resource_label: format!("{app} record {}", route.record),
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

/// Whether `address` is the door's read of `route` ([`descriptor`]).
pub(super) fn reads(route: &DocumentRoute, address: &ActionAddress) -> bool {
    *address == self::address(route)
}

/// The records `replica` holds, as the door reads them.
struct Held(HashMap<String, Vec<u8>>);

impl RecordReads for Held {
    fn read(
        &self,
        _: &Caller,
        address: &RecordAddress,
    ) -> Result<RecordCopy, tilo_actions::remote::ActionHostError> {
        Ok(self
            .0
            .get(&address.record)
            .map_or(RecordCopy::Absent, |bytes| {
                RecordCopy::Document(bytes.clone())
            }))
    }

    /// Read once: what the agent holds now is what it plans with.
    fn watch(
        &self,
        _: &Caller,
        _: &RecordAddress,
        _: &RecordWake,
    ) -> Result<(), tilo_actions::remote::ActionHostError> {
        Ok(())
    }
}

/// `replica`'s record of `route`, and every record held beside it, through the door, as
/// `agent` reads them.
pub(super) fn read(
    route: &DocumentRoute,
    replica: &ConfinedReplica,
    agent: &str,
) -> Result<ActionOutput, String> {
    let mut held = HashMap::new();
    let mut asks = Vec::new();
    for record in std::iter::once(replica.record_id().clone()).chain(replica.records_beside()) {
        let Some(bytes) = replica.read_record_history(&record, automerge::Automerge::save) else {
            continue;
        };
        asks.push(RecordAsk {
            address: RecordAddress {
                app: route.app_id.clone(),
                record: record.route().to_owned(),
            },
            joined: false,
        });
        held.insert(record.route().to_owned(), bytes);
    }
    let readings = read_records(
        &Held(held),
        &Caller::local_agent(agent),
        &asks,
        None,
        tilo_clock::Instant::now(),
    )
    .map_err(|error| error.to_string())?;
    Ok(ActionOutput {
        value: Some(readings.readable()?),
        resources: Vec::new(),
    })
}

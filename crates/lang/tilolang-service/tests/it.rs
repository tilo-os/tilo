//! A workspace's tilolang resources as the session's service: their readings, actions, history,
//! notices and shares, over replicas of one workspace running the real compiled packages.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::needless_pass_by_value,
    dead_code,
    reason = "test assertions; the fixtures are tilolang-tilo's, of which these tests read a part"
)]

#[path = "../../tilolang-tilo/tests/fixtures.rs"]
mod fixtures;

mod app_calls;
mod block_edit;
mod direct_readers;
mod kept_in_workspace;
mod notices;
mod resource_service;
mod share;

use tilolang_service::ResourceService;

/// The service a session keeps in the workspace over a replica's host.
trait IntoService {
    /// The service over this replica's host, for `person`.
    fn into_service(self, person: &str) -> ResourceService;
    /// The service over this replica's host, for `person`, with the workspace still reachable as
    /// the other devices' sync takes it in.
    fn into_service_with_workspace(self, person: &str) -> (ResourceService, fixtures::Workspace);
}

impl IntoService for fixtures::Replica {
    fn into_service(self, person: &str) -> ResourceService {
        ResourceService::new(self.host, person)
    }

    fn into_service_with_workspace(self, person: &str) -> (ResourceService, fixtures::Workspace) {
        (ResourceService::new(self.host, person), self.doc)
    }
}

/// `value`, an object, as an action's arguments.
fn args(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().cloned().unwrap()
}

/// The resource of `kind` the service publishes.
fn published_of(service: &ResourceService, kind: &str) -> tilo_actions::ResourceId {
    use tilo_service::Service as _;
    service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == kind)
        .unwrap_or_else(|| panic!("a {kind} is published"))
        .id
}

/// The resource `action` on `at` made, the action done and its writes settled.
fn made(
    service: &mut ResourceService,
    at: &tilo_actions::ResourceId,
    action: &str,
    arguments: serde_json::Value,
) -> String {
    use tilo_service::Service as _;
    let started = service
        .invoke(at.as_str(), action, &args(arguments))
        .unwrap();
    let tilo_service::Started::Done(output) = started else {
        panic!("a tilolang action is done once committed");
    };
    service.settle();
    output
        .resources
        .first()
        .cloned()
        .expect("the resource it made")
}

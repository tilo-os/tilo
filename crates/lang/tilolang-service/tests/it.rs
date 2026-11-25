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

//! The action broker's half that every host runs: who takes a step, what an app publishes, the
//! steps a host serves, the receipts a caller reads, and the route an app's traffic is addressed
//! on.
//!
//! A host serves the person's actions — the ones its apps publish, and the ones it runs itself
//! ([`steps`]) — and admits each one as an [`actor`]. Which process or module carries a step out,
//! and over which socket an app is asked, is that host's own: a Linux desktop's broker over its
//! app sockets, the page over the apps it runs in-process. The session's roles name only what is
//! here: [`services`] is what a role installs and reads, [`access`] what an app declared.

pub mod access;
pub mod actor;
pub mod custody;
pub mod published;
pub mod route;
pub mod services;
pub mod steps;

pub use access::{AppActionAccess, AppActionAccessResolver};
pub use actor::{BrokerActor, RunFacts, principal_id_for_root};
pub use published::{
    PublishedAction, PublishedInput, PublishedSurface, validate_published_surface,
};
pub use route::CrdtRoute;
pub use steps::{
    ContinuationServices, HostStepOutcome, HostStepRequest, HostStepServices, LiveRead, LiveView,
    TaskRun,
};

// Workspace records are decoded in automerge worker processes started from this test binary;
// this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

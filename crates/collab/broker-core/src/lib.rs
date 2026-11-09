//! The action broker's half that every host runs: what a step is taken by, the steps a host
//! serves, the receipts a caller reads, and the route an app's traffic is addressed on.
//!
//! A host serves the person's actions — the ones its apps publish, and the ones it runs itself
//! ([`steps`]). Which process or module carries a step out, and over which socket an app is asked,
//! is that host's own: a Linux desktop's broker over its app sockets, the page over the apps it
//! runs in-process. The session's roles name only what is here.

pub mod route;
pub mod steps;

pub use route::CrdtRoute;
pub use steps::{
    ContinuationServices, HostStepOutcome, HostStepRequest, HostStepServices, LiveRead, LiveView,
    StepActor, TaskRun,
};

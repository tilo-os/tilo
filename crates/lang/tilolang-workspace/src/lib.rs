//! A tilolang app's record of a workspace, as a device's own workspace service reaches it: the
//! records of the apps built into this build, the styles a device's installed packages offer, and
//! the rich text an app's actions take.
//!
//! The workspace service names no language ([`tilo_workspace_service::tl_records`]): it keeps the
//! store and the policy, and the daemon's program hands it a value that answers the rest
//! ([`host`]). This crate is that value, the one place the service's record model and the
//! language runtime meet.

pub mod notices;
pub mod records;
pub mod rich;
pub mod styles;

use std::sync::Arc;

pub use tilolang_tilo::naming;
pub use tilolang_tilo::publish;
pub use tilolang_tilo::runtime;

use tilo_workspace_service::tl_records::Tilolang;

/// What answers for the workspace service: one value, the records, styles and rich text read
/// through this build's language runtime.
#[derive(Default)]
pub struct Service {
    /// The records read here, each kept open between asks for the person reading it.
    kept: records::Kept,
}

/// What the daemon's program hands its workspace service, as it starts.
#[must_use]
pub fn host() -> Tilolang {
    let service: Arc<Service> = Arc::new(Service::default());
    Tilolang {
        records: service.clone(),
        styles: service.clone(),
        rich_text: service,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
tilo_automerge_isolate::test_worker!();

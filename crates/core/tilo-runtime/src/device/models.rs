//! The workspace's model providers as this device's daemon publishes them to the apps attached
//! to it: the providers a workspace keeps, the models each offers, the model each kind of work
//! runs on and the providers a person may add, read as `models`.
//!
//! The reading is the workspace service's, which this daemon holds ([`Workspaces`]): the same
//! list its `tilo.models` action answers, and no key in it. A watch follows the accounts' and
//! providers' changes, so a window listing the providers draws again as one is added, changed or
//! removed anywhere. A key travels only into `set-key`, and this publication never carries one.

use std::sync::Arc;

use super::reading::{Changes, Read, Served};

pub use tilo_actions::remote::model_providers::RESOURCE as MODELS;

/// The workspace's model providers, published to the apps this device serves, read where the
/// workspaces keep them.
#[must_use]
pub fn published(workspaces: &Arc<crate::device::workspaces::ServiceWorkspaces>) -> Served {
    let reading = Arc::clone(workspaces);
    let changing = Arc::clone(workspaces);
    let read: Arc<Read> = Arc::new(move |caller: &tilo_app_host::services::Caller| {
        reading.copies().models_reading(&caller.workspace)
    });
    let changes: Arc<Changes> = Arc::new(move || changing.copies().person_settings_changes());
    Served::new(MODELS, read, changes)
}

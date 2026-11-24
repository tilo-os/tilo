//! The `tilo-runtime` program: this computer's one Tilo daemon. The device daemon itself is the
//! core's (`tilo_runtime`); this program mounts on it what the core leaves optional
//! ([`capabilities`]), each through the daemon's one registration
//! ([`tilo_runtime::device::capabilities`]):
//!
//! - the device's image model, a provider on the daemon's dispatch;
//! - the language runtime, whose records, styles and rich text the workspace service reads and
//!   changes the built-in apps through;
//! - the account sign-ins a login's cloud service, model manager and origin connectors reach
//!   ([`account_sign_ins`]);
//! - on Linux, the hold on suspend a request from another of the person's devices takes, and
//!   whether a Wi-Fi radio wakes its machine, as the OS actions read it.

use std::sync::Arc;

use tilo_actions::remote::Caller;
use tilo_runtime::device::capabilities::{Capabilities, Mounting, SignInsServed};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::Reach;

pub mod account_sign_ins;
#[cfg(target_os = "linux")]
mod stay_awake;

/// What this program mounts on the daemon it runs.
#[must_use]
pub fn capabilities() -> Capabilities {
    Capabilities {
        // The language runtime: the daemon's own roles read and change the records of the apps
        // built into the build through it, and offer the styles the device's packages declare.
        tilolang: Some(Box::new(tilolang_workspace::host)),
        providers: vec![Box::new(image_model)],
        #[cfg(target_os = "linux")]
        stay_awake: Some(Box::new(|| Box::pin(stay_awake::hold()))),
        #[cfg(not(target_os = "linux"))]
        stay_awake: None,
        account_sign_ins: Some(Box::new(account_sign_ins_here)),
        #[cfg(target_os = "linux")]
        radio_wakes: Some(os_actions::network_wake::wifi_magic_packet),
        #[cfg(not(target_os = "linux"))]
        radio_wakes: None,
    }
}

/// The device's image model, which apps ask by the addresses it publishes rather than through a
/// connection of their own: one copy on the daemon's dispatch.
fn image_model(at: &Mounting<'_>) {
    use tilo_vision_service::device::{APP, ImageModel, PROVIDER};
    if let Ok(model) = ImageModel::start(at.blob_socket, at.search.clone(), at.person) {
        at.providers
            .provide(PROVIDER, APP, Reach::ThisDevice, Arc::new(model));
    } else {
        tracing::warn!("this device runs no image model");
    }
}

/// Serve the account sign-ins below this login's runtime directory, as `person`: `None`, said in
/// the log, where there is no runtime directory or the socket cannot be made there, and the cloud
/// service and origin connectors then reach no account's sign-in.
fn account_sign_ins_here(workspaces: &Workspaces, person: Caller) -> Option<SignInsServed> {
    let Some(path) = accounts::sign_ins::socket() else {
        tracing::warn!(
            "no runtime directory, so the cloud service and origin connectors reach no account's \
             sign-in"
        );
        return None;
    };
    let Some(models) = accounts::sign_ins::models_socket() else {
        tracing::warn!("no runtime directory, so the model manager reaches no account's sign-in");
        return None;
    };
    let served = account_sign_ins::serve(&path, &models, workspaces.clone(), person)
        .inspect_err(|error| {
            tracing::warn!(%error, path = %path.display(), "the account sign-ins socket is not served");
        })
        .ok()?;
    Some(SignInsServed {
        hands: Box::new(served.hands()),
        served: Box::new(served),
    })
}

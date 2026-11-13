//! The model that names what a run of changes was: the one the person's AI settings choose for
//! Activity, else their default, asked through the host's model gateway, which may ask a completion
//! without tools and nothing more. A session with no provider configured never asks, and its
//! history keeps the deterministic digest.

use tilo_desktop_surface::activity::describer::{self, Job};

use crate::platform::Models;

/// The package the describer asks as, as every question this session asks does.
#[cfg(target_os = "linux")]
pub(crate) use crate::search_ask::PACKAGE;

/// Ask the model `job`'s question through `models`: the answer's text, or nothing when no model is
/// available or the ask failed. Nothing about a failure reaches the document.
#[allow(
    clippy::future_not_send,
    reason = "asked on the app runtime's own thread, whose model gateway handle is not Send"
)]
pub(crate) async fn answer(models: Option<&dyn Models>, job: &Job) -> String {
    let Some(models) = models.and_then(Models::models) else {
        return String::new();
    };
    let request = job.request();
    models
        .complete(&request)
        .await
        .map(|response| describer::answer_text(&response))
        .unwrap_or_default()
}

/// The model gateway this desktop runs, as a question asked here reaches it.
#[cfg(target_os = "linux")]
pub(crate) struct Gateway;

#[cfg(target_os = "linux")]
impl Models for Gateway {
    fn models(&self) -> Option<ui::models::Models> {
        models()
    }
}

/// The model Activity asks, as a capability of this session; `None` while settings choose none or
/// no gateway runs in this session.
#[cfg(target_os = "linux")]
pub(crate) fn models() -> Option<ui::models::Models> {
    let config = agent_provider_config::ProviderConfig::load();
    let (profile_id, model_id) = config
        .selection_for(crate::search_ask::PACKAGE)
        .map(|(profile, model)| (profile.id.clone(), model.id.clone()))?;
    let gateway = gateway()?;
    match tilo_model_client::runtime_client::from_descriptor_selection(
        std::os::fd::AsFd::as_fd(&gateway),
        profile_id,
        model_id,
    ) {
        Ok(backend) => Some(ui::models::Models::new(backend)),
        Err(error) => {
            tracing::warn!(%error, "the history is described without a model");
            None
        }
    }
}

/// A connection to this session's model gateway, made for Activity the way the launcher makes one
/// for an app it starts.
#[cfg(target_os = "linux")]
fn gateway() -> Option<std::os::fd::OwnedFd> {
    let name = launcher::services::MODEL_GATEWAY_SERVICE;
    let requirement = launcher::manifest::ServiceReq::parse(
        name,
        &format!("optional@{}", runtime_portal::RUNTIME_INTERFACE_VERSION),
    )
    .ok()?;
    let wanted = std::collections::BTreeMap::from([(name.to_owned(), requirement)]);
    let context = launcher::services::ServiceLaunchContext {
        app_id: PACKAGE,
        workspace_id: None,
    };
    match launcher::services::resolve_service_binds_for_launch(&wanted, false, context) {
        Ok(resolved) => resolved.into_iter().next().map(|service| service.host),
        Err(error) => {
            tilo_failure::ignored!(
                "a session with no model gateway describes the history without one",
                tracing::debug!(%error, "no model gateway for describing the history")
            );
            None
        }
    }
}

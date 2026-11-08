//! The model that names what a run of changes was: the one the person's AI settings choose for
//! Activity, else their default, asked through the model gateway as Activity, which may ask a
//! completion without tools and nothing more. A session with no provider configured never asks, and
//! its history keeps the deterministic digest.

use agent_provider_config::ProviderConfig;
use tilo_desktop_surface::activity::describer;

/// The package the describer asks as: what the person's AI settings can choose a model for, and
/// what the gateway authorizes for a plain completion.
pub(crate) const PACKAGE: &str = "so.tilo.activity";

/// Whether the person chose any model for Activity: what the describer asks of the desk's thread,
/// before a request is worth building.
pub(crate) fn configured() -> bool {
    ProviderConfig::load().selection_for(PACKAGE).is_some()
}

/// Ask the model `job`'s question, on a thread of its own: the answer's text,
/// or nothing when no model is configured or the ask failed. Nothing about a
/// failure reaches the document.
pub(crate) fn ask(job: &tilo_desktop_surface::activity::describer::Job) -> Option<String> {
    let models = models()?;
    let request = job.request();
    let answer = tilo_rt::try_app()
        .ok()?
        .block_on(async move { models.complete(&request).await.ok() })
        .ok()??;
    Some(describer::answer_text(&answer))
}

/// The model Activity asks, as a capability of this session; `None` while settings choose none or
/// no gateway runs in this session.
pub(crate) fn models() -> Option<ui::models::Models> {
    let config = ProviderConfig::load();
    let (profile_id, model_id) = config
        .selection_for(PACKAGE)
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
            // ignored-failure: a session with no model gateway describes the history without one
            tracing::debug!(%error, "no model gateway for describing the history");
            None
        }
    }
}

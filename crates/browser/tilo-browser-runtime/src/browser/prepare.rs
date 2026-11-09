//! What a mount is prepared with before it draws: the stores, identity and settings its desk
//! mounts with, its history, and the session that serves its windows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use ui::application_view::InstanceViewStore;
use ui::permissions::SharedPermissions;
use ui::record::RecordOwners;
use wasm_bindgen::JsValue;

use super::{Config, error};

/// What the page's desktop mounts with: its stores, identity and settings.
pub(super) fn desktop_inputs(
    config: &Config,
    dyn_store: &Rc<dyn durable_store::DurableStore>,
    permissions: SharedPermissions,
) -> Result<crate::desktop::Inputs, JsValue> {
    let views = Rc::new(
        InstanceViewStore::with_store(Rc::clone(dyn_store), &config.device, &config.workspace)
            .map_err(error)?,
    );
    Ok(crate::desktop::Inputs {
        feedback: config.peer.and_then(crate::feedback_code::take_startup),
        owners: RecordOwners::with_store(Rc::clone(dyn_store)),
        store: Rc::clone(dyn_store),
        views,
        device: config.device.clone(),
        salt: config
            .peer
            .and_then(crate::replication::device_of)
            .unwrap_or_else(|| config.device.clone()),
        workspace: config.workspace.clone(),
        author: config.author(),
        permissions,
        models: config.models()?,
        model_name: config
            .models
            .as_ref()
            .map(crate::host_services::ModelConfig::name)
            .unwrap_or_default(),
        provider_admin: config.provider_admin()?,
        peer: config.peer,
    })
}

/// What the page offers its desktop beyond the workspace: inviting, its host and build, the About
/// sheet's links and the layout it opens in.
pub(super) fn host_offers(config: &Config) -> crate::shell::HostOffers {
    crate::shell::HostOffers {
        invite: config.invite.clone(),
        mac: config.mac,
        host: config.host.clone(),
        about: config.about.clone(),
        build: config.build.clone(),
        link_offer: config.link_offer.clone(),
        directory: config.directory.clone(),
        layout: config.layout,
    }
}

/// What the page's desk asks its history: the workspace's own records, the model this page may ask,
/// and who the people its places name are.
///
/// The model is the host's, else a provider the person set up here: without one nothing is asked
/// and the history keeps its deterministic digest.
pub(super) fn serve_history(
    surface: &mut tilo_desktop_surface::DesktopSurface,
    config: &Config,
    store: Rc<dyn durable_store::DurableStore>,
    shared: &crate::desktop::Shared,
    persistence: &Rc<crate::desktop::Persistence>,
    here: &tilo_desktop::here::Here,
    people: &Rc<RefCell<HashMap<String, crate::desktop::PersonIdentity>>>,
) {
    let models = (config.models.is_some() || crate::host_services::page_models_set_up())
        .then(|| config.models().ok())
        .flatten();
    surface.set_history_source(Box::new(crate::history::PageHistory::new(
        &config.workspace,
        config.peer,
        config.author(),
        store,
        shared.clone(),
        crate::history::person_of(here, people, config.author()),
        Rc::clone(persistence),
        models,
    )));
}

/// The session that serves this desktop's windows, what it opens, its agent door, and the
/// notifications the desk posts: served before the chrome reads any of them.
pub(super) fn serve_session(
    surface: &mut tilo_desktop_surface::DesktopSurface,
    config: &Config,
    shared: &crate::desktop::Shared,
    persistence: &crate::desktop::Persistence,
) -> (
    Arc<tilo_desktop_surface::session::DesktopSession>,
    tilo_desktop_surface::session::Openings,
    crate::agent_door::Revoked,
) {
    let (session, openings) =
        tilo_desktop_surface::session::serve(config.workspace.clone(), Arc::clone(&shared.woken));
    surface.serve_activity(openings.activity());
    let revoked = config.agent_door(&session, &shared.woken);
    surface.set_notifications(persistence.notifications.clone());
    if let Some(inbox) = crate::host_services::notification_inbox() {
        surface.set_app_notifications(inbox);
    }
    (session, openings, revoked)
}

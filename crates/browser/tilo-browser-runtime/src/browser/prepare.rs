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
        content: content(config),
    })
}

/// The content store a mount lends its windows: its workspace's, or one of its own outside any.
pub(super) fn content(config: &Config) -> tilo_content::ContentStore {
    config
        .peer
        .and_then(crate::replication::content)
        .unwrap_or_else(crate::page_content::store)
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

/// What the page's desk asks its history: the session's activity over this page's store, its apps
/// and the people presence knows, and the model this page may ask.
///
/// The model is the host's, else a provider the person set up here: without one nothing is asked
/// and the history keeps its deterministic digest. Only where this page's copy is the desk: in a
/// workspace the desk is the workspace's document, and its metadata reaches everyone from the
/// device that writes it there.
pub(super) fn serve_history(
    surface: &mut tilo_desktop_surface::DesktopSurface,
    config: &Config,
    store: Rc<dyn durable_store::DurableStore>,
    shared: &crate::desktop::Shared,
    persistence: &Rc<crate::desktop::Persistence>,
    here: &tilo_desktop::here::Here,
    people: &Rc<RefCell<HashMap<String, crate::desktop::PersonIdentity>>>,
) -> tilo_session::activity::Activity {
    let describes = config.peer.is_none()
        && (config.models.is_some() || crate::host_services::page_models_set_up());
    let models = describes.then(|| {
        Arc::new(PageModels(config.models.clone())) as Arc<dyn tilo_session::platform::Models>
    });
    let people = crate::page_activity::person_of(here, people, config.author());
    let activity = tilo_session::activity::Activity::over(tilo_session::activity::Given {
        desk: tilo_session::desk_doc::DeskDoc::over(persistence.desk()),
        person: config.author(),
        store: Some(store),
        state: None,
        apps: Some(Arc::new(crate::page_apps::PageApps)),
        writers: crate::page_activity::writers(people.clone(), &config.author()),
        people,
        pane: Rc::new(crate::page_activity::PagePane {
            desktop: shared.clone(),
        }),
        models,
        changes: crate::replication::changes(),
        wake: None,
    });
    surface.set_history_source(Box::new(activity.clone()));
    activity
}

/// The page's model service, as the session's own questions ask it: its models hold the page's own
/// handles, so each question builds the capability where it is asked.
struct PageModels(Option<crate::host_services::ModelConfig>);

impl tilo_session::platform::Models for PageModels {
    fn models(&self) -> Option<ui::models::Models> {
        match &self.0 {
            Some(config) => config.connect().ok(),
            None => Some(crate::host_services::page_models()),
        }
    }
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

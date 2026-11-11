//! A page showing one application alone, and the record it opens there.
//!
//! A page names one app instead of mounting the session desktop, and its window
//! shows that app on one record. In a workspace whose host sends a device a
//! record only once it is opened, that record is not here when the page mounts:
//! the window shows the app arriving, and the component is built once the
//! workspace sent the record or answered it holds none of it — the same wait a
//! window on the desktop's shared desk takes.

use std::cell::RefCell;
use std::rc::Rc;

use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::application_view::InstanceViewStore;
use ui::permissions::{SharedPermissions, UserId};
use ui::record::{RecordOwners, RecordStore};
use wasm_bindgen::JsValue;

use super::{Config, Presentation, Root, error};
use crate::workspace_records::in_page;

/// A page showing one application, and the record it opens there.
///
/// In a workspace whose host sends a device a record only once it is opened,
/// the record a page opens is not here yet when the page mounts: the window
/// shows the app arriving, and [`Self::present`] builds the component once the
/// workspace sent the record or answered it holds none of it. Opening it
/// before founded a fresh document, and its first publication overwrote or
/// forked the workspace's own.
pub(super) struct AppPage {
    definition: &'static ui::application::ApplicationDefinition,
    config: Rc<Config>,
    dyn_store: Rc<dyn durable_store::DurableStore>,
    permissions: SharedPermissions,
    /// Every record owner the page opened, so the apps a window opens beside it
    /// share what it has.
    owners: RefCell<RecordOwners>,
    /// The app's store, kept with what the workspace replicates into it: the
    /// same store a waiting window comes back to.
    stores: Rc<RefCell<Vec<RecordStore>>>,
    /// The content store the page lends the app's window.
    content: tilo_content::ContentStore,
}

impl AppPage {
    fn of(
        definition: &'static ui::application::ApplicationDefinition,
        config: Rc<Config>,
        dyn_store: Rc<dyn durable_store::DurableStore>,
        permissions: SharedPermissions,
    ) -> Self {
        Self {
            definition,
            content: super::prepare::content(&config),
            config,
            owners: RefCell::new(RecordOwners::with_store(Rc::clone(&dyn_store))),
            dyn_store,
            permissions,
            stores: Rc::default(),
        }
    }

    /// The app's store, whose records come from the workspace.
    fn store(&self) -> Result<RecordStore, JsValue> {
        crate::app_page::store(
            &mut self.owners.borrow_mut(),
            self.definition,
            &self.config.workspace,
            self.config.peer.is_some(),
            &self.stores,
        )
        .map_err(error)
    }

    /// Whether what this page opens is still on its way from the workspace: its
    /// window shows the app arriving until it is here, or until the workspace
    /// answered it holds none of it.
    pub(super) fn arriving(&self) -> bool {
        let Ok(store) = self.store() else {
            return false;
        };
        matches!(
            crate::app_page::opening(
                self.definition,
                &store,
                self.config.record.as_deref(),
                self.config.new_document,
                self.config.instance,
            ),
            Ok(crate::app_page::Opening::Arriving)
        )
    }

    /// The app's window, on the record the page opens — or, while that record is
    /// on its way, the window arriving until it is here.
    pub(super) fn present(self: &Rc<Self>) -> Result<Presentation, JsValue> {
        let config = &self.config;
        let definition = self.definition;
        let record = self.store()?;
        let opening = crate::app_page::opening(
            definition,
            &record,
            config.record.as_deref(),
            config.new_document,
            config.instance,
        )
        .map_err(|unopened| match unopened {
            crate::app_page::Unopened::InvalidRecord => error("invalid record id"),
            crate::app_page::Unopened::Records(open) => error(open),
        })?;
        let (target, named_new) = match opening {
            crate::app_page::Opening::Target(target) => (target, None),
            crate::app_page::Opening::New(id) => (LaunchTarget::New, Some(id)),
            crate::app_page::Opening::Arriving => return Ok(self.arrival()),
        };
        let views = Rc::new(
            InstanceViewStore::with_store(
                Rc::clone(&self.dyn_store),
                &config.device,
                &config.workspace,
            )
            .map_err(error)?,
        );
        let restore = views
            .load(config.instance)
            .map_err(error)?
            .map_or(RestoreInput::None, RestoreInput::Local);
        let inputs = HostContextInputs {
            records: record,
            views,
            device_id: config.device.clone(),
            instance_id: config.instance,
            permissions: self.permissions.clone(),
            author: UserId(config.author()),
            action_context: tilo_actions::ActionContext::default(),
        };
        let prepared = match named_new {
            Some(id) => HostContext::prepare_new_record(definition, inputs, id),
            None => HostContext::prepare(definition, inputs, &target),
        };
        let mut context = match prepared {
            // A named record the workspace still sends: the window waits for it
            // like one this page named itself.
            Err(ui::launch::LaunchError::Record(ui::record::RecordOpenError::Arriving)) => {
                return Ok(self.arrival());
            }
            prepared => prepared.map_err(error)?,
        };
        context.provide_models(config.models()?);
        context.provide_content(self.content.clone());
        context.provide_workspace_records(in_page(
            &self.owners.borrow(),
            &self.stores,
            &config.workspace,
            config.author(),
            config.peer.is_some(),
        ));
        crate::person_records::provide(&mut context, &self.dyn_store).map_err(error)?;
        config.lend_provider_admin(&mut context)?;
        let loaders = Rc::new(RefCell::new(Vec::new()));
        let image_owners = crate::page_content::ImageOwners::default();
        crate::page_content::serve_window(&context.wake_handle(), &loaders, &image_owners);
        let component = definition
            .instantiate(context, target, restore)
            .map_err(error)?;
        let checkout = component.record().clone();
        Ok(Presentation {
            root: Root::Component(Box::new(component)),
            stores: Rc::clone(&self.stores),
            checkout: Some(checkout),
            images: loaders,
            image_owners,
            desktop: None,
            waiting: None,
        })
    }

    /// The page as it stands while what it opens is on its way: the app's window
    /// shows its arrival — the place it reserves, with the shape it declared for
    /// it — and [`Self::present`] builds it once the record is here.
    fn arrival(self: &Rc<Self>) -> Presentation {
        let shown = ui::widget::TileHost::new(ui::widget::TileContent::Arriving)
            .arriving_from(crate::desktop::code_host())
            .with_arrival_shape(ui::application::registry::declared_arrival_shape(
                self.definition.app_id(),
            ));
        Presentation {
            root: Root::Arriving(Box::new(shown)),
            stores: Rc::clone(&self.stores),
            checkout: None,
            images: Rc::default(),
            image_owners: crate::page_content::ImageOwners::default(),
            desktop: None,
            waiting: Some(Rc::clone(self)),
        }
    }
}

pub(super) fn present_component(
    definition: &'static ui::application::ApplicationDefinition,
    config: Rc<Config>,
    dyn_store: &Rc<dyn durable_store::DurableStore>,
    permissions: SharedPermissions,
) -> Result<Presentation, JsValue> {
    if definition.app_id() == tilo_desktop::feedback::PACKAGE {
        return present_private_feedback(&config);
    }
    Rc::new(AppPage::of(
        definition,
        config,
        Rc::clone(dyn_store),
        permissions,
    ))
    .present()
}

fn present_private_feedback(config: &Config) -> Result<Presentation, JsValue> {
    // The page loads a deferred composer's code before mounting it alone
    // (`load_application`).
    let code = crate::feedback_composer::code()
        .ok_or_else(|| error("the Feedback composer's code is not loaded"))?;
    let component =
        (code.prepare)(None, &config.device, &config.author(), config.instance).map_err(error)?;
    Ok(Presentation {
        root: Root::Component(Box::new(component)),
        stores: Rc::default(),
        checkout: None,
        images: Rc::default(),
        image_owners: crate::page_content::ImageOwners::default(),
        desktop: None,
        waiting: None,
    })
}

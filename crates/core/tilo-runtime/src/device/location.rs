//! Where this device is, as the device answers it: provider `so.tilo.device.location` on
//! `devices/<device>/location` (`locate`).
//!
//! The platform's location service is the device's to reach: an app runs sandboxed and speaks to
//! no D-Bus and no portal. A request is admitted through the device's one dispatch under the
//! caller's grants, then answered here. A program that declared the `location` capability, and
//! the person themselves, are answered. A program that did not is asked through the dispatch's
//! asked record the first time and answered once the person says yes; nothing is answered
//! silently. The person's lockdown policy turns location off for everyone, the person included.

use std::sync::Arc;

use serde_json::json;
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_actions::{ActionOutput, InvocationUpdate};

/// The provider the device's location is published under.
pub const PROVIDER: &str = "so.tilo.device.location";

/// The kind of the device's location resource.
pub const RESOURCE_KIND: &str = "devices.location";

/// The action that reads where the device is.
pub const LOCATE: &str = "locate";

/// The `location` capability an app declares to be answered without asking.
pub const CAPABILITY: &str = "location";

/// The resource a device's location is published on.
#[must_use]
pub fn resource(device: &[u8; 32]) -> String {
    format!("devices/{}/location", hex::encode(device))
}

/// Where the device is, as the platform reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
    /// Reported horizontal accuracy in metres; `0` when unknown.
    pub accuracy_m: f64,
}

/// The platform's location service, as the daemon reaches it.
pub trait Locate: Send + Sync {
    /// This device's position now.
    ///
    /// # Errors
    /// Why no position could be had: no service, permission denied, no fix.
    fn locate(&self) -> Result<Position, String>;
}

/// This target's location service (`tilo_location`).
pub struct System;

impl Locate for System {
    fn locate(&self) -> Result<Position, String> {
        match tilo_location::system()
            .locate()
            .recv()
            .map_err(|_| "the location service went away".to_owned())?
        {
            tilo_location::LocationOutcome::Located {
                lat,
                lon,
                accuracy_m,
            } => Ok(Position {
                latitude: lat,
                longitude: lon,
                accuracy_m,
            }),
            tilo_location::LocationOutcome::Unavailable(reason) => Err(reason),
        }
    }
}

/// The device's location, as its actions publish it.
#[derive(tilo_resource::Row)]
#[row(kind = "devices.location", label = "Location")]
struct LocationRow;

/// What a caller asks of the device's location.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.device.location")]
pub enum LocationAction {
    /// Where this device is now, as its latitude and longitude.
    #[effect(kind = "outside", returns)]
    Locate,
}

/// This device's location as an [`ActionProvider`].
pub struct Location {
    resource: String,
    resources: Vec<tilo_actions::ResourceSpec>,
    locate: Arc<dyn Locate>,
    /// Whether `app` declared the `location` capability.
    declared: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Whether the person turned location off for everything on this device.
    switched_off: Arc<dyn Fn() -> bool + Send + Sync>,
}

fn resources(resource: &str) -> Vec<tilo_actions::ResourceSpec> {
    let mut spec = tilo_resource::offer::<LocationAction>(tilo_resource::typed_resource::<
        LocationRow,
    >(resource, "Location"));
    spec.app_id = Some(PROVIDER.to_owned());
    vec![spec]
}

impl Location {
    /// The location of the device published on `resource`, read through `locate`, answered
    /// without asking to an app `declared` says holds the capability, and switched off wherever
    /// `switched_off` says so.
    #[must_use]
    pub fn new(
        resource: String,
        locate: Arc<dyn Locate>,
        declared: Arc<dyn Fn(&str) -> bool + Send + Sync>,
        switched_off: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            resources: resources(&resource),
            resource,
            locate,
            declared,
            switched_off,
        }
    }

    /// The device `device`'s own location, read through `locate`, with apps' declarations read
    /// from this daemon's installed packages and the person's lockdown policy.
    #[must_use]
    pub fn this_device(
        device: &[u8; 32],
        locate: Arc<dyn Locate>,
        declared: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Self {
        Self::new(
            resource(device),
            locate,
            declared,
            Arc::new(|| settings_store::lockdown::LockdownPolicy::get().disable_location),
        )
    }

    /// The app a call is made for, when it is one: the program the caller acts through.
    fn app_of(caller: &Caller) -> Option<String> {
        caller.acting().app.as_ref().map(ToString::to_string)
    }

    /// What a program that did not declare location is asked, before it is answered: the
    /// dispatch keeps the call as its asked record, and sends it back with the person's yes.
    fn ask(caller: &Caller) -> Option<ActionCallError> {
        let app = Self::app_of(caller)?;
        Some(ActionCallError::ConfirmationRequired(Box::new(
            AskedFacts {
                by: tilo_actions::remote::asked::AskedBy::App {
                    app: app.clone(),
                    title: app,
                    author: None,
                },
                words: "Use your location".to_owned(),
                ..AskedFacts::default()
            },
        )))
    }

    /// The answer to one `locate`.
    fn answered(&self) -> Result<ActionOutput, ActionCallError> {
        let at = self
            .locate
            .locate()
            .map_err(|reason| ActionCallError::Refused { reason })?;
        Ok(ActionOutput {
            value: Some(json!({
                "lat": at.latitude,
                "lon": at.longitude,
                "accuracy_m": at.accuracy_m,
            })),
            resources: Vec::new(),
        })
    }
}

impl ActionProvider for Location {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.resources.clone()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::published(
            PROVIDER,
            &self.resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let address = &request.target.address;
        if request.target.provider != PROVIDER
            || address.resource != self.resource
            || address.action != LOCATE
        {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        if (self.switched_off)() {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "Location is turned off on this device".to_owned(),
            }));
        }
        // A program that did not declare location is asked, unless this is the call the person's
        // yes sent back. The person themselves is answered: the setting is their own.
        if request.approval().is_none()
            && let Some(app) = Self::app_of(caller)
            && !(self.declared)(&app)
        {
            return Pending::ready(Err(Self::ask(caller).unwrap_or_else(|| {
                ActionCallError::Refused {
                    reason: "Location is not available to this caller".to_owned(),
                }
            })));
        }
        let answer = match self.answered() {
            Ok(output) => InvocationState::new(
                request.invocation,
                InvocationUpdate::Succeeded {
                    invocation: request.invocation,
                    output,
                },
            )
            .receipt(),
            Err(error) => return Pending::ready(Err(error)),
        };
        Pending::ready(Ok(answer))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

#[cfg(test)]
#[path = "location_tests.rs"]
mod tests;

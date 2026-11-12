//! The apps an app uses that are apps of their own: Today's cards over Tasks, Notes and Chat.
//!
//! A used package installed as an app keeps its records in that app's own record, never in the
//! record of the app that uses it. The using app's runtime loads its package as another app's
//! ([`Host::load_foreign_package`]): its views draw the resources from their readings, and what
//! they change is asked of that app's door. Each used app is read here from its record, held
//! read-only, through its own package ([`UsedApp`]), and the readings the using app's views ask
//! for are answered from it ([`answer`]), again whenever the record moves.

use std::collections::BTreeMap;

use crate::host::{Host, HostError};
use crate::records_service::{app_of, reading_of};
use crate::runtime::{Meta, Principal, ResourceRef, Runtime};

/// One used app, read from its own record through its own package.
pub struct UsedApp {
    app: String,
    host: Host,
    /// The resources whose readings the using app's runtime asked for, by published id, each
    /// with its type (`<app>#<Type>`): what is read again when the record moves.
    handed: BTreeMap<String, String>,
}

impl UsedApp {
    /// The app of `packages`' last package, read for `person`: the packages before it are those
    /// it uses that its record holds, in the order they load.
    ///
    /// # Errors
    /// A package does not load, or `packages` is empty.
    pub fn new(packages: &[(&[u8], &Meta)], person: &str) -> Result<Self, HostError> {
        let (_, last) = packages
            .last()
            .ok_or_else(|| HostError::NotFound("a used app's package".to_owned()))?;
        let host = Host::new(&tilo_resource::Doc::new(), Principal::new(person), || None)?;
        for (wasm, meta) in packages {
            host.load_module(wasm, (*meta).clone(), String::new())?;
        }
        Ok(Self {
            app: last.app.clone(),
            host,
            handed: BTreeMap::new(),
        })
    }

    /// The app's id.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The runtime the record is read through.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Read again every resource handed to `runtime`, the record having moved. Whether a
    /// reading changed.
    pub fn refresh(&self, runtime: &Runtime) -> bool {
        let mut moved = false;
        for (id, ty) in &self.handed {
            moved |= self.hand(runtime, id, ty);
        }
        moved
    }

    /// Hand `runtime` the reading of `id`, a resource of type `ty`. A resource the record does
    /// not hold (yet) is left reading as it would start. Whether its reading changed.
    fn hand(&self, runtime: &Runtime, id: &str, ty: &str) -> bool {
        let Some(reading) = self.host.with_schemas(|| reading_of(&self.host, id)) else {
            return false;
        };
        let Some(target) = ResourceRef::parse_id(id, ty.to_owned()) else {
            return false;
        };
        runtime
            .set_reading(&target, &reading.to_string())
            .unwrap_or_else(|error| {
                tracing::warn!(app = self.app, id, %error, "a used app's reading was refused");
                false
            })
    }
}

/// Answer the readings `runtime`'s views asked for of resources of the apps in `used`. A
/// resource of an app not among them stays as it would start. Whether a reading changed.
pub fn answer(runtime: &Runtime, used: &mut [&mut UsedApp]) -> bool {
    let mut moved = false;
    for (id, ty) in runtime.take_wanted() {
        let Some(app) = app_of(&ty) else {
            continue;
        };
        let Some(used) = used.iter_mut().find(|used| used.app == app) else {
            tracing::debug!(id, ty, "a view read a resource of an app it does not use");
            continue;
        };
        moved |= used.hand(runtime, &id, &ty);
        used.handed.insert(id, ty);
    }
    moved
}

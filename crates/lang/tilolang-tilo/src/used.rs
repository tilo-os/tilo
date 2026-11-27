//! The apps an app uses that are apps of their own: Today's cards over Tasks, Notes and Chat.
//!
//! A used package installed as an app keeps its resources as that app's, never as the app that
//! uses it. The using app's runtime loads its package as another app's
//! ([`Host::load_foreign_package`]): its views draw the resources from their readings, and what
//! they change is asked of that app's api. Each used app is read here from the workspace's store
//! through its own package ([`UsedApp`]), and the readings the using app's views ask for are
//! answered from it ([`answer`]), again whenever the store moves.

use std::collections::BTreeMap;

use crate::host::{Host, HostError};
use crate::readings::{app_of, reading_of};
use crate::resources::ResourceStore;
use crate::runtime::{Actor, Meta, ResourceRef, Runtime};

/// One used app, read from the workspace's store through its own package.
pub struct UsedApp {
    app: String,
    host: Host,
    /// The resources whose readings the using app's runtime asked for, by published id, each
    /// with its type (`<app>#<Type>`): what is read again when the store moves.
    handed: BTreeMap<String, String>,
}

impl UsedApp {
    /// The app of `packages`' last package, read from `store` for `person`: the packages before
    /// it are those it uses, in the order they load.
    ///
    /// # Errors
    /// A package does not load, or `packages` is empty.
    pub fn new(
        store: ResourceStore,
        packages: &[(&[u8], &Meta)],
        person: &str,
    ) -> Result<Self, HostError> {
        let (_, last) = packages
            .last()
            .ok_or_else(|| HostError::NotFound("a used app's package".to_owned()))?;
        let host = Host::open(
            store,
            Actor::Member(crate::runtime::ids::MemberId::person(person)),
            None,
            || None,
        )?;
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

    /// The runtime the app is read through.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Read again every resource handed to `runtime`, the store having moved. Whether a
    /// reading changed.
    pub fn refresh(&self, runtime: &Runtime) -> bool {
        let mut moved = false;
        for (id, ty) in &self.handed {
            moved |= self.hand(runtime, id, ty);
        }
        moved
    }

    /// Hand `runtime` the reading of `id`, a resource of type `ty`. A resource the store does
    /// not hold (yet) is left reading as it would start. Whether its reading changed.
    fn hand(&self, runtime: &Runtime, id: &str, ty: &str) -> bool {
        // The view asked by the resource's own id (`root-<type>`, `root-<type>/<item>`), or by
        // the address a named root of another app is read as (`tl:<app>:<id>`): the store reads
        // its fields from that app's published address either way.
        let address = if id.starts_with("tl:") {
            id.to_owned()
        } else {
            crate::naming::resource_id(&self.app, id)
        };
        let Some(reading) = reading_of(&self.host, &address) else {
            return false;
        };
        // Kept under the id the view asked by, never one parsed out of it: a reading held under
        // another key is one that view never looks at, and the place it draws stays as its type
        // starts.
        let target = ResourceRef {
            doc: id.to_owned(),
            item: None,
            ty: ty.to_owned(),
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

//! The person's workspace and the device owner's in memory, for tests of what reads and sets the
//! settings: each a real resource store with the settings' surface, keeping their history.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use serde_json::Value;
use tilo_resource_store::wire::{self, Command, Reply};
use tilo_resource_store::{Schema, Store};

use crate::workspace::{Stores, WorkspaceSettings};
use settings_store::workspace::surface_json;
use settings_store::{SettingsSource, SourceScope, placement::Holder};

/// The person's workspace and the device owner's, each a store that declares the settings'
/// surface, as the workspace service keeps them.
pub struct Workspaces {
    stores: Mutex<BTreeMap<Holder, Store>>,
    woken: AtomicUsize,
}

impl Workspaces {
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the surface is compiled in: one that does not read fails every test at once"
    )]
    pub fn new() -> Arc<Self> {
        let surface = collab_types::surface::SurfaceFile::from_json(surface_json())
            .expect("the settings' surface reads");
        let store = || {
            let mut store = Store::new();
            for resource in &surface.resources {
                store.declare(Schema::from_surface(resource));
            }
            store
        };
        Arc::new(Self {
            stores: Mutex::new(BTreeMap::from([
                (Holder::Person, store()),
                (Holder::Device, store()),
            ])),
            woken: AtomicUsize::new(0),
        })
    }

    /// How often a write asked for a pass.
    #[must_use]
    pub fn woken(&self) -> usize {
        self.woken.load(Ordering::Relaxed)
    }

    /// The content of `section`'s resource as the store holds it.
    #[must_use]
    pub fn content(&self, holder: Holder, id: &str, kind: &str) -> Option<Value> {
        match self.command(
            holder,
            Command::Content {
                kind: kind.to_owned(),
                id: id.to_owned(),
            },
        ) {
            Ok(Reply::Content(content)) => content,
            _ => None,
        }
    }
}

impl Stores for Workspaces {
    fn command(&self, holder: Holder, command: Command) -> Result<Reply, String> {
        let mut stores = self.stores.lock_recover();
        let store = stores
            .get_mut(&holder)
            .ok_or_else(|| format!("no {holder:?} workspace"))?;
        let reply = if command.writes() {
            let written = wire::write(store, command).map_err(|refused| refused.to_string())?;
            // What a holder that receives the change decides: the verb declares what it writes.
            for write in &written {
                store
                    .check_write(write, [7; 32])
                    .map_err(|refusal| format!("{refusal:?}"))?;
            }
            Reply::Written
        } else {
            wire::read(store, &command, &|_| true).ok_or("no read answers that")?
        };
        drop(stores);
        Ok(reply)
    }

    fn readers(&self, _holder: Holder) -> String {
        "*".to_owned()
    }

    fn device(&self) -> String {
        "ab".repeat(32)
    }

    fn wake(&self) {
        self.woken.fetch_add(1, Ordering::Relaxed);
    }
}

/// Settings over fresh [`Workspaces`], installed as this process's source.
#[must_use]
pub fn installed() -> (Arc<WorkspaceSettings>, Arc<Workspaces>) {
    let (settings, workspaces) = attached();
    settings_store::install_source(Some(Arc::clone(&settings) as Arc<dyn SettingsSource>));
    (settings, workspaces)
}

/// Settings over fresh [`Workspaces`], kept as the session keeps the person's: what a test host
/// gives a profile of its own ([`settings_store::keep_profile`]).
#[must_use]
pub fn attached() -> (Arc<WorkspaceSettings>, Arc<Workspaces>) {
    let workspaces = Workspaces::new();
    let settings = Arc::new(WorkspaceSettings::new());
    settings.attach(Arc::clone(&workspaces) as Arc<dyn Stores>);
    (settings, workspaces)
}

/// Settings over fresh [`Workspaces`], of this thread alone while the scope lives: what no other
/// test sees.
#[must_use]
pub fn scoped() -> SourceScope {
    settings_store::scoped_source(Some(attached().0))
}

//! The applications a multi-app host can mount, registered once per process.
//!
//! A host that links applications in-process (the browser's desktop bundle,
//! a shell hosting apps in-process) reads this list instead of naming apps in
//! its own code. Registration is idempotent per app id: the first definition
//! for an id wins, so a bundle that lists an app twice still shows it once.
//!
//! An application can also be registered *deferred*: by id and title, with
//! a function that loads its code and yields its definition. The browser
//! bundle registers most apps this way, so their code is fetched when one
//! first opens instead of with the page. Until then the app is listed
//! ([`catalogue`]) but has no definition ([`application`]); [`load`] starts
//! the load, and its definition replaces the deferred entry in place.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use super::ApplicationDefinition;
use tilo_lock::MutexRecover as _;

/// Loads a deferred application's code; `None` when it could not be loaded.
pub type Loading = Pin<Box<dyn Future<Output = Option<&'static ApplicationDefinition>> + Send>>;

/// Starts loading a deferred application.
pub type Load = fn() -> Loading;

#[derive(Clone, Copy)]
enum Entry {
    Ready(&'static ApplicationDefinition),
    Deferred {
        app_id: &'static str,
        title: &'static str,
        load: Load,
    },
}

impl Entry {
    fn app_id(&self) -> &'static str {
        match self {
            Self::Ready(definition) => definition.app_id,
            Self::Deferred { app_id, .. } => app_id,
        }
    }
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn entries() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    REGISTRY.lock_recover()
}

/// Register `definition`. Returns `false` when an application with the same
/// id is already registered; that earlier definition stays. A deferred
/// entry for the id is replaced, keeping its place in the list.
pub fn register(definition: &'static ApplicationDefinition) -> bool {
    let mut registry = entries();
    match registry
        .iter_mut()
        .find(|entry| entry.app_id() == definition.app_id)
    {
        Some(entry @ Entry::Deferred { .. }) => {
            *entry = Entry::Ready(definition);
            true
        }
        Some(Entry::Ready(_)) => false,
        None => {
            registry.push(Entry::Ready(definition));
            true
        }
    }
}

/// Register an application whose code `load` fetches when it is first
/// needed. Returns `false` when the id is already registered.
pub fn defer(app_id: &'static str, title: &'static str, load: Load) -> bool {
    let mut registry = entries();
    if registry.iter().any(|entry| entry.app_id() == app_id) {
        return false;
    }
    registry.push(Entry::Deferred {
        app_id,
        title,
        load,
    });
    true
}

/// Every registered application whose definition is loaded, in
/// registration order.
pub fn applications() -> Vec<&'static ApplicationDefinition> {
    entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Ready(definition) => Some(*definition),
            Entry::Deferred { .. } => None,
        })
        .collect()
}

/// Every registered application, loaded or not, as `(id, title)` in
/// registration order: what a launcher lists.
pub fn catalogue() -> Vec<(&'static str, &'static str)> {
    entries()
        .iter()
        .map(|entry| match entry {
            Entry::Ready(definition) => (definition.app_id, definition.title),
            Entry::Deferred { app_id, title, .. } => (*app_id, *title),
        })
        .collect()
}

/// The registered application with id `app_id`, once its definition is
/// loaded.
pub fn application(app_id: &str) -> Option<&'static ApplicationDefinition> {
    entries().iter().find_map(|entry| match entry {
        Entry::Ready(definition) if definition.app_id == app_id => Some(*definition),
        _ => None,
    })
}

/// Whether `app_id` is registered but its code is not loaded yet.
pub fn is_deferred(app_id: &str) -> bool {
    entries()
        .iter()
        .any(|entry| matches!(entry, Entry::Deferred { app_id: id, .. } if *id == app_id))
}

/// The definition of `app_id`, loading its code first when it is deferred:
/// `None` for an id nobody registered or code that failed to load (a later
/// call tries again). Concurrent loads of one app share the fetch where the
/// loader does.
pub async fn load(app_id: &str) -> Option<&'static ApplicationDefinition> {
    let load = entries().iter().find_map(|entry| match entry {
        Entry::Ready(definition) if definition.app_id == app_id => Some(Err(*definition)),
        Entry::Deferred {
            app_id: id, load, ..
        } if *id == app_id => Some(Ok(*load)),
        _ => None,
    })?;
    match load {
        Err(ready) => Some(ready),
        Ok(load) => {
            let definition = load().await?;
            if definition.app_id != app_id {
                return None;
            }
            register(definition);
            application(app_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordKind;
    use crate::widget::IconDescriptor;

    fn definition(app_id: &'static str) -> &'static ApplicationDefinition {
        Box::leak(Box::new(ApplicationDefinition {
            app_id,
            title: "Probe",
            icon: IconDescriptor::Named("notes".into()),
            record_kind: RecordKind::new("probe.page"),
            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: false,
            create: |_, _, _| Err(crate::launch::LaunchError::MissingComponentHost),
        }))
    }

    #[test]
    fn first_registration_per_id_wins() {
        let first = definition("test.registry.first-wins");
        let second = definition("test.registry.first-wins");
        assert!(register(first));
        assert!(!register(second));
        let found = application("test.registry.first-wins").unwrap();
        assert!(std::ptr::eq(found, first));
        assert_eq!(
            applications()
                .iter()
                .filter(|d| d.app_id == "test.registry.first-wins")
                .count(),
            1
        );
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
        }
    }

    #[test]
    fn a_deferred_app_is_listed_before_it_loads_and_keeps_its_place_once_loaded() {
        assert!(defer("test.registry.deferred", "Deferred", || {
            Box::pin(async { Some(definition("test.registry.deferred")) })
        }));
        assert!(register(definition("test.registry.after-deferred")));
        assert!(application("test.registry.deferred").is_none());
        assert!(is_deferred("test.registry.deferred"));
        let listed = |catalogue: Vec<(&'static str, &'static str)>| {
            catalogue
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| id.starts_with("test.registry.") && id.contains("deferred"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            listed(catalogue()),
            ["test.registry.deferred", "test.registry.after-deferred"]
        );

        let loaded = block_on(load("test.registry.deferred")).unwrap();
        assert_eq!(loaded.app_id, "test.registry.deferred");
        assert!(!is_deferred("test.registry.deferred"));
        assert!(std::ptr::eq(
            application("test.registry.deferred").unwrap(),
            loaded
        ));
        assert_eq!(
            listed(catalogue()),
            ["test.registry.deferred", "test.registry.after-deferred"]
        );
    }

    #[test]
    fn a_failed_load_leaves_the_app_deferred() {
        assert!(defer("test.registry.failing", "Failing", || {
            Box::pin(async { None })
        }));
        assert!(block_on(load("test.registry.failing")).is_none());
        assert!(is_deferred("test.registry.failing"));
        assert!(block_on(load("test.registry.unknown")).is_none());
    }
}

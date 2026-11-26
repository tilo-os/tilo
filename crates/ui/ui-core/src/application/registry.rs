//! The applications a multi-app host can mount, registered once per process. A host that links
//! applications in-process (the browser's shell bundle, a shell hosting apps in-process) reads this
//! list instead of naming apps in its code.

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
            Self::Ready(definition) => definition.app_id(),
            Self::Deferred { app_id, .. } => app_id,
        }
    }
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// The shape each app declared for its window while its code is on its way
/// (its manifest's `[appearance] arrival-skeleton`, through the toolkit's
/// [`SkeletonShape`](tilo_ui_base::widget::SkeletonShape): the placeholder is the
/// app shape, never the host's guess), kept apart from the registry so it outlives the app's code: a window
/// takes the place of an app that is built but has not drawn its first frame yet,
/// and that is when the declaration is needed most.
static DECLARED_SHAPES: Mutex<Vec<(&'static str, tilo_ui_base::widget::SkeletonShape)>> =
    Mutex::new(Vec::new());

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
        .find(|entry| entry.app_id() == definition.app_id())
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

/// Register an application whose code `load` fetches when it is first needed.
pub fn defer(app_id: &'static str, title: &'static str, load: Load) -> bool {
    defer_arriving(
        app_id,
        title,
        tilo_ui_base::widget::SkeletonShape::Ground,
        load,
    )
}

/// [`defer`] with the shape the app's window shows while its code is on its way: what the app declares
/// for it in its manifest.
pub fn defer_arriving(
    app_id: &'static str,
    title: &'static str,
    arrival_shape: tilo_ui_base::widget::SkeletonShape,
    load: Load,
) -> bool {
    let mut registry = entries();
    if registry.iter().any(|entry| entry.app_id() == app_id) {
        return false;
    }
    registry.push(Entry::Deferred {
        app_id: app_id.into(),
        title,
        load,
    });
    drop(registry);
    let mut declared = DECLARED_SHAPES.lock_recover();
    declared.retain(|(id, _)| *id != app_id);
    declared.push((app_id, arrival_shape));
    true
}

/// An app's claim on links: the schemes its manifest declares (`[providers] schemes`) and the
/// resource it offers `open-link` on (`[[offers]]`), which a host hands a link of those schemes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkClaim {
    /// The app, by its package id.
    pub app_id: &'static str,
    /// The resource the app shows a link through `open-link` on.
    pub resource: &'static str,
    /// The schemes it claims, lowercase.
    pub schemes: &'static [&'static str],
}

static LINK_CLAIMS: Mutex<Vec<LinkClaim>> = Mutex::new(Vec::new());

/// Register `claim`, replacing an earlier one of the same app.
pub fn claim_links(claim: LinkClaim) {
    let mut claims = LINK_CLAIMS.lock_recover();
    claims.retain(|known| known.app_id != claim.app_id);
    claims.push(claim);
}

/// The registered app that claims `link`'s scheme, the first registered when several do.
pub fn link_claim(link: &str) -> Option<LinkClaim> {
    let scheme = tilo_actions::remote::link_scheme(link)?;
    LINK_CLAIMS
        .lock_recover()
        .iter()
        .find(|claim| claim.schemes.iter().any(|claimed| *claimed == scheme))
        .copied()
}

/// The shape `app_id` declared for its window while it has nothing of it to show — its code on its
/// way, or its first frame not drawn yet; [`SkeletonShape::Ground`] for an app whose host was told no
/// declaration.
pub fn declared_arrival_shape(app_id: &str) -> tilo_ui_base::widget::SkeletonShape {
    DECLARED_SHAPES
        .lock_recover()
        .iter()
        .find_map(|(id, shape)| (*id == app_id).then_some(*shape))
        .unwrap_or(tilo_ui_base::widget::SkeletonShape::Ground)
}

/// Every registered application whose definition is loaded, in registration order.
pub fn applications() -> Vec<&'static ApplicationDefinition> {
    entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Ready(definition) => Some(*definition),
            Entry::Deferred { .. } => None,
        })
        .collect()
}

/// Every registered application, loaded or not, as `(id, title)` in registration order: what a launcher
/// lists.
pub fn catalogue() -> Vec<(&'static str, &'static str)> {
    entries()
        .iter()
        .map(|entry| match entry {
            Entry::Ready(definition) => (definition.app_id(), definition.title()),
            Entry::Deferred { app_id, title, .. } => (*app_id, *title),
        })
        .collect()
}

/// The registered application with id `app_id`, once its definition is loaded.
pub fn application(app_id: &str) -> Option<&'static ApplicationDefinition> {
    entries().iter().find_map(|entry| match entry {
        Entry::Ready(definition) if definition.app_id() == app_id => Some(*definition),
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
        Entry::Ready(definition) if definition.app_id() == app_id => Some(Err(*definition)),
        Entry::Deferred {
            app_id: id, load, ..
        } if *id == app_id => Some(Ok(*load)),
        _ => None,
    })?;
    match load {
        Err(ready) => Some(ready),
        Ok(load) => {
            let definition = load().await?;
            if definition.app_id() != app_id {
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
    use crate::resource::ResourceType;
    use crate::widget::IconDescriptor;

    fn definition(app_id: &'static str) -> &'static ApplicationDefinition {
        Box::leak(Box::new(ApplicationDefinition {
            app_id: app_id.into(),
            title: "Probe".into(),
            icon: IconDescriptor::Named("notes".into()),
            resource_type: ResourceType::new("probe.page"),
            required: vec![],
            optional: vec![],
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: |_, _, _| Err(crate::launch::LaunchError::MissingComponentHost),
        }))
    }

    tilo_test_env::scenario! {
        fn the_application_registry_keeps_first_registrations_and_deferred_places;

        phase first_registration_per_id_wins;
        {
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

        phase a_deferred_app_is_listed_before_it_loads_and_keeps_its_place_once_loaded;
        {
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

        phase a_deferred_app_declares_what_its_window_shows_while_its_code_is_on_its_way;
        {
            use tilo_ui_base::widget::SkeletonShape;
            assert!(defer_arriving(
                "test.registry.shaped",
                "Shaped",
                SkeletonShape::Page,
                || Box::pin(async { Some(definition("test.registry.shaped")) })
            ));
            assert_eq!(
                declared_arrival_shape("test.registry.shaped"),
                SkeletonShape::Page
            );
            assert!(defer("test.registry.unshaped", "Unshaped", || Box::pin(
                async { Some(definition("test.registry.unshaped")) }
            )));
            assert_eq!(
                declared_arrival_shape("test.registry.unshaped"),
                SkeletonShape::Ground
            );
            assert_eq!(
                declared_arrival_shape("test.registry.nobody"),
                SkeletonShape::Ground
            );
            let loaded = block_on(load("test.registry.shaped")).unwrap();
            assert_eq!(loaded.app_id, "test.registry.shaped");
            assert_eq!(
                declared_arrival_shape("test.registry.shaped"),
                SkeletonShape::Page
            );
        }

        phase a_failed_load_leaves_the_app_deferred;
        {
            assert!(defer("test.registry.failing", "Failing", || {
                Box::pin(async { None })
            }));
            assert!(block_on(load("test.registry.failing")).is_none());
            assert!(is_deferred("test.registry.failing"));
            assert!(block_on(load("test.registry.unknown")).is_none());
        }
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

    /// A host finds the app that claims a link's scheme, and the resource it shows the link on.
    #[test]
    fn a_link_names_the_app_that_claims_its_scheme() {
        claim_links(LinkClaim {
            app_id: "so.tilo.probe-maps",
            resource: "tl.so.tilo.maps.Atlas",
            schemes: &["geo-probe"],
        });
        let claim = link_claim("GEO-PROBE:52.5,13.4").expect("the scheme is claimed");
        assert_eq!(claim.app_id, "so.tilo.probe-maps");
        assert_eq!(claim.resource, "tl.so.tilo.maps.Atlas");
        assert_eq!(link_claim("unclaimed-probe:x"), None);
        assert_eq!(link_claim("/a/path"), None);
    }
}

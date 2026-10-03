//! [`LaunchSpec::as_definition`]: a hand-built launch specification as an
//! [`ApplicationDefinition`], so apps that predate components mount in every
//! host that mounts definitions (the browser desktop, in-process shells).
//!
//! The adapted component's root is the spec's live root, synchronized
//! against the component's record document instead of a process-wide state
//! file. Its History is the one the native launch installs: the root's own
//! registration, else the spec's [`LaunchSpec::history`] definition, both
//! over the component's record. Screenshot, live-window and command hooks
//! are process-only and are not part of the component; the actions an app
//! registers through [`LaunchSpec::component_actions`] are.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use crate::record::RecordKind;
use crate::widget::{AppKind, AppWidget, Decorator, IconDescriptor, Widget};

use super::{ArgsRootFactory, LaunchArgs, LaunchError, LaunchSpec, RootFactory};

/// Registers an app's actions on a component's registry.
pub type ComponentActions = fn(&mut tilo_actions::ActionRegistry);

/// An app's Fluent catalogs, as `(locale, source)` pairs.
pub type ComponentLocales = &'static [(&'static str, &'static str)];

#[derive(Clone, Copy)]
struct Adapted {
    definition: &'static ApplicationDefinition,
    live_root: Option<RootFactory>,
    live_root_with_args: Option<ArgsRootFactory>,
    actions: Option<ComponentActions>,
    locales: ComponentLocales,
    history: Option<&'static crate::history_mode::HistoryDefinition>,
}

static ADAPTED: Mutex<Option<HashMap<&'static str, Adapted>>> = Mutex::new(None);

/// How an adapted root came to its record, as [`record_launch`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordLaunch {
    /// This mount created the record: it alone writes the initial content.
    Created,
    /// The record already existed, created by this or another participant.
    /// Its document is the only state: the root seeds nothing, even while
    /// the creator's content has not arrived, and reads no app-wide file.
    Joined,
}

thread_local! {
    static RECORD_LAUNCH: std::cell::Cell<Option<RecordLaunch>> =
        const { std::cell::Cell::new(None) };
}

/// While an in-process host builds an adapted root: whether it is mounted on
/// a record it created or joined. `None` for a standalone launch, whose
/// document is the app's own state file. A root reads this when it is
/// constructed.
#[must_use]
pub fn record_launch() -> Option<RecordLaunch> {
    RECORD_LAUNCH.with(std::cell::Cell::get)
}

fn with_record_launch<R>(launch: RecordLaunch, build: impl FnOnce() -> R) -> R {
    let earlier = RECORD_LAUNCH.with(|cell| cell.replace(Some(launch)));
    let built = build();
    RECORD_LAUNCH.with(|cell| cell.set(earlier));
    built
}

fn adapted(app_id: &str) -> Option<Adapted> {
    ADAPTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|adapted| adapted.get(app_id).copied())
}

impl LaunchSpec {
    /// The id of the app this spec launches.
    #[must_use]
    pub fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// Register the actions a component built from this spec exposes. Native
    /// launches register actions through [`Self::configure_app`]; components
    /// have no `App`, so they take this hook instead.
    #[must_use]
    pub fn component_actions(mut self, register: ComponentActions) -> Self {
        self.component_actions = Some(register);
        self
    }

    /// The Fluent catalogs a component built from this spec adds to the
    /// host's shared catalog. Native launches install their catalogs before
    /// running the spec; in-process hosts have one catalog for every app.
    #[must_use]
    pub fn component_locales(mut self, catalogs: ComponentLocales) -> Self {
        self.component_locales = catalogs;
        self
    }

    /// This spec as an application definition every in-process host can
    /// mount. Specs built with [`Self::from_definition`] return their
    /// definition. Otherwise the live root becomes the component's root and
    /// its document is the component's record, of kind `<app id>.app`.
    ///
    /// The definition is created once per app id; later calls return it.
    #[must_use]
    pub fn as_definition(&self, title: &'static str) -> &'static ApplicationDefinition {
        if let Some(definition) = self.definition {
            return definition;
        }
        let mut guard = ADAPTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let adapted = guard.get_or_insert_with(HashMap::new);
        if let Some(existing) = adapted.get(self.app_id) {
            return existing.definition;
        }
        let definition: &'static ApplicationDefinition =
            Box::leak(Box::new(ApplicationDefinition {
                app_id: self.app_id,
                title,
                icon: IconDescriptor::Named("application".into()),
                record_kind: RecordKind::new(format!("{}.app", self.app_id)),
                required: vec![],
                optional: vec![],
                opens: &[],
                create: create_adapted,
            }));
        adapted.insert(
            self.app_id,
            Adapted {
                definition,
                live_root: self.live_root,
                live_root_with_args: self.live_root_with_args,
                actions: self.component_actions,
                locales: self.component_locales,
                history: self.history_definition,
            },
        );
        definition
    }
}

fn create_adapted(
    mut context: HostContext,
    target: LaunchTarget,
    _restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    let adapted = adapted(context.app_id())
        .ok_or_else(|| LaunchError::UnknownApplication(context.app_id().to_owned()))?;
    register_locales(adapted.definition.app_id, adapted.locales);
    let launch = if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(LaunchError::Record)?;
        RecordLaunch::Created
    } else {
        RecordLaunch::Joined
    };
    let root = with_record_launch(launch, || {
        match (adapted.live_root_with_args, adapted.live_root) {
            (Some(factory), _) => factory(&LaunchArgs::component()),
            (None, Some(factory)) => Ok(factory()),
            (None, None) => Err(LaunchError::Operation(
                "launch specification has no live root".into(),
            )),
        }
    })?;
    let own_history = crate::history_mode::capability::history_registration(root.as_ref())
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let mut registry = tilo_actions::ActionRegistry::new();
    if let Some(register) = adapted.actions {
        register(&mut registry);
    }
    let mut component = AppComponent::new(
        context,
        Box::new(AdaptedRoot {
            root,
            title: adapted.definition.title,
        }),
        registry,
    );
    let history = match (own_history, adapted.history) {
        (Some(registration), _) => component.register_history(registration),
        (None, Some(definition)) => component.register_document_history(definition),
        (None, None) => Ok(()),
    };
    history.map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(component)
}

/// Add an app's catalogs to the shared one, once per app.
fn register_locales(app_id: &'static str, catalogs: ComponentLocales) {
    static REGISTERED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    if catalogs.is_empty() {
        return;
    }
    let mut registered = REGISTERED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if registered.contains(&app_id) {
        return;
    }
    registered.push(app_id);
    crate::i18n::I18nManager::register_widget_messages();
    for (locale, source) in catalogs {
        crate::i18n::I18nManager::add_resource(locale, (*source).to_owned());
    }
}

/// A legacy live root presented as a component's application root.
struct AdaptedRoot {
    root: Box<dyn Widget>,
    title: &'static str,
}

impl Decorator for AdaptedRoot {
    type Inner = dyn Widget;

    fn inner(&self) -> &Self::Inner {
        self.root.as_ref()
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        self.root.as_mut()
    }

    fn name(&self) -> &str {
        "AdaptedRoot"
    }

    // Host traversals meet this decorator in the app root's place, so it
    // must own the node for the root's accessibility subtree to resolve.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.root.build_layout(taffy, theme, node_map);
        crate::widget::decorator::register_node(node_map, node, self);
        node
    }
}

impl AppWidget for AdaptedRoot {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        self.title
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Named("application".into())
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::application::{ApplicationHost, ComponentRunnerInputs};
    use crate::history_mode::{
        HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
        PreviewInput,
    };
    use crate::permissions::{Actor, PermissionContext, Role, UserId};
    use crate::record::{RecordCheckout, RecordId};

    fn root() -> Box<dyn Widget> {
        Box::new(crate::widget::Text::new("adapted"))
    }

    fn no_preview(_: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
        Err("no preview in this test".into())
    }

    static HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: "test.adapted-history",
        scopes: &[HistoryScopeDefinition {
            key: "document",
            title: "Document",
            presentation: HistoryPresentation::Domain { create: no_preview },
            content_schema: None,
        }],
    };

    fn mount(definition: &'static ApplicationDefinition) -> AppComponent {
        let profile = tempfile::tempdir().unwrap();
        let permissions = Rc::new(RefCell::new(PermissionContext::from_capabilities(
            Actor::person(Some(Role::Owner), "author"),
            vec![],
        )));
        let mut host = ApplicationHost::new(
            profile.path().join("state"),
            "device".into(),
            "workspace",
            UserId("author".into()),
            permissions,
            tilo_actions::ActionContext::default(),
        )
        .unwrap();
        let checkout = RecordCheckout::new(
            RecordId::derived("default", &RecordId::for_tab(0)),
            definition.record_kind.clone(),
        );
        let prepared = host
            .prepare_default(
                definition,
                1,
                checkout,
                None,
                ComponentRunnerInputs::from_native_process(),
            )
            .unwrap();
        definition
            .instantiate(prepared.context, prepared.target, prepared.restore)
            .unwrap()
    }

    #[test]
    fn an_adapted_spec_keeps_the_history_its_native_launch_installs() {
        let with = LaunchSpec::new("test.adapted-history", root)
            .history(&HISTORY)
            .as_definition("With history");
        let component = mount(with);
        let registration = component.history.as_ref().expect("History is registered");
        assert!(std::ptr::eq(registration.definition, &raw const HISTORY));

        let without = LaunchSpec::new("test.adapted-plain", root).as_definition("Plain");
        assert!(mount(without).history.is_none());
    }
}

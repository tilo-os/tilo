//! Firewall as a [`tilo_resource::Resource`]: the config document is shared;
//! applying it through `firewall-cmd` is an Effect (no outbox queue today).

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{FIREWALL_ACTION_TYPE, FirewallAction, register};
use crate::app::FirewallApp;
use crate::doc::FirewallDoc;
use crate::model::FirewallConfig;

/// The firewall configuration resource.
pub struct Firewall;

impl Resource for Firewall {
    const KIND: &'static str = "firewall.config";
    const LABEL: &'static str = "Firewall";
    const ACTION_TYPE: &'static str = FIREWALL_ACTION_TYPE;

    type State = FirewallConfig;
    type Action = FirewallAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<FirewallConfig, ModelError> {
        Ok(FirewallDoc::load(doc).unwrap_or_else(FirewallConfig::empty))
    }

    fn bound_actions(state: &FirewallConfig) -> Vec<ActionSpec> {
        // Start/stop and panic carry the current on/off. Static
        // `local_operations` cannot see that.
        crate::actions::toggle_specs(state)
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Apply pushes the stored ruleset through firewall-cmd. There is no
        // drain queue today, so undo of a host apply is not a compensating
        // document op. The on/off toggles ride [`bound_actions`].
        vec![crate::actions::apply_effect_spec()]
    }

    fn children(state: &FirewallConfig) -> Vec<tilo_actions::Resource> {
        crate::actions::zone_children(state)
    }

    fn search(state: &FirewallConfig) -> Vec<SearchEntry> {
        state
            .zones
            .iter()
            .map(|zone| SearchEntry {
                anchor: Some(format!("firewall.zone:{}", zone.kind.name())),
                title: zone.kind.name().to_owned(),
                text: format!("{} rules", zone.rules.len()),
                action: None,
            })
            .collect()
    }
}

/// Framework app: the firewall widget over [`Firewall`].
pub struct FirewallShell;

impl ResourceApp for FirewallShell {
    type Resource = Firewall;
    const ID: &'static str = "so.tilo.firewall";
    const TITLE: &'static str = "Firewall";
    const ICON: Option<&'static str> = Some("firewall");
    const SHOT_VIEWPORT: (u32, u32) = (1100, 720);

    fn view(cx: &mut ViewCx<Firewall>) -> Element {
        cx.surface(FirewallApp::new())
    }
}

impl ResourceSurface<Firewall> for FirewallApp {
    fn present(&mut self, state: &std::rc::Rc<FirewallConfig>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Firewall>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Firewall>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Firewall>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &FirewallAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<FirewallShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::FIREWALL_APPLY_TYPE;
    use crate::model::FirewallConfig;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_apply_and_hosted_keeps_no_duplicate_root() {
        let config = FirewallConfig::empty();
        let published =
            ui::tilo_resource::publish::<Firewall>("firewall.config", "Firewall", &config);
        assert_eq!(published.kind, "firewall.config");
        let apply = published
            .actions
            .iter()
            .find(|action| action.id == "apply-config")
            .expect("apply Effect must ride local_operations via publish");
        assert_eq!(apply.kind, OperationKind::Effect);
        assert_eq!(apply.operation_type.as_deref(), Some(FIREWALL_APPLY_TYPE));
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "toggle-enabled"),
            "toggle Effects stay on the framework root"
        );

        let mut app = FirewallApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.resources(&doc);
        assert!(
            standalone.iter().any(|root| {
                root.kind == "firewall.config"
                    && root
                        .actions
                        .iter()
                        .any(|action| action.id == "apply-config")
            }),
            "standalone still publishes apply"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &doc, "firewall.config").is_none());
        if let Some(zone_id) = standalone.iter().find_map(|root| {
            root.children
                .iter()
                .find(|child| child.kind == "firewall.zone")
                .map(|child| child.id.as_str().to_owned())
        }) {
            assert!(
                ui::resources::resource(&app, &doc, &zone_id).is_some(),
                "a hosted zone stays the published row"
            );
        }
        let extras = app.resources(&doc);
        assert!(
            extras.is_empty()
                || extras
                    .iter()
                    .all(|resource| resource.kind == "firewall.recovery"),
            "hosted must not republish firewall.config beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            !extras
                .iter()
                .any(|resource| resource.kind == "firewall.config"),
            "hosted must not republish the config door"
        );
    }
}

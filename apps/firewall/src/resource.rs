//! Firewall as a [`tilo_resource::Resource`]: the config document is shared;
//! applying it through `firewall-cmd` is an Effect (no outbox queue today).

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_actions::{
    ActionContract, ActionSpec, ConfirmationPolicy, DiscoveryScope, OperationKind,
    PermissionRequirement, RetryPolicy, RiskLevel,
};
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

    fn local_operations() -> Vec<ActionSpec> {
        // Host apply Effects. The intended on/off and panic bits also live in
        // the document via FirewallAction; there is no drain queue today, so
        // undo of a host apply is not recorded as a compensating document op.
        [
            (
                "toggle-enabled",
                "Toggle firewall",
                "firewall.set-enabled",
                true,
            ),
            (
                "toggle-panic",
                "Toggle panic mode",
                "firewall.set-panic",
                true,
            ),
        ]
        .into_iter()
        .filter_map(|(id, label, type_id, value)| {
            Some(
                ActionSpec::operation(id, label, OperationKind::Effect, type_id, &value)
                    .ok()?
                    .with_contract(ActionContract {
                        permission: PermissionRequirement::Admin,
                        risk: RiskLevel::Privileged,
                        confirmation: ConfirmationPolicy::Confirm,
                        retry: RetryPolicy::Idempotent,
                        discovery: DiscoveryScope::Launchable,
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        tags: vec!["security".into(), "firewall".into()],
                        ..ActionContract::default()
                    }),
            )
        })
        .collect()
    }

    fn children(state: &FirewallConfig) -> Vec<tilo_actions::Resource> {
        crate::actions::provide_resources(state)
            .into_iter()
            .flat_map(|root| root.children)
            .collect()
    }

    fn search(state: &FirewallConfig) -> Vec<SearchEntry> {
        state
            .zones
            .iter()
            .map(|zone| SearchEntry {
                anchor: Some(format!("firewall.zone:{}", zone.kind.name())),
                title: zone.kind.name().to_owned(),
                text: format!("{} rules", zone.rules.len()),
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
    local: ui::ActionHost,
    framework: Option<Dispatcher<Firewall>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Firewall>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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

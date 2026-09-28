//! VPN as a [`tilo_resource::Resource`]: tunnel profiles in the document;
//! import / connect / provision against NetworkManager are Effects.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_actions::{
    ActionContract, ActionSpec, ConfirmationPolicy, ExecutionMode, OperationKind,
    PermissionRequirement, ResultSpec, RetryPolicy, RiskLevel,
};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{VPN_ACTION_TYPE, VPN_IMPORT_TYPE, VpnAction, register};
use crate::app::VpnApp;
use crate::doc::VpnDoc;
use crate::model::{ConnKind, Vault};

/// The VPN vault resource.
pub struct Vpn;

impl Resource for Vpn {
    const KIND: &'static str = "vpn.vault";
    const LABEL: &'static str = "VPN";
    const ACTION_TYPE: &'static str = VPN_ACTION_TYPE;

    type State = Vault;
    type Action = VpnAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Vault, ModelError> {
        Ok(VpnDoc::read_vault(doc))
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Import is a host Effect (file → validated profile → later document
        // add). There is no outbox queue today: the shared profile lands via a
        // separate document action after validation; connect/disconnect stay
        // Effects without a compensating document record.
        [ConnKind::WireGuard, ConnKind::OpenVpn]
            .into_iter()
            .filter_map(|kind| {
                let (id, label) = match kind {
                    ConnKind::WireGuard => ("import-wireguard", "Import WireGuard profile"),
                    ConnKind::OpenVpn => ("import-openvpn", "Import OpenVPN profile"),
                };
                let mut spec = ActionSpec::operation(
                    id,
                    label,
                    OperationKind::Effect,
                    VPN_IMPORT_TYPE,
                    &kind.as_str(),
                )
                .ok()?;
                spec.contract = ActionContract {
                    permission: PermissionRequirement::Admin,
                    risk: RiskLevel::Privileged,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::AtMostOnce,
                    execution: ExecutionMode::Background { cancellable: true },
                    result: ResultSpec::Job,
                    tags: vec!["vpn".into(), "import".into(), kind.as_str().into()],
                    ..ActionContract::default()
                };
                Some(spec)
            })
            .collect()
    }

    fn children(state: &Vault) -> Vec<tilo_actions::Resource> {
        crate::actions::provide_resources(state)
            .into_iter()
            .flat_map(|root| root.children)
            .collect()
    }

    fn search(state: &Vault) -> Vec<SearchEntry> {
        state
            .connections
            .iter()
            .map(|connection| SearchEntry {
                anchor: Some(format!("vpn.connection:{}", connection.id)),
                title: connection.name.clone(),
                text: connection.kind.as_str().to_owned(),
            })
            .collect()
    }
}

/// Framework app: the VPN widget over [`Vpn`].
pub struct VpnShell;

impl ResourceApp for VpnShell {
    type Resource = Vpn;
    const ID: &'static str = "so.tilo.vpn";
    const TITLE: &'static str = "VPN";
    const ICON: Option<&'static str> = Some("vpn");
    const SHOT_VIEWPORT: (u32, u32) = (1100, 720);

    fn view(cx: &mut ViewCx<Vpn>) -> Element {
        cx.surface(VpnApp::new())
    }
}

impl ResourceSurface<Vpn> for VpnApp {
    fn present(&mut self, state: &std::rc::Rc<Vault>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Vpn>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Vpn>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Vpn>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &VpnAction,
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
    ui::framework::launch_spec::<VpnShell>()
}

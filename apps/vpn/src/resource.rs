//! VPN as a [`tilo_resource::Resource`]: tunnel profiles in the document;
//! import / connect / disconnect / provision against NetworkManager are Effects.

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

    fn bound_actions(state: &Vault) -> Vec<ActionSpec> {
        // Global Tor follows the saved preference. Import stays on
        // `local_operations`.
        vec![crate::actions::tor_global_spec(state)]
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Import is a host Effect (file → validated profile → later document
        // add). There is no outbox queue today: the shared profile lands via a
        // separate document action after validation. Connect / disconnect live
        // on connection children (see [`crate::actions::tunnel_toggle_specs`]).
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
        crate::actions::connection_resources(state)
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
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Vpn>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Vpn>) {
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
#[cfg(not(target_arch = "wasm32"))]
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<VpnShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{VPN_TOGGLE_TYPE, provide_resources};
    use crate::model::sample_vault;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_connect_disconnect_and_hosted_keeps_no_duplicate_root() {
        let vault = sample_vault();
        let published = ui::tilo_resource::publish::<Vpn>("vpn.vault", "VPN", &vault);
        assert_eq!(published.kind, "vpn.vault");
        assert!(
            published.actions.iter().any(|action| {
                action.id == "import-wireguard" && action.kind == OperationKind::Effect
            }),
            "import Effects must ride the framework vault root"
        );
        let home = published
            .children
            .iter()
            .find(|child| child.label.starts_with("Home"))
            .expect("sample Home connection child");
        let ids: Vec<&str> = home.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            ids.contains(&"connect"),
            "connect Effect must be on the connection child via publish: {ids:?}"
        );
        assert!(
            ids.contains(&"disconnect"),
            "disconnect Effect must be on the connection child via publish: {ids:?}"
        );
        let connect = home
            .actions
            .iter()
            .find(|action| action.id == "connect")
            .expect("connect");
        assert_eq!(connect.kind, OperationKind::Effect);
        assert_eq!(connect.operation_type.as_deref(), Some(VPN_TOGGLE_TYPE));

        // children() is the same door provide_resources exports under the vault.
        let from_children = Vpn::children(&vault);
        assert_eq!(
            from_children.len(),
            provide_resources(&vault)[0].children.len()
        );
        assert!(from_children.iter().any(|child| {
            child.actions.iter().any(|a| a.id == "connect")
                && child.actions.iter().any(|a| a.id == "disconnect")
        }));

        let mut app = VpnApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "vpn.vault");
        assert!(
            standalone[0].children.iter().any(|child| {
                child.actions.iter().any(|a| a.id == "connect")
                    && child.actions.iter().any(|a| a.id == "disconnect")
            }),
            "standalone still publishes tunnel Effects"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &doc, "vpn.vault").is_none());
        let connection_id = standalone[0].children[0].id.as_str().to_owned();
        assert!(
            ui::resources::resource(&app, &doc, &connection_id).is_some_and(|connection| {
                connection
                    .actions
                    .iter()
                    .any(|action| action.id == "connect")
            }),
            "a hosted connection stays the published row"
        );
        let extras = app.resources(&doc);
        assert!(
            extras.is_empty(),
            "hosted must not republish vpn.vault beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
    }
}

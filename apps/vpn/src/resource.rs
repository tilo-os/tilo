//! VPN as a [`tilo_resource::Resource`]: the shared, secret-free tunnel
//! profiles are the document; what a profile does on this device (import,
//! set up, connect, disconnect, delete) is a [`VpnEffect`] the surface
//! answers.

use std::rc::Rc;

use ui::framework::{
    EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{VPN_ACTION_TYPE, VpnAction, register};
use crate::app::VpnApp;
use crate::device::DeviceFacts;
use crate::doc::VpnDoc;
use crate::effects::VpnEffect;
use crate::model::{ConnStatus, Vault, sample_vault};

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

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "rename-connection",
                "Rename connection",
                vec![
                    ActionParam::required_owned(
                        "connection",
                        ParamType::Text,
                        "Id of the saved profile",
                    ),
                    ActionParam::required_owned("name", ParamType::Text, "The new name"),
                ],
                |args, _| {
                    let id = crate::effects::connection_id(args.text("connection").unwrap_or(""))?;
                    Ok(VpnAction::RenameConnection {
                        id,
                        name: args.text("name").unwrap_or("").to_owned(),
                    })
                },
            )
            .describe("Rename a saved profile for everyone it is shared with"),
        ]
    }

    fn children(state: &Vault) -> Vec<tilo_actions::Resource> {
        crate::actions::connection_resources(state)
    }

    fn search(state: &Vault) -> Vec<SearchEntry> {
        state
            .connections
            .iter()
            .map(|connection| SearchEntry {
                anchor: Some(crate::effects::connection_resource_id(connection.id)),
                title: connection.name.clone(),
                text: connection.kind.as_str().to_owned(),
                action: None,
            })
            .collect()
    }
}

/// The scenarios that show the sample fleet.
const SCENARIOS: &[&str] = &[
    "confirm",
    "add",
    "import",
    "detail",
    "config",
    "logs",
    "loading",
    "empty",
    "error",
    "disconnected",
    "connecting",
    "unprovisioned",
    "setup",
];

/// Framework app: the VPN widget over [`Vpn`].
pub struct VpnShell;

impl ResourceApp for VpnShell {
    type Resource = Vpn;
    const ID: &'static str = "so.tilo.vpn";
    const TITLE: &'static str = "VPN";
    const ICON: Option<&'static str> = Some("vpn");

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1100, 720)
    }

    fn view(cx: &mut ViewCx<Vpn>) -> Element {
        cx.surface(VpnApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Vpn>> {
        match name {
            None | Some("empty") => Vec::new(),
            Some(_) => sample_vault()
                .connections
                .into_iter()
                .filter_map(|connection| {
                    Invoke::apply(&crate::actions::add_connection_action(connection)).ok()
                })
                .collect(),
        }
    }

    /// The device a scenario shows: every sample profile set up here with
    /// fictional facts, 'Home' connected; the scenarios about a profile not
    /// set up here lack 'Home', and 'Work' is connecting in `connecting`.
    fn fixture(fixture: &mut Fixture<Vpn>) {
        let Some(scenario) = fixture.scenario() else {
            return;
        };
        if scenario == "empty" {
            return;
        }
        let vault = sample_vault();
        let mut facts = DeviceFacts::demo(&vault);
        let named = |name: &str| {
            vault
                .connections
                .iter()
                .find(|connection| connection.name == name)
                .map(|connection| connection.id)
        };
        match scenario {
            "unprovisioned" | "setup" => {
                if let Some(home) = named("Home") {
                    facts.profiles.remove(&home);
                    facts.status.insert(home, ConnStatus::Disconnected);
                }
            }
            "connecting" => {
                if let Some(work) = named("Work") {
                    facts.status.insert(work, ConnStatus::Connecting);
                }
            }
            _ => {}
        }
        fixture.provide(facts);
    }
}

impl ResourceSurface<Vpn, VpnEffect> for VpnApp {
    fn present(&mut self, state: &Rc<Vault>) {
        self.present_vault(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Vpn>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    fn effect(
        &mut self,
        effect: VpnEffect,
        cx: &mut EffectCx<Vpn, Self>,
    ) -> ui::framework::EffectAnswer {
        self.answer(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.profile_rows()
    }
}

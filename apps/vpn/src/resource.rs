//! VPN as a resource: the shared, secret-free tunnel profiles are the
//! document ([`Vpn`]), each edited at its path; what a profile does on this
//! device (import, set up, connect, disconnect, delete) is a [`VpnEffect`]
//! the surface answers.

use std::rc::Rc;

use ui::framework::{
    EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{self, Invoke, SearchEntry, resource};
use ui::widget::Element;

use crate::app::VpnApp;
use crate::device::DeviceFacts;
use crate::effects::VpnEffect;
pub use crate::model::Vpn;
use crate::model::{ConnStatus, Profile, Vault, sample_vault};

#[resource(kind = "vpn.vault", label = "VPN", search = Vpn::search)]
impl Vpn {}

impl Vpn {
    /// Each profile, found by its name and protocol, anchored at the row
    /// this device publishes for it.
    fn search(&self) -> Vec<SearchEntry> {
        Vault::of(self)
            .connections
            .iter()
            .map(|connection| SearchEntry {
                anchor: Some(crate::effects::connection_resource_id(connection.id)),
                title: connection.name.clone(),
                text: connection.kind.as_str().to_owned(),
                ..Default::default()
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

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

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
                .iter()
                .enumerate()
                .filter_map(|(at, connection)| {
                    Invoke::edit(&Vpn::paths().connections.insert_as(
                        &Vpn::item(connection.id),
                        at,
                        &Profile::of(connection),
                    ))
                    .ok()
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
    fn present(&mut self, state: &Rc<Vpn>) {
        self.present_vault(&Rc::new(Vault::of(state)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnKind, Connection};
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::{Edit, Replica};

    fn add(replica: &mut Replica<Vpn>, connection: &Connection) {
        let added = Vpn::paths().connections.insert_as(
            &Vpn::item(connection.id),
            usize::MAX,
            &Profile::of(connection),
        );
        replica.apply(Invoke::edit(&added).unwrap()).unwrap();
    }

    fn edit(replica: &mut Replica<Vpn>, edit: &Edit<Vpn>) -> Result<(), String> {
        replica
            .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn sampled() -> Replica<Vpn> {
        let mut replica = Replica::<Vpn>::new();
        for connection in &sample_vault().connections {
            add(&mut replica, connection);
        }
        replica
    }

    /// The shared fleet holds names, protocols and Tor preferences only:
    /// no window selection and no device's live tunnel state.
    #[test]
    fn the_fleet_round_trips_without_live_state() {
        let replica = sampled();
        let back = Vault::of(&replica.state().unwrap());
        assert_eq!(back.connections.len(), sample_vault().connections.len());
        assert_eq!(back.selected, None, "window selection is not synchronized");
        let home = back.connections.iter().find(|c| c.name == "Home").unwrap();
        assert_eq!(home.kind, ConnKind::WireGuard);
        assert!(back.connections.iter().all(|connection| {
            connection.status == ConnStatus::Disconnected
                && connection.last_handshake_secs.is_none()
        }));
        let relay = back
            .connections
            .iter()
            .find(|c| c.name == "Cloud Relay")
            .unwrap();
        assert!(relay.tor);
    }

    /// Renaming, rerouting and removing are field edits at the profile's
    /// path; a nameless profile or an absent one is refused.
    #[test]
    fn profiles_are_edited_at_their_paths() {
        let mut replica = sampled();
        let home = sample_vault()
            .connections
            .into_iter()
            .find(|c| c.name == "Home")
            .unwrap()
            .id;
        let profile = Vpn::paths().connections.item(&Vpn::item(home));
        edit(&mut replica, &profile.name.replace("House")).unwrap();
        edit(&mut replica, &profile.tor.set(true)).unwrap();
        let state = replica.state().unwrap();
        let house = state.connections.get(&Vpn::item(home)).unwrap();
        assert_eq!((house.name.as_str(), house.tor), ("House", true));
        assert!(edit(&mut replica, &profile.name.replace(" ")).is_err());
        assert!(
            edit(&mut replica, &profile.kind.set(ConnKind::OpenVpn)).is_err(),
            "the protocol is the imported profile's"
        );
        // Removing a profile asks first.
        assert!(
            edit(
                &mut replica,
                &Vpn::paths().connections.remove(&Vpn::item(home))
            )
            .unwrap_err()
            .contains("confirmation")
        );
        replica
            .apply_confirmed(Vpn::paths().connections.remove(&Vpn::item(home)))
            .unwrap();
        assert!(
            edit(
                &mut replica,
                &Vpn::paths().connections.remove(&Vpn::item(home))
            )
            .is_err(),
            "a profile that is gone is not removed again"
        );
    }

    /// Two devices adding and rerouting concurrently converge.
    #[test]
    fn two_devices_converge() {
        let mut left = sampled();
        let mut right = left.fork();
        add(&mut left, &Connection::new("Travel", ConnKind::WireGuard));
        let work = sample_vault()
            .connections
            .into_iter()
            .find(|c| c.name == "Work")
            .unwrap()
            .id;
        edit(
            &mut right,
            &Vpn::paths()
                .connections
                .item(&Vpn::item(work))
                .tor
                .set(true),
        )
        .unwrap();
        left.sync_with(&mut right).unwrap();
        assert_eq!(left.state().unwrap(), right.state().unwrap());
        let names: Vec<String> = Vault::of(&left.state().unwrap())
            .connections
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"Travel".to_owned()) && names.contains(&"Home".to_owned()));
    }

    /// An agent reroutes a profile in one call, and history says so.
    #[test]
    fn an_agent_reroutes_a_profile_and_history_says_so() {
        let mut app = TestApp::<VpnShell>::new().unwrap();
        let lab = Connection::new("Lab", ConnKind::OpenVpn);
        let added = Vpn::paths()
            .connections
            .insert_as(&Vpn::item(lab.id), 0, &Profile::of(&lab));
        app.invoke(Invoke::edit(&added).unwrap()).unwrap();
        let tor = Vpn::paths().connections.item(&Vpn::item(lab.id)).tor;
        app.call("field-edit", serde_json::json!({ "edit": tor.set(true) }))
            .unwrap();
        assert!(app.state().connections.values().all(|profile| profile.tor));
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(sentences, ["Added a VPN profile", "Routed Lab over Tor"]);
    }
}

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::network_settings::{Ipv6Configuration, VpnProfileSettings};
use os_actions::{SavedNetworkProfile, WifiNetworkInfo};
use ui::collaboration::{NetworkWakeStatus, WifiContinuityCommand, WifiContinuityReply};
use ui::framework::Effected;
use ui::tilo_resource::Effect;
use ui::widget::{DisclosureRow, Widget};
use zeroize::Zeroizing;

use super::vpn::VpnEffect;
use super::*;

mod device_vpn;
mod vpn_kill_switch;

#[test]
fn network_pane_relabels_retained_controls_when_locale_changes() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};
    use ui::i18n::I18nManager;

    let fake = Arc::new(FakeNetwork::default());
    let mut network = model(&fake);
    network.open_page(PAGE_ADVANCED, t("net-advanced"));
    let _ = t("net-run-diagnostics");
    I18nManager::set_locale("en-US");
    let root = ModelPane::new(Rc::new(RefCell::new(network)));
    let mut app = Framed::new(
        ui::State::new(),
        root,
        viewport((1200.0, 1800.0)),
        ui::Theme::dark(),
    );
    let host = Framed::harness_mut(&mut app);
    for (locale, expected) in [
        ("en-US", "Run diagnostics"),
        ("de-DE", "Diagnose ausführen"),
        ("en-US", "Run diagnostics"),
    ] {
        I18nManager::set_locale(locale);
        host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
            .map_err(|error| error.to_string())?;
        let name = ui::widget::find_test_id(host.root_widget(), "settings.network.diagnostics")
            .and_then(Widget::accessibility)
            .and_then(|node| node.name);
        if name.as_deref() != Some(expected) {
            return Err(format!(
                "the actual network diagnostics control must follow {locale}: {name:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn network_editor_retains_caret_through_window_resize() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};

    let fake = Arc::new(FakeNetwork::default());
    let mut network = model(&fake);
    network.open_page(PAGE_ADVANCED, t("net-advanced"));
    network.enterprise_setup.set_open(true);
    let profile = network.enterprise.profile.clone();
    let root = ModelPane::new(Rc::new(RefCell::new(network)));
    let mut app = Framed::new(
        ui::State::new(),
        root,
        viewport((1200.0, 1800.0)),
        ui::Theme::dark(),
    );
    let host = Framed::harness_mut(&mut app);
    host.click_named("network.enterprise.profile")?;
    host.type_text("abcd");
    if profile.get() != "abcd" {
        return Err("the real network profile editor must accept typing".into());
    }
    host.key("Left");
    host.key("Left");
    host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
        .map_err(|error| error.to_string())?;
    host.type_text("X");
    if profile.get() != "abXcd" {
        return Err("resizing must retain the network editor's actual caret and focus".into());
    }
    Ok(())
}

/// The work or school network's setup in the composition: its row, and
/// whether that row is open.
fn enterprise_setup_open(widget: &dyn Widget) -> Option<(bool, vello::kurbo::Rect)> {
    if let Some(row) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<DisclosureRow>())
        && row
            .accessibility()
            .and_then(|node| node.name)
            .is_some_and(|name| name.starts_with(&t("net-enterprise-entry")))
    {
        return Some((row.is_open(), row.get_rect()));
    }
    widget
        .children()
        .into_iter()
        .find_map(enterprise_setup_open)
}

/// The work or school network's setup opens in place under its row on the
/// standard disclose motion. Its action and a press on the row open the
/// same row: an agent and a person land in the same place.
#[test]
fn the_work_or_school_setup_opens_under_its_row() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};

    let fake = Arc::new(FakeNetwork::default());
    let network = Rc::new(RefCell::new(model(&fake)));
    network
        .borrow_mut()
        .open_page(PAGE_ADVANCED, t("net-advanced"));
    let shown_open = |network: &Rc<RefCell<NetworkModel>>| {
        let composed = network.borrow().build_composition();
        enterprise_setup_open(composed.as_ref()).map(|(open, _)| open)
    };
    if shown_open(&network) != Some(false) {
        return Err("the setup starts closed, under a disclosure row".into());
    }
    run(
        &mut network.borrow_mut(),
        NetworkEffect::ShowEnterpriseSetup,
    )?;
    if shown_open(&network) != Some(true) {
        return Err("the action opens the row".into());
    }
    run(
        &mut network.borrow_mut(),
        NetworkEffect::ShowEnterpriseSetup,
    )?;
    if network.borrow().enterprise_setup.is_open() {
        return Err("the action closes the row again".into());
    }

    let root = ModelPane::new(Rc::clone(&network));
    let mut app = Framed::new(
        ui::State::new(),
        root,
        viewport((1000.0, 1800.0)),
        ui::Theme::dark(),
    );
    let host = Framed::harness_mut(&mut app);
    let (open, rect) =
        enterprise_setup_open(host.root_widget()).ok_or("the setup's row is on the page")?;
    if open {
        return Err("the setup is closed before the press".into());
    }
    host.click_at(rect.center().x, rect.y0 + 12.0);
    if !network.borrow().enterprise_setup.is_open() {
        return Err("a press on the row opens the setup the action opens".into());
    }
    host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
        .map_err(|error| error.to_string())?;
    host.click_named("network.enterprise.profile")?;
    host.type_text("Campus");
    if network.borrow().enterprise.profile.get() != "Campus" {
        return Err("the opened setup takes typing".into());
    }
    Ok(())
}

#[test]
fn network_pane_adopts_model_changes_after_layout_reuse() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};

    let fake = Arc::new(FakeNetwork::default());
    let network = Rc::new(RefCell::new(model(&fake)));
    let root = ModelPane::new(Rc::clone(&network));
    let mut app = Framed::new(
        ui::State::new(),
        root,
        viewport((1200.0, 1800.0)),
        ui::Theme::dark(),
    );
    let host = Framed::harness_mut(&mut app);
    host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
        .map_err(|error| error.to_string())?;
    {
        let mut model = network.borrow_mut();
        model.open_page(PAGE_ADVANCED, t("net-advanced"));
        model.enterprise_setup.set_open(true);
        model.changed = true;
    }
    host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
        .map_err(|error| error.to_string())?;
    host.click_named("network.enterprise.profile")?;
    host.type_text("New profile");
    if network.borrow().enterprise.profile.get() != "New profile" {
        return Err("a changed model must expose its newly composed controls".into());
    }
    Ok(())
}

#[test]
fn network_picker_remains_open_through_window_resize() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};

    fn expanded(host: &ui::headless::FrameHarness) -> Result<bool, String> {
        ui::widget::find_test_id(host.root_widget(), "network.hotspot.band")
            .and_then(Widget::accessibility)
            .and_then(|node| node.state.expanded)
            .ok_or_else(|| "the actual band picker must expose its expanded state".into())
    }
    let fake = Arc::new(FakeNetwork::default());
    let mut network = model(&fake);
    network.open_page(PAGE_HOTSPOT, t("net-hotspot"));
    // The networks were just looked for, and the clock holds still: a
    // periodic scan landing between the click and the resize rebuilds
    // the pane, which closes any picker, and is not what is under test.
    let held = ui::time::now();
    network.scanned_at = Some(held);
    ui::time::testing::with_monotonic_time(held, || {
        let root = ModelPane::new(Rc::new(RefCell::new(network)));
        let mut app = Framed::new(
            ui::State::new(),
            root,
            viewport((1200.0, 1800.0)),
            ui::Theme::dark(),
        );
        let host = Framed::harness_mut(&mut app);
        host.click_named("network.hotspot.band")?;
        if !expanded(host)? {
            return Err("the real network band picker must open".into());
        }
        host.relayout_themed(viewport((1000.0, 1800.0)), ui::Theme::dark())
            .map_err(|error| error.to_string())?;
        if !expanded(host)? {
            return Err("resizing must retain the open network picker".into());
        }
        Ok(())
    })
}

struct SnapshotGate {
    started: crate::channel::Sender<()>,
    release: crate::channel::Receiver<()>,
}

struct ReleaseSnapshot(Option<crate::channel::Sender<()>>);

impl Drop for ReleaseSnapshot {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn pending_network() -> Result<(Shared<NetworkModel>, ReleaseSnapshot), String> {
    let (started, starts) = crate::channel::channel();
    let (release, releases) = crate::channel::channel();
    let fake = Arc::new(FakeNetwork::default());
    *fake.snapshot_gate.lock().unwrap() = Some(SnapshotGate {
        started,
        release: releases,
    });
    let profile = tempfile::tempdir().map_err(|error| error.to_string())?;
    let mut network =
        NetworkModel::with_service(PaneCx::in_file(profile.path().join("settings.json")), fake);
    let release = ReleaseSnapshot(Some(release));
    network.start_snapshot();
    starts
        .recv_timeout(tilo_test_env::hang_bound())
        .map_err(|error| format!("the actual network read must start: {error}"))?;
    Ok((Rc::new(RefCell::new(network)), release))
}

#[test]
fn pending_network_read_does_not_animate_the_pane() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};

    let (network, _release) = pending_network()?;
    let pane = ModelPane::new(network);
    let app = Framed::new(
        ui::State::new(),
        pane,
        viewport((1200.0, 1800.0)),
        ui::Theme::dark(),
    );
    if Framed::harness(&app).frame_schedule().needs_animation_frame {
        return Err(
            "waiting for a network read must not request continuous animation frames".into(),
        );
    }
    Ok(())
}

#[test]
fn completed_network_read_wakes_the_owner_and_updates_the_pane() -> Result<(), String> {
    use crate::panes::model::ModelPane;
    use ui::headless::{Framed, viewport};

    let mut scope = ui::WakeScope::new();
    let (woke, wakes) = crate::channel::channel();
    scope
        .bind(move |_| {
            let _ = woke.send(());
        })
        .map_err(|error| error.to_string())?;
    let _entry = scope.enter();
    let (network, release) = pending_network()?;
    let pane = ModelPane::new(Rc::clone(&network));
    let mut app = Framed::new(
        ui::State::new(),
        pane,
        viewport((1200.0, 1800.0)),
        ui::Theme::dark(),
    );
    drop(release);
    wakes
        .recv_timeout(tilo_test_env::hang_bound())
        .map_err(|error| format!("a completed network read must wake its real owner: {error}"))?;
    Framed::harness_mut(&mut app)
        .relayout_at(viewport((1200.0, 1800.0)))
        .map_err(|error| error.to_string())?;
    if !network.borrow().loaded {
        return Err("the awakened production pane must adopt its completed snapshot".into());
    }
    Ok(())
}

/// A device whose `NetworkManager` answers from what the test sets,
/// recording each change and keeping each saved profile's settings as the
/// changes leave them.
#[derive(Default)]
struct FakeNetwork {
    networks: Mutex<Vec<WifiNetworkInfo>>,
    unreadable: Mutex<Option<String>>,
    refuse: Mutex<Option<String>>,
    shared_password: Mutex<Option<String>>,
    changes: Mutex<Vec<String>>,
    continuity: Mutex<Vec<WifiContinuityCommand>>,
    vpns: Mutex<Vec<VpnInfo>>,
    vpn_settings: Mutex<HashMap<String, VpnProfileSettings>>,
    /// The VPNs set up, as `kind name-or-file`.
    created: Mutex<Vec<String>>,
    profiles: Mutex<Vec<SavedNetworkProfile>>,
    settings: Mutex<HashMap<String, os_actions::ConnectionProfileSettings>>,
    snapshot_gate: Mutex<Option<SnapshotGate>>,
    snapshot_reads: std::sync::atomic::AtomicUsize,
    connectivity_rechecks: std::sync::atomic::AtomicUsize,
    /// Scans, each recorded with the snapshot reads made before it.
    scans: Mutex<Vec<usize>>,
    scan_refusal: Mutex<Option<String>>,
    /// What the session publishes of the person's devices as VPNs.
    device_vpn: Mutex<Option<settings_store::device_vpn::DeviceVpnStatus>>,
}

impl FakeNetwork {
    fn apply(&self, change: &NetworkChange) {
        match change {
            NetworkChange::Policy { uuid, patch, .. } => {
                let mut settings = self.settings.lock().unwrap();
                let held = settings.entry(uuid.clone()).or_default();
                if let Some(priority) = patch.priority {
                    held.priority = Some(priority);
                }
                if let Some(metered) = patch.metered {
                    held.metered = Some(metered);
                }
                if let Some(mac) = patch.mac {
                    held.mac = Some(mac);
                }
                if let Some(hidden) = patch.hidden {
                    held.hidden = Some(hidden);
                }
                if let Some(proxy) = &patch.proxy {
                    held.proxy = Some(proxy.clone());
                }
                drop(settings);
            }
            NetworkChange::Dns { uuid, servers, .. } => {
                if let Some(ipv4) = self
                    .settings
                    .lock()
                    .unwrap()
                    .entry(uuid.clone())
                    .or_default()
                    .ipv4
                    .as_mut()
                {
                    ipv4.dns.clone_from(servers);
                }
            }
            NetworkChange::Ipv6 { uuid, config, .. } => {
                self.settings
                    .lock()
                    .unwrap()
                    .entry(uuid.clone())
                    .or_default()
                    .ipv6 = Some(config.clone());
            }
            NetworkChange::Autoconnect { uuid, on, .. } => {
                if let Some(profile) = self
                    .profiles
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|profile| &profile.uuid == uuid)
                {
                    profile.autoconnect = *on;
                }
            }
            NetworkChange::Forget { uuid, .. } => {
                self.profiles
                    .lock()
                    .unwrap()
                    .retain(|profile| &profile.uuid != uuid);
            }
            NetworkChange::Vpn { uuid, on, .. } => {
                if let Some(vpn) = self
                    .vpns
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|vpn| &vpn.uuid == uuid)
                {
                    vpn.active = *on;
                }
            }
            NetworkChange::VpnProfile { uuid, patch, .. } => {
                if let Some(held) = self.vpn_settings.lock().unwrap().get_mut(uuid) {
                    if let Some(name) = &patch.name {
                        held.name.clone_from(name);
                    }
                    if let Some(dns) = &patch.dns {
                        held.dns.clone_from(dns);
                    }
                    if let Some(keepalive) = patch.keepalive {
                        held.keepalive = keepalive;
                    }
                    if let Some(on) = patch.all_traffic {
                        held.all_traffic = on;
                    }
                    if let Some(on) = patch.autoconnect {
                        held.autoconnect = on;
                    }
                    if let Some(mtu) = patch.mtu {
                        held.mtu = mtu;
                    }
                    if let Some(addresses) = &patch.allowed_ips {
                        held.allowed_ips.clone_from(addresses);
                    }
                }
            }
            NetworkChange::VpnDelete { uuid, .. } => {
                self.vpns.lock().unwrap().retain(|vpn| &vpn.uuid != uuid);
            }
            _ => {}
        }
    }
}

impl NetworkService for FakeNetwork {
    /// The periodic look reads only the networks, not a whole snapshot.
    fn networks(&self) -> NetworkSnapshot {
        let unreadable = self.unreadable.lock().unwrap().clone();
        NetworkSnapshot::networks_only(match unreadable {
            Some(reason) => Err(reason),
            None => Ok(self.networks.lock().unwrap().clone()),
        })
    }

    fn scan(&self) -> Result<(), String> {
        let reads = self
            .snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        self.scans.lock().unwrap().push(reads);
        self.scan_refusal
            .lock()
            .unwrap()
            .clone()
            .map_or(Ok(()), Err)
    }

    fn recheck_connectivity(&self) {
        self.connectivity_rechecks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn snapshot(&self, _initial: bool) -> NetworkSnapshot {
        self.snapshot_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let gate = self.snapshot_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.started.send(()).unwrap();
            gate.release
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap();
        }
        let unreadable = self.unreadable.lock().unwrap().clone();
        if let Some(reason) = unreadable {
            fn failed<T>(reason: &str) -> Result<T, String> {
                Err(reason.to_string())
            }
            return NetworkSnapshot {
                networks: Err(reason.clone()),
                wifi_enabled: Some(failed(&reason)),
                wired: Some(failed(&reason)),
                radios: Some(failed(&reason)),
                connectivity: Some(failed(&reason)),
                profiles: Some(failed(&reason)),
                profile_settings: HashMap::new(),
                wake_settings: HashMap::new(),
                hotspot_clients: Some(failed(&reason)),
                vpns: Some(failed(&reason)),
                link: None,
                plugins: None,
                device_vpn: DeviceVpnPart::NotRead,
            };
        }
        let mut snapshot = NetworkSnapshot::of_networks(self.networks.lock().unwrap().clone());
        snapshot.vpns = Some(Ok(self.vpns.lock().unwrap().clone()));
        snapshot.profiles = Some(Ok(self.profiles.lock().unwrap().clone()));
        snapshot.profile_settings = self.settings.lock().unwrap().clone();
        snapshot.plugins = Some(vec![os_actions::network_settings::VpnPlugin {
            service: "org.freedesktop.NetworkManager.openvpn".into(),
            name: "OpenVPN".into(),
        }]);
        snapshot.device_vpn = DeviceVpnPart::Read(self.device_vpn());
        snapshot
    }

    fn device_vpn(&self) -> Option<settings_store::device_vpn::DeviceVpnStatus> {
        self.device_vpn.lock().unwrap().clone()
    }

    fn change(&self, change: &NetworkChange) -> Result<(), String> {
        let said = match change {
            NetworkChange::Wifi(on) => format!("wifi {on}"),
            NetworkChange::Radio(radio, on) => format!("{radio:?} {on}"),
            NetworkChange::Vpn { name, on, .. } => format!("vpn {name} {on}"),
            NetworkChange::VpnProfile { name, patch, .. } => {
                format!("vpn-profile {name} {patch:?}")
            }
            NetworkChange::VpnDelete { name, .. } => format!("vpn-delete {name}"),
            NetworkChange::Connect { ssid, password } => format!(
                "connect {ssid} {}",
                password.as_deref().map_or("-", String::as_str)
            ),
            NetworkChange::JoinHidden { ssid, password } => format!(
                "join-hidden {ssid} {}",
                password.as_deref().map_or("-", String::as_str)
            ),
            NetworkChange::Wake { uuid, on } => format!("wake {uuid} {on}"),
            NetworkChange::Forget { uuid, .. } => format!("forget {uuid}"),
            NetworkChange::Hotspot(config) => format!("hotspot {}", config.ssid),
            NetworkChange::Policy { uuid, .. } => format!("policy {uuid}"),
            NetworkChange::Dns { uuid, servers, .. } => format!("dns {uuid} {}", servers.join(",")),
            NetworkChange::Ipv6 { uuid, .. } => format!("ipv6 {uuid}"),
            NetworkChange::Password { uuid, password, .. } => {
                format!("password {uuid} {}", password.as_str())
            }
            _ => "other".into(),
        };
        self.changes.lock().unwrap().push(said);
        let refusal = self.refuse.lock().unwrap().clone();
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        self.apply(change);
        Ok(())
    }

    fn create_vpn(&self, setup: &VpnSetup) -> Result<String, String> {
        crate::device_read::off_ui_thread();
        let said = match setup {
            VpnSetup::Text { kind, name, text } => {
                if text.is_empty() {
                    return Err("a tunnel is set up from its text".into());
                }
                format!("{kind} {name}")
            }
            VpnSetup::File { kind, path } => format!(
                "{kind} {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            VpnSetup::Plugin {
                service,
                name,
                server,
            } => format!("{service} {name} {server}"),
        };
        self.created.lock().unwrap().push(said);
        let uuid = "c0ffee00-0000-4000-8000-000000000001".to_owned();
        self.vpns.lock().unwrap().push(VpnInfo {
            name: "New".into(),
            uuid: uuid.clone(),
            kind: "wireguard".into(),
            service: None,
            active: false,
        });
        Ok(uuid)
    }

    fn vpn_settings(&self, uuid: &str) -> Result<VpnProfileSettings, String> {
        self.vpn_settings
            .lock()
            .unwrap()
            .get(uuid)
            .cloned()
            .ok_or_else(|| "no such VPN".into())
    }

    fn export_vpn(&self, _uuid: &str) -> Result<Zeroizing<String>, String> {
        Ok(Zeroizing::new("[Interface]\n".into()))
    }

    fn wifi_password(&self, _uuid: &str) -> Result<Zeroizing<String>, String> {
        Ok(Zeroizing::new("saved-secret".into()))
    }

    fn continuity(&self, command: WifiContinuityCommand) -> Result<WifiContinuityReply, String> {
        self.continuity.lock().unwrap().push(command.clone());
        Ok(match command {
            WifiContinuityCommand::Lookup { .. } => {
                let shared = self.shared_password.lock().unwrap().clone();
                shared.map_or(WifiContinuityReply::NotFound, |password| {
                    WifiContinuityReply::Password { password }
                })
            }
            WifiContinuityCommand::NetworkWake { profile_uuid, .. } => {
                WifiContinuityReply::NetworkWake {
                    status: NetworkWakeStatus {
                        profile: profile_uuid,
                        interface: None,
                        configured: false,
                        supported: None,
                        applied: None,
                        problem: None,
                    },
                }
            }
            WifiContinuityCommand::Share { ssid, .. }
            | WifiContinuityCommand::ShareSavedProfile { profile_uuid: ssid } => {
                WifiContinuityReply::Shared { ssid }
            }
        })
    }

    fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String> {
        Err("no bus".into())
    }

    fn sign_in_page(&self) -> Result<String, String> {
        Err("no portal".into())
    }
}

fn networks() -> Vec<WifiNetworkInfo> {
    SampleNetwork::networks()
}

fn saved(uuid: &str, name: &str, last_used: u64) -> SavedNetworkProfile {
    SavedNetworkProfile {
        uuid: uuid.into(),
        name: name.into(),
        kind: "802-11-wireless".into(),
        autoconnect: true,
        last_used: Some(last_used),
    }
}

fn model(fake: &Arc<FakeNetwork>) -> NetworkModel {
    let profile = tempfile::tempdir().unwrap().keep();
    NetworkModel::loaded_from(
        PaneCx::in_file(profile.join("settings.json")),
        Arc::clone(fake) as Arc<dyn NetworkService>,
    )
}

/// A model over `service` in `app`'s window, its controls bound to the
/// actions the window publishes.
fn hosted_model(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    service: Arc<dyn NetworkService>,
) -> NetworkModel {
    let cx = app
        .surface::<SettingsApp, _>(|settings| {
            crate::panes::network::area(settings).unwrap().cx.clone()
        })
        .unwrap();
    NetworkModel::loaded_from(cx, service)
}

fn mount_network(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    observed: NetworkModel,
) -> Option<()> {
    app.with_surface::<SettingsApp, _>(|settings| {
        *crate::panes::network::area(settings)
            .unwrap()
            .model()
            .borrow_mut() = observed;
    })
}

/// Settle `app` until `done` says the model is where the test waits for it.
fn settle_until(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    done: impl Fn(&NetworkModel) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let reached = app
            .surface::<SettingsApp, _>(|settings| {
                let model = crate::panes::network::area(settings)
                    .unwrap()
                    .model()
                    .borrow();
                done(&model)
            })
            .unwrap();
        if reached {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the network work never landed"
        );
        std::thread::yield_now();
    }
}

#[test]
fn declared_network_inspection_reads_the_mounted_model_without_rescanning() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    let observed = model(&fake);
    observed.enterprise_setup.set_open(true);
    *observed.hotspot_password.lock().unwrap() = "private-hotspot-input".into();
    let mut app =
        ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(ui::Role::Viewer)
            .expect("read-only Settings opens");
    mount_network(&mut app, observed).expect("the ordinary mounted network model");
    crate::effects::show_pane(&mut app, crate::categories::Category::Network);
    app.settle();
    let before = app
        .call("inspect-network-state", serde_json::json!({}))
        .expect("the network state must have a declared read-only door")
        .expect("the inspector returns the observed model");
    assert_eq!(before["network_count"], networks().len());
    assert_eq!(before["connectivity"]["state"], "full");
    assert_eq!(
        before["source"], "preview",
        "test stand-ins cannot masquerade as device observations"
    );
    let row_ids = app
        .surface::<SettingsApp, _>(|settings| {
            settings
                .areas()
                .find::<crate::panes::network::NetworkArea>()
                .unwrap()
                .rows()
                .into_iter()
                .map(|row| row.id().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap();
    for network in before["networks"].as_array().unwrap() {
        assert!(
            row_ids
                .iter()
                .any(|id| id == network["resource"].as_str().unwrap())
        );
    }
    let shown = crate::widgets::announced_text(app.harness().root_widget());
    let show_all = before["labels"]["show_all"].as_str().unwrap().to_owned();
    assert!(
        shown.iter().any(|line| line.contains(&show_all)),
        "the Inspector and mounted pane use the same resolved caption: {shown:#?}"
    );
    let encoded = serde_json::to_string(&before).unwrap();
    for secret in [
        "private-enterprise-input",
        "private-hotspot-input",
        "private-pin-input",
    ] {
        assert!(
            !encoded.contains(secret),
            "explicit network reads omit private input buffers"
        );
    }
    assert!(
        app.call("refresh-networks", serde_json::json!({})).is_err(),
        "Read does not grant scan/write authority"
    );
    assert!(
        app.call("inspect-network-state", serde_json::json!({"on":false}))
            .is_err(),
        "Read accepts no mutation-shaped arguments"
    );
    let after = app
        .call("inspect-network-state", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(
        before, after,
        "inspection never refreshes or changes network state"
    );
    assert_eq!(
        fake.snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert!(fake.changes.lock().unwrap().is_empty());
    assert!(app.harness().component_contract_violations().is_empty());
}

#[test]
fn network_inspection_observes_the_actual_pending_refresh_and_failed_snapshot() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    let observed = model(&fake);
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    mount_network(&mut app, observed).unwrap();
    app.settle();
    let (started, starts) = crate::channel::channel();
    let (release, releases) = crate::channel::channel();
    *fake.snapshot_gate.lock().unwrap() = Some(SnapshotGate {
        started,
        release: releases,
    });
    let mut release = ReleaseSnapshot(Some(release));
    *fake.unreadable.lock().unwrap() = Some("NetworkManager query refused".into());
    let effect = SettingsEffect::from(NetworkEffect::RefreshNetworks);
    assert!(
        app.operate(ui::tilo_resource::Invoke::effect(&effect))
            .unwrap()
            .is_none(),
        "scan completion awaits its actual worker"
    );
    starts.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    let pending = app
        .call("inspect-network-state", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(pending["scan"]["jobs_pending"], 1);
    assert_eq!(
        pending["network_count"],
        networks().len(),
        "pending scans preserve the last adopted rows"
    );
    release.0.take().unwrap().send(()).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let failed = loop {
        app.settle();
        let snapshot = app
            .call("inspect-network-state", serde_json::json!({}))
            .unwrap()
            .unwrap();
        if snapshot["scan"]["jobs_pending"] == 0 {
            break snapshot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the real worker outcome must land"
        );
        std::thread::yield_now();
    };
    assert_eq!(failed["errors"]["networks"], "NetworkManager query refused");
    assert_eq!(
        failed["network_count"], 0,
        "failed scans do not publish stale rows as current"
    );
    assert!(
        failed["wifi"]["enabled"].is_null(),
        "unreadable radio state is never fabricated as off"
    );
    assert!(failed["connectivity"]["state"].is_null());
    assert_eq!(
        fake.snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[test]
fn network_inspection_does_not_initialize_an_unopened_area_and_bounds_private_reads() {
    let profile = tempfile::tempdir().unwrap();
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    let area = NetworkArea::new(&cx);
    assert_eq!(area.inspection().unwrap()["state"], "unopened");
    assert!(
        area.model.get().is_none(),
        "Read must not lazily start a device scan"
    );
    let fake = Arc::new(FakeNetwork::default());
    let mut observed = NetworkModel::with_service(cx, fake);
    let initial = observed.inspection().unwrap();
    assert_eq!(initial["state"], "loading");
    assert!(initial["wifi"]["enabled"].is_null());
    assert!(initial["connectivity"]["state"].is_null());
    observed.networks = vec![
        WifiNetworkInfo {
            ssid: "x".repeat(1024),
            signal: 0,
            secure: false,
            security: os_actions::WifiSecurity::Open,
            connected: false,
            known: false,
        };
        257
    ];
    assert!(
        observed.inspection().is_err(),
        "the explicit Read has a finite byte bound"
    );
    for row in &mut observed.networks {
        row.ssid = "bounded".into();
    }
    let bounded = observed.inspection().unwrap();
    assert_eq!(bounded["network_count"], 257);
    assert_eq!(bounded["networks"].as_array().unwrap().len(), 256);
    assert_eq!(bounded["networks_truncated"], true);
}

/// Begin `effect` and run its work here, as a worker would; what it
/// answered.
fn run(model: &mut NetworkModel, effect: NetworkEffect) -> Result<Option<String>, String> {
    finish(model, |model| model.begin(effect))
}

fn run_vpn(model: &mut NetworkModel, effect: VpnEffect) -> Result<Option<String>, String> {
    finish(model, |model| model.begin_vpn(effect))
}

fn finish(
    model: &mut NetworkModel,
    begin: impl FnOnce(&mut NetworkModel) -> Result<Begun, String>,
) -> Result<Option<String>, String> {
    match begin(model)? {
        Begun::Done => Ok(None),
        Begun::Work(work) => match model.land(work())? {
            Effected::Done(output) => Ok(output
                .value
                .and_then(|value| value.as_str().map(str::to_owned))),
            Effected::Pending => Ok(None),
        },
    }
}

/// The ordinary worker completion must honor the action a caller invoked,
/// while keeping its human-readable outcome in the pane.
#[test]
fn network_workers_complete_their_published_unit_jobs() -> Result<(), String> {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().map_err(|error| error.to_string())? = networks();
    let mut model = model(&fake);
    for (effect, notice) in [
        (NetworkEffect::RefreshNetworks, t("net-notice-refreshed")),
        (NetworkEffect::SetWifi { on: true }, t("net-notice-wifi-on")),
    ] {
        let contract = NetworkEffect::specs()
            .into_iter()
            .find(|spec| spec.id == effect.id())
            .ok_or("the invoked action must be published")?
            .contract;
        if contract.result != ui::tilo_actions::ResultSpec::Job {
            return Err("the network action must declare a unit job".into());
        }
        let Begun::Work(work) = model.begin(effect)? else {
            return Err("the invoked network action must start real work".into());
        };
        let Effected::Done(output) = model.land(work())? else {
            return Err("finished network work must complete".into());
        };
        contract.result.validate(&output)?;
        if output.value.is_some() || !output.resources.is_empty() {
            return Err("the unit job must not return a value or resources".into());
        }
        if model.notice.as_deref() != Some(notice.as_str()) {
            return Err("the completed job must retain its status notice".into());
        }
        if !shown(&model).contains(&notice) {
            return Err("the completed job's notice must remain visible".into());
        }
    }
    if model.networks().len() != networks().len() || !model.wifi_enabled {
        return Err("the completed workers must adopt networks and enable Wi-Fi".into());
    }
    if fake
        .changes
        .lock()
        .map_err(|error| error.to_string())?
        .as_slice()
        != ["wifi true"]
    {
        return Err("the worker must issue exactly one Wi-Fi enable request".into());
    }
    Ok(())
}

fn shown(model: &NetworkModel) -> Vec<String> {
    crate::widgets::announced_text(model.build_composition().as_ref())
}

/// The titles of the network rows composed, top to bottom.
fn network_rows(model: &NetworkModel) -> Vec<String> {
    fn walk(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(id) = widget.test_id()
            && let Some(ssid) = id.strip_prefix("settings.network.connect.")
        {
            out.push(ssid.to_owned());
            return;
        }
        if let Some(row) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<DisclosureRow>())
            && let Some(name) = row.accessibility().and_then(|node| node.name)
        {
            out.push(name.split(',').next().unwrap_or_default().to_owned());
            return;
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    let composed = model.build_composition();
    let mut out = Vec::new();
    walk(composed.as_ref(), &mut out);
    out
}

/// Wait for the read a landed change started, as the pane's poll does.
fn settle(model: &mut NetworkModel) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        model.poll_background();
        if model.snapshot.is_none() && model.jobs.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the read never landed"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The Wi-Fi switch as composed: whether it is on, and whether it can be
/// used.
fn wifi_switch(model: &NetworkModel) -> (Option<bool>, bool) {
    let composed = model.build_composition();
    let node = ui::widget::find_test_id(composed.as_ref(), "network.wifi")
        .and_then(Widget::accessibility)
        .expect("the Wi-Fi switch is composed");
    (node.state.checked, !node.state.disabled)
}

/// `NetworkManager`'s flag and the kernel's kill switch, as far as Wi-Fi on
/// and off goes: turning on fails while a hardware switch holds the radio
/// off, and every read says how the radio stands.
#[derive(Default)]
struct NmRadio {
    on: Mutex<bool>,
    hard: Mutex<bool>,
}

impl NetworkService for NmRadio {
    fn snapshot(&self, _initial: bool) -> NetworkSnapshot {
        let on = *self.on.lock().unwrap() && !*self.hard.lock().unwrap();
        let mut snapshot = NetworkSnapshot::of_networks(if on { networks() } else { Vec::new() });
        snapshot.wifi_enabled = Some(Ok(on));
        snapshot.radios = Some(Ok(os_actions::RadioState {
            wifi_available: true,
            wifi_enabled: on,
            wifi_hardware_blocked: *self.hard.lock().unwrap(),
            ..os_actions::RadioState::default()
        }));
        snapshot
    }
    fn change(&self, change: &NetworkChange) -> Result<(), String> {
        if let NetworkChange::Wifi(on) = change {
            if *on && *self.hard.lock().unwrap() {
                return Err("Wi-Fi is switched off with the hardware switch".into());
            }
            *self.on.lock().unwrap() = *on;
        }
        Ok(())
    }
    fn create_vpn(&self, _: &VpnSetup) -> Result<String, String> {
        Err("no VPN".into())
    }
    fn vpn_settings(&self, _: &str) -> Result<VpnProfileSettings, String> {
        Err("no VPN".into())
    }
    fn export_vpn(&self, _: &str) -> Result<Zeroizing<String>, String> {
        Err("no VPN".into())
    }
    fn wifi_password(&self, _: &str) -> Result<Zeroizing<String>, String> {
        Err("no password".into())
    }
    fn continuity(&self, _: WifiContinuityCommand) -> Result<WifiContinuityReply, String> {
        Ok(WifiContinuityReply::NotFound)
    }
    fn diagnostics(&self) -> Result<os_actions::ConnectionDiagnostics, String> {
        Err("no bus".into())
    }
    fn sign_in_page(&self) -> Result<String, String> {
        Err("no portal".into())
    }
}

/// The Wi-Fi switch is the radio as `NetworkManager` and the kernel report
/// it, read back after every change: off and on again lands each time, and
/// a radio a hardware switch holds off shows off, with the switch unusable
/// and the reason in place of the networks.
#[test]
fn the_wifi_switch_follows_the_radio_and_not_the_request() {
    let nm = Arc::new(NmRadio::default());
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let mut model = hosted_model(&mut app, Arc::clone(&nm) as Arc<dyn NetworkService>);
    assert_eq!(wifi_switch(&model), (Some(false), true));
    let text = shown(&model).join("\n");
    assert!(
        text.contains(&t("net-wifi-off")),
        "the Wi-Fi card says it is off, its switch the fix: {text}"
    );

    for on in [true, false, true] {
        run(&mut model, NetworkEffect::SetWifi { on }).unwrap();
        settle(&mut model);
        assert_eq!(wifi_switch(&model), (Some(on), true), "after turning {on}");
        assert_eq!(*nm.on.lock().unwrap(), on);
    }
    assert!(
        shown(&model).join("\n").contains("Home_Network"),
        "on, the networks in range are listed"
    );

    run(&mut model, NetworkEffect::SetWifi { on: false }).unwrap();
    settle(&mut model);
    *nm.hard.lock().unwrap() = true;
    let refused = run(&mut model, NetworkEffect::SetWifi { on: true });
    assert!(refused.is_err());
    settle(&mut model);
    assert_eq!(
        wifi_switch(&model),
        (Some(false), false),
        "the radio's state, not the request; nothing to switch"
    );
    assert!(
        shown(&model)
            .join("\n")
            .contains(&t("net-wifi-hardware-switch"))
    );
}

/// The first page lists the network in use, every known network in range
/// and the five strongest others, in that order; Show all counts every
/// network in range and opens them all, narrowed by the window's search.
#[test]
fn the_wifi_list_shows_the_known_and_five_others_and_show_all_counts_the_rest() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.profiles.lock().unwrap() = vec![saved("library-uuid", "Library", 1)];
    let mut model = model(&fake);
    let first = network_rows(&model);
    assert_eq!(
        first,
        [
            "Home_Network",
            "Library",
            "Cafe_Guest",
            "Airport_Free",
            "Office_5G",
            "Printer_Direct",
            "Bakery",
        ],
        "in use, known, then the five strongest others, equal strengths by name"
    );
    let show_all = t_args("net-show-all", &[("count", networks().len().into())]);
    assert!(shown(&model).contains(&show_all), "{:#?}", shown(&model));

    run(&mut model, NetworkEffect::ShowAllNetworks).unwrap();
    let all = network_rows(&model);
    assert_eq!(all.len(), networks().len(), "every network: {all:?}");
    assert_eq!(&all[..2], ["Home_Network", "Library"]);
    assert!(
        shown(&model)
            .iter()
            .any(|line| line.contains(&t("net-hidden-entry"))),
        "the way into a hidden network is on the full list"
    );
    model.set_filter("5");
    assert_eq!(network_rows(&model), ["Office_5G", "Studio_5"]);
    assert!(model.cx.pages().back());
    let _ = model.poll();
    assert!(model.filter.is_empty(), "leaving the page drops its search");
}

/// While the pointer is over the list, a network that grows stronger keeps
/// its place; let go, it moves.
#[test]
fn a_held_list_keeps_its_rows_until_the_pointer_leaves() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    let mut model = model(&fake);
    let before = network_rows(&model);
    model.held_short.set(true);
    let mut stronger = networks();
    if let Some(bakery) = stronger.iter_mut().find(|net| net.ssid == "Bakery") {
        bakery.signal = 4;
    }
    let _ = model.adopt_snapshot(NetworkSnapshot::networks_only(Ok(stronger)));
    assert_eq!(network_rows(&model), before, "held, nothing moves");
    model.held_short.set(false);
    assert!(model.poll(), "letting go recomposes");
    assert_eq!(
        network_rows(&model)[2],
        "Bakery",
        "let go, the stronger network moves up to lead the unknown ones"
    );
}

/// A network's page shows its status and, saved, its settings; each change
/// there is its action on the network's row, and Forget closes the page.
#[test]
fn a_network_page_shows_its_status_and_settings_and_forget_closes_it() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.profiles.lock().unwrap() = vec![saved("home-uuid", "Home_Network", 1)];
    fake.settings
        .lock()
        .unwrap()
        .insert("home-uuid".into(), SampleNetwork::profile_settings());
    let mut model = model(&fake);
    model.link = SampleNetwork.snapshot(false).link;
    run(
        &mut model,
        NetworkEffect::OpenNetworkPage {
            ssid: "Home_Network".into(),
        },
    )
    .unwrap();
    let text = shown(&model).join("\n");
    for expected in [
        t("net-security-type-wpa3"),
        "866 Mbit/s".to_owned(),
        "192.168.1.24/24".to_owned(),
        t("net-priority"),
        t("net-hidden-network"),
        t("net-forget-network"),
    ] {
        assert!(text.contains(&expected), "{expected} in {text}");
    }
    let row = model.wifi_live("Home_Network").into_resource();
    for id in [
        "set-priority",
        "set-metered-policy",
        "set-mac-address",
        "set-hidden-network",
        "set-dns",
        "show-network-password",
        "forget-network",
        "show-network-code",
    ] {
        assert!(
            row.actions.iter().any(|action| action.id == id),
            "{id} on the network's row"
        );
    }
    run(
        &mut model,
        NetworkEffect::ShowNetworkPassword {
            profile: "home-uuid".into(),
        },
    )
    .unwrap();
    assert!(shown(&model).join("\n").contains("saved-secret"));
    run(
        &mut model,
        NetworkEffect::ShowNetworkCode {
            ssid: "Home_Network".into(),
        },
    )
    .unwrap();
    assert!(
        model
            .code
            .as_ref()
            .is_some_and(|code| code.secret.contains("P:saved-secret;")),
        "the QR code joins with the saved password"
    );
    run(
        &mut model,
        NetworkEffect::ForgetNetwork {
            profile: "home-uuid".into(),
        },
    )
    .unwrap();
    assert!(model.revealed.is_none(), "the shown password goes with it");
    // In range, the page stays to join it again.
    assert_eq!(model.page_key().as_deref(), Some("wifi:Home_Network"));
}

/// Every setting of a saved network round-trips through the effect layer:
/// invoked by id, carried out by the device, and read back.
#[test]
fn profile_settings_round_trip_through_the_effect_layer() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.profiles.lock().unwrap() = vec![saved("home-uuid", "Home_Network", 1)];
    fake.settings
        .lock()
        .unwrap()
        .insert("home-uuid".into(), SampleNetwork::profile_settings());
    let observed = model(&fake);
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    mount_network(&mut app, observed).unwrap();
    app.settle();
    for (id, arguments) in [
        ("set-priority", serde_json::json!({"priority": 7})),
        (
            "set-metered-policy",
            serde_json::json!({"policy": "Metered"}),
        ),
        (
            "set-mac-address",
            serde_json::json!({"policy": "Random each time"}),
        ),
        ("set-hidden-network", serde_json::json!({"on": true})),
        (
            "set-dns",
            serde_json::json!({"servers": "9.9.9.9, 2620:fe::fe"}),
        ),
        ("set-ipv6", serde_json::json!({"method": "Off"})),
        (
            "set-proxy",
            serde_json::json!({"mode": "Manual", "address": "proxy.lan:3128"}),
        ),
    ] {
        app.call_on("network.wifi:Home_Network", id, arguments)
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        settle_until(&mut app, |model| {
            model.in_flight == 0 && model.snapshot.is_none()
        });
    }
    let held = fake.settings.lock().unwrap()["home-uuid"].clone();
    let read = app
        .surface::<SettingsApp, _>(|settings| {
            crate::panes::network::area(settings)
                .unwrap()
                .model()
                .borrow()
                .profile_settings["home-uuid"]
                .clone()
        })
        .unwrap();
    assert_eq!(read, held, "the pane reads back what the device holds");
    assert_eq!(held.priority, Some(7));
    assert_eq!(held.metered, Some(os_actions::MeteredPolicy::Metered));
    assert_eq!(held.mac, Some(os_actions::MacPolicy::Random));
    assert_eq!(held.hidden, Some(true));
    assert_eq!(
        held.ipv4.as_ref().map(|ipv4| ipv4.dns.clone()),
        Some(vec!["9.9.9.9".to_owned(), "2620:fe::fe".to_owned()])
    );
    assert_eq!(
        held.ipv6.as_ref().map(|ipv6| ipv6.method),
        Some(os_actions::network_settings::Ipv6Method::Disabled)
    );
    assert_eq!(
        held.proxy.and_then(|proxy| proxy.manual),
        Some("proxy.lan:3128".to_owned())
    );
}

fn rendered_enabled(widget: &dyn ui::widget::Widget, id: &str) -> Option<bool> {
    if widget.test_id() == Some(id) {
        return Some(widget.is_enabled());
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| rendered_enabled(child, id))
}

/// A saved network's page and a VPN's page are live: every setting there
/// is a control bound to its row's action, and changing one reaches the
/// device.
#[test]
fn the_settings_on_a_network_and_a_vpn_page_change_the_device() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.profiles.lock().unwrap() = vec![saved("home-uuid", "Home_Network", 1)];
    fake.settings
        .lock()
        .unwrap()
        .insert("home-uuid".into(), SampleNetwork::profile_settings());
    let vpn = work_vpn();
    *fake.vpns.lock().unwrap() = vec![vpn.clone()];
    fake.vpn_settings.lock().unwrap().insert(
        vpn.uuid.clone(),
        SampleNetwork::vpn_settings(SAMPLE_VPN_HOME).unwrap(),
    );
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let observed = hosted_model(&mut app, Arc::clone(&fake) as Arc<dyn NetworkService>);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1600, 6000)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.call_on(
        "network.wifi:Home_Network",
        "open-network-page",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    for id in [
        "network.profile.autoconnect.home-uuid",
        "network.profile.priority.home-uuid",
        "network.profile.metered.home-uuid",
        "network.profile.mac.home-uuid",
        "network.profile.hidden.home-uuid",
        "network.profile.dns.home-uuid",
    ] {
        assert_eq!(
            rendered_enabled(app.harness().root_widget(), id),
            Some(true),
            "`{id}` is a live control"
        );
    }
    app.harness()
        .click_named("network.profile.hidden.home-uuid")
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(
        fake.settings.lock().unwrap()["home-uuid"].hidden,
        Some(true),
        "the switch reached the device"
    );

    let row = format!("vpn.connection:{}", vpn.uuid);
    app.call_on(&row, "show-vpn-profile", serde_json::json!({}))
        .unwrap();
    settle_until(&mut app, |model| model.vpn.settings.contains_key(&vpn.uuid));
    for id in [
        "autoconnect",
        "all-traffic",
        "mtu-choice",
        "name",
        "dns.add",
    ] {
        let id = format!("network.vpn.{id}.{}", vpn.uuid);
        assert_eq!(
            rendered_enabled(app.harness().root_widget(), &id),
            Some(true),
            "`{id}` is a live control"
        );
    }
    let before = fake.vpn_settings.lock().unwrap()[&vpn.uuid].autoconnect;
    app.harness()
        .click_named(&format!("network.vpn.autoconnect.{}", vpn.uuid))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(
        fake.vpn_settings.lock().unwrap()[&vpn.uuid].autoconnect,
        !before,
        "the switch reached the device"
    );
}

/// A VPN's MTU is chosen automatically or set by the person, its size
/// field shown only then; its name servers are a list, each added and
/// removed on its own, and what is no address is refused.
#[test]
fn a_vpns_mtu_and_name_servers_are_set_on_its_page() {
    let fake = Arc::new(FakeNetwork::default());
    let vpn = work_vpn();
    *fake.vpns.lock().unwrap() = vec![vpn.clone()];
    fake.vpn_settings.lock().unwrap().insert(
        vpn.uuid.clone(),
        SampleNetwork::vpn_settings(SAMPLE_VPN_HOME).unwrap(),
    );
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let observed = hosted_model(&mut app, Arc::clone(&fake) as Arc<dyn NetworkService>);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1600, 6000)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    let row = format!("vpn.connection:{}", vpn.uuid);
    app.call_on(&row, "show-vpn-profile", serde_json::json!({}))
        .unwrap();
    settle_until(&mut app, |model| model.vpn.settings.contains_key(&vpn.uuid));
    let held = |fake: &FakeNetwork| fake.vpn_settings.lock().unwrap()[&vpn.uuid].clone();
    let size = format!("network.vpn.mtu.{}", vpn.uuid);
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &size),
        None,
        "an automatic MTU has no size to set"
    );

    app.call_on(&row, "set-vpn-mtu", serde_json::json!({"mtu": 1420}))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(held(&fake).mtu, Some(1420));
    app.settle();
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &size),
        Some(true),
        "a custom MTU shows its size"
    );
    assert!(
        app.call_on(&row, "set-vpn-mtu", serde_json::json!({"mtu": 100}))
            .is_err()
    );
    app.call_on(&row, "set-vpn-mtu", serde_json::json!({}))
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(held(&fake).mtu, None, "back to automatic");

    for server in ["9.9.9.9", "2620:fe::fe"] {
        app.call_on(
            &row,
            "add-vpn-dns-server",
            serde_json::json!({"server": server}),
        )
        .unwrap();
        settle_until(&mut app, |model| model.in_flight == 0);
    }
    assert_eq!(held(&fake).dns, ["10.7.0.1", "9.9.9.9", "2620:fe::fe"]);
    assert!(
        app.call_on(
            &row,
            "add-vpn-dns-server",
            serde_json::json!({"server": "dns.example"}),
        )
        .is_err()
    );
    app.settle();
    app.harness()
        .click_named("settings.network.vpn.dns.remove.10.7.0.1")
        .unwrap();
    settle_until(&mut app, |model| model.in_flight == 0);
    assert_eq!(held(&fake).dns, ["9.9.9.9", "2620:fe::fe"]);
}

/// The known networks list every saved Wi-Fi network, the last joined first,
/// and forgets one from its row.
#[test]
fn known_networks_list_every_saved_network_by_last_use_and_forget_one() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.profiles.lock().unwrap() = vec![
        saved("hotel-uuid", "Hotel_Lisbon", 100),
        saved("home-uuid", "Home_Network", 300),
        saved("library-uuid", "Library", 200),
    ];
    let mut model = model(&fake);
    let count = t_args("net-known-count", &[("count", 3.into())]);
    assert!(shown(&model).contains(&count), "the row says how many");
    run(&mut model, NetworkEffect::ShowKnownNetworks).unwrap();
    let text = shown(&model);
    let at = |name: &str| {
        text.iter()
            .position(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("{name} in {text:#?}"))
    };
    assert!(at("Home_Network") < at("Library") && at("Library") < at("Hotel_Lisbon"));
    let row = model.wifi_live("Hotel_Lisbon").into_resource();
    assert!(
        row.actions
            .iter()
            .any(|action| action.id == "forget-network"),
        "a network out of range is still a row to forget"
    );
    run(
        &mut model,
        NetworkEffect::ForgetNetwork {
            profile: "hotel-uuid".into(),
        },
    )
    .unwrap();
    assert!(
        !shown(&model)
            .iter()
            .any(|line| line.starts_with("Hotel_Lisbon")),
        "the forgotten network's row is gone"
    );
}

/// A wired-only machine names its missing adapter without treating it as a
/// read failure or offering a radio switch.
#[test]
fn a_machine_without_wifi_says_so_instead_of_reporting_a_read_error() {
    let radios = Ok(os_actions::RadioState::default());
    assert_eq!(wifi_enabled_from(&radios), Ok(false));
    assert_eq!(
        wifi_enabled_from(&Err("nmcli not installed".into())),
        Err("nmcli not installed".to_string())
    );
    let fake = Arc::new(FakeNetwork::default());
    let mut model = model(&fake);
    let mut snapshot = NetworkSnapshot::of_networks(Vec::new());
    snapshot.wifi_enabled = Some(wifi_enabled_from(&radios));
    snapshot.radios = Some(radios);
    model.adopt_snapshot(snapshot).unwrap();
    let text = shown(&model);
    assert!(
        text.iter()
            .any(|line| line == "No Wi-Fi adapter on this device"),
        "{text:#?}"
    );
    assert!(
        !text
            .iter()
            .any(|line| line.contains("Can't read network status")),
        "{text:#?}"
    );
    assert!(!text.contains(&t("net-wifi-off")), "{text:#?}");
}

/// A status that could not be read is shown as that failure, never as a
/// state; a later successful read clears it.
#[test]
fn unreadable_network_state_is_shown_as_an_error_not_as_a_state() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.unreadable.lock().unwrap() = Some("org.freedesktop.NetworkManager is gone".into());
    let mut model = model(&fake);
    let text = shown(&model).join("\n");
    assert!(
        text.contains("org.freedesktop.NetworkManager is gone"),
        "{text}"
    );
    assert!(!text.contains(&t("net-not-connected")), "{text}");
    assert!(
        model.begin(NetworkEffect::SetWifi { on: true }).is_err(),
        "an unread radio is not switched"
    );
    assert_eq!(
        fake.connectivity_rechecks
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "opening Network reads the last verdict; it does not probe the internet"
    );
    *fake.unreadable.lock().unwrap() = None;
    *fake.networks.lock().unwrap() = networks();
    run(&mut model, NetworkEffect::RefreshNetworks).unwrap();
    assert_eq!(model.read_errors, NetworkReadErrors::default());
    assert_eq!(model.networks().len(), networks().len());
    assert_eq!(
        fake.connectivity_rechecks
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the explicit refresh asks for a fresh connectivity verdict"
    );
}

/// Opening Network shows the connected network from what `NetworkManager`
/// already knows; it never waits for a radio scan. Only Refresh scans,
/// before it reads, and a failed scan leaves the status and list readable.
#[test]
fn network_status_never_waits_for_a_scan_and_refresh_scans_first() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    let mut model = model(&fake);
    assert!(
        fake.scans.lock().unwrap().is_empty(),
        "opening Network must not scan"
    );
    let connected = t("net-connected");
    let text = shown(&model).join("\n");
    assert!(
        text.contains("Home_Network") && text.contains(&connected),
        "{text}"
    );
    assert_eq!(model.wifi_status_error(), None);

    run(&mut model, NetworkEffect::RefreshNetworks).unwrap();
    assert_eq!(
        *fake.scans.lock().unwrap(),
        vec![1],
        "Refresh scans once, before the snapshot it adopts"
    );
    assert_eq!(
        fake.snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );

    *fake.scan_refusal.lock().unwrap() = Some("Scanning not allowed".into());
    let refused = run(&mut model, NetworkEffect::RefreshNetworks)
        .expect_err("a failed scan is reported to whoever pressed Refresh");
    assert!(refused.contains("Scanning not allowed"), "{refused}");
    assert_eq!(model.read_errors, NetworkReadErrors::default());
    assert_eq!(model.wifi_status_error(), None);
    assert_eq!(model.networks().len(), networks().len());
    let text = shown(&model).join("\n");
    assert!(
        text.contains("Home_Network") && text.contains(&connected),
        "{text}"
    );
    assert!(!text.contains("Can't read network status"), "{text}");
}

/// The window's Network pane over `fake`, open on its first page.
fn hosted_network(
    fake: &Arc<FakeNetwork>,
) -> ui::framework::testing::TestApp<crate::SettingsShell> {
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let observed = hosted_model(&mut app, Arc::clone(fake) as Arc<dyn NetworkService>);
    mount_network(&mut app, observed).unwrap();
    app.harness().resize((1280, 900)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.settle();
    app
}

/// A secure network the person's devices share no password for asks for
/// it with the window's password prompt; Cancel closes it and joins
/// nothing, and what is typed the next time joins, closes the prompt and is
/// sealed for the person's devices.
#[test]
fn joining_a_secure_network_asks_with_the_password_prompt() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    let mut app = hosted_network(&fake);
    let connect = |app: &mut ui::framework::testing::TestApp<crate::SettingsShell>| {
        app.call_on(
            "network.wifi:Office_5G",
            "connect-network",
            serde_json::json!({}),
        )
        .unwrap();
        settle_until(app, |model| model.asking_to_join("Office_5G"));
    };
    connect(&mut app);
    assert!(
        fake.changes.lock().unwrap().is_empty(),
        "nothing joined yet"
    );
    app.harness().key("Escape");
    settle_until(&mut app, |model| model.asking.is_none());
    assert!(
        fake.changes.lock().unwrap().is_empty(),
        "a cancel joins nothing"
    );

    connect(&mut app);
    app.harness().type_text("hunter22");
    app.harness().key("Return");
    settle_until(&mut app, |model| {
        model.asking.is_none() && model.connecting.is_empty()
    });
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["connect Office_5G hunter22"]
    );
    assert!(
        fake.continuity
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, WifiContinuityCommand::Share { .. })),
        "the password is sealed for the person's devices"
    );
}

/// A known network joins at once with what this device saved for it.
#[test]
fn a_known_network_joins_without_asking() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.profiles.lock().unwrap() = vec![saved("library-uuid", "Library", 1)];
    let mut model = model(&fake);
    run(
        &mut model,
        NetworkEffect::ConnectNetwork {
            ssid: "Library".into(),
            password: None,
        },
    )
    .unwrap();
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["connect Library -"]
    );
    assert!(fake.continuity.lock().unwrap().is_empty());
    assert!(model.asking.is_none());
}

/// A password the person's devices share joins without a prompt; an open
/// network joins at once.
#[test]
fn a_shared_password_or_an_open_network_joins_directly() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.shared_password.lock().unwrap() = Some("from-phone".into());
    let mut model = model(&fake);
    run(
        &mut model,
        NetworkEffect::ConnectNetwork {
            ssid: "Office_5G".into(),
            password: None,
        },
    )
    .unwrap();
    run(
        &mut model,
        NetworkEffect::ConnectNetwork {
            ssid: "Cafe_Guest".into(),
            password: None,
        },
    )
    .unwrap();
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["connect Office_5G from-phone", "connect Cafe_Guest -"]
    );
    assert!(model.asking.is_none());
    assert!(
        model
            .begin(NetworkEffect::ConnectNetwork {
                ssid: "Elsewhere".into(),
                password: None,
            })
            .is_err(),
        "a network out of range is refused"
    );
}

/// A network that hides its name is asked for with the password prompt:
/// its name, then its password; joined, the prompt closes.
#[test]
fn a_hidden_network_is_joined_from_the_password_prompt() {
    let fake = Arc::new(FakeNetwork::default());
    let mut app = hosted_network(&fake);
    app.call("show-hidden-network", serde_json::json!({}))
        .unwrap();
    settle_until(&mut app, |model| model.asking.is_some());
    app.harness().type_text("Attic");
    app.harness().key("Tab");
    app.harness().type_text("rafters1");
    app.harness().key("Return");
    settle_until(&mut app, |model| {
        model.asking.is_none() && model.in_flight == 0
    });
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["join-hidden Attic rafters1"]
    );
}

/// A switch shows the requested state at once and puts it back when the
/// device refuses.
#[test]
fn a_refused_switch_is_put_back() {
    let fake = Arc::new(FakeNetwork::default());
    let mut model = model(&fake);
    run(&mut model, NetworkEffect::SetAirplaneMode { on: true }).unwrap();
    assert!(model.radios.airplane_mode);

    *fake.refuse.lock().unwrap() = Some("rfkill is blocked".into());
    let refused = run(&mut model, NetworkEffect::SetAirplaneMode { on: false });
    assert!(refused.unwrap_err().contains("rfkill is blocked"));
    assert!(model.radios.airplane_mode, "the switch is put back");
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["Airplane true", "Airplane false"]
    );
}

fn work_vpn() -> VpnInfo {
    VpnInfo {
        name: "Work".into(),
        uuid: "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e".into(),
        kind: "wireguard".into(),
        service: None,
        active: false,
    }
}

/// Each Wi-Fi network (in range or saved), other saved profile and VPN is a
/// row offering what can be done with it.
#[test]
fn networks_saved_profiles_and_vpns_are_rows() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.networks.lock().unwrap() = networks();
    *fake.vpns.lock().unwrap() = vec![work_vpn()];
    *fake.profiles.lock().unwrap() = vec![
        saved("1234-abcd", "Hotel_Lisbon", 1),
        SavedNetworkProfile {
            uuid: "wired-uuid".into(),
            name: "Wired connection 1".into(),
            kind: "802-3-ethernet".into(),
            autoconnect: true,
            last_used: None,
        },
    ];
    let profile = tempfile::tempdir().unwrap().keep();
    let cx = PaneCx::in_file(profile.join("settings.json"));
    let area = NetworkArea::new(&cx);
    let _ = area.model.set(Rc::new(RefCell::new(model(&fake))));
    let ids: Vec<String> = area.rows().iter().map(|row| row.id().to_owned()).collect();
    for id in [
        "network.wifi:Home_Network",
        "network.wifi:Office_5G",
        "network.wifi:Hotel_Lisbon",
        "settings.network-profile:wired-uuid",
        "vpn.connection:b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e",
    ] {
        assert!(ids.iter().any(|row| row == id), "{id} in {ids:?}");
    }
}

#[test]
fn supported_network_wake_is_named_and_routes_through_the_worker() {
    fn named_switch(widget: &dyn Widget, name: &str) -> bool {
        widget.accessibility().is_some_and(|node| {
            node.role == ui::a11y::Role::Switch && node.name.as_deref() == Some(name)
        }) || widget
            .children()
            .iter()
            .any(|child| named_switch(*child, name))
    }
    let fake = Arc::new(FakeNetwork::default());
    let uuid = "11111111-1111-4111-8111-111111111111";
    *fake.profiles.lock().unwrap() = vec![saved(uuid, "Home", 1)];
    fake.settings
        .lock()
        .unwrap()
        .insert(uuid.into(), SampleNetwork::profile_settings());
    let mut model = model(&fake);
    model.wake_settings.insert(
        uuid.into(),
        Ok(NetworkWakeStatus {
            profile: uuid.into(),
            interface: Some("wlan0".into()),
            configured: false,
            supported: Some(true),
            applied: Some(false),
            problem: None,
        }),
    );
    let area = NetworkArea::new(&model.cx);
    let _ = area.model.set(Rc::new(RefCell::new(model)));
    let row = area
        .rows()
        .into_iter()
        .find(|row| row.id() == "network.wifi:Home")
        .unwrap()
        .into_resource();
    assert!(
        row.actions
            .iter()
            .any(|action| action.label == "Wake over network")
    );
    assert!(
        row.presentation
            .unwrap()
            .preview
            .unwrap()
            .contains("must be tested separately")
    );
    let mut model = area.model().borrow_mut();
    run(
        &mut model,
        NetworkEffect::OpenNetworkPage {
            ssid: "Home".into(),
        },
    )
    .unwrap();
    assert!(named_switch(
        model.build_composition().as_ref(),
        "Wake over Home"
    ));
    run(
        &mut model,
        NetworkEffect::SetNetworkWake {
            profile: uuid.into(),
            on: true,
        },
    )
    .unwrap();
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        [format!("wake {uuid} true")]
    );
    assert!(
        model
            .begin(NetworkEffect::SetNetworkWake {
                profile: "missing".into(),
                on: true
            })
            .is_err()
    );
    *fake.refuse.lock().unwrap() = Some("not authorized".into());
    assert!(
        run(
            &mut model,
            NetworkEffect::SetNetworkWake {
                profile: uuid.into(),
                on: false
            }
        )
        .unwrap_err()
        .contains("not authorized")
    );
}

/// The hotspot starts from its form, and a password shorter than eight
/// characters is refused before anything reaches the device.
#[test]
fn the_hotspot_starts_from_its_form() {
    let fake = Arc::new(FakeNetwork::default());
    let mut model = model(&fake);
    *model.hotspot_password.lock().unwrap() = "short".into();
    assert!(
        model
            .begin(NetworkEffect::StartHotspot {
                name: None,
                password: None,
                band: None,
            })
            .is_err()
    );
    *model.hotspot_password.lock().unwrap() = "long enough secret".into();
    model.hotspot_name.set("Kitchen".into());
    run(
        &mut model,
        NetworkEffect::StartHotspot {
            name: None,
            password: None,
            band: None,
        },
    )
    .unwrap();
    assert_eq!(fake.changes.lock().unwrap().as_slice(), ["hotspot Kitchen"]);
    assert!(model.radios.hotspot_active);
}

const WIREGUARD_FILE: &str = "\
[Interface]
PrivateKey = aGVsbG8=
Address = 10.7.0.2/32

[Peer]
PublicKey = kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0=
Endpoint = home.example:51820
AllowedIPs = 0.0.0.0/0, ::/0
";

const OPENVPN_FILE: &str = "\
client
dev tun
remote vpn.example.com 1194
<ca>
-----BEGIN CERTIFICATE-----
MIIB
-----END CERTIFICATE-----
</ca>
";

/// Import a file through the pane, as a picked file is, and wait for it.
fn import(model: &mut NetworkModel, path: std::path::PathBuf, kind: Option<&'static str>) {
    model.import_vpn_path(path, kind);
    settle(model);
}

/// `WireGuard` and `OpenVPN` files are checked before anything is set up:
/// a good file becomes a VPN, whose page opens; a file missing what its
/// tunnel needs, or of the other kind, is refused with why.
#[test]
fn vpn_files_are_checked_and_imported() {
    let fake = Arc::new(FakeNetwork::default());
    let mut model = model(&fake);
    let files = tempfile::tempdir().unwrap();
    let write = |name: &str, text: &str| {
        let path = files.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    import(&mut model, write("home.conf", WIREGUARD_FILE), None);
    import(
        &mut model,
        write("work.ovpn", OPENVPN_FILE),
        Some("openvpn"),
    );
    assert_eq!(
        fake.created.lock().unwrap().as_slice(),
        ["wireguard home.conf", "openvpn work.ovpn"]
    );
    assert_eq!(model.notice.as_deref(), Some(t("net-vpn-created").as_str()));
    assert_eq!(
        model.page_key().as_deref(),
        Some("vpn:c0ffee00-0000-4000-8000-000000000001"),
        "the new VPN's page opens"
    );

    import(
        &mut model,
        write("broken.conf", "[Peer]\nEndpoint = x:1\n"),
        None,
    );
    assert!(
        model
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("Interface.PrivateKey")),
        "{:?}",
        model.notice
    );
    import(
        &mut model,
        write("nocheck.ovpn", "client\nremote a 1\n"),
        None,
    );
    assert!(
        model
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("cannot be checked")),
        "{:?}",
        model.notice
    );
    import(
        &mut model,
        write("home2.conf", WIREGUARD_FILE),
        Some("openvpn"),
    );
    assert_eq!(
        model.notice.as_deref(),
        Some(
            t_args(
                "net-vpn-create-err",
                &[("error", t("net-vpn-file-wrong-kind").into())]
            )
            .as_str()
        )
    );
    assert_eq!(
        fake.created.lock().unwrap().len(),
        2,
        "nothing refused was set up"
    );
}

/// A pasted `WireGuard` configuration and a tunnel made from generated keys
/// are set up from their text; the form's secrets are zeroed.
#[test]
fn a_wireguard_tunnel_is_made_from_generated_keys_or_a_pasted_file() {
    let fake = Arc::new(FakeNetwork::default());
    let mut model = model(&fake);
    run_vpn(&mut model, VpnEffect::ShowAddWireguard).unwrap();
    run_vpn(&mut model, VpnEffect::GenerateWireguardKeys).unwrap();
    let public = model.vpn_public_key().expect("the public key is shown");
    assert!(shown(&model).contains(&public), "to give the server");
    model.fill_wireguard_form(
        "Cabin",
        "10.9.0.2/32",
        WIREGUARD_PEER,
        "cabin.example:51820",
    );
    run_vpn(
        &mut model,
        VpnEffect::CreateWireguard {
            name: None,
            address: None,
            server_public_key: None,
            endpoint: None,
            allowed_ips: None,
            dns: None,
            keepalive: None,
            private_key: None,
        },
    )
    .unwrap();
    assert!(
        model.vpn_private_key_is_empty(),
        "the private key is zeroed"
    );
    assert!(
        run_vpn(
            &mut model,
            VpnEffect::ImportWireguardConfig {
                name: Some("Bad".into()),
                config: Some("[Interface]\n".into()),
            },
        )
        .unwrap_err()
        .contains("missing")
    );
    run_vpn(
        &mut model,
        VpnEffect::ImportWireguardConfig {
            name: Some("Pasted".into()),
            config: Some(WIREGUARD_FILE.into()),
        },
    )
    .unwrap();
    assert_eq!(
        fake.created.lock().unwrap().as_slice(),
        ["wireguard Cabin", "wireguard Pasted"]
    );
}

const WIREGUARD_PEER: &str = "kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0E=";

/// A VPN is switched from its row, edited on its page — each setting its
/// action — and deleted there, which closes the page.
#[test]
fn a_vpn_is_switched_edited_and_deleted() {
    let fake = Arc::new(FakeNetwork::default());
    let vpn = work_vpn();
    *fake.vpns.lock().unwrap() = vec![vpn.clone()];
    fake.vpn_settings.lock().unwrap().insert(
        vpn.uuid.clone(),
        SampleNetwork::vpn_settings(SAMPLE_VPN_HOME).unwrap(),
    );
    let mut model = model(&fake);
    run_vpn(&mut model, VpnEffect::ShowVpns).unwrap();
    assert!(shown(&model).join("\n").contains("Work"));
    run_vpn(
        &mut model,
        VpnEffect::SetVpnConnected {
            connection: "Work".into(),
            on: true,
        },
    )
    .unwrap();
    assert!(model.vpns[0].active);
    run_vpn(
        &mut model,
        VpnEffect::ShowVpnProfile {
            connection: vpn.uuid.clone(),
        },
    )
    .unwrap();
    settle(&mut model);
    let text = shown(&model).join("\n");
    assert!(text.contains("home.example.net:51820"), "{text}");
    let row = model.vpn_rows().remove(0).into_resource();
    for id in [
        "set-vpn-name",
        "add-vpn-dns-server",
        "remove-vpn-dns-server",
        "set-vpn-keepalive",
        "set-vpn-server",
        "export-vpn",
        "delete-connection",
        "disconnect",
    ] {
        assert!(row.actions.iter().any(|action| action.id == id), "{id}");
    }
    run_vpn(
        &mut model,
        VpnEffect::SetVpnKeepalive {
            connection: vpn.uuid.clone(),
            seconds: 0,
        },
    )
    .unwrap();
    settle(&mut model);
    assert_eq!(
        model.vpn.settings[&vpn.uuid].keepalive, None,
        "read back as the device holds it"
    );
    run_vpn(
        &mut model,
        VpnEffect::DeleteConnection {
            connection: vpn.uuid,
        },
    )
    .unwrap();
    assert!(model.vpns.is_empty());
    assert_eq!(model.page_key().as_deref(), Some(super::vpn::PAGE_VPN));
    let changes = fake.changes.lock().unwrap().clone();
    assert_eq!(changes[0], "vpn Work true");
    assert_eq!(changes.last().map(String::as_str), Some("vpn-delete Work"));
}

/// A plugin's tunnel is called by the protocol its service type names,
/// before its page is opened and without its plugin installed here.
#[test]
fn a_plugin_vpn_is_called_by_its_protocol() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.vpns.lock().unwrap() = vec![VpnInfo {
        kind: "vpn".into(),
        service: Some("org.freedesktop.NetworkManager.openvpn".into()),
        ..work_vpn()
    }];
    let mut model = model(&fake);
    run_vpn(&mut model, VpnEffect::ShowVpns).unwrap();
    let text = shown(&model).join("\n");
    assert!(text.contains("OpenVPN"), "{text}");
    assert!(model.vpn.settings.is_empty(), "no profile was read for it");
}

/// What asked the VPN app to import, connect, disconnect or delete finds
/// the same action in Settings.
#[test]
fn the_vpn_apps_actions_resolve_in_settings() {
    for (id, arguments, expected) in [
        (
            "import-wireguard",
            serde_json::json!({}),
            VpnEffect::ImportWireguard,
        ),
        (
            "import-openvpn",
            serde_json::json!({}),
            VpnEffect::ImportOpenvpn,
        ),
        (
            "connect",
            serde_json::json!({"connection": "Work"}),
            VpnEffect::Connect {
                connection: "Work".into(),
            },
        ),
        (
            "disconnect",
            serde_json::json!({"connection": "Work"}),
            VpnEffect::Disconnect {
                connection: "Work".into(),
            },
        ),
        (
            "delete-connection",
            serde_json::json!({"connection": "Work"}),
            VpnEffect::DeleteConnection {
                connection: "Work".into(),
            },
        ),
    ] {
        let decoded = SettingsEffect::decode(id, arguments.as_object().unwrap())
            .unwrap_or_else(|error| panic!("{id} resolves: {error}"));
        assert_eq!(decoded, SettingsEffect::from(expected), "{id}");
    }
    let fake = Arc::new(FakeNetwork::default());
    *fake.vpns.lock().unwrap() = vec![work_vpn()];
    let mut model = model(&fake);
    run_vpn(
        &mut model,
        VpnEffect::Connect {
            connection: "Work".into(),
        },
    )
    .unwrap();
    run_vpn(
        &mut model,
        VpnEffect::Disconnect {
            connection: "Work".into(),
        },
    )
    .unwrap();
    assert_eq!(
        fake.changes.lock().unwrap().as_slice(),
        ["vpn Work true", "vpn Work false"]
    );
}

/// The availability Settings publishes for `id`, as agents, the palette
/// and assistive technology read it.
fn published(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    id: &str,
) -> ui::tilo_actions::Availability {
    app.actions()
        .expect("the actions are published")
        .into_iter()
        .find(|action| action.address.action == id)
        .unwrap_or_else(|| panic!("`{id}` is published"))
        .contract
        .availability
}

fn unavailable(code: &str, reason: &str) -> ui::tilo_actions::Availability {
    ui::tilo_actions::Availability::Unavailable {
        code: code.into(),
        reason: reason.into(),
    }
}

/// A wired-only machine publishes Wi-Fi, the hotspot, mobile broadband and
/// airplane mode unavailable with why, and refuses them; a Wi-Fi dongle
/// plugged in is offered at once, before `NetworkManager` is read again.
#[test]
fn without_a_wifi_radio_wifi_is_unavailable_until_one_is_plugged_in() {
    use crate::radios::Present;
    use ui::tilo_actions::Availability;

    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    let fake = Arc::new(FakeNetwork::default());
    let mut wired_only = model(&fake);
    let mut snapshot = NetworkSnapshot::of_networks(Vec::new());
    snapshot.radios = Some(Ok(os_actions::RadioState::default()));
    snapshot.wifi_enabled = Some(Ok(false));
    wired_only.adopt_snapshot(snapshot).unwrap();
    mount_network(&mut app, wired_only).unwrap();
    let radios = app.surface_mut::<SettingsApp>().host().radios().clone();

    radios.set(Present::NONE);
    app.settle();
    let no_wifi = unavailable("no_adapter", "No Wi-Fi adapter on this device");
    for id in ["set-wifi", "set-hotspot", "start-hotspot"] {
        assert_eq!(published(&mut app, id), no_wifi, "`{id}`");
    }
    assert_eq!(
        published(&mut app, "set-mobile-broadband"),
        unavailable("no_modem", "No mobile broadband modem on this device")
    );
    assert_eq!(
        published(&mut app, "set-airplane-mode"),
        unavailable(
            "no_radio",
            "No Wi-Fi or mobile broadband radio on this device"
        )
    );
    assert_eq!(
        published(&mut app, "refresh-networks"),
        Availability::Available,
        "a wired connection is still read"
    );
    app.effect(&SettingsEffect::from(NetworkEffect::SetWifi { on: true }))
        .expect_err("an unavailable radio is refused, not tried");
    assert!(fake.changes.lock().unwrap().is_empty(), "nothing was tried");

    radios.set(Present {
        wifi: true,
        ..Present::NONE
    });
    app.settle();
    for id in [
        "set-wifi",
        "set-hotspot",
        "start-hotspot",
        "set-airplane-mode",
    ] {
        assert_eq!(published(&mut app, id), Availability::Available, "`{id}`");
    }
    assert_eq!(
        published(&mut app, "set-mobile-broadband"),
        unavailable("no_modem", "No mobile broadband modem on this device")
    );
}

/// Either the kernel or `NetworkManager` seeing a radio is enough. Neither
/// having said offers everything, and the area's model is never read for
/// it.
#[test]
fn a_radio_either_source_sees_is_offered() {
    use crate::radios::Present;

    let profile = tempfile::tempdir().unwrap();
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    let area = NetworkArea::new(&cx);
    assert!(
        area.statuses().is_empty(),
        "radios not read offer everything"
    );
    assert!(!area.is_open(), "the networks were not read");

    cx.radios().set(Present::ALL);
    assert!(
        area.statuses().is_empty(),
        "with every radio nothing changes"
    );

    let reported = os_actions::RadioState {
        wwan_available: true,
        ..os_actions::RadioState::default()
    };
    cx.radios().set(Present::NONE);
    assert_eq!(
        NetworkRadios::of(&cx, Some(&reported)),
        NetworkRadios {
            wifi: Some(false),
            wwan: Some(true),
        }
    );
    assert_eq!(
        NetworkRadios::of(&PaneCx::in_file(profile.path().join("other.json")), None),
        NetworkRadios::default(),
        "neither has said"
    );
}

#[test]
fn ipv6_settings_apply_from_their_form() {
    let fake = Arc::new(FakeNetwork::default());
    *fake.profiles.lock().unwrap() = vec![saved("home-uuid", "Home_Network", 1)];
    fake.settings
        .lock()
        .unwrap()
        .insert("home-uuid".into(), SampleNetwork::profile_settings());
    let mut model = model(&fake);
    let form = &model.ipv6_forms["home-uuid"];
    form.method.set(t("net-ipv6-manual"));
    form.addresses.set("2001:db8::10/64".into());
    run(
        &mut model,
        NetworkEffect::SetIpv6 {
            profile: "home-uuid".into(),
            method: None,
            addresses: None,
            gateway: None,
        },
    )
    .unwrap();
    settle(&mut model);
    assert_eq!(
        model.profile_settings["home-uuid"].ipv6,
        Some(Ipv6Configuration {
            method: os_actions::network_settings::Ipv6Method::Manual,
            addresses: vec!["2001:db8::10/64".into()],
            gateway: None,
            dns: Vec::new(),
        })
    );
}

/// The VPN tool opens Settings: its list, a profile's page, or its import.
#[test]
fn the_vpn_tool_opens_settings_where_it_asks() {
    use os_actions::{ToolIntent, VpnRoute};
    let effect = |route, profile: Option<&str>| {
        crate::resource::tool_effect(&ToolIntent::Vpn {
            route,
            profile: profile.map(str::to_owned),
        })
        .unwrap()
    };
    assert_eq!(
        effect(VpnRoute::Overview, None),
        SettingsEffect::from(VpnEffect::ShowVpns)
    );
    assert_eq!(
        effect(VpnRoute::Profile, Some("Work")),
        SettingsEffect::from(VpnEffect::ShowVpnProfile {
            connection: "Work".into()
        })
    );
    assert_eq!(
        effect(VpnRoute::Import, None),
        SettingsEffect::from(VpnEffect::ImportVpnFile)
    );
    assert_eq!(
        ToolIntent::Vpn {
            route: VpnRoute::Overview,
            profile: None
        }
        .package_id(),
        Some("so.tilo.settings")
    );

    // Asked for before the VPNs were read, the page opens once they are.
    let fake = Arc::new(FakeNetwork::default());
    *fake.vpns.lock().unwrap() = vec![work_vpn()];
    let profile = tempfile::tempdir().unwrap().keep();
    let mut model = NetworkModel::with_service(
        PaneCx::in_file(profile.join("settings.json")),
        Arc::clone(&fake) as Arc<dyn NetworkService>,
    );
    run_vpn(
        &mut model,
        VpnEffect::ShowVpnProfile {
            connection: "Work".into(),
        },
    )
    .unwrap();
    assert_eq!(model.page_key().as_deref(), Some(super::vpn::PAGE_VPN));
    let snapshot = fake.snapshot(true);
    let _ = model.adopt_snapshot(snapshot);
    model.poll_background();
    assert_eq!(
        model.page_key(),
        Some(super::vpn::profile_page_key(&work_vpn().uuid))
    );
}

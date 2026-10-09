use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::quick_settings::{root::QuickSettingsRoot, wifi::QuickSettingsWifiPane};
use crate::shell::search::{SearchContext, SearchProvider, providers::actions::ActionsProvider};
use crate::shell::state::FreshId as _;

fn read(chrome: &Chrome) -> serde_json::Value {
    let mut document = automerge::AutoCommit::new();
    let row = {
        let state = chrome.borrow();
        ActionsProvider::new()
            .query(SearchContext {
                query_scope: None,
                action_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[crate::shell::search::ResourceKind::Action],
                chrome: &state,
                doc: &document,
                doc_root: &automerge::ROOT,
            })
            .into_iter()
            .find(|row| row.id == "quick-settings.network.inspect")
            .unwrap()
    };
    let receipt = row.on_invoke.unwrap()(
        &mut ui::widget::Edits::lend(&mut document),
        &automerge::ROOT,
        chrome,
        &crate::shell::search::action_api::ActionArgs::none(),
    )
    .unwrap();
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
        panic!("cached read must complete immediately");
    };
    output.value.unwrap()
}

fn assert_painted_labels(chrome: &Chrome, value: &serde_json::Value) {
    let state = chrome.borrow();
    let panel = QuickSettingsWifiPane::default().accessibility_snapshot(&state.quick_settings);
    assert_eq!(value["wifi_panel"]["label"].as_str(), panel.name.as_deref());
    assert_eq!(
        value["wifi_panel"]["description"].as_str(),
        panel.description.as_deref()
    );
    let root = QuickSettingsRoot::default().accessibility_snapshot(&state.quick_settings);
    let label = value["network_control"]["label"].as_str().unwrap();
    let tile = root
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some(label))
        .expect("read label must identify the actual painted root tile");
    assert_eq!(
        value["network_control"]["subline"].as_str(),
        tile.description.as_deref()
    );
}

#[test]
fn quick_settings_network_inspection_reads_ordinary_worker_adoption_and_failed_cache() {
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "Network inspector");
    workspace.set_owner("shell-fixture");
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    let handle: WorkspaceTreeHandle = Arc::new(parking_lot::RwLock::new(tree));
    let chrome = ChromeState::new_with_fixture_handle(
        handle,
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    {
        let mut state = chrome.borrow_mut();
        state.quick_settings.radios.wifi_available = true;
        state.quick_settings.wifi = true;
        state.quick_settings.wifi_scanning = true;
    }
    let (release, released) = std::sync::mpsc::channel();
    host.qs_refresh.kick_wifi_observations_with(
        || {
            Ok(vec![os_actions::WifiNetworkInfo {
                ssid: "Fixture network".into(),
                signal: 3,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: true,
            }])
        },
        move || {
            tilo_test_env::recv("explicit scan release", &released);
            Err("fixture discovery refused".into())
        },
    );
    tilo_test_env::recv("cached observation must wake its actual owner", &wake_rx);
    assert!(host.update(&automerge::AutoCommit::new(), &automerge::ROOT));
    let adopted = read(&chrome);
    assert_eq!(adopted["wifi_panel"]["open"], false);
    assert_ne!(adopted["wifi_panel"]["label"], "qs-wifi");
    assert_eq!(adopted["network_control"]["subline"], "Fixture network");
    assert_eq!(adopted["wifi_panel"]["listed_network_count"], 1);
    assert_eq!(adopted["networks"][0]["connected"], true);
    assert_painted_labels(&chrome, &adopted);
    let revision = chrome.borrow().revision;
    assert_eq!(read(&chrome), adopted);
    assert_eq!(
        chrome.borrow().revision,
        revision,
        "inspection must not refresh or mutate the cache"
    );
    assert!(
        host.qs_refresh.is_in_flight(),
        "cached APs never complete fresh discovery"
    );
    release.send(()).unwrap();
    let pending = read(&chrome);
    assert_eq!(pending["scanning"], true);
    assert_eq!(pending["wifi_panel"]["listed_network_count"], 1);
    assert_eq!(pending["networks"][0]["listed"], true);
    assert_painted_labels(&chrome, &pending);
    tilo_test_env::recv("failed scan must wake its actual owner", &wake_rx);
    assert!(host.update(&automerge::AutoCommit::new(), &automerge::ROOT));
    drop(host);
    let failed = read(&chrome);
    assert_eq!(failed["scanning"], false);
    assert_eq!(failed["scan_error"], "fixture discovery refused");
    assert_eq!(
        failed["networks"][0]["ssid"], "Fixture network",
        "failed scans retain previous cached rows"
    );
    assert_eq!(failed["wifi_panel"]["listed_network_count"], 1);
    assert_painted_labels(&chrome, &failed);
}

fn connection_fixture() -> (Chrome, ShellOverlayHost, crate::shell::status::StatusBridge) {
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "Network status fixture");
    workspace.set_owner("shell-fixture");
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    let handle: WorkspaceTreeHandle = Arc::new(parking_lot::RwLock::new(tree));
    let chrome = ChromeState::new_with_fixture_handle(
        handle,
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    );
    {
        let mut state = chrome.borrow_mut();
        state.quick_settings.radios.wifi_available = true;
        state.quick_settings.wifi = true;
        state.quick_settings.wifi_scan_error = Some("fixture scan timed out".into());
        state.quick_settings.wifi_networks = vec![crate::shell::quick_settings::WifiNetwork {
            ssid: "Old observed AP".into(),
            signal: 2,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: false,
        }];
    }
    let bridge = crate::shell::status::StatusBridge::new();
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_status_bridge(bridge.clone());
    host.qs_refresh.disable_os_reads();
    (chrome, host, bridge)
}

#[test]
fn quick_settings_connection_observes_status_bridge_without_replacing_failed_scan_cache() {
    let (chrome, mut host, bridge) = connection_fixture();
    let cached = read(&chrome);
    for (link, expected) in [
        (
            os_actions::LinkStatus {
                kind: os_actions::LinkKind::Wifi,
                name: Some("Observed active connection".into()),
                signal: 3,
            },
            "Observed active connection".to_owned(),
        ),
        (
            os_actions::LinkStatus {
                kind: os_actions::LinkKind::Wifi,
                name: None,
                signal: 3,
            },
            crate::shell::i18n::t("qs-connected"),
        ),
        (
            os_actions::LinkStatus::offline(),
            crate::shell::i18n::t("qs-not-connected"),
        ),
        (
            os_actions::LinkStatus {
                kind: os_actions::LinkKind::Wired,
                name: Some("Wired profile".into()),
                signal: 0,
            },
            crate::shell::i18n::t("qs-not-connected"),
        ),
    ] {
        let observed_kind = match link.kind {
            os_actions::LinkKind::Wifi => "wifi",
            os_actions::LinkKind::Offline => "offline",
            os_actions::LinkKind::Wired => "wired",
        };
        bridge.set(crate::shell::status::SystemStatus {
            link: Some(link),
            ..Default::default()
        });
        host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
        let observed = read(&chrome);
        assert_eq!(observed["network_control"]["subline"], expected);
        assert_eq!(observed["observed_link"]["kind"], observed_kind);
        assert_eq!(observed["observed_link"]["source"], "status-bridge");
        assert_eq!(
            observed["networks"], cached["networks"],
            "link observations must not invent APs"
        );
        assert_eq!(observed["scan_error"], cached["scan_error"]);
        assert_eq!(observed["scanning"], false);
        assert_eq!(observed["wifi_panel"], cached["wifi_panel"]);
        assert_painted_labels(&chrome, &observed);
    }
    chrome.borrow_mut().quick_settings.wifi_networks[0].connected = true;
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        read(&chrome)["network_control"]["subline"],
        "Old observed AP",
        "primary wired status cannot assert a separate Wi-Fi link is disconnected"
    );
    bridge.set(crate::shell::status::SystemStatus::default());
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        read(&chrome)["network_control"]["subline"],
        "Old observed AP",
        "unobserved status preserves the existing AP fallback"
    );
    assert!(
        !host.qs_refresh.is_in_flight(),
        "status adoption initiates no scan"
    );
}

#[test]
fn observed_offline_overrides_old_connected_ap_without_changing_radio_or_scan_state() {
    let (chrome, mut host, bridge) = connection_fixture();
    chrome.borrow_mut().quick_settings.wifi_networks[0].connected = true;
    bridge.set(crate::shell::status::SystemStatus {
        link: Some(os_actions::LinkStatus::offline()),
        ..Default::default()
    });
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    let observed = read(&chrome);
    assert_eq!(
        observed["network_control"]["subline"],
        crate::shell::i18n::t("qs-not-connected")
    );
    assert_eq!(
        observed["networks"][0]["connected"], true,
        "scan cache is preserved"
    );
    assert_eq!(observed["wifi_enabled"], true);
    assert_eq!(observed["scan_error"], "fixture scan timed out");
    assert_painted_labels(&chrome, &observed);
    assert!(!host.qs_refresh.is_in_flight());
}

#[test]
fn network_inspection_bounds_observed_connection_names() {
    let (chrome, mut host, bridge) = connection_fixture();
    bridge.set(crate::shell::status::SystemStatus {
        link: Some(os_actions::LinkStatus {
            kind: os_actions::LinkKind::Wifi,
            name: Some("x".repeat(1024)),
            signal: 2,
        }),
        ..Default::default()
    });
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    assert!(crate::shell::quick_settings::inspection::network(&chrome.borrow()).is_ok());
    bridge.set(crate::shell::status::SystemStatus {
        link: Some(os_actions::LinkStatus {
            kind: os_actions::LinkKind::Wifi,
            name: Some("x".repeat(1025)),
            signal: 2,
        }),
        ..Default::default()
    });
    host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        crate::shell::quick_settings::inspection::network(&chrome.borrow()),
        Err("quick-settings-network-inspect-limit")
    );
}

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

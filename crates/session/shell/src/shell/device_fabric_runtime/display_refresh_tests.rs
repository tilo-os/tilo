//! Exercise policy changes through the production pump and settings watcher.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::{DeviceFabricRuntime, LocalFabricCapabilities};
use crate::compositor::selection::{
    SelectionStore, Target, clipboard_history, merge_shared_clipboard_history, pin_history_item,
    set_mirror_text,
};
use crate::notifications::NotificationBridge;
use crate::shell::device_fabric::{ClipboardEntry, ClipboardPayload, FabricStamp};
use crate::shell::personal_continuity::DeviceId;

#[test]
#[serial_test::serial]
fn a_settings_revocation_reaches_the_pump_and_removes_only_imported_clipboard_history() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    write_policy(true);
    let (network, _commands) = crate::shell::network::shell_network_for_test(
        crate::shell::state::WorkspaceTree::default().shared(),
        1,
    );
    let (wake, awakened) = mpsc::channel();
    network.set_wake(Arc::new(move || {
        let _ = wake.send(());
    }));
    let selection = Arc::new(Mutex::new(SelectionStore::default()));
    let local = LocalFabricCapabilities {
        display: None,
        pointer: false,
        keyboard: false,
        audio_output: false,
    };
    let mut runtime = DeviceFabricRuntime::new(&network, NotificationBridge::new());
    runtime.pump(&network, &selection, local);
    assert!(set_mirror_text(
        &selection,
        Target::Clipboard,
        "local pinned copy"
    ));
    let local_id = clipboard_history(&selection).first().unwrap().id;
    assert!(pin_history_item(&selection, local_id, true));
    let remote = DeviceId::new("dirty-refresh-remote").unwrap();
    let imported = ClipboardEntry::shareable(
        ClipboardPayload::Text("private imported copy".into()),
        remote.clone(),
        42,
        FabricStamp {
            counter: 7,
            device: remote,
        },
    )
    .unwrap();
    merge_shared_clipboard_history(&selection, &[imported]);
    assert_eq!(clipboard_history(&selection).len(), 2);

    write_policy(false);
    awakened.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime.pump(&network, &selection, local);
    let history = clipboard_history(&selection);
    assert_eq!(history.len(), 1, "actual pump must revoke imported history");
    assert_eq!(history.first().unwrap().id, local_id);
    assert!(
        history.first().unwrap().pinned,
        "local device-private history survives"
    );
}

fn write_policy(clipboard: bool) {
    let section = serde_json::json!({"enabled": true, "clipboard": clipboard});
    settings_store::update_section("continuity", section.as_object().unwrap().clone()).unwrap();
}

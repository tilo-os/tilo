//! Bluetooth pane — controller toggle + paired-device list.
//!
//! Plumbs `os_actions::bluetooth::{get_bluetooth, set_bluetooth,
//! list_bt_devices, bt_connect, bt_disconnect}`. Each paired device's
//! Connect / Disconnect button really acts: clicking it runs
//! `bluetoothctl connect|disconnect <mac>` through [`BtControl`] against the
//! device's MAC, then re-reads the paired list so the row's status and its
//! button label follow the new connection state. On a host without
//! `bluetoothctl` the shell-out returns a clean error (logged, no crash) and
//! the list simply stays as it was — the pane never fabricates a connection.
//
// CRDT keys:
//   `bluetooth.enabled`           — bool, controller toggle
//   `bluetooth.connect.<name>`    — bool, per-device connect/disconnect

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::{BtDeviceInfo, Dispatcher};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;
use crate::widgets::{El, caption, card_with_action, rows};

pub const BT_ENABLED_KEY: &str = "bluetooth.enabled";

fn connect_key(name: &str) -> String {
    format!("bluetooth.connect.{name}")
}

type BtLister = Arc<dyn Fn() -> Result<Vec<BtDeviceInfo>, String> + Send + Sync>;

/// Side-effect interface for connecting / disconnecting a paired device by
/// MAC. Production shells out to `bluetoothctl`; tests inject a recorder so
/// the connect/disconnect path is asserted without touching the radio.
pub trait BtControl: Send + Sync {
    fn connect(&self, mac: &str) -> Result<(), String>;
    fn disconnect(&self, mac: &str) -> Result<(), String>;
}

/// Production impl — `bluetoothctl connect|disconnect <mac>`.
pub struct RealBt;

impl BtControl for RealBt {
    fn connect(&self, mac: &str) -> Result<(), String> {
        os_actions::bt_connect(mac)
    }
    fn disconnect(&self, mac: &str) -> Result<(), String> {
        os_actions::bt_disconnect(mac)
    }
}

pub struct BluetoothPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    bt: Arc<dyn BtControl>,
    lister: BtLister,
    devices: Vec<BtDeviceInfo>,
    initial_bt_enabled: bool,
    last_bt_enabled: bool,
    needs_seed: bool,
    /// Set when a connect/disconnect re-read the device list, so the host
    /// rebuilds the pane and the row's status + button label update.
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for BluetoothPane {
    fn default() -> Self {
        Self::new()
    }
}

impl BluetoothPane {
    pub fn new() -> Self {
        let lister: BtLister = Arc::new(os_actions::list_bt_devices);
        let devices = lister().unwrap_or_default();
        let initial_bt = os_actions::get_bluetooth().unwrap_or(false);
        Self {
            dispatcher: dispatcher_or_real(None),
            bt: Arc::new(RealBt),
            lister,
            devices,
            initial_bt_enabled: initial_bt,
            last_bt_enabled: initial_bt,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_bt_enabled: bool,
        devices: Vec<BtDeviceInfo>,
    ) -> Self {
        let snapshot = devices.clone();
        let lister: BtLister = Arc::new(move || Ok(snapshot.clone()));
        Self {
            dispatcher,
            bt: Arc::new(RealBt),
            lister,
            devices,
            initial_bt_enabled,
            last_bt_enabled: initial_bt_enabled,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` builder: inject the connect/disconnect control and,
    /// optionally, a lister that reflects the post-toggle device state, so the
    /// real connect path is exercised without touching `bluetoothctl`.
    pub fn with_bt_control(mut self, bt: Arc<dyn BtControl>, lister: BtLister) -> Self {
        self.bt = bt;
        self.lister = lister;
        self
    }

    pub fn devices(&self) -> &[BtDeviceInfo] {
        &self.devices
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let toggle: El = Box::new(
            Toggle::new("settings.bluetooth.toggle", BT_ENABLED_KEY).with_id("bluetooth.toggle"),
        );

        let mut device_rows: Vec<El> = Vec::new();
        if self.devices.is_empty() {
            device_rows.push(caption(
                "No paired devices yet. Pair a device to see it here.",
            ));
        } else {
            for dev in &self.devices {
                let battery_str = if dev.battery > 0 {
                    format!("  ·  {}%", dev.battery)
                } else {
                    String::new()
                };
                let key = connect_key(&dev.name);
                let cta_id = format!("settings.bluetooth.connect.{}", dev.name);
                let cta_label = if dev.connected {
                    "Disconnect"
                } else {
                    "Connect"
                };
                // Name + action inline on the top line (a single leading Text
                // keeps the button on the same line); the connection status on
                // its own line beneath.
                let top = Row::new()
                    .push(Box::new(Text::new(&dev.name).with_size(13.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(&cta_id, cta_label, Some(&key))));
                let status = Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(top))
                    .push(Box::new(
                        Text::new(&format!(
                            "{}{battery_str}",
                            if dev.connected { "Connected" } else { "Paired" }
                        ))
                        .with_size(11.0)
                        .with_color(if dev.connected {
                            ui::Theme::dark().status_success
                        } else {
                            ui::Theme::dark().text_dim
                        }),
                    ));
                device_rows.push(Box::new(status));
            }
        }
        device_rows.push(caption(
            "Turn Bluetooth on, then put a device in pairing mode to \
             connect it.",
        ));

        crate::widgets::cards(vec![card_with_action(
            "Bluetooth",
            toggle,
            rows(device_rows),
        )])
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(BluetoothPane::new())
}

impl Widget for BluetoothPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(
                root,
                BT_ENABLED_KEY,
                automerge::ScalarValue::Boolean(self.initial_bt_enabled),
            );
            self.needs_seed = false;
        }
        if let Ok(Some((val, _))) = state.get(root, BT_ENABLED_KEY)
            && let Value::Scalar(s) = val
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
            && *b != self.last_bt_enabled
        {
            self.last_bt_enabled = *b;
            if let Err(e) = self.dispatcher.set_bluetooth(*b) {
                warn!(target: "qos::settings", op = "set_bluetooth", error = %e, "bluetooth pane dispatch failed");
            }
        }
        // Per-device Connect / Disconnect clicks. The button writes its
        // `bluetooth.connect.<name>` key true; we read it as a one-shot edge
        // (clearing it immediately), then drive the real action against the
        // device's MAC: disconnect a connected device, connect an idle one.
        // After acting we re-read the paired list so the row's status and its
        // button label follow the new connection state.
        let targets: Vec<(String, String, bool, String)> = self
            .devices
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    d.mac.clone(),
                    d.connected,
                    connect_key(&d.name),
                )
            })
            .collect();
        let mut toggled_any = false;
        for (name, mac, connected, key) in targets {
            let pressed = matches!(
                state.get(root, key.as_str()),
                Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if !pressed {
                continue;
            }
            let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
            let res = if connected {
                self.bt.disconnect(&mac)
            } else {
                self.bt.connect(&mac)
            };
            if let Err(e) = res {
                warn!(
                    target: "qos::settings",
                    op = if connected { "bt_disconnect" } else { "bt_connect" },
                    device = %name,
                    error = %e,
                    "bluetooth connect/disconnect failed"
                );
            }
            toggled_any = true;
        }
        if toggled_any {
            // Re-read so the list reflects the new connected state; a failed
            // shell-out leaves the prior list, never a fabricated status.
            if let Ok(devs) = (self.lister)() {
                self.devices = devs;
            }
            self.structure_dirty = true;
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "BluetoothPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;
    use std::sync::Mutex;

    fn sample_devices() -> Vec<BtDeviceInfo> {
        vec![
            BtDeviceInfo {
                name: "AirPods Pro".to_string(),
                mac: "AA:BB:CC:DD:EE:01".to_string(),
                battery: 80,
                connected: true,
            },
            BtDeviceInfo {
                name: "Magic Mouse".to_string(),
                mac: "AA:BB:CC:DD:EE:02".to_string(),
                battery: 0,
                connected: false,
            },
        ]
    }

    /// Records the connect/disconnect calls a device toggle forwards, so the
    /// test can assert the right MAC + direction reached the backend.
    #[derive(Default)]
    struct RecordingBt {
        calls: Mutex<Vec<String>>,
    }
    impl BtControl for RecordingBt {
        fn connect(&self, mac: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("connect({mac})"));
            Ok(())
        }
        fn disconnect(&self, mac: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("disconnect({mac})"));
            Ok(())
        }
    }

    #[test]
    fn bt_list_renders_each_paired_device() {
        let rec = Arc::new(RecordingDispatcher::default());
        let devs = sample_devices();
        let pane =
            BluetoothPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, true, devs);
        assert_eq!(pane.devices().len(), 2);
        let names: Vec<&str> = pane.devices().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["AirPods Pro", "Magic Mouse"]);
    }

    #[test]
    fn bt_toggle_invokes_set_bluetooth() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = BluetoothPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_devices(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(
            &root,
            BT_ENABLED_KEY,
            automerge::ScalarValue::Boolean(false),
        );
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls(), vec!["set_bluetooth(false)"]);
    }

    /// Clicking a connected device's button disconnects it (by MAC), then
    /// re-reads the list so the row flips to "Paired" — the real action path,
    /// not a logged stub.
    #[test]
    fn device_button_disconnects_connected_device_and_refreshes() {
        let rec = Arc::new(RecordingDispatcher::default());
        let bt = Arc::new(RecordingBt::default());
        // After the disconnect the radio reports AirPods as no longer connected.
        let after = vec![
            BtDeviceInfo {
                name: "AirPods Pro".into(),
                mac: "AA:BB:CC:DD:EE:01".into(),
                battery: 80,
                connected: false,
            },
            BtDeviceInfo {
                name: "Magic Mouse".into(),
                mac: "AA:BB:CC:DD:EE:02".into(),
                battery: 0,
                connected: false,
            },
        ];
        let lister: BtLister = {
            let a = after;
            Arc::new(move || Ok(a.clone()))
        };
        let mut pane = BluetoothPane::with_dispatcher(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_devices(),
        )
        .with_bt_control(bt.clone() as Arc<dyn BtControl>, lister);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(bt.calls.lock().unwrap().is_empty(), "seed must not act");
        // Press the AirPods button (currently connected → should disconnect).
        let _ = doc.put(&root, connect_key("AirPods Pro").as_str(), true);
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root); // one-shot: must not re-fire while at rest
        assert_eq!(
            bt.calls.lock().unwrap().as_slice(),
            ["disconnect(AA:BB:CC:DD:EE:01)"]
        );
        assert!(
            pane.take_structure_dirty(),
            "a toggle re-reads the list → rebuild"
        );
        assert!(
            !pane
                .devices()
                .iter()
                .any(|d| d.name == "AirPods Pro" && d.connected),
            "the re-read reflects the disconnected state: {:?}",
            pane.devices()
        );
    }

    /// Clicking an idle device's button connects it (by MAC).
    #[test]
    fn device_button_connects_idle_device() {
        let rec = Arc::new(RecordingDispatcher::default());
        let bt = Arc::new(RecordingBt::default());
        let lister: BtLister = Arc::new(|| Ok(Vec::new()));
        let mut pane = BluetoothPane::with_dispatcher(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_devices(),
        )
        .with_bt_control(bt.clone() as Arc<dyn BtControl>, lister);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Magic Mouse is idle → the button connects it.
        let _ = doc.put(&root, connect_key("Magic Mouse").as_str(), true);
        pane.sync(&mut doc, &root);
        assert_eq!(
            bt.calls.lock().unwrap().as_slice(),
            ["connect(AA:BB:CC:DD:EE:02)"]
        );
    }
}

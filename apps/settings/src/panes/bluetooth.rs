//! Bluetooth pane — controller toggle + paired-device list.
//!
//! Plumbs `os_actions::bluetooth::{get_bluetooth, set_bluetooth,
//! list_bt_devices}`. The connect/disconnect buttons set a per-MAC
//! CRDT key; the pane reads the key on `sync` and would ordinarily
//! shell out to `bluetoothctl connect <mac>`. We don't have a typed
//! dispatcher method for that today (the QS panel doesn't either) so
//! the click is logged as a stub — clean place to grow the
//! integration without rejiggering the pane wiring.
//
// CRDT keys:
//   `bluetooth.enabled`           — bool, controller toggle
//   `bluetooth.connect.<name>`    — bool, per-device connect/disconnect

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use os_actions::{BtDeviceInfo, Dispatcher};
use taffy::{NodeId, TaffyTree};
use tracing::{info, warn};
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;

pub const BT_ENABLED_KEY: &str = "bluetooth.enabled";

fn connect_key(name: &str) -> String {
    format!("bluetooth.connect.{name}")
}

type BtLister = Arc<dyn Fn() -> Result<Vec<BtDeviceInfo>, String> + Send + Sync>;

pub struct BluetoothPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    #[allow(dead_code)]
    lister: BtLister,
    devices: Vec<BtDeviceInfo>,
    initial_bt_enabled: bool,
    last_bt_enabled: bool,
    needs_seed: bool,
    dispatched_connects: Vec<String>,
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
        let lister: BtLister = Arc::new(|| os_actions::list_bt_devices());
        let devices = lister().unwrap_or_default();
        let initial_bt = os_actions::get_bluetooth().unwrap_or(false);
        Self {
            dispatcher: dispatcher_or_real(None),
            lister,
            devices,
            initial_bt_enabled: initial_bt,
            last_bt_enabled: initial_bt,
            needs_seed: true,
            dispatched_connects: Vec::new(),
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
            lister,
            devices,
            initial_bt_enabled,
            last_bt_enabled: initial_bt_enabled,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn devices(&self) -> &[BtDeviceInfo] {
        &self.devices
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let toggle_row = Row::new()
            .push(Box::new(Text::new("Bluetooth").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Toggle::new("settings.bluetooth.toggle", BT_ENABLED_KEY)
                    .with_id("bluetooth.toggle"),
            ));
        let mut col = Column::new().push(Box::new(toggle_row));

        if self.devices.is_empty() {
            col = col.push(Box::new(
                Text::new("No paired devices yet — pair via bluetoothctl.")
                    .with_size(11.0),
            ));
        } else {
            for dev in &self.devices {
                let battery_str = if dev.battery > 0 {
                    format!(" · {}%", dev.battery)
                } else {
                    String::new()
                };
                let connected_str = if dev.connected {
                    " (connected)"
                } else {
                    ""
                };
                let label = format!("{}{battery_str}{connected_str}", dev.name);
                let key = connect_key(&dev.name);
                let cta_id = format!("settings.bluetooth.connect.{}", dev.name);
                let cta_label = if dev.connected { "Disconnect" } else { "Connect" };
                let row = Row::new()
                    .push(Box::new(Text::new(&label).with_size(12.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(&cta_id, cta_label, Some(&key))));
                col = col.push(Box::new(row));
            }
        }
        col = col.push(Box::new(
            Text::new(
                "Bluetooth pairing currently requires bluetoothctl from a \
                 terminal; the connect button only requests an existing \
                 link.",
            )
            .with_size(11.0),
        ));
        Box::new(col)
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
        if let Ok(Some((val, _))) = state.get(root, BT_ENABLED_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    if *b != self.last_bt_enabled {
                        self.last_bt_enabled = *b;
                        if let Err(e) = self.dispatcher.set_bluetooth(*b) {
                            warn!(target: "qos::settings", op = "set_bluetooth", error = %e, "bluetooth pane dispatch failed");
                        }
                    }
                }
            }
        }
        // Per-device connect/disconnect clicks. We don't have a typed
        // dispatcher method for connect-by-MAC; the click is logged as
        // a stub. Future work: extend `Dispatcher` with
        // `bt_connect/bt_disconnect` once the QS popover gains the same
        // capability.
        let names: Vec<String> = self.devices.iter().map(|d| d.name.clone()).collect();
        for name in names {
            let key = connect_key(&name);
            let pressed = matches!(
                state.get(root, key.as_str()),
                Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if pressed && !self.dispatched_connects.contains(&name) {
                self.dispatched_connects.push(name.clone());
                let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
                info!(
                    target: "qos::settings",
                    op = "bt_connect_stub",
                    device = %name,
                    "bluetooth connect requested (no dispatcher hook yet)"
                );
            }
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
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
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

    fn name(&self) -> &str {
        "BluetoothPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    fn sample_devices() -> Vec<BtDeviceInfo> {
        vec![
            BtDeviceInfo {
                name: "AirPods Pro".to_string(),
                battery: 80,
                connected: true,
            },
            BtDeviceInfo {
                name: "Magic Mouse".to_string(),
                battery: 0,
                connected: false,
            },
        ]
    }

    #[test]
    fn bt_list_renders_each_paired_device() {
        let rec = Arc::new(RecordingDispatcher::default());
        let devs = sample_devices();
        let pane = BluetoothPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            devs.clone(),
        );
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
        let _ = doc.put(&root, BT_ENABLED_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls(), vec!["set_bluetooth(false)"]);
    }
}

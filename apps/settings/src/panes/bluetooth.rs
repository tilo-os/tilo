//! Bluetooth settings backed directly by BlueZ over D-Bus. Settings starts
//! operations; the compositor-owned BlueZ Agent owns security prompts.

use crate::widgets::{El, caption, card_with_action, rows, setting_row, value_row};
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use qos_bluez::{DeviceInfo, PairOutcome};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

pub const BT_ENABLED_KEY: &str = "bluetooth.enabled";
const SCAN_KEY: &str = "bluetooth.scan";
const REFRESH_KEY: &str = "bluetooth.refresh";
fn token(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).replace('.', "_")
}
fn key(path: &str, action: &str) -> String {
    format!("bluetooth.device.{}.{}", token(path), action)
}

pub trait BluetoothControl: Send + Sync {
    fn devices(&self) -> Result<Vec<DeviceInfo>, String>;
    fn powered(&self) -> Result<bool, String>;
    fn scanning(&self) -> Result<bool, String>;
    fn set_powered(&self, on: bool) -> Result<(), String>;
    fn start_scan(&self) -> Result<(), String>;
    fn stop_scan(&self) -> Result<(), String>;
    fn pair(&self, path: &str) -> Result<PairOutcome, String>;
    fn cancel_pairing(&self, path: &str) -> Result<(), String>;
    fn connect(&self, path: &str) -> Result<(), String>;
    fn disconnect(&self, path: &str) -> Result<(), String>;
    fn set_trusted(&self, path: &str, trusted: bool) -> Result<(), String>;
    fn set_alias(&self, path: &str, alias: &str) -> Result<(), String>;
    fn forget(&self, path: &str) -> Result<(), String>;
}

pub struct RealBluetooth;
impl BluetoothControl for RealBluetooth {
    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        os_actions::bluez::devices()
    }
    fn powered(&self) -> Result<bool, String> {
        os_actions::bluez::is_powered()
    }
    fn scanning(&self) -> Result<bool, String> {
        os_actions::bluez::is_scanning()
    }
    fn set_powered(&self, on: bool) -> Result<(), String> {
        os_actions::bluez::set_powered(on)
    }
    fn start_scan(&self) -> Result<(), String> {
        os_actions::bluez::start_scan()
    }
    fn stop_scan(&self) -> Result<(), String> {
        os_actions::bluez::stop_scan()
    }
    fn pair(&self, path: &str) -> Result<PairOutcome, String> {
        os_actions::bluez::pair(path)
    }
    fn cancel_pairing(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::cancel_pairing(path)
    }
    fn connect(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::connect(path)
    }
    fn disconnect(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::disconnect(path)
    }
    fn set_trusted(&self, path: &str, v: bool) -> Result<(), String> {
        os_actions::bluez::set_trusted(path, v)
    }
    fn set_alias(&self, path: &str, alias: &str) -> Result<(), String> {
        os_actions::bluez::set_alias(path, alias)
    }
    fn forget(&self, path: &str) -> Result<(), String> {
        os_actions::bluez::forget(path)
    }
}

pub struct BluetoothPane {
    control: Arc<dyn BluetoothControl>,
    devices: Vec<DeviceInfo>,
    powered: bool,
    scanning: bool,
    notice: Option<String>,
    expanded: Option<String>,
    confirm_forget: Option<String>,
    pending_pair: Option<String>,
    pair_result: Option<Receiver<(String, Result<PairOutcome, String>)>>,
    needs_seed: bool,
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
        let c: Arc<dyn BluetoothControl> = Arc::new(RealBluetooth);
        let devices = c.devices().unwrap_or_default();
        let powered = c.powered().unwrap_or(false);
        let scanning = c.scanning().unwrap_or(false);
        Self::with_control(c, devices, powered, scanning)
    }
    pub fn with_control(
        control: Arc<dyn BluetoothControl>,
        devices: Vec<DeviceInfo>,
        powered: bool,
        scanning: bool,
    ) -> Self {
        Self {
            control,
            devices,
            powered,
            scanning,
            notice: None,
            expanded: None,
            confirm_forget: None,
            pending_pair: None,
            pair_result: None,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }
    pub fn devices(&self) -> &[DeviceInfo] {
        &self.devices
    }
    fn refresh(&mut self) {
        match self.control.devices() {
            Ok(v) => self.devices = v,
            Err(e) => self.notice = Some(format!("Could not refresh devices: {e}")),
        }
        self.scanning = self.control.scanning().unwrap_or(self.scanning);
        self.powered = self.control.powered().unwrap_or(self.powered);
        self.structure_dirty = true;
    }
    fn device_rows(&self) -> Vec<El> {
        let mut out: Vec<El> = Vec::new();
        for d in &self.devices {
            let pairing = self.pending_pair.as_deref() == Some(&d.path);
            let primary = if pairing {
                "Cancel pairing"
            } else if !d.paired {
                "Pair"
            } else if d.connected {
                "Disconnect"
            } else {
                "Connect"
            };
            let action = if pairing {
                "cancel"
            } else if !d.paired {
                "pair"
            } else if d.connected {
                "disconnect"
            } else {
                "connect"
            };
            let mut status = if d.connected {
                "Connected".to_string()
            } else if d.paired {
                "Paired".into()
            } else {
                "Nearby".into()
            };
            if let Some(b) = d.battery {
                status.push_str(&format!(" · Battery {b}%"));
            }
            let top = Row::new()
                .push(Box::new(Text::new(&d.name).with_size(13.0)))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(
                    &format!("settings.bluetooth.{action}.{}", token(&d.path)),
                    primary,
                    Some(&key(&d.path, action)),
                )))
                .push(Box::new(Button::new(
                    &format!("settings.bluetooth.details.{}", token(&d.path)),
                    if self.expanded.as_deref() == Some(&d.path) {
                        "Less"
                    } else {
                        "Details"
                    },
                    Some(&key(&d.path, "details")),
                )));
            let mut block: Vec<El> = vec![Box::new(top), caption(&status)];
            if self.expanded.as_deref() == Some(&d.path) {
                block.push(value_row("Address", &d.address));
                if let Some(rssi) = d.rssi {
                    block.push(value_row("Signal", &format!("{rssi} dBm")));
                }
                if d.paired {
                    block.push(setting_row(
                        "Trust this device",
                        Box::new(Toggle::new(
                            &format!("settings.bluetooth.trusted.{}", token(&d.path)),
                            &key(&d.path, "trusted"),
                        )),
                    ));
                    block.push(caption("A trusted device may use previously authorised services without another confirmation. This does not force reconnection."));
                    block.push(Box::new(
                        TextInput::new(Vec::new())
                            .with_id(&format!("bluetooth.alias.{}", token(&d.path)))
                            .with_on_change_path(&key(&d.path, "alias"))
                            .with_placeholder(&d.name),
                    ));
                    block.push(Box::new(
                        Row::new()
                            .push(Box::new(Button::new(
                                &format!("settings.bluetooth.rename.{}", token(&d.path)),
                                "Rename",
                                Some(&key(&d.path, "rename")),
                            )))
                            .push(Box::new(Button::new(
                                &format!("settings.bluetooth.forget.{}", token(&d.path)),
                                if self.confirm_forget.as_deref() == Some(&d.path) {
                                    "Confirm forget"
                                } else {
                                    "Forget…"
                                },
                                Some(&key(&d.path, "forget")),
                            )))
                            .push(Box::new(Spacer::new())),
                    ));
                }
                if d.icon.starts_with("audio-") {
                    block.push(caption("Sound exposes profiles and codecs only when PipeWire reports selectable capabilities."));
                }
            }
            out.push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(rows(block)),
            ));
        }
        if out.is_empty() {
            out.push(caption(if self.scanning {
                "Looking for nearby devices…"
            } else {
                "No devices found. Put a device in pairing mode, then start discovery."
            }));
        }
        out
    }
    fn build_composition(&self) -> Box<dyn Widget> {
        let mut body: Vec<El> = vec![Box::new(
            Row::new()
                .push(Box::new(Button::new(
                    "settings.bluetooth.scan",
                    if self.scanning {
                        "Stop discovery"
                    } else {
                        "Find devices"
                    },
                    Some(SCAN_KEY),
                )))
                .push(Box::new(Button::new(
                    "settings.bluetooth.refresh",
                    "Refresh",
                    Some(REFRESH_KEY),
                )))
                .push(Box::new(Spacer::new())),
        )];
        if let Some(n) = &self.notice {
            body.push(caption(n));
        }
        body.extend(self.device_rows());
        crate::widgets::cards(vec![card_with_action(
            "Bluetooth",
            Box::new(
                Toggle::new("settings.bluetooth.toggle", BT_ENABLED_KEY)
                    .with_id("bluetooth.toggle"),
            ),
            rows(body),
        )])
    }
    fn act(&mut self, state: &mut AutoCommit, root: &ObjId, d: &DeviceInfo, action: &str) {
        let k = key(&d.path, action);
        if !read_bool(state, root, &k) {
            return;
        }
        let _ = state.put(root, &k, false);
        let result = match action {
            "connect" => self
                .control
                .connect(&d.path)
                .map(|_| format!("Connected {}.", d.name)),
            "disconnect" => self
                .control
                .disconnect(&d.path)
                .map(|_| format!("Disconnected {}.", d.name)),
            _ => return,
        };
        self.notice = Some(result.unwrap_or_else(|e| {
            format!(
                "Could not {action} {}: {e} Check that it is awake and in range, then retry.",
                d.name
            )
        }));
        self.refresh();
    }
}
pub fn build() -> Box<dyn Widget> {
    Box::new(BluetoothPane::new())
}
impl Widget for BluetoothPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(rx) = &self.pair_result
            && let Ok((path, result)) = rx.try_recv()
        {
            let name = self
                .devices
                .iter()
                .find(|d| d.path == path)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "Device".into());
            self.notice = Some(match result {
                Ok(PairOutcome::PairedAndConnected) => format!("{name} is paired and connected."),
                Ok(PairOutcome::PairedNotConnected { reason }) => {
                    format!("{name} paired, but did not connect: {reason} Use Connect to retry.")
                }
                Err(error) => format!(
                    "Could not pair {name}: {error} Check that it is awake and in pairing mode, then retry."
                ),
            });
            self.pending_pair = None;
            self.pair_result = None;
            self.refresh();
        }
        if self.needs_seed {
            let _ = state.put(root, BT_ENABLED_KEY, self.powered);
            let _ = state.put(root, SCAN_KEY, false);
            let _ = state.put(root, REFRESH_KEY, false);
            for d in &self.devices {
                for a in [
                    "pair",
                    "connect",
                    "cancel",
                    "disconnect",
                    "details",
                    "rename",
                    "forget",
                ] {
                    let _ = state.put(root, key(&d.path, a), false);
                }
                let _ = state.put(root, key(&d.path, "trusted"), d.trusted);
                let _ = crate::ensure_text_object(state, root, &key(&d.path, "alias"));
            }
            self.needs_seed = false;
        }
        if let Some(on) = read_bool_opt(state, root, BT_ENABLED_KEY)
            && on != self.powered
        {
            match self.control.set_powered(on) {
                Ok(()) => {
                    self.powered = on;
                    self.notice = Some(if on {
                        "Bluetooth is on.".into()
                    } else {
                        "Bluetooth is off.".into()
                    })
                }
                Err(e) => {
                    self.notice = Some(format!("Could not change Bluetooth power: {e}"));
                    let _ = state.put(root, BT_ENABLED_KEY, self.powered);
                }
            }
            self.structure_dirty = true;
        }
        if read_bool(state, root, SCAN_KEY) {
            let _ = state.put(root, SCAN_KEY, false);
            let r = if self.scanning {
                self.control.stop_scan()
            } else {
                self.control.start_scan()
            };
            match r {
                Ok(()) => {
                    self.scanning = !self.scanning;
                    self.notice = Some(if self.scanning {
                        "Discovery started. Use Refresh as devices wake up.".into()
                    } else {
                        "Discovery stopped.".into()
                    })
                }
                Err(e) => self.notice = Some(format!("Could not change discovery: {e}")),
            };
            self.refresh();
        }
        if read_bool(state, root, REFRESH_KEY) {
            let _ = state.put(root, REFRESH_KEY, false);
            self.refresh();
        }
        let snapshot = self.devices.clone();
        for d in &snapshot {
            if read_bool(state, root, &key(&d.path, "pair")) {
                let _ = state.put(root, key(&d.path, "pair"), false);
                if self.pending_pair.is_none() {
                    let control = Arc::clone(&self.control);
                    let path = d.path.clone();
                    let (tx, rx) = mpsc::channel();
                    self.pending_pair = Some(path.clone());
                    self.pair_result = Some(rx);
                    self.notice = Some(format!(
                        "Pairing {}… Security prompts appear in system UI.",
                        d.name
                    ));
                    std::thread::spawn(move || {
                        let result = control.pair(&path);
                        let _ = tx.send((path, result));
                    });
                    self.structure_dirty = true;
                }
            }
            if read_bool(state, root, &key(&d.path, "cancel")) {
                let _ = state.put(root, key(&d.path, "cancel"), false);
                match self.control.cancel_pairing(&d.path) {
                    Ok(()) => self.notice = Some(format!("Cancelled pairing {}.", d.name)),
                    Err(e) => self.notice = Some(format!("Could not cancel pairing: {e}")),
                }
            }
            for a in ["connect", "disconnect"] {
                self.act(state, root, d, a);
            }
            if read_bool(state, root, &key(&d.path, "details")) {
                let _ = state.put(root, key(&d.path, "details"), false);
                self.expanded = (self.expanded.as_deref() != Some(&d.path)).then(|| d.path.clone());
                self.structure_dirty = true;
            }
            if let Some(v) = read_bool_opt(state, root, &key(&d.path, "trusted"))
                && v != d.trusted
            {
                match self.control.set_trusted(&d.path, v) {
                    Ok(()) => {
                        self.notice = Some(if v {
                            format!("{} is now trusted.", d.name)
                        } else {
                            format!("{} now requires service authorisation.", d.name)
                        })
                    }
                    Err(e) => {
                        self.notice = Some(format!("Could not change device trust: {e}"));
                        let _ = state.put(root, key(&d.path, "trusted"), d.trusted);
                    }
                }
                self.refresh();
            }
            if read_bool(state, root, &key(&d.path, "rename")) {
                let _ = state.put(root, key(&d.path, "rename"), false);
                let alias = read_text(state, root, &key(&d.path, "alias")).unwrap_or_default();
                match self.control.set_alias(&d.path, &alias) {
                    Ok(()) => self.notice = Some(format!("Renamed device to {}.", alias.trim())),
                    Err(e) => self.notice = Some(format!("Could not rename {}: {e}", d.name)),
                };
                self.refresh();
            }
            if read_bool(state, root, &key(&d.path, "forget")) {
                let _ = state.put(root, key(&d.path, "forget"), false);
                if self.confirm_forget.as_deref() == Some(&d.path) {
                    match self.control.forget(&d.path) {
                        Ok(()) => {
                            self.notice =
                                Some(format!("Forgot {}. Pair it again to reconnect.", d.name));
                            self.confirm_forget = None
                        }
                        Err(e) => self.notice = Some(format!("Could not forget {}: {e}", d.name)),
                    };
                    self.refresh();
                } else {
                    self.confirm_forget = Some(d.path.clone());
                    self.notice = Some(format!(
                        "Forget {}? This removes its bond and keys.",
                        d.name
                    ));
                    self.structure_dirty = true;
                }
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
        let mut c = self.build_composition();
        let child = c.build_layout(taffy, theme, node_map);
        self.composed = Some(c);
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
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
            && let Ok(ch) = taffy.children(node)
            && let Some(&cn) = ch.first()
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
            && let Ok(ch) = taffy.children(node_id)
            && let Some(&cn) = ch.first()
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
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|c| c.take_structure_dirty())
    }
}
fn read_bool_opt(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(v) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    read_bool_opt(state, root, key).unwrap_or(false)
}
fn read_text(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Object(automerge::ObjType::Text), o))) => state.text(&o).ok(),
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Mock {
        calls: Mutex<Vec<String>>,
        devices: Vec<DeviceInfo>,
    }
    impl BluetoothControl for Mock {
        fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
            Ok(self.devices.clone())
        }
        fn powered(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn scanning(&self) -> Result<bool, String> {
            Ok(false)
        }
        fn set_powered(&self, v: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("power:{v}"));
            Ok(())
        }
        fn start_scan(&self) -> Result<(), String> {
            Ok(())
        }
        fn stop_scan(&self) -> Result<(), String> {
            Ok(())
        }
        fn pair(&self, _: &str) -> Result<PairOutcome, String> {
            Ok(PairOutcome::PairedAndConnected)
        }
        fn cancel_pairing(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn connect(&self, p: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("connect:{p}"));
            Ok(())
        }
        fn disconnect(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn set_trusted(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_alias(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn forget(&self, p: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("forget:{p}"));
            Ok(())
        }
    }
    fn dev() -> DeviceInfo {
        DeviceInfo {
            path: "/org/bluez/hci0/dev_AA".into(),
            address: "AA".into(),
            name: "Headphones".into(),
            icon: "audio-headset".into(),
            paired: true,
            trusted: true,
            connected: false,
            battery: Some(0),
            rssi: Some(-42),
        }
    }
    #[test]
    fn uses_object_path_and_preserves_zero_battery() {
        let m = Arc::new(Mock {
            calls: Mutex::new(vec![]),
            devices: vec![dev()],
        });
        let mut p = BluetoothPane::with_control(m.clone(), vec![dev()], true, false);
        let mut d = AutoCommit::new();
        let r = automerge::ROOT;
        p.sync(&mut d, &r);
        let _ = d.put(&r, key(&dev().path, "connect"), true);
        p.sync(&mut d, &r);
        assert_eq!(
            m.calls.lock().unwrap().as_slice(),
            ["connect:/org/bluez/hci0/dev_AA"]
        );
        assert_eq!(p.devices()[0].battery, Some(0));
    }
    #[test]
    fn forget_requires_confirmation() {
        let m = Arc::new(Mock {
            calls: Mutex::new(vec![]),
            devices: vec![dev()],
        });
        let mut p = BluetoothPane::with_control(m.clone(), vec![dev()], true, false);
        let mut d = AutoCommit::new();
        let r = automerge::ROOT;
        p.sync(&mut d, &r);
        let k = key(&dev().path, "forget");
        let _ = d.put(&r, &k, true);
        p.sync(&mut d, &r);
        assert!(m.calls.lock().unwrap().is_empty());
        let _ = d.put(&r, &k, true);
        p.sync(&mut d, &r);
        assert_eq!(
            m.calls.lock().unwrap().as_slice(),
            ["forget:/org/bluez/hci0/dev_AA"]
        );
    }
}

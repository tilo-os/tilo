//! Network pane — Wi-Fi radio toggle, scanned network list, refresh
//! button, connection-status row.
//!
//! Plumbs `os_actions::network::{get_wifi, set_wifi,
//! list_wifi_networks, connect_wifi}`. The network list is sampled once
//! on construction (cached) and refreshed when the user clicks the
//! `Refresh` button — re-scanning every frame would be wasteful and
//! visibly stutter the UI on slow nmcli backends.
//
// CRDT keys:
//   `network.wifi_enabled` — bool, wifi toggle
//   `network.refresh`      — bool, click sets true; pane resets on
//                             sync after rescan
//   `network.connect.<ssid>` — bool, click sets true; pane resets on
//                             sync after dispatch

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use os_actions::{Dispatcher, WifiNetworkInfo};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;

pub const WIFI_ENABLED_KEY: &str = "network.wifi_enabled";
pub const REFRESH_KEY: &str = "network.refresh";

/// Build the per-SSID "connect" CRDT key. Inverse of
/// [`ssid_from_connect_key`].
fn connect_key(ssid: &str) -> String {
    format!("network.connect.{ssid}")
}

/// Network-list provider — the prod impl shells out to nmcli, tests
/// supply a stub closure. Boxed so the pane holds it as a trait object;
/// the box satisfies `Send + Sync` because the closure captures nothing
/// (or only thread-safe state).
type NetworkLister = Arc<dyn Fn() -> Result<Vec<WifiNetworkInfo>, String> + Send + Sync>;

pub struct NetworkPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    lister: NetworkLister,
    /// Cached current network list — refreshed at construction and on
    /// `Refresh` button press.
    networks: Vec<WifiNetworkInfo>,
    initial_wifi_enabled: bool,
    last_wifi_enabled: bool,
    needs_seed: bool,
    /// Track which `network.connect.<ssid>` keys we already dispatched
    /// for, so a single button press doesn't re-fire on every frame.
    dispatched_connects: Vec<String>,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for NetworkPane {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkPane {
    pub fn new() -> Self {
        let lister: NetworkLister = Arc::new(|| os_actions::list_wifi_networks());
        let networks = lister().unwrap_or_default();
        let initial_wifi = os_actions::get_wifi().unwrap_or(true);
        Self {
            dispatcher: dispatcher_or_real(None),
            lister,
            networks,
            initial_wifi_enabled: initial_wifi,
            last_wifi_enabled: initial_wifi,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test-only constructor that pins the dispatcher, the wifi-state,
    /// and the network-list source. The list is captured into the
    /// pane's cache immediately.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_wifi_enabled: bool,
        networks: Vec<WifiNetworkInfo>,
    ) -> Self {
        let snapshot = networks.clone();
        let lister: NetworkLister = Arc::new(move || Ok(snapshot.clone()));
        Self {
            dispatcher,
            lister,
            networks,
            initial_wifi_enabled,
            last_wifi_enabled: initial_wifi_enabled,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Cached network list. Tests read this to assert the panes
    /// rendered the right rows.
    pub fn networks(&self) -> &[WifiNetworkInfo] {
        &self.networks
    }

    fn connection_status_text(&self) -> String {
        if !self.last_wifi_enabled {
            return "Wi-Fi is off".to_string();
        }
        match self.networks.iter().find(|n| n.connected) {
            Some(n) => format!("Connected to {}", n.ssid),
            None => "Not connected".to_string(),
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let status_row = Row::new()
            .push(Box::new(
                Text::new(&self.connection_status_text()).with_size(12.0),
            ))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Button::new(
                "settings.network.refresh",
                "Refresh",
                Some(REFRESH_KEY),
            )));
        let toggle_row = Row::new()
            .push(Box::new(Text::new("Wi-Fi").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Toggle::new("settings.network.wifi", WIFI_ENABLED_KEY)
                    .with_id("network.wifi"),
            ));
        let mut col = Column::new()
            .push(Box::new(status_row))
            .push(Box::new(toggle_row));

        for net in &self.networks {
            let signal_glyph = match net.signal {
                0 => "▁",
                1 => "▂",
                2 => "▃",
                3 => "▆",
                _ => "█",
            };
            let lock_glyph = if net.secure { "🔒" } else { "  " };
            let label = format!(
                "{signal_glyph} {lock_glyph} {ssid}",
                ssid = net.ssid
            );
            let key = connect_key(&net.ssid);
            let cta_id = format!("settings.network.connect.{}", net.ssid);
            let net_row = Row::new()
                .push(Box::new(Text::new(&label).with_size(12.0)))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(
                    &cta_id,
                    if net.connected { "Connected" } else { "Connect" },
                    Some(&key),
                )));
            col = col.push(Box::new(net_row));
        }
        col = col.push(Box::new(
            Text::new(
                "Wi-Fi list and connection are best-effort via nmcli; \
                 secure networks require a known password.",
            )
            .with_size(11.0),
        ));
        Box::new(col)
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(NetworkPane::new())
}

impl Widget for NetworkPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(
                root,
                WIFI_ENABLED_KEY,
                automerge::ScalarValue::Boolean(self.initial_wifi_enabled),
            );
            let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
            self.needs_seed = false;
        }

        // Wi-Fi toggle.
        if let Ok(Some((val, _))) = state.get(root, WIFI_ENABLED_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    if *b != self.last_wifi_enabled {
                        self.last_wifi_enabled = *b;
                        if let Err(e) = self.dispatcher.set_wifi(*b) {
                            warn!(target: "qos::settings", op = "set_wifi", error = %e, "network pane dispatch failed");
                        }
                    }
                }
            }
        }

        // Refresh button — when the button writes `true`, rescan and
        // clear the bit so subsequent frames don't loop.
        let refresh_pressed = matches!(
            state.get(root, REFRESH_KEY),
            Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        );
        if refresh_pressed {
            let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
            match (self.lister)() {
                Ok(nets) => self.networks = nets,
                Err(e) => warn!(target: "qos::settings", op = "list_wifi_networks", error = %e, "rescan failed"),
            }
        }

        // Per-network connect buttons.
        let ssids: Vec<String> = self.networks.iter().map(|n| n.ssid.clone()).collect();
        for ssid in ssids {
            let key = connect_key(&ssid);
            let pressed = matches!(
                state.get(root, key.as_str()),
                Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if pressed && !self.dispatched_connects.contains(&ssid) {
                self.dispatched_connects.push(ssid.clone());
                let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
                if let Err(e) = self.dispatcher.connect_wifi(&ssid, None) {
                    warn!(target: "qos::settings", op = "connect_wifi", error = %e, "network pane dispatch failed");
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
        "NetworkPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    fn sample_networks() -> Vec<WifiNetworkInfo> {
        vec![
            WifiNetworkInfo {
                ssid: "Home_Network".to_string(),
                signal: 4,
                secure: true,
                connected: true,
            },
            WifiNetworkInfo {
                ssid: "Cafe_Guest".to_string(),
                signal: 3,
                secure: false,
                connected: false,
            },
            WifiNetworkInfo {
                ssid: "Neighbour_5G".to_string(),
                signal: 2,
                secure: true,
                connected: false,
            },
        ]
    }

    #[test]
    fn wifi_list_renders_each_returned_network() {
        let rec = Arc::new(RecordingDispatcher::default());
        let nets = sample_networks();
        let pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            nets.clone(),
        );
        assert_eq!(pane.networks().len(), 3);
        let by_ssid: Vec<&str> = pane.networks().iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(by_ssid, vec!["Home_Network", "Cafe_Guest", "Neighbour_5G"]);
        // Status line picks the connected network.
        assert_eq!(pane.connection_status_text(), "Connected to Home_Network");
    }

    #[test]
    fn wifi_toggle_invokes_set_wifi() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // User flips the toggle off.
        let _ = doc.put(&root, WIFI_ENABLED_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls(), vec!["set_wifi(false)"]);
    }

    #[test]
    fn connect_button_invokes_connect_wifi_once() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(
            &root,
            connect_key("Cafe_Guest").as_str(),
            automerge::ScalarValue::Boolean(true),
        );
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls(), vec!["connect_wifi(Cafe_Guest)"]);
    }

    #[test]
    fn refresh_button_replaces_network_list() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        // Swap the lister to a fresh set so we can observe the refresh.
        let fresh = vec![WifiNetworkInfo {
            ssid: "NewSpot".to_string(),
            signal: 4,
            secure: false,
            connected: false,
        }];
        pane.lister = {
            let f = fresh.clone();
            Arc::new(move || Ok(f.clone()))
        };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, REFRESH_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(pane.networks(), fresh.as_slice());
    }
}

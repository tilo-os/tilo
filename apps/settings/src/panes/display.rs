//! Display pane — real connected-output list, brightness, and the display
//! comfort preferences.
//!
//! - **Displays** enumerates the machine's real DRM connectors from
//!   `/sys/class/drm/*/status`, so the arrangement card reflects what's
//!   actually plugged in (built-in panel + any external monitors), not a
//!   hard-coded pair of rows.
//! - **Brightness** forwards to `os_actions::set_brightness`
//!   (`brightnessctl`), sampled once on construction.
//! - **Automatic brightness / Night light** are durable preferences
//!   persisted to the shared store ([`crate::prefs::SystemPrefs`]); the
//!   compositor's output layer reads them.
//!
//! Resolution and scale are deliberately *not* offered here: qOS's
//! compositor owns mode-setting and there is no unprivileged, compositor-
//! agnostic way to apply a mode from this app, so a picker would list modes
//! it could not switch to. The connector list shows the real hardware
//! instead.
//
// CRDT keys this pane reads / writes:
//   `display.brightness`        — f64 in `[0.0, 1.0]`, slider value
//   `display.auto_brightness`   — bool, durable preference
//   `display.night_light`       — bool, durable preference

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use os_actions::Dispatcher;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Slider, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;
use crate::prefs::{DisplayPrefs, SystemPrefs};
use crate::widgets::{card, caption, rows, setting_row, value_row, El};

/// CRDT key the brightness slider writes into.
pub const BRIGHTNESS_KEY: &str = "display.brightness";
/// CRDT key the auto-brightness toggle writes into.
pub const AUTO_BRIGHTNESS_KEY: &str = "display.auto_brightness";
/// CRDT key the night-light toggle writes into.
pub const NIGHT_LIGHT_KEY: &str = "display.night_light";

/// One real display output, derived from a DRM connector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayOutput {
    /// User-facing name, e.g. `"Built-in display"` or `"HDMI-A-1"`.
    pub label: String,
    pub connected: bool,
    pub builtin: bool,
}

/// Map a raw DRM connector directory name + its `status` file contents to a
/// [`DisplayOutput`]. `conn` looks like `card0-eDP-1`; the `cardN-` prefix
/// is stripped and built-in panels (eDP/LVDS/DSI) get a friendly label.
pub fn classify_connector(conn: &str, status: &str) -> DisplayOutput {
    // Strip the leading `cardN-` so we're left with the connector, e.g.
    // `eDP-1`, `HDMI-A-1`, `DP-2`.
    let name = conn.split_once('-').map(|(_, rest)| rest).unwrap_or(conn);
    let upper = name.to_ascii_uppercase();
    let builtin = upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI");
    let label = if builtin {
        "Built-in display".to_string()
    } else {
        name.to_string()
    };
    DisplayOutput {
        label,
        connected: status.trim() == "connected",
        builtin,
    }
}

/// Whether a DRM connector directory name is a *virtual* connector rather
/// than a user-facing display port. The compositor's writeback connector
/// (`card0-Writeback-1`) and any `Virtual`/`None` connectors are not physical
/// outputs a user plugs a monitor into, so they must not appear in the
/// arrangement list.
fn is_virtual_connector(conn: &str) -> bool {
    let lower = conn.to_ascii_lowercase();
    lower.contains("writeback") || lower.contains("virtual")
}

/// Enumerate real DRM connectors from `/sys/class/drm`. Best-effort: returns
/// an empty vec when the directory is unavailable (a container under
/// `--shots`), in which case the pane shows an honest "no displays" note.
/// Virtual connectors (compositor writeback) are skipped — they are not
/// displays a user can plug into.
fn read_outputs() -> Vec<DisplayOutput> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else {
        return out;
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // A connector directory is `cardN-<connector>` and carries a
        // `status` file; the bare `cardN` render node does not.
        if !name.contains('-') || !name.starts_with("card") {
            continue;
        }
        if is_virtual_connector(&name) {
            continue;
        }
        let status_path = entry.path().join("status");
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            out.push(classify_connector(&name, &status));
        }
    }
    // Deterministic order: built-in first, then connected, then alphabetical.
    out.sort_by(|a, b| {
        b.builtin
            .cmp(&a.builtin)
            .then(b.connected.cmp(&a.connected))
            .then(a.label.cmp(&b.label))
    });
    out
}

/// Display pane widget.
pub struct DisplayPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    outputs: Vec<DisplayOutput>,
    initial_brightness: f32,
    initial_prefs: DisplayPrefs,
    last_brightness: f32,
    last_prefs: DisplayPrefs,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for DisplayPane {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayPane {
    pub fn new() -> Self {
        let initial = os_actions::get_brightness().unwrap_or(0.5);
        Self {
            dispatcher: dispatcher_or_real(None),
            outputs: read_outputs(),
            initial_brightness: initial,
            initial_prefs: SystemPrefs::load().display,
            last_brightness: initial,
            last_prefs: DisplayPrefs::default(),
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` constructor: inject the dispatcher, initial
    /// brightness, and a deterministic output list so the render/assertions
    /// don't depend on the host's hardware.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_brightness: f32,
    ) -> Self {
        Self {
            dispatcher,
            outputs: vec![
                DisplayOutput { label: "Built-in display".into(), connected: true, builtin: true },
            ],
            initial_brightness,
            initial_prefs: DisplayPrefs::default(),
            last_brightness: initial_brightness,
            last_prefs: DisplayPrefs::default(),
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Pin the output list (tests / `--shots`).
    pub fn with_outputs(mut self, outputs: Vec<DisplayOutput>) -> Self {
        self.outputs = outputs;
        self
    }

    pub fn outputs(&self) -> &[DisplayOutput] {
        &self.outputs
    }

    /// The outputs the arrangement card lists as rows: only the connected
    /// ones. Disconnected physical ports are unused hardware, not displays —
    /// enumerating each one (a laptop's GPU can expose a dozen idle DP
    /// connectors) is noise, so they are summarised in a caption instead.
    fn connected_outputs(&self) -> Vec<&DisplayOutput> {
        self.outputs.iter().filter(|o| o.connected).collect()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // --- Arrangement card: the real *connected* outputs.
        let connected = self.connected_outputs();
        let disconnected = self.outputs.len() - connected.len();
        let arrangement = if connected.is_empty() {
            card(
                "Displays",
                rows(vec![caption(
                    "No displays detected on this device.",
                )]),
            )
        } else {
            let mut out_rows: Vec<El> = Vec::new();
            for o in &connected {
                let status = if o.builtin { "Active · primary" } else { "Connected" };
                out_rows.push(value_row(&o.label, status));
            }
            // One honest line about the idle ports rather than a row each.
            out_rows.push(caption(if disconnected == 0 {
                "Connect a second display to extend or mirror your screen."
            } else {
                "Connect a display to a free port to extend or mirror your screen."
            }));
            card("Displays", rows(out_rows))
        };

        // --- Brightness & comfort card.
        let brightness = card(
            "Brightness",
            rows(vec![
                setting_row(
                    "Brightness",
                    Box::new(
                        Slider::new("settings.display.brightness", BRIGHTNESS_KEY)
                            .with_id("display.brightness"),
                    ),
                ),
                setting_row(
                    "Automatic brightness",
                    Box::new(
                        Toggle::new("settings.display.auto_brightness", AUTO_BRIGHTNESS_KEY)
                            .with_id("display.auto_brightness"),
                    ),
                ),
                setting_row(
                    "Night light",
                    Box::new(
                        Toggle::new("settings.display.night_light", NIGHT_LIGHT_KEY)
                            .with_id("display.night_light"),
                    ),
                ),
                caption(
                    "Brightness drives the backlight now; automatic brightness \
                     and night light are applied by the compositor.",
                ),
            ]),
        );

        crate::widgets::cards(vec![arrangement, brightness])
    }

    /// Load prefs fresh, apply `f` to the display section, persist.
    fn persist_display(f: impl FnOnce(&mut DisplayPrefs)) {
        let mut p = SystemPrefs::load();
        f(&mut p.display);
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "display_prefs_save", error = %e, "failed to persist display prefs");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(DisplayPane::new())
}

impl Widget for DisplayPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(
                root,
                BRIGHTNESS_KEY,
                automerge::ScalarValue::F64(self.initial_brightness as f64),
            );
            let _ = state.put(
                root,
                AUTO_BRIGHTNESS_KEY,
                automerge::ScalarValue::Boolean(self.initial_prefs.auto_brightness),
            );
            let _ = state.put(
                root,
                NIGHT_LIGHT_KEY,
                automerge::ScalarValue::Boolean(self.initial_prefs.night_light),
            );
            self.last_prefs = self.initial_prefs;
            self.needs_seed = false;
        }

        // Brightness → dispatcher, guarded so we don't shell out every frame.
        if let Ok(Some((val, _))) = state.get(root, BRIGHTNESS_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::F64(f) = s.as_ref() {
                    let new = *f as f32;
                    if (new - self.last_brightness).abs() > 0.001 {
                        self.last_brightness = new;
                        if let Err(e) =
                            self.dispatcher.set_brightness((new.clamp(0.0, 1.0)) * 100.0)
                        {
                            warn!(target: "qos::settings", op = "set_brightness", error = %e, "display pane dispatch failed");
                        }
                    }
                }
            }
        }

        // Comfort preferences → persist to the shared store on change.
        if let Some(b) = read_bool(state, root, AUTO_BRIGHTNESS_KEY) {
            if b != self.last_prefs.auto_brightness {
                self.last_prefs.auto_brightness = b;
                Self::persist_display(|d| d.auto_brightness = b);
            }
        }
        if let Some(b) = read_bool(state, root, NIGHT_LIGHT_KEY) {
            if b != self.last_prefs.night_light {
                self.last_prefs.night_light = b;
                Self::persist_display(|d| d.night_light = b);
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
                height: taffy::Dimension::auto(),
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
        "DisplayPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Read a `bool` scalar at `key`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    #[test]
    fn classify_builtin_and_external_connectors() {
        let builtin = classify_connector("card0-eDP-1", "connected\n");
        assert_eq!(builtin.label, "Built-in display");
        assert!(builtin.builtin);
        assert!(builtin.connected);

        let hdmi = classify_connector("card0-HDMI-A-1", "disconnected\n");
        assert_eq!(hdmi.label, "HDMI-A-1");
        assert!(!hdmi.builtin);
        assert!(!hdmi.connected);

        let dp = classify_connector("card1-DP-2", "connected");
        assert_eq!(dp.label, "DP-2");
        assert!(dp.connected);
    }

    #[test]
    fn brightness_slider_invokes_set_brightness_on_drag() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(rec.calls().is_empty(), "seed sync should not dispatch");
        let _ = doc.put(&root, BRIGHTNESS_KEY, automerge::ScalarValue::F64(0.75));
        pane.sync(&mut doc, &root);
        let calls = rec.calls();
        assert_eq!(calls.len(), 1, "expected one dispatch, got {:?}", calls);
        assert!(calls[0].starts_with("set_brightness("));
        assert!(calls[0].contains("75."), "expected 75% in {:?}", calls[0]);
    }

    #[test]
    fn brightness_unchanged_does_not_redispatch() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, BRIGHTNESS_KEY, automerge::ScalarValue::F64(0.75));
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls().len(), 1);
    }

    #[test]
    fn seed_writes_initial_brightness_into_crdt() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.42);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let (val, _) = doc.get(&root, BRIGHTNESS_KEY).unwrap().unwrap();
        match val {
            automerge::Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::F64(f) => assert!((f - 0.42).abs() < 1e-6, "got {f}"),
                other => panic!("expected F64 scalar, got {other:?}"),
            },
            other => panic!("expected scalar, got {other:?}"),
        }
    }

    /// The arrangement card is driven by the injected output list, proving
    /// it renders real hardware rather than a hard-coded pair.
    #[test]
    fn arrangement_reflects_injected_outputs() {
        let rec = Arc::new(RecordingDispatcher::default());
        let pane = DisplayPane::with_dispatcher(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            0.5,
        )
        .with_outputs(vec![
            DisplayOutput { label: "Built-in display".into(), connected: true, builtin: true },
            DisplayOutput { label: "HDMI-A-1".into(), connected: true, builtin: false },
        ]);
        assert_eq!(pane.outputs().len(), 2);
        assert_eq!(pane.outputs()[1].label, "HDMI-A-1");
    }

    /// The compositor's writeback connector and other virtual connectors are
    /// not displays a user plugs into, so they are filtered from the list.
    #[test]
    fn virtual_connectors_are_filtered() {
        assert!(is_virtual_connector("card0-Writeback-1"));
        assert!(is_virtual_connector("card0-Virtual-1"));
        assert!(!is_virtual_connector("card0-eDP-1"));
        assert!(!is_virtual_connector("card0-HDMI-A-1"));
        assert!(!is_virtual_connector("card0-DP-2"));
    }

    /// Only connected outputs become rows; a laptop's dozen idle DP ports are
    /// summarised in a caption rather than enumerated one-per-row.
    #[test]
    fn arrangement_lists_only_connected_outputs() {
        let rec = Arc::new(RecordingDispatcher::default());
        let pane = DisplayPane::with_dispatcher(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            0.5,
        )
        .with_outputs(vec![
            DisplayOutput { label: "Built-in display".into(), connected: true, builtin: true },
            DisplayOutput { label: "DP-1".into(), connected: false, builtin: false },
            DisplayOutput { label: "DP-2".into(), connected: false, builtin: false },
            DisplayOutput { label: "HDMI-A-1".into(), connected: false, builtin: false },
        ]);
        let shown = pane.connected_outputs();
        assert_eq!(shown.len(), 1, "only the connected built-in display is listed");
        assert_eq!(shown[0].label, "Built-in display");
    }
}

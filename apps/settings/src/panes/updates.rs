//! Updates pane — the system update manager surface.
//!
//! qOS is an immutable bootc image: updates are atomic image pulls staged
//! for the next boot, not piecemeal package upgrades. This pane shows the
//! current deployment, whether a newer image is *actually* staged (read from
//! `bootc status --format json`), the release channel, and real actions:
//!
//! - **Check for updates** re-queries the system and stamps the last-checked
//!   time (persisted so it survives restarts).
//! - **Restart & update** (shown only when an image is staged) reboots into
//!   the staged deployment via [`Rebooter`] (`systemctl reboot`).
//! - **Release channel** and **Automatic updates** are durable preferences
//!   persisted to the shared store ([`crate::prefs::SystemPrefs`]); the
//!   update timer/service reads them.
//
// CRDT keys this pane reads / writes:
//   `updates.check`    — bool, "Check for updates" button
//   `updates.restart`  — bool, "Restart & update" button
//   `updates.channel`  — string, release-channel dropdown
//   `updates.auto`     — bool, automatic-updates toggle

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Dropdown, Row, Spacer, Text, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::prefs::{SystemPrefs, UpdatePrefs};
use crate::widgets::{card, caption, rows, setting_row, value_row, El};

pub const CHECK_KEY: &str = "updates.check";
pub const RESTART_KEY: &str = "updates.restart";
pub const AUTO_KEY: &str = "updates.auto";
pub const CHANNEL_KEY: &str = "updates.channel";

/// The channels offered by the release-channel dropdown.
pub const CHANNELS: &[&str] = &["Stable", "Testing", "Nightly"];

/// The system-derived part of the update state: the current image and any
/// image staged for the next boot. Preferences (channel, auto, last-checked)
/// live in [`UpdatePrefs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    pub current_version: String,
    pub staged_version: Option<String>,
}

impl UpdateStatus {
    /// Query the real system: pretty name from `/etc/os-release`, staged
    /// deployment from `bootc status`.
    pub fn from_system() -> Self {
        let current = fs::read_to_string(Path::new("/etc/os-release"))
            .ok()
            .and_then(|s| parse_pretty_name(&s))
            .unwrap_or_else(|| "qOS (Fedora bootc)".to_string());
        Self {
            current_version: current,
            staged_version: read_bootc_staged(),
        }
    }
}

fn parse_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(rest) = line.trim().strip_prefix("PRETTY_NAME=") {
            let v = rest.trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Ask `bootc` whether an image is staged for the next boot. Returns `None`
/// when `bootc` is missing, fails, or reports nothing staged.
fn read_bootc_staged() -> Option<String> {
    let out = Command::new("bootc")
        .args(["status", "--format", "json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_bootc_staged(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `bootc status --format json` for a staged deployment. Defensive
/// against schema drift: any non-null `status.staged` counts as staged, and
/// we surface the first human-readable label we can find under it.
pub fn parse_bootc_staged(json: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    let staged = root.get("status")?.get("staged")?;
    if staged.is_null() {
        return None;
    }
    // Prefer an explicit version, then an image reference, else a generic
    // "ready" label so the surface is honest without inventing a version.
    if let Some(v) = staged.get("version").and_then(|v| v.as_str()) {
        return Some(v.to_string());
    }
    if let Some(img) = staged
        .get("image")
        .and_then(|i| i.get("image"))
        .and_then(|i| i.get("image"))
        .and_then(|v| v.as_str())
    {
        return Some(img.to_string());
    }
    Some("Update staged for next boot".to_string())
}

/// Side-effect interface for applying a staged update by rebooting. Tests
/// inject a recorder so the assertion doesn't reboot the machine.
pub trait Rebooter: Send + Sync {
    fn reboot(&self) -> Result<(), String>;
}

/// Production impl: `systemctl reboot`.
pub struct RealRebooter;

impl Rebooter for RealRebooter {
    fn reboot(&self) -> Result<(), String> {
        let out = Command::new("systemctl")
            .arg("reboot")
            .output()
            .map_err(|e| format!("systemctl not available: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

type StatusProvider = Arc<dyn Fn() -> UpdateStatus + Send + Sync>;
type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// A real wall-clock "last checked" stamp, e.g. `Monday, 4 July 2026 14:05`.
fn now_stamp() -> String {
    let date = os_actions::now_date().unwrap_or_default();
    let time = os_actions::now_clock(true).unwrap_or_default();
    format!("{date} {time}").trim().to_string()
}

pub struct UpdatesPane {
    status: UpdateStatus,
    prefs: UpdatePrefs,
    last_prefs: UpdatePrefs,
    status_provider: StatusProvider,
    clock: Clock,
    rebooter: Arc<dyn Rebooter>,
    dispatched_restart: bool,
    needs_seed: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for UpdatesPane {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdatesPane {
    pub fn new() -> Self {
        let status_provider: StatusProvider = Arc::new(UpdateStatus::from_system);
        let status = status_provider();
        Self {
            status,
            prefs: SystemPrefs::load().updates,
            last_prefs: UpdatePrefs::default(),
            status_provider,
            clock: Arc::new(now_stamp),
            rebooter: Arc::new(RealRebooter),
            dispatched_restart: false,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test constructor: inject the status provider, the clock, the rebooter,
    /// and the starting preferences so the actions are deterministic.
    pub fn with_parts(
        status: UpdateStatus,
        prefs: UpdatePrefs,
        status_provider: StatusProvider,
        clock: Clock,
        rebooter: Arc<dyn Rebooter>,
    ) -> Self {
        Self {
            status,
            prefs: prefs.clone(),
            last_prefs: prefs,
            status_provider,
            clock,
            rebooter,
            dispatched_restart: false,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn status(&self) -> &UpdateStatus {
        &self.status
    }

    pub fn prefs(&self) -> &UpdatePrefs {
        &self.prefs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // Status card — current + staged + the primary action.
        let action: El = match &self.status.staged_version {
            Some(staged) => Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(
                        Text::new(&format!("Update ready: {staged}")).with_size(13.0),
                    ))
                    .push(Box::new(
                        Text::new("Restart to apply the staged image.")
                            .with_size(11.0)
                            .with_color(ui::Theme::dark().text_dim),
                    ))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(Button::new(
                                "settings.updates.restart",
                                "\u{f0709}  Restart & update",
                                Some(RESTART_KEY),
                            )))
                            .push(Box::new(Spacer::new())),
                    )),
            ),
            None => Box::new(Text::new("Your system is up to date.").with_size(13.0)),
        };
        let status_card = card("Software updates", action);

        let detail = card(
            "Details",
            rows(vec![
                value_row("Current version", &self.status.current_version),
                value_row("Last checked", &self.prefs.last_checked),
                setting_row(
                    "Release channel",
                    Box::new(
                        Dropdown::new(
                            CHANNELS.iter().map(|c| c.to_string()).collect(),
                            CHANNEL_KEY,
                        )
                        .with_id("updates.channel")
                        .with_placeholder(&self.prefs.channel),
                    ),
                ),
                setting_row(
                    "Automatic updates",
                    Box::new(Toggle::new("settings.updates.auto", AUTO_KEY).with_id("updates.auto")),
                ),
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "settings.updates.check",
                            "\u{f0450}  Check for updates",
                            Some(CHECK_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ),
            ]),
        );

        let note = card(
            "How updates work",
            rows(vec![caption(
                "qOS updates are atomic: a new image is staged and applied on \
                 restart, so an update never half-installs. If something is \
                 wrong you can roll back to the previous image at boot.",
            )]),
        );

        crate::widgets::cards(vec![status_card, detail, note])
    }

    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.updates = self.prefs.clone();
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "update_prefs_save", error = %e, "failed to persist update prefs");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(UpdatesPane::new())
}

impl Widget for UpdatesPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(root, CHANNEL_KEY, self.prefs.channel.as_str());
            let _ = state.put(root, AUTO_KEY, automerge::ScalarValue::Boolean(self.prefs.auto));
            let _ = state.put(root, CHECK_KEY, automerge::ScalarValue::Boolean(false));
            let _ = state.put(root, RESTART_KEY, automerge::ScalarValue::Boolean(false));
            self.last_prefs = self.prefs.clone();
            self.needs_seed = false;
        }

        // Check for updates → re-query the system + stamp the time + persist.
        if read_bool(state, root, CHECK_KEY) {
            let _ = state.put(root, CHECK_KEY, automerge::ScalarValue::Boolean(false));
            self.status = (self.status_provider)();
            self.prefs.last_checked = (self.clock)();
            self.last_prefs.last_checked = self.prefs.last_checked.clone();
            self.persist();
            self.structure_dirty = true;
        }

        // Restart & update → reboot into the staged image (dedup).
        if read_bool(state, root, RESTART_KEY) {
            let _ = state.put(root, RESTART_KEY, automerge::ScalarValue::Boolean(false));
            if !self.dispatched_restart {
                self.dispatched_restart = true;
                if let Err(e) = self.rebooter.reboot() {
                    warn!(target: "qos::settings", op = "reboot", error = %e, "restart & update failed");
                }
            }
        }

        // Release channel → persist on change.
        if let Some(ch) = read_str(state, root, CHANNEL_KEY) {
            if CHANNELS.contains(&ch.as_str()) && ch != self.last_prefs.channel {
                self.last_prefs.channel = ch.clone();
                self.prefs.channel = ch;
                self.persist();
            }
        }

        // Automatic updates → persist on change.
        if let Some(b) = read_bool_opt(state, root, AUTO_KEY) {
            if b != self.last_prefs.auto {
                self.last_prefs.auto = b;
                self.prefs.auto = b;
                self.persist();
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
        "UpdatesPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }
}

/// Read a bool at `key`, treating absent/non-bool as `false`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    read_bool_opt(state, root, key).unwrap_or(false)
}

fn read_bool_opt(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingRebooter {
        calls: Mutex<u32>,
    }
    impl Rebooter for RecordingRebooter {
        fn reboot(&self) -> Result<(), String> {
            *self.calls.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn pane_with(
        staged: Option<&str>,
        rebooter: Arc<RecordingRebooter>,
    ) -> UpdatesPane {
        let status = UpdateStatus {
            current_version: "qOS 2026.06".into(),
            staged_version: staged.map(|s| s.to_string()),
        };
        let requeried = UpdateStatus {
            current_version: "qOS 2026.06".into(),
            staged_version: Some("qOS 2026.07".into()),
        };
        UpdatesPane::with_parts(
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            rebooter,
        )
    }

    #[test]
    fn parses_pretty_name() {
        assert_eq!(
            parse_pretty_name("PRETTY_NAME=\"qOS 42\"\n").as_deref(),
            Some("qOS 42")
        );
    }

    #[test]
    fn parse_bootc_staged_detects_and_ignores() {
        let staged = r#"{"status":{"staged":{"version":"qOS 2026.07"},"booted":{"version":"qOS 2026.06"}}}"#;
        assert_eq!(parse_bootc_staged(staged).as_deref(), Some("qOS 2026.07"));

        let none = r#"{"status":{"staged":null,"booted":{"version":"qOS 2026.06"}}}"#;
        assert_eq!(parse_bootc_staged(none), None);

        let image = r#"{"status":{"staged":{"image":{"image":{"image":"quay.io/qos/qos:latest"}}}}}"#;
        assert_eq!(
            parse_bootc_staged(image).as_deref(),
            Some("quay.io/qos/qos:latest")
        );
    }

    /// Check for updates re-queries the system and stamps a real
    /// last-checked time — proving the button acts, not no-ops.
    #[test]
    fn check_requeries_and_stamps_last_checked() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_check");
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(None, reb);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().last_checked, "Never");
        assert_eq!(pane.status().staged_version, None);
        // Press Check.
        let _ = doc.put(&root, CHECK_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().last_checked, "CHECKED-NOW", "timestamp updated");
        assert_eq!(
            pane.status().staged_version.as_deref(),
            Some("qOS 2026.07"),
            "the re-query result landed"
        );
        assert!(pane.take_structure_dirty(), "check refreshes the view");
    }

    /// Restart & update reboots exactly once via the injected rebooter.
    #[test]
    fn restart_reboots_once() {
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(Some("qOS 2026.07"), reb.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, RESTART_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        // A second stray press must not reboot again.
        let _ = doc.put(&root, RESTART_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(*reb.calls.lock().unwrap(), 1);
    }

    /// Changing the release channel is adopted (a live control), and only a
    /// known channel is accepted.
    #[test]
    fn channel_change_is_adopted_and_validated() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("updates_channel");
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(None, reb);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, CHANNEL_KEY, "Nightly");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().channel, "Nightly");
        // An unknown value is ignored.
        let _ = doc.put(&root, CHANNEL_KEY, "Bogus");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().channel, "Nightly");
    }
}

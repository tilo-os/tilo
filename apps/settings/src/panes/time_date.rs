//! Time & Date pane — clock, time zone, and date/time display format.
//!
//! The clock-format controls here are a *local* view onto the same locale
//! concepts the Region & Formats pane owns globally; this pane focuses on
//! the clock surface (automatic time, 24-hour clock, time zone) while
//! Region owns the system-wide currency/date/measurement contract.
//!
//! ## Live vs. sample
//!
//! - **Current time / date** are read from the real wall clock via
//!   `os_actions::time::{now_clock, now_date}` at build time, so the panel
//!   shows the actual time (formatted per the 24-hour toggle), not a frozen
//!   string.
//! - **Time zone** is read from `/etc/timezone` (or the `/etc/localtime`
//!   symlink target), falling back to a sample.
//! - **Set time automatically** drives `timedatectl set-ntp` through
//!   `os_actions::set_ntp`; its initial state is sampled via
//!   `os_actions::get_ntp`. The setter needs privilege, so a denial is
//!   logged rather than surfaced as a crash.
//! - **24-hour clock** is a display preference (re-renders the clock); it
//!   carries a CRDT key the rest of the system can read.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::region::{RegionSettings, TimeStyle};
use crate::widgets::{caption, card, rows, setting_row, value_row};

pub const AUTO_TIME_KEY: &str = "time.auto";
pub const H24_KEY: &str = "time.h24";

/// Side-effect interface for the automatic-time (NTP) toggle. Production
/// forwards to `os_actions::set_ntp` (`timedatectl set-ntp`); tests inject a
/// recorder so the "auto → set-ntp" path is asserted without shelling out.
pub trait NtpControl: Send + Sync {
    fn set_ntp(&self, enabled: bool) -> Result<(), String>;
}

/// Production impl — `timedatectl set-ntp`.
pub struct RealNtp;

impl NtpControl for RealNtp {
    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        os_actions::set_ntp(enabled)
    }
}

/// Sink the clock-format toggle persists through. In production it writes the
/// shared locale (`locale.time_format`) that Region & Formats owns, so the two
/// panes agree and the choice survives a restart; tests inject a no-op so the
/// hermetic unit tests never touch the real `settings.json`.
type FormatSink = Arc<dyn Fn(bool) + Send + Sync>;

/// The production format sink: fold the 24-hour choice into the shared locale.
fn persist_time_format(h24: bool) {
    let mut region = RegionSettings::load();
    region.locale.time_format = if h24 { TimeStyle::H24 } else { TimeStyle::H12 };
    if let Err(e) = region.save() {
        warn!(target: "qos::settings", op = "time_format_save", error = %e, "failed to persist clock format");
    }
}

/// Read the configured time zone from `/etc/timezone`, falling back to a
/// sample so the pane is never blank.
fn timezone() -> String {
    for p in ["/etc/timezone"] {
        if let Ok(s) = fs::read_to_string(Path::new(p)) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    // `/etc/localtime` is usually a symlink into zoneinfo; derive the zone
    // from its target when `/etc/timezone` is absent (common on Fedora).
    if let Ok(target) = fs::read_link("/etc/localtime")
        && let Some(idx) = target.to_string_lossy().find("zoneinfo/")
    {
        return target.to_string_lossy()[idx + "zoneinfo/".len()..].to_string();
    }
    "Europe/Amsterdam".to_string()
}

/// Time & Date pane. Stateful so it can drive `set-ntp` on toggle and
/// re-render the live clock when the 24-hour preference flips.
pub struct TimeDatePane {
    ntp: Arc<dyn NtpControl>,
    persist_format: FormatSink,
    timezone: String,
    h24: bool,
    initial_auto: bool,
    last_auto: bool,
    last_h24: bool,
    needs_seed: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for TimeDatePane {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeDatePane {
    pub fn new() -> Self {
        let initial_auto = os_actions::get_ntp().unwrap_or(true);
        // The clock format is the same system-wide preference Region & Formats
        // owns (`locale.time_format`), so the two panes agree and the choice
        // survives a restart. Reading it here — rather than defaulting to 24h —
        // makes the toggle rest on the user's actual setting.
        let h24 = matches!(RegionSettings::load().locale.time_format, TimeStyle::H24);
        Self {
            ntp: Arc::new(RealNtp),
            persist_format: Arc::new(persist_time_format),
            timezone: timezone(),
            h24,
            initial_auto,
            last_auto: initial_auto,
            last_h24: h24,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test constructor: inject the NTP control and starting auto-state. The
    /// clock format starts at 24-hour and persists through a no-op sink, so the
    /// hermetic unit tests neither read the host's locale nor write to disk.
    pub fn with_ntp(ntp: Arc<dyn NtpControl>, initial_auto: bool) -> Self {
        Self {
            ntp,
            persist_format: Arc::new(|_| {}),
            timezone: timezone(),
            h24: true,
            initial_auto,
            last_auto: initial_auto,
            last_h24: true,
            needs_seed: true,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let now = os_actions::now_clock(self.h24).unwrap_or_else(|_| "—".to_string());
        let today = os_actions::now_date().unwrap_or_else(|_| "—".to_string());

        let clock = card(
            "Clock",
            rows(vec![
                setting_row(
                    "Set time automatically",
                    Box::new(Toggle::new("settings.time.auto", AUTO_TIME_KEY).with_id("time.auto")),
                ),
                setting_row(
                    "24-hour clock",
                    Box::new(Toggle::new("settings.time.h24", H24_KEY).with_id("time.h24")),
                ),
                value_row("Time zone", &self.timezone),
                value_row("Current time", &now),
                value_row("Current date", &today),
            ]),
        );

        let formats = card(
            "Display",
            rows(vec![
                value_row("Clock format", if self.h24 { "24-hour" } else { "12-hour" }),
                caption(
                    "Date and number formats are set system-wide in \
                     Region & Formats; this pane shows the clock surface.",
                ),
            ]),
        );

        crate::widgets::cards(vec![clock, formats])
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(TimeDatePane::new())
}

impl Widget for TimeDatePane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            if state.get(root, AUTO_TIME_KEY).ok().flatten().is_none() {
                let _ = state.put(
                    root,
                    AUTO_TIME_KEY,
                    automerge::ScalarValue::Boolean(self.initial_auto),
                );
            }
            if state.get(root, H24_KEY).ok().flatten().is_none() {
                let _ = state.put(root, H24_KEY, automerge::ScalarValue::Boolean(self.h24));
            }
            self.needs_seed = false;
        }

        // Automatic-time toggle → timedatectl set-ntp.
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, AUTO_TIME_KEY)
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
            && *b != self.last_auto
        {
            self.last_auto = *b;
            if let Err(e) = self.ntp.set_ntp(*b) {
                warn!(target: "qos::settings", op = "set_ntp", error = %e, "time pane dispatch failed");
            }
        }

        // 24-hour preference → re-render the live clock AND persist to the
        // shared locale so Region & Formats and every consumer stay in sync
        // and the choice survives a restart.
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, H24_KEY)
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
            && *b != self.last_h24
        {
            self.last_h24 = *b;
            self.h24 = *b;
            self.structure_dirty = true;
            (self.persist_format)(*b);
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
        "TimeDatePane"
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
    use std::collections::HashMap;
    use std::sync::Mutex;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    #[derive(Default)]
    struct RecordingNtp {
        calls: Mutex<Vec<bool>>,
    }
    impl NtpControl for RecordingNtp {
        fn set_ntp(&self, enabled: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(enabled);
            Ok(())
        }
    }

    /// Toggling "Set time automatically" forwards to `set_ntp` exactly once
    /// per change, with the new value — the real dispatch path.
    #[test]
    fn auto_time_toggle_invokes_set_ntp() {
        let ntp = Arc::new(RecordingNtp::default());
        // Start with auto=true (the seed), then flip off.
        let mut pane = TimeDatePane::with_ntp(ntp.clone(), true);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(
            ntp.calls.lock().unwrap().is_empty(),
            "seed must not dispatch"
        );
        let _ = doc.put(&root, AUTO_TIME_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root); // idempotent at rest
        assert_eq!(ntp.calls.lock().unwrap().as_slice(), [false]);
        // Flip back on.
        let _ = doc.put(&root, AUTO_TIME_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(ntp.calls.lock().unwrap().as_slice(), [false, true]);
    }

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn timezone_is_never_empty() {
        assert!(!timezone().is_empty());
    }

    /// Flipping the 24-hour key re-renders the clock (structure dirty), updates
    /// the pane's format flag, and forwards the new value to the format sink —
    /// proof the toggle is live and drives the persistence path (asserted
    /// against disk in the `prefs_persistence` integration test).
    #[test]
    fn h24_toggle_reformats_clock_and_persists_format() {
        let sunk: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_calls = sunk.clone();
        // Build the hermetic test pane (24h start, no disk) but swap in a
        // recording format sink so we can prove the toggle drives persistence.
        let mut pane = TimeDatePane::with_ntp(Arc::new(RecordingNtp::default()), true);
        pane.persist_format = Arc::new(move |b| sink_calls.lock().unwrap().push(b));
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(sunk.lock().unwrap().is_empty(), "seed must not persist");
        // Seed wrote h24=true; flip to false.
        let _ = doc.put(&root, H24_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);
        assert!(!pane.h24, "pane adopts the new 24-hour preference");
        assert!(
            pane.take_structure_dirty(),
            "changing the clock format requests a re-render"
        );
        assert_eq!(
            sunk.lock().unwrap().as_slice(),
            [false],
            "the new clock format is forwarded to the persistence sink once"
        );
    }
}

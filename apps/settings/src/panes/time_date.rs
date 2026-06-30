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

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{card, caption, rows, setting_row, value_row};

pub const AUTO_TIME_KEY: &str = "time.auto";
pub const H24_KEY: &str = "time.h24";

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
    if let Ok(target) = fs::read_link("/etc/localtime") {
        if let Some(idx) = target.to_string_lossy().find("zoneinfo/") {
            return target.to_string_lossy()[idx + "zoneinfo/".len()..].to_string();
        }
    }
    "Europe/Amsterdam".to_string()
}

/// Time & Date pane. Stateful so it can drive `set-ntp` on toggle and
/// re-render the live clock when the 24-hour preference flips.
pub struct TimeDatePane {
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
        Self {
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
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, AUTO_TIME_KEY) {
            if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                if *b != self.last_auto {
                    self.last_auto = *b;
                    if let Err(e) = os_actions::set_ntp(*b) {
                        warn!(target: "qos::settings", op = "set_ntp", error = %e, "time pane dispatch failed");
                    }
                }
            }
        }

        // 24-hour preference → re-render the live clock in the new format.
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, H24_KEY) {
            if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                if *b != self.last_h24 {
                    self.last_h24 = *b;
                    self.h24 = *b;
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
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

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

    /// Flipping the 24-hour key re-renders the clock (structure dirty) and
    /// updates the pane's format flag — proof the toggle is live.
    #[test]
    fn h24_toggle_reformats_clock() {
        let mut pane = TimeDatePane::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Seed wrote h24=true; flip to false.
        let _ = doc.put(&root, H24_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);
        assert!(!pane.h24, "pane adopts the new 24-hour preference");
        assert!(
            pane.take_structure_dirty(),
            "changing the clock format requests a re-render"
        );
    }
}

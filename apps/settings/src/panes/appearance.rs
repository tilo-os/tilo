//! Appearance pane — the editor for the **shared appearance preferences**.
//!
//! This panel edits [`crate::prefs::AppearancePrefs`], the durable
//! look-and-feel section of the shared `settings.json`
//! ([`crate::region::RegionSettings::store_path`]). The OS theming layer
//! resolves those preferences into the concrete [`ui::Theme`] every app
//! draws against (see [`crate::prefs::AppearancePrefs::resolve_theme`]) — so
//! choosing a theme here is the single source of truth the rest of the
//! system follows, exactly like Region & Formats owns the locale.
//!
//! Controls:
//! - **Theme** — a named preset ([`crate::prefs::AppearancePrefs::THEMES`]).
//! - **High contrast** — overrides the palette with the WCAG-AAA track.
//! - **Reduce motion** — snaps animations to their resting state.
//! - **Text size** — a UI-wide text-scale multiplier for low-vision comfort.
//!
//! Each control is bound to a CRDT key; `sync` reads the key, folds the
//! change into the loaded prefs, and persists the shared store. On first
//! sync it seeds the controls from the loaded prefs so they rest on the
//! user's current choice.

use std::collections::HashMap;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::prefs::{AppearancePrefs, SystemPrefs};
use crate::widgets::{card, choice_row, rows, setting_row_described, value_row};

pub const THEME_KEY: &str = "appearance.theme";
pub const HIGH_CONTRAST_KEY: &str = "appearance.high_contrast";
pub const REDUCE_MOTION_KEY: &str = "appearance.reduce_motion";
pub const TEXT_SIZE_KEY: &str = "appearance.text_size";
pub const UI_SCALE_KEY: &str = "appearance.ui_scale";

/// The offered global UI-scale steps as `(display label, factor)`. The factor is
/// what [`AppearancePrefs::ui_scale`] persists; it grows the whole design system
/// (spacing, radii, and type together) and composes with the per-output
/// fractional device scale. Spans the daily-driver-comfortable 90 %–150 % range.
pub const UI_SCALES: &[(&str, f64)] = &[
    ("90%", 0.90),
    ("100%", 1.0),
    ("110%", 1.10),
    ("125%", 1.25),
    ("150%", 1.50),
];

/// Map a UI-scale factor to the closest offered label (defaults to "100%").
fn ui_scale_label(scale: f64) -> &'static str {
    UI_SCALES
        .iter()
        .min_by(|(_, a), (_, b)| {
            (a - scale).abs().partial_cmp(&(b - scale).abs()).unwrap()
        })
        .map(|(label, _)| *label)
        .unwrap_or("100%")
}

/// Map a UI-scale label back to its factor, if recognised.
fn ui_scale_factor(label: &str) -> Option<f64> {
    UI_SCALES.iter().find(|(l, _)| *l == label).map(|(_, s)| *s)
}

/// The offered text sizes as `(display label, scale factor)`. The label is
/// what the dropdown shows and stores; the factor is what
/// [`AppearancePrefs::text_scale`] persists.
pub const TEXT_SIZES: &[(&str, f64)] = &[
    ("Small", 0.85),
    ("Default", 1.0),
    ("Large", 1.15),
    ("Larger", 1.30),
];

/// Map a text-scale factor to the closest offered label (defaults to
/// "Default"). Shared with the Accessibility pane, which offers the same
/// text-size control over the same [`AppearancePrefs::text_scale`] field.
pub fn text_size_label(scale: f64) -> &'static str {
    TEXT_SIZES
        .iter()
        .min_by(|(_, a), (_, b)| {
            (a - scale).abs().partial_cmp(&(b - scale).abs()).unwrap()
        })
        .map(|(label, _)| *label)
        .unwrap_or("Default")
}

/// Map a text-size label back to its scale factor, if recognised. Shared with
/// the Accessibility pane.
pub fn text_size_scale(label: &str) -> Option<f64> {
    TEXT_SIZES
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, s)| *s)
}

pub struct AppearancePane {
    prefs: AppearancePrefs,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for AppearancePane {
    fn default() -> Self {
        Self::new()
    }
}

impl AppearancePane {
    pub fn new() -> Self {
        Self::with_prefs(SystemPrefs::load().appearance)
    }

    /// Construct from explicit preferences — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_prefs(prefs: AppearancePrefs) -> Self {
        Self {
            prefs,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn prefs(&self) -> &AppearancePrefs {
        &self.prefs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let theme_opts: Vec<String> =
            AppearancePrefs::THEMES.iter().map(|t| t.to_string()).collect();
        let theme = Dropdown::new(theme_opts, THEME_KEY)
            .with_id("appearance.theme")
            .with_placeholder(&self.prefs.theme);

        let size_opts: Vec<String> = TEXT_SIZES.iter().map(|(l, _)| l.to_string()).collect();
        let size = Dropdown::new(size_opts, TEXT_SIZE_KEY)
            .with_id("appearance.text_size")
            .with_placeholder(text_size_label(self.prefs.text_scale));

        let ui_scale_opts: Vec<String> = UI_SCALES.iter().map(|(l, _)| l.to_string()).collect();
        let ui_scale = Dropdown::new(ui_scale_opts, UI_SCALE_KEY)
            .with_id("appearance.ui_scale")
            .with_placeholder(ui_scale_label(self.prefs.ui_scale));

        let theme_card = card(
            "Theme",
            rows(vec![
                choice_row("Theme", Box::new(theme)),
                choice_row("UI scale", Box::new(ui_scale)),
                choice_row("Text size", Box::new(size)),
            ]),
        );

        let access_card = card(
            "Accessibility",
            rows(vec![
                setting_row_described(
                    "High contrast",
                    "Maximise contrast for low-vision comfort",
                    Box::new(
                        Toggle::new("settings.appearance.high_contrast", HIGH_CONTRAST_KEY)
                            .with_id("appearance.high_contrast"),
                    ),
                ),
                setting_row_described(
                    "Reduce motion",
                    "Skip animations and transitions",
                    Box::new(
                        Toggle::new("settings.appearance.reduce_motion", REDUCE_MOTION_KEY)
                            .with_id("appearance.reduce_motion"),
                    ),
                ),
            ]),
        );

        // A live summary of the theme these choices resolve to, so the effect
        // is legible before it is applied system-wide.
        let resolved = self.prefs.resolve_theme();
        let preview = card(
            "Applied theme",
            rows(vec![
                value_row("Active theme", &resolved.name),
                value_row(
                    "UI scale",
                    &format!("{:.0}%", resolved.ui_scale as f64 * 100.0),
                ),
                value_row(
                    "Text scale",
                    &format!("{:.0}%", resolved.text_scale as f64 * 100.0),
                ),
                value_row("Reduced motion", if resolved.reduce_motion { "On" } else { "Off" }),
            ]),
        );

        crate::widgets::cards(vec![theme_card, access_card, preview])
    }

    /// Persist the current prefs into the shared store, preserving other
    /// sections. Reads fresh so a change here never clobbers another pane's.
    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.appearance = self.prefs.clone();
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "appearance_save", error = %e, "failed to persist appearance prefs");
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

    fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                automerge::ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AppearancePane::new())
}

impl Widget for AppearancePane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(root, THEME_KEY, self.prefs.theme.as_str());
            let _ = state.put(root, TEXT_SIZE_KEY, text_size_label(self.prefs.text_scale));
            let _ = state.put(root, UI_SCALE_KEY, ui_scale_label(self.prefs.ui_scale));
            let _ = state.put(
                root,
                HIGH_CONTRAST_KEY,
                automerge::ScalarValue::Boolean(self.prefs.high_contrast),
            );
            let _ = state.put(
                root,
                REDUCE_MOTION_KEY,
                automerge::ScalarValue::Boolean(self.prefs.reduce_motion),
            );
            self.needs_seed = false;
        }

        let mut changed = false;

        if let Some(name) = Self::read_str(state, root, THEME_KEY) {
            if AppearancePrefs::THEMES.contains(&name.as_str()) && name != self.prefs.theme {
                self.prefs.theme = name;
                changed = true;
            }
        }
        if let Some(label) = Self::read_str(state, root, TEXT_SIZE_KEY) {
            if let Some(scale) = text_size_scale(&label) {
                if (scale - self.prefs.text_scale).abs() > f64::EPSILON {
                    self.prefs.text_scale = scale;
                    changed = true;
                }
            }
        }
        if let Some(label) = Self::read_str(state, root, UI_SCALE_KEY) {
            if let Some(factor) = ui_scale_factor(&label) {
                if (factor - self.prefs.ui_scale).abs() > f64::EPSILON {
                    self.prefs.ui_scale = factor;
                    changed = true;
                }
            }
        }
        if let Some(b) = Self::read_bool(state, root, HIGH_CONTRAST_KEY) {
            if b != self.prefs.high_contrast {
                self.prefs.high_contrast = b;
                changed = true;
            }
        }
        if let Some(b) = Self::read_bool(state, root, REDUCE_MOTION_KEY) {
            if b != self.prefs.reduce_motion {
                self.prefs.reduce_motion = b;
                changed = true;
            }
        }

        if changed {
            self.persist();
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
                        event, scale_factor, modifiers, state, path, handled, focus_target, cn,
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
        "AppearancePane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Surface the user's current appearance choice so the running Settings
    /// window re-scales and re-themes live the instant a control changes. The
    /// host [`ui::App`] (with `appearance_reconcile` enabled by `main`) polls
    /// this each frame and applies any field that differs. We hand back the
    /// *base* colour theme unscaled — the App folds in high-contrast, the UI
    /// scale, and the text scale itself, keeping that bookkeeping in one place.
    fn appearance_intent(&self) -> Option<ui::theme::AppearanceIntent> {
        Some(ui::theme::AppearanceIntent {
            base_theme: crate::prefs::theme_by_name(&self.prefs.theme),
            high_contrast: self.prefs.high_contrast,
            reduce_motion: self.prefs.reduce_motion,
            text_scale: self.prefs.text_scale as f32,
            ui_scale: self.prefs.ui_scale as f32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_writes_current_prefs_into_crdt() {
        let mut pane = AppearancePane::with_prefs(AppearancePrefs {
            theme: "Nord".into(),
            high_contrast: true,
            reduce_motion: false,
            text_scale: 1.15,
            ui_scale: 1.25,
        });
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            AppearancePane::read_str(&doc, &root, THEME_KEY).as_deref(),
            Some("Nord")
        );
        assert_eq!(
            AppearancePane::read_str(&doc, &root, TEXT_SIZE_KEY).as_deref(),
            Some("Large"),
            "1.15 seeds the 'Large' label"
        );
        assert_eq!(
            AppearancePane::read_str(&doc, &root, UI_SCALE_KEY).as_deref(),
            Some("125%"),
            "1.25 seeds the '125%' UI-scale label"
        );
        assert_eq!(
            AppearancePane::read_bool(&doc, &root, HIGH_CONTRAST_KEY),
            Some(true)
        );
    }

    #[test]
    fn changing_ui_scale_updates_prefs_and_resolves() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("appearance_uiscale");
        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // The UI-scale dropdown writes the chosen label.
        let _ = doc.put(&root, UI_SCALE_KEY, "150%");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().ui_scale, 1.50);
        // The pref folds into the resolved theme's ui_scale + scaled tokens.
        let resolved = pane.prefs().resolve_theme();
        assert_eq!(resolved.ui_scale, 1.50);
        let base_gap = ui::Theme::dark().tokens.spacing.s3;
        assert!((resolved.tokens.spacing.s3 - base_gap * 1.50).abs() < 1e-2);
    }

    #[test]
    fn ui_scale_label_and_factor_round_trip() {
        assert_eq!(ui_scale_label(1.0), "100%");
        assert_eq!(ui_scale_label(1.25), "125%");
        assert_eq!(ui_scale_label(1.50), "150%");
        assert_eq!(ui_scale_factor("90%"), Some(0.90));
        assert_eq!(ui_scale_factor("nope"), None);
    }

    #[test]
    fn appearance_intent_reflects_ui_scale_and_text_size_for_live_apply() {
        let pane = AppearancePane::with_prefs(AppearancePrefs {
            theme: "Nord".into(),
            high_contrast: false,
            reduce_motion: true,
            text_scale: 1.15,
            ui_scale: 1.25,
        });
        let intent = ui::widget::Widget::appearance_intent(&pane)
            .expect("the appearance pane always expresses an intent");
        assert_eq!(intent.base_theme.name, "Nord", "base theme is handed back unscaled");
        assert_eq!(intent.ui_scale, 1.25);
        assert_eq!(intent.text_scale, 1.15);
        assert!(intent.reduce_motion);
        assert!(!intent.high_contrast);
    }

    #[test]
    fn changing_theme_key_updates_and_resolves() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("appearance_theme");
        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Dropdown writes the chosen theme name.
        let _ = doc.put(&root, THEME_KEY, "Gruvbox");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().theme, "Gruvbox");
        // The pref resolves to the real theme constructor.
        assert_eq!(pane.prefs().resolve_theme().name, "Gruvbox");
    }

    #[test]
    fn changing_text_size_and_toggles_updates_prefs() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("appearance_textsize");
        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, TEXT_SIZE_KEY, "Larger");
        let _ = doc.put(&root, HIGH_CONTRAST_KEY, automerge::ScalarValue::Boolean(true));
        let _ = doc.put(&root, REDUCE_MOTION_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().text_scale, 1.30);
        assert!(pane.prefs().high_contrast);
        assert!(pane.prefs().reduce_motion);
        // high_contrast wins the resolved palette.
        assert_eq!(pane.prefs().resolve_theme().name, "High Contrast");
    }

    #[test]
    fn unknown_theme_value_is_ignored() {
        let mut pane = AppearancePane::with_prefs(AppearancePrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, THEME_KEY, "not-a-theme");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.prefs().theme, "Dark", "stayed on the default");
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
    fn text_size_label_and_scale_round_trip() {
        assert_eq!(text_size_label(1.0), "Default");
        assert_eq!(text_size_label(1.30), "Larger");
        assert_eq!(text_size_scale("Small"), Some(0.85));
        assert_eq!(text_size_scale("Nonsense"), None);
    }
}

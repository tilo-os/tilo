//! `SettingsApp` — the top-level settings hub.
//!
//! ## Layout
//!
//! On a wide window the app is a two-pane split: a categorized sidebar of
//! [`Category`] entries on the left, and the selected category's content
//! pane on the right. On a narrow window (below [`NARROW_BREAKPOINT_PX`])
//! it collapses to a single pane — the sidebar shows alone until the user
//! taps a category, which "pushes" the panel in over the full width with a
//! back affordance to return to the list. This mirrors the Adwaita
//! leaflet/overlay-flap behaviour.
//!
//! ## How responsiveness works without a viewport hook
//!
//! The `Widget` trait's `build_layout` doesn't receive the window size, so
//! the app reads its own measured width from the *previous* frame
//! ([`SettingsApp::last_width`], set in `apply_layout`) to choose the
//! composition. The one-frame lag is invisible in practice and fully
//! deterministic under `--shots` (the offscreen renderer fixes the
//! viewport before the first `build_layout`, and the app seeds
//! `last_width` from a render hint so the very first frame is already
//! correct — see [`SettingsApp::with_width_hint`]).
//!
//! ## State
//!
//! The composed child tree is rebuilt at the top of every `build_layout`
//! so a category switch or a narrow/wide transition shows up next frame
//! without a separate message bus — the same pattern the sibling native
//! apps use. Category selection and the narrow drill-in flag live on the
//! widget; everything a panel persists lives in that panel's own store.

use crate::categories::{Category, Section};
use crate::panes;
use ui::widget::{Button, Column, Container, Row, Scrollable, Text, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// Below this measured content width (logical px) the app uses the
/// single-pane narrow layout.
pub const NARROW_BREAKPOINT_PX: f32 = 720.0;

/// CRDT key the narrow-mode "back" affordance writes to. Reading it true
/// in `sync` pops the drill-in back to the category list.
pub const BACK_KEY: &str = "settings.nav.back";

pub struct SettingsApp {
    /// Currently-selected sidebar category.
    selected: Category,
    /// In narrow mode, whether the panel is "pushed in" (showing the
    /// selected category) vs. the category list. Ignored in wide mode.
    drilled_in: bool,
    /// Last measured content width, used to pick the responsive layout.
    last_width: f32,
    rect: vello::kurbo::Rect,
    /// Composed child widgets, rebuilt each `build_layout`. One entry in
    /// narrow mode (the active pane); two in wide mode (sidebar, content).
    /// They're assembled into a non-wrapping flex row with explicit widths
    /// in `build_layout` so the sidebar stays a fixed fraction and the
    /// content fills the rest — `ui`'s `Row` widget wraps, which can't
    /// express a stable two-pane split.
    composed: Vec<Box<dyn Widget>>,
}

/// Fraction of the width the sidebar takes in the wide two-pane layout.
const SIDEBAR_FRACTION: f32 = 0.24;

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsApp {
    pub fn new() -> Self {
        Self {
            selected: Category::DEFAULT,
            drilled_in: false,
            // Assume wide until the first measured frame proves otherwise.
            last_width: 1200.0,
            rect: vello::kurbo::Rect::ZERO,
            composed: Vec::new(),
        }
    }

    /// Construct with a specific category pre-selected. Used by tests and
    /// future deep-linking (`settings://network`).
    pub fn with_selected(category: Category) -> Self {
        let mut s = Self::new();
        s.selected = category;
        s
    }

    /// Seed the responsive layout with a known width before the first
    /// frame — the offscreen `--shots` renderer calls this so the cold
    /// screenshot is already in the right layout.
    pub fn with_width_hint(mut self, width: f32) -> Self {
        self.last_width = width;
        // In narrow mode the cold screenshot is most useful drilled into
        // the selected panel (that's the interesting single-pane state).
        self.drilled_in = width < NARROW_BREAKPOINT_PX;
        self
    }

    pub fn selected(&self) -> Category {
        self.selected
    }

    pub fn select(&mut self, category: Category) {
        self.selected = category;
        self.drilled_in = true;
    }

    /// Whether the current frame uses the narrow single-pane layout.
    pub fn is_narrow(&self) -> bool {
        self.last_width < NARROW_BREAKPOINT_PX
    }

    pub fn drilled_in(&self) -> bool {
        self.drilled_in
    }

    /// The header shown above every content pane: the category icon +
    /// label + one-line subtitle. In narrow mode it leads with a back
    /// affordance that pops to the category list.
    fn build_pane_header(&self, with_back: bool) -> Box<dyn Widget> {
        // The header reuses the proven card structure: a background
        // `Container` whose inner column carries the (optional) back row,
        // the title, and the subtitle. Putting the lines inside a
        // background `Container` is what makes them draw reliably — bare
        // text at the head of a plain column can be skipped by the cached
        // draw path.
        let mut inner = Column::new().with_no_padding();

        if with_back {
            inner = inner.push(Box::new(
                Row::new().push(Box::new(Button::new(
                    "settings.nav.back",
                    "\u{f0141}  Back",
                    Some(BACK_KEY),
                ))),
            ));
        }

        inner = inner
            .push(Box::new(
                Row::new().push(Box::new(
                    Text::new(self.selected.label()).with_size(22.0),
                )),
            ))
            .push(Box::new(
                Row::new().push(Box::new(
                    Text::new(self.selected.subtitle())
                        .with_size(12.0)
                        .with_color(ui::Theme::dark().text_dim),
                )),
            ));

        Box::new(Container::new().with_background().with_child(Box::new(inner)))
    }

    /// Build the content pane: header + the selected category's body.
    ///
    /// The body is a vertical stack of cards. We deliberately use a plain
    /// `Column` (filling its pane) rather than a `Scrollable` wrapper: in
    /// the retained-mode layout a `Scrollable` whose child sizes itself
    /// intrinsically collapses to a zero-height viewport unless the host
    /// drives its measure pass, which the standalone settings window does
    /// not. The panels fit the window at the sizes they ship; making them
    /// scroll is a follow-up that belongs in `ui` (a measured
    /// `Scrollable` viewport), noted in the growth ledger.
    fn build_content(&self, with_back: bool) -> Box<dyn Widget> {
        let header = self.build_pane_header(with_back);
        let body = self.build_pane_body();
        let inner = Column::new()
            .with_intrinsic_height()
            .push(header)
            .push(body);
        Box::new(Container::new().filling().with_child(Box::new(inner)))
    }

    /// Dispatch to the per-category pane body.
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            Category::Network => panes::network::build(),
            Category::Bluetooth => panes::bluetooth::build(),
            Category::Sound => panes::sound::build(),
            Category::Displays => panes::display::build(),
            Category::MouseKeyboard => panes::mouse_keyboard::build(),
            Category::TimeDate => panes::time_date::build(),
            Category::Users => panes::users::build(),
            Category::Printers => panes::printers::build(),
            Category::ExternalAccounts => panes::accounts::build(),
            Category::RegionFormats => panes::region::build(),
            Category::Permissions => panes::permissions::build(),
            Category::Updates => panes::updates::build(),
            Category::About => panes::about::build(),
        }
    }

    /// The categorized sidebar: app title, then one labelled section per
    /// [`Section`], each listing its categories as buttons. Scrollable so
    /// the full list reaches on short windows.
    fn build_sidebar(&self) -> Box<dyn Widget> {
        let mut col = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Text::new("Settings").with_size(20.0)));

        for section in Section::ALL {
            col = col.push(Box::new(
                Text::new(&section.label().to_uppercase())
                    .with_size(10.0)
                    .with_color(ui::Theme::dark().text_dim),
            ));
            for category in Category::in_section(*section) {
                let id = format!("settings.sidebar.{}", category.slug());
                let key = format!("settings.select.{}", category.slug());
                let marker = if category == self.selected && !self.is_narrow() {
                    "\u{2022} " // bullet marks the active row in wide mode
                } else {
                    "  "
                };
                let label = format!("{}{}  {}", marker, category.glyph(), category.label());
                col = col.push(Box::new(Button::new(&id, &label, Some(&key))));
            }
        }

        let scroll = Scrollable::new_vertical().with_child(Box::new(col));
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
    }

    /// Build this frame's child widgets, picking wide vs. narrow based on
    /// the last measured width. Wide returns `[sidebar, content]`; narrow
    /// returns a single pane (the category list, or the drilled-in panel).
    fn build_children(&self) -> Vec<Box<dyn Widget>> {
        if self.is_narrow() {
            if self.drilled_in {
                vec![self.build_content(true)]
            } else {
                vec![self.build_sidebar()]
            }
        } else {
            vec![self.build_sidebar(), self.build_content(false)]
        }
    }
}

impl Widget for SettingsApp {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        use automerge::{transaction::Transactable, ReadDoc, Value};

        // Narrow-mode "back" press pops the drill-in.
        let back_pressed = matches!(
            state.get(path, BACK_KEY),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        );
        if back_pressed {
            let _ = state.put(path, BACK_KEY, automerge::ScalarValue::Boolean(false));
            self.drilled_in = false;
        }

        // Category selection: each sidebar button writes
        // `settings.select.<slug>` = true. The first one seen wins.
        for category in Category::ALL {
            let key = format!("settings.select.{}", category.slug());
            let pressed = matches!(
                state.get(path, key.as_str()),
                Ok(Some((Value::Scalar(s), _)))
                    if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if pressed {
                let _ = state.put(path, key.as_str(), automerge::ScalarValue::Boolean(false));
                self.select(*category);
            }
        }

        for c in &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let mut children = self.build_children();
        let two_pane = children.len() == 2;

        let mut child_nodes: Vec<taffy::NodeId> = Vec::with_capacity(children.len());
        for c in &mut children {
            child_nodes.push(c.build_layout(taffy, theme, node_map));
        }
        self.composed = children;

        if two_pane {
            // Pin explicit widths so the sidebar holds a fixed fraction and
            // the content fills the rest. This is the `ResizableSplit`
            // technique — overwrite child size + lock flex — applied inline
            // because our two children are dynamic `Box<dyn Widget>`s.
            let widths = [SIDEBAR_FRACTION, 1.0 - SIDEBAR_FRACTION];
            for (node, frac) in child_nodes.iter().zip(widths) {
                if let Ok(mut style) = taffy.style(*node).cloned() {
                    style.size = taffy::Size {
                        width: taffy::Dimension::percent(frac),
                        height: taffy::Dimension::percent(1.0),
                    };
                    style.flex_grow = 0.0;
                    style.flex_shrink = 0.0;
                    let _ = taffy.set_style(*node, style);
                }
            }
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
            // Wide: row (sidebar | content), no wrap. Narrow: single child,
            // direction is immaterial.
            flex_direction: taffy::FlexDirection::Row,
            flex_wrap: taffy::FlexWrap::NoWrap,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &child_nodes).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        // Record width for the next frame's responsive decision.
        self.last_width = self.rect.width() as f32;
        if let Ok(children) = taffy.children(node) {
            for (c, child_node) in self.composed.iter_mut().zip(children) {
                c.apply_layout(
                    taffy,
                    child_node,
                    vello::kurbo::Point::new(self.rect.x0, self.rect.y0),
                );
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        for c in &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        if let Ok(children) = taffy.children(node_id) {
            for (c, child_node) in self.composed.iter_mut().zip(children) {
                c.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    child_node,
                    taffy,
                );
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.iter().map(|c| c.as_ref() as &dyn Widget).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn name(&self) -> &str {
        "SettingsApp"
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
}

impl AppWidget for SettingsApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        "Settings"
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{f0493}") // cog
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};

    fn layout_at(app: &mut SettingsApp, w: f32, h: f32) {
        let theme = ui::Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(w),
                        height: taffy::Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
    }

    #[test]
    fn new_starts_on_default_category() {
        assert_eq!(SettingsApp::new().selected(), Category::DEFAULT);
    }

    #[test]
    fn with_selected_overrides_default() {
        assert_eq!(
            SettingsApp::with_selected(Category::About).selected(),
            Category::About
        );
    }

    #[test]
    fn select_changes_category_and_drills_in() {
        let mut app = SettingsApp::new();
        app.select(Category::Users);
        assert_eq!(app.selected(), Category::Users);
        assert!(app.drilled_in());
    }

    #[test]
    fn width_hint_sets_narrow_and_drilled_in() {
        let app = SettingsApp::new().with_width_hint(420.0);
        assert!(app.is_narrow());
        assert!(app.drilled_in());
        let wide = SettingsApp::new().with_width_hint(1200.0);
        assert!(!wide.is_narrow());
    }

    #[test]
    fn layout_records_width_for_responsive_decision() {
        let mut app = SettingsApp::new();
        layout_at(&mut app, 480.0, 700.0);
        assert!(app.is_narrow(), "480px should be narrow");
        let mut app2 = SettingsApp::new();
        layout_at(&mut app2, 1200.0, 700.0);
        assert!(!app2.is_narrow(), "1200px should be wide");
    }

    #[test]
    fn wide_and_narrow_both_lay_out_without_panicking() {
        let mut wide = SettingsApp::with_selected(Category::ExternalAccounts);
        layout_at(&mut wide, 1200.0, 720.0);
        assert!(wide.get_rect().width() > 0.0);

        let mut narrow = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(420.0);
        layout_at(&mut narrow, 420.0, 800.0);
        assert!(narrow.get_rect().width() > 0.0);
    }
}

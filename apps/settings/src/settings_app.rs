//! `SettingsApp` — the top-level settings widget.
//!
//! Two-column layout: a 200 px sidebar on the left listing every
//! `Category` as a button, and a content pane on the right that swaps
//! based on the currently-selected category. The widget defers all
//! five trait passes (`sync`, `build_layout`, `apply_layout`, `draw`,
//! `handle_event`) to a composed child tree built on demand — same
//! pattern as `FileExplorerWidget`. Composition is regenerated each
//! `build_layout` so a category switch shows up in the next frame
//! without piping events through a custom message bus.

use crate::categories::Category;
use crate::panes;
use ui::widget::{Button, Column, Container, Row, Text, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// Fixed sidebar width in logical pixels. Picked to match the spec
/// (200 px) and stays slightly wider than the file-explorer sidebar
/// because the labels here are longer ("Collaboration" being the
/// worst-case word).
pub const SIDEBAR_WIDTH_PX: f32 = 200.0;

pub struct SettingsApp {
    /// Currently-selected sidebar category. Drives which pane the
    /// content area shows.
    selected: Category,
    rect: vello::kurbo::Rect,
    /// Composed child widget tree, rebuilt at the top of every
    /// `build_layout`. Same pattern as `FileExplorerWidget`.
    composed: Option<Box<dyn Widget>>,
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsApp {
    pub fn new() -> Self {
        Self {
            // Default to Display — it's the first row in the sidebar and
            // the category most users will be looking for on first launch.
            selected: Category::Display,
            rect: vello::kurbo::Rect::ZERO,
            composed: None,
        }
    }

    /// Construct a SettingsApp with a specific category pre-selected.
    /// Used by tests; future shell-side deep-linking
    /// (e.g. `settings://wifi`) will rely on the same entry point.
    pub fn with_selected(category: Category) -> Self {
        let mut s = Self::new();
        s.selected = category;
        s
    }

    /// The currently-displayed category.
    pub fn selected(&self) -> Category {
        self.selected
    }

    /// Switch which category the content pane shows. Has no visible
    /// effect until the next `build_layout` rebuilds the composition.
    pub fn select(&mut self, category: Category) {
        self.selected = category;
    }

    /// Build the sidebar Column — one Button per `Category` plus the
    /// app heading at the top. The heading is rendered as a Text
    /// rather than a Button so it doesn't grab focus.
    fn build_sidebar(&self) -> Box<dyn Widget> {
        let mut col = Column::new();
        col = col.push(Box::new(Text::new("Settings").with_size(16.0)));
        for category in Category::ALL {
            let id = format!("settings.sidebar.{}", category.slug());
            // The label combines the Nerd Font glyph with the category
            // name — same convention the file-explorer sidebar follows
            // for its Favorites/System rows.
            let label = format!("{} {}", category.glyph(), category.label());
            col = col.push(Box::new(Button::new(&id, &label, None)));
        }
        Box::new(
            Container::new()
                .with_child(Box::new(col)),
        )
    }

    /// Build the content pane: header + per-category body. Header
    /// labels the active category so even a screenshot makes it
    /// obvious which pane is in view.
    fn build_pane(&self) -> Box<dyn Widget> {
        let body: Box<dyn Widget> = match self.selected {
            Category::Display => panes::display::build(),
            Category::Audio => panes::audio::build(),
            Category::Network => panes::network::build(),
            Category::Bluetooth => panes::bluetooth::build(),
            Category::Workspaces => panes::workspaces::build(),
            Category::Collaboration => panes::collaboration::build(),
            Category::About => panes::about::build(),
        };
        Box::new(
            Column::new()
                .push(Box::new(Text::new(self.selected.label()).with_size(20.0)))
                .push(body),
        )
    }

    /// Assemble the full sidebar | pane two-column composition.
    fn build_composition(&self) -> Box<dyn Widget> {
        Box::new(
            Row::new()
                .push(self.build_sidebar())
                .push(self.build_pane()),
        )
    }
}

impl Widget for SettingsApp {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Rebuild the composition every frame so a selected-category
        // change shows up next frame without a separate dirty flag.
        let mut composed = self.build_composition();
        let child_node = composed.build_layout(taffy, theme, node_map);
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
        let node = taffy.new_with_children(style, &[child_node]).unwrap();
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&child_node) = children.first() {
                    c.apply_layout(
                        taffy,
                        child_node,
                        vello::kurbo::Point::new(self.rect.x0, self.rect.y0),
                    );
                }
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
        if let Some(c) = &mut self.composed {
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&child_node) = children.first() {
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
        // Nerd Font cog glyph — the convention for settings/preferences
        // across the qOS UI surfaces (quick-settings, command palette).
        IconDescriptor::Glyph("󰒓")
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

    #[test]
    fn new_starts_on_display_category() {
        let app = SettingsApp::new();
        assert_eq!(app.selected(), Category::Display);
    }

    #[test]
    fn with_selected_overrides_default() {
        let app = SettingsApp::with_selected(Category::About);
        assert_eq!(app.selected(), Category::About);
    }

    #[test]
    fn select_changes_displayed_category() {
        let mut app = SettingsApp::new();
        app.select(Category::Network);
        assert_eq!(app.selected(), Category::Network);
    }

    #[test]
    fn build_layout_then_apply_layout_succeeds() {
        // Smoke test: run the two layout passes and assert the widget
        // ends up with a non-zero rect — proves the composition wires
        // up cleanly without panicking.
        let mut app = SettingsApp::new();
        let theme = ui::Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(800.0),
                        height: taffy::Dimension::length(600.0),
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
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
        assert!(app.get_rect().width() > 0.0, "rect should be non-empty");
        assert!(app.get_rect().height() > 0.0, "rect should be non-empty");
    }
}

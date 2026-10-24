//! An extension's side panel (`chrome.sidePanel`) beside the page: at the
//! window's trailing edge with a rule between them, never over the page.
//! Where the window has no room for both, the panel shows in the page's
//! place until the person closes it, one at a time.
//!
//! The rule is the shared split divider ([`SplitHandle`]): the person drags
//! it, or steps it from the keyboard or assistive technology, to widen or
//! narrow the panel within the theme's bounds, leaving the page a readable
//! column, and each extension's panel keeps the width it was left at.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{SplitHandle, SplitOrientation, SplitRatio, WebView, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use super::BrowserWidget;
use crate::extension_controller::side_panel_bounds;

/// The side panel's page, by its test id.
const PANEL_ID: &str = crate::extension_controller::SIDE_PANEL_PAGE;
/// The edge between the page and the panel, as assistive technology and
/// agents address it.
pub(super) const EDGE_ID: &str = "browser.extension.side-panel-edge";

impl BrowserWidget {
    /// Whether the window has room for an extension's side panel beside a
    /// readable page: below it the panel shows in the page's place, one at a
    /// time, until the person closes it.
    fn side_panel_docks(&self, theme: &Theme) -> bool {
        let room = ui::num::f32_of_f64(self.rect.width());
        room >= side_panel_bounds(theme).0 + theme.tokens.metrics.reading_min_width
    }

    /// How wide the docked side panel stands in this window.
    fn side_panel_extent(&self, theme: &Theme) -> f32 {
        self.extensions
            .side_panel_extent(theme, ui::num::f32_of_f64(self.rect.width()))
    }

    /// The extension whose side panel shows, whether it docks and how wide
    /// it stands: when any of them changes the window is composed again.
    pub(super) fn side_panel_key(&self, theme: &Theme) -> Option<(String, bool, f32)> {
        let id = self.extensions.side_panel_shown()?;
        Some((
            id.to_owned(),
            self.side_panel_docks(theme),
            self.side_panel_extent(theme),
        ))
    }

    /// The page, named `page_name` for assistive technology, with the side
    /// panel an extension shows beside it at the trailing edge, the edge
    /// between them, never over it.
    pub(super) fn page_beside_side_panel(
        &self,
        page_name: String,
        theme: &Theme,
    ) -> Box<dyn Widget> {
        let page: Box<dyn Widget> = Box::new(
            WebView::new(self.engine.clone())
                .with_scale_factor(self.window_scale)
                .with_accessible_name(page_name)
                .with_test_id("browser.surface"),
        );
        let docks = self.side_panel_docks(theme);
        let width = if docks {
            self.side_panel_extent(theme)
        } else {
            ui::num::f32_of_f64(self.rect.width())
        };
        let Some(panel) = self.extensions.side_panel(width) else {
            return page;
        };
        let row = ui::widget::Row::new()
            .no_wrap()
            .with_fill_remaining_height();
        if !docks {
            return Box::new(row.with_gap(theme.tokens.spacing.s0).push(panel));
        }
        // The edge paints its rule in the gap between them.
        Box::new(
            row.with_gap(theme.tokens.metrics.chrome_rule_px.max(1.0))
                .push(Box::new(
                    ui::widget::Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .filling()
                        .with_child(page),
                ))
                .push(panel),
        )
    }

    /// The edge while a docked side panel shows: kept from the last layout,
    /// so a drag goes on, its bounds those of `theme` in this window, and
    /// its node registered so the pointer finds it (its resize cursor).
    /// Gone with the panel.
    pub(super) fn build_side_panel_edge(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        let room = self.rect.width();
        if self.extensions.side_panel_shown().is_none()
            || !self.side_panel_docks(theme)
            || room <= 0.0
        {
            self.side_panel_edge = None;
            return None;
        }
        let width = f64::from(self.side_panel_extent(theme));
        let (narrowest, widest) = side_panel_bounds(theme);
        let widest = f64::from(widest)
            .min(room - f64::from(theme.tokens.metrics.reading_min_width))
            .max(f64::from(narrowest));
        let region = self.side_panel_region().unwrap_or(self.rect);
        let edge = self.side_panel_edge.get_or_insert_with(|| {
            Box::new(
                SplitHandle::new(SplitOrientation::Horizontal, SplitRatio::default())
                    .with_label(crate::CATALOG.t("browser-extension-side-panel-width"))
                    .with_test_id(EDGE_ID),
            )
        });
        if !edge.is_dragging() {
            // The page's share of the window: the panel takes the rest.
            let mut ratio = SplitRatio::default().with_bounds(
                ui::num::f32_of_f64((room - widest) / room),
                ui::num::f32_of_f64((room - f64::from(narrowest)) / room),
            );
            ratio.set(ui::num::f32_of_f64((room - width) / room));
            *edge.ratio_mut() = ratio;
        }
        edge.set_rect(region, theme);
        Some(Widget::build_layout(edge.as_mut(), taffy, theme, node_map))
    }

    /// The band the edge moves in: the window's width, as tall as the
    /// side panel.
    fn side_panel_region(&self) -> Option<Rect> {
        let panel = self.find_rect(PANEL_ID)?;
        Some(Rect::new(self.rect.x0, panel.y0, self.rect.x1, panel.y1))
    }

    /// Put the edge where the side panel was laid out.
    pub(super) fn place_side_panel_edge(&mut self) {
        let Some(region) = self.side_panel_region() else {
            return;
        };
        if let Some(edge) = self.side_panel_edge.as_mut()
            && !edge.is_dragging()
        {
            edge.set_bounds(region);
        }
    }

    /// A press on the edge, the drag that follows and a keyboard step while
    /// it has focus widen or narrow the side panel; letting go, or a step,
    /// keeps the width for the panel's extension. Returns whether the edge
    /// took the event.
    pub(super) fn side_panel_edge_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let Some(node) = self.side_panel_edge_node else {
            return false;
        };
        let Some(edge) = self.side_panel_edge.as_deref_mut() else {
            return false;
        };
        let was_dragging = edge.is_dragging();
        let mut took = false;
        let moved = SplitHandle::handle_event(
            edge,
            event,
            &mut ui::widget::EventCtx {
                handled: &mut took,
                ..ctx.reborrow(ctx.path, node)
            },
        );
        // A press elsewhere takes the keyboard from it.
        if !took
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            )
        {
            edge.set_focused(false);
        }
        let dragging = edge.is_dragging();
        let page = edge.ratio().first_extent(self.rect.width());
        if moved {
            self.adopt_side_panel_width(self.rect.width() - page);
        }
        if !dragging && (was_dragging || moved) {
            self.extensions.remember_side_panel_width();
        }
        if took {
            *ctx.handled = true;
        }
        took
    }

    /// The edge as assistive technology and agents meet it: a separator
    /// named for the panel's width, its value the panel's share of the
    /// window.
    pub(super) fn side_panel_edge_access(&self) -> Option<ui::a11y::AccessNode> {
        let edge = self
            .side_panel_edge
            .as_deref()
            .filter(|_| self.side_panel_edge_node.is_some())?;
        let page = edge.ratio().percent();
        Some(
            edge.accessibility()
                .with_value(format!("{}%", 100_u32.saturating_sub(page)))
                .with_action_target(EDGE_ID)
                .with_hashed_key(EDGE_ID)
                .with_bounds(edge.hit_rect()),
        )
    }

    /// The edge as assistive technology and agents step it or set it: an
    /// increment widens the panel, a value is its share of the window in
    /// percent.
    pub(super) fn side_panel_edge_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
    ) -> bool {
        use ui::a11y::Action;
        if target != EDGE_ID {
            return false;
        }
        let Some(edge) = self.side_panel_edge.as_deref_mut() else {
            return false;
        };
        // The divider's value is the page's share: the panel has the rest.
        let action = match action {
            Action::Focus => {
                edge.set_focused(true);
                return true;
            }
            Action::Increment => Action::Decrement,
            Action::Decrement => Action::Increment,
            Action::SetValue(value) => {
                let Some(panel) = value
                    .trim_end_matches('%')
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|share| share.is_finite())
                else {
                    return false;
                };
                Action::SetValue(format!("{}", 100.0 - panel))
            }
            _ => return false,
        };
        if !SplitHandle::perform_action(edge, &action) {
            return false;
        }
        let page = edge.ratio().first_extent(self.rect.width());
        self.adopt_side_panel_width(self.rect.width() - page);
        self.extensions.remember_side_panel_width();
        true
    }

    /// The side panel is `width` logical pixels wide now. Returns whether
    /// that changed it, and composes the window again if so.
    fn adopt_side_panel_width(&mut self, width: f64) -> bool {
        let changed = self
            .extensions
            .set_side_panel_width(ui::num::f32_of_f64(width));
        self.structure_dirty |= changed;
        changed
    }

    /// The edge's rule: a hairline in the gap at rest, the shared divider's
    /// lit fill under the pointer, while dragged and while focused.
    pub(super) fn paint_side_panel_edge(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) {
        let Some(edge) = self.side_panel_edge.as_deref() else {
            self.side_panel_edge_drawn = None;
            return;
        };
        if self.side_panel_edge_node.is_none() {
            return;
        }
        edge.paint(
            scene,
            transform,
            theme.tokens.colors.surfaces.content,
            theme,
        );
        self.side_panel_edge_drawn = Some(edge_look(edge));
    }

    /// Where the edge must be painted again: it lit, went dark or moved
    /// since it was drawn.
    pub(super) fn side_panel_edge_damage(&self) -> Option<Rect> {
        let now = self
            .side_panel_edge
            .as_deref()
            .filter(|_| self.side_panel_edge_node.is_some())
            .map(edge_look);
        if now == self.side_panel_edge_drawn {
            return None;
        }
        [now, self.side_panel_edge_drawn]
            .into_iter()
            .flatten()
            .map(|(_, rect)| rect)
            .reduce(|a, b| a.union(b))
    }
}

/// Whether the edge is lit (under the pointer, dragged or focused), and
/// where it takes the pointer.
fn edge_look(edge: &SplitHandle) -> (bool, Rect) {
    (
        edge.is_hovered() || edge.is_dragging() || Widget::is_focused(edge),
        edge.hit_rect(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{layout_at, layout_keeping_tree};
    use super::*;
    use crate::extension_controller::{ExtensionController, SidePanelWidths, side_panel_width};
    use crate::extension_toolbar::{Access, Installed};
    use ui::webview::test_engine::TestEngine;
    use ui::webview::{
        ExtensionShortcut, ExtensionSidePanelChange, KeyInput, KeyModifiers, WebViewCommand,
        WebViewEvent,
    };
    use vello::kurbo::Point;
    use winit::event::{ElementState, MouseButton};
    use winit::keyboard::{Key, ModifiersState};

    fn with_side_panel(id: &str) -> Installed {
        Installed {
            id: id.into(),
            name: "Notes".into(),
            root: std::path::PathBuf::new(),
            enabled: true,
            pinned: true,
            action: None,
            options: None,
            access: Access {
                permissions: vec!["sidePanel".into()],
                ..Access::default()
            },
            keyword: None,
        }
    }

    /// A window whose extension `id` shows its side panel, its widths kept
    /// in `store`.
    fn window_with_panel(id: &str, store: ui::storage::Settings<SidePanelWidths>) -> BrowserWidget {
        let mut widget = BrowserWidget::with_engine(TestEngine::new(), true);
        widget.extensions = ExtensionController::new(true);
        widget
            .extensions
            .use_installed_for_tests(vec![with_side_panel(id)]);
        widget.attach_side_panel_widths(store);
        widget.extensions.on_engine_event(
            WebViewEvent::ExtensionSidePanel {
                id: id.into(),
                change: ExtensionSidePanelChange::Open(format!(
                    "chrome-extension://{id}/panel.html"
                )),
            },
            &widget.engine,
        );
        widget
    }

    fn send(widget: &mut BrowserWidget, event: &WindowEvent) -> bool {
        let (tree, node) = layout_keeping_tree(widget, 1400.0, 800.0, &Theme::dark());
        ui::testing::Dispatch::new()
            .layout(&tree, node)
            .send(widget, event)
            .handled
    }

    #[test]
    fn the_person_drags_the_side_panels_edge_within_its_bounds_and_the_width_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = || {
            ui::storage::Settings::<SidePanelWidths>::for_app("so.tilo.browser")
                .in_file(dir.path().join("settings.json"))
        };
        let theme = Theme::dark();
        let (narrowest, widest) = side_panel_bounds(&theme);
        let id = "n".repeat(32);
        let mut widget = window_with_panel(&id, store());
        layout_at(&mut widget, 1400.0, 800.0);
        layout_at(&mut widget, 1400.0, 800.0);
        let width = |widget: &BrowserWidget| widget.find_rect(PANEL_ID).expect("the panel").width();
        assert!(
            (width(&widget) - f64::from(side_panel_width(&theme))).abs() < 1.0,
            "{:?} in {:?}",
            widget.find_rect(PANEL_ID),
            widget.rect
        );
        let panel = widget.find_rect(PANEL_ID).unwrap();
        let edge = widget
            .side_panel_edge
            .as_ref()
            .expect("its edge")
            .hit_rect();
        assert!(
            edge.contains(Point::new(panel.x0, panel.center().y)),
            "{edge:?} {panel:?}"
        );

        // Dragged 200 pixels toward the page, the panel is that much wider.
        let (x, y) = (edge.center().x, panel.center().y);
        send(&mut widget, &ui::synthetic_input::cursor_moved(x, y));
        assert!(send(
            &mut widget,
            &ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left)
        ));
        assert!(send(
            &mut widget,
            &ui::synthetic_input::cursor_moved(x - 200.0, y)
        ));
        layout_at(&mut widget, 1400.0, 800.0);
        let dragged = f64::from(side_panel_width(&theme)) + 200.0;
        assert!((width(&widget) - dragged).abs() < 2.0, "{}", width(&widget));
        // Never past its widest, and never narrower than its narrowest.
        send(
            &mut widget,
            &ui::synthetic_input::cursor_moved(x - 2000.0, y),
        );
        layout_at(&mut widget, 1400.0, 800.0);
        assert!(
            (width(&widget) - f64::from(widest)).abs() < 2.0,
            "{}",
            width(&widget)
        );
        send(
            &mut widget,
            &ui::synthetic_input::cursor_moved(x + 2000.0, y),
        );
        layout_at(&mut widget, 1400.0, 800.0);
        assert!(
            (width(&widget) - f64::from(narrowest)).abs() < 2.0,
            "{}",
            width(&widget)
        );
        send(
            &mut widget,
            &ui::synthetic_input::cursor_moved(x - 200.0, y),
        );
        assert!(send(
            &mut widget,
            &ui::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left)
        ));
        layout_at(&mut widget, 1400.0, 800.0);
        assert!((width(&widget) - dragged).abs() < 2.0);

        // Assistive technology and agents step it, an increment widening the
        // panel, and read its share of the window.
        let mut doc = automerge::AutoCommit::new();
        assert!(widget.perform_semantic_action(
            EDGE_ID,
            &ui::a11y::Action::Increment,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        layout_at(&mut widget, 1400.0, 800.0);
        let stepped = width(&widget);
        assert!(stepped > dragged + 10.0, "{stepped}");
        let share = format!("{}%", (stepped / 1400.0 * 100.0).round());
        let access = widget.side_panel_edge_access().expect("a separator");
        assert_eq!(access.value.as_deref(), Some(share.as_str()));

        // A later window shows that extension's panel at the width it was
        // left at.
        let mut later = window_with_panel(&id, store());
        layout_at(&mut later, 1400.0, 800.0);
        layout_at(&mut later, 1400.0, 800.0);
        assert!((width(&later) - stepped).abs() < 2.0, "{}", width(&later));

        // A narrower window keeps the page readable; too narrow, the panel
        // takes the page's place and has no edge.
        layout_at(&mut later, 700.0, 800.0);
        layout_at(&mut later, 700.0, 800.0);
        let reading = f64::from(theme.tokens.metrics.reading_min_width);
        assert!(
            (width(&later) - (700.0 - reading)).abs() < 2.0,
            "{}",
            width(&later)
        );
        layout_at(&mut later, 600.0, 800.0);
        layout_at(&mut later, 600.0, 800.0);
        assert!(later.side_panel_edge.is_none());
        assert!((width(&later) - 600.0).abs() < 1.0);
    }

    #[test]
    fn a_command_runs_while_the_address_has_the_keyboard_after_the_browsers_own_keys() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        widget.extensions = ExtensionController::new(true);
        let conventions = ui::platform::PlatformConventions::default();
        let command = KeyModifiers::command(conventions);
        let alt_shift = KeyModifiers {
            alt: true,
            shift: true,
            ..KeyModifiers::default()
        };
        // As the engine tells it: Alt+Shift+K, and (were the Browser not
        // to keep it) the command key and F.
        widget.extensions.on_engine_event(
            WebViewEvent::ExtensionShortcuts(vec![
                ExtensionShortcut {
                    key: "K".into(),
                    modifiers: alt_shift,
                },
                ExtensionShortcut {
                    key: "F".into(),
                    modifiers: command,
                },
            ]),
            &widget.engine,
        );
        let theme = Theme::dark();
        layout_at(&mut widget, 1200.0, 800.0);
        let (tree, node) = layout_keeping_tree(&mut widget, 1200.0, 800.0, &theme);
        assert!(widget.focus_address_input());
        let address = widget.address.with(Clone::clone);
        let press = |widget: &mut BrowserWidget, text: &str, held: ModifiersState| {
            ui::testing::Dispatch::new()
                .layout(&tree, node)
                .modifiers(held)
                .send(
                    widget,
                    &ui::synthetic_input::key_press(Key::Character(text.into()), Some(text)),
                )
                .handled
        };
        let sent = |text: &str| {
            engine
                .commands()
                .iter()
                .filter(|sent| {
                    matches!(sent, WebViewCommand::ExtensionCommandKey { key: KeyInput::Character(key), .. } if key == text)
                })
                .count()
        };

        // The command's key runs it over the page, and the address never
        // sees it.
        assert!(press(
            &mut widget,
            "K",
            ModifiersState::ALT | ModifiersState::SHIFT
        ));
        assert_eq!(sent("K"), 1);
        assert!(
            engine
                .commands()
                .contains(&WebViewCommand::ExtensionCommandKey {
                    key: KeyInput::Character("K".into()),
                    modifiers: alt_shift,
                })
        );
        widget.sync(&automerge::AutoCommit::new(), &automerge::ROOT);
        assert_eq!(widget.address.with(Clone::clone), address);

        // The Browser's own key is the Browser's first: Find opens.
        assert!(press(&mut widget, "f", conventions.command_modifier()));
        assert_eq!(sent("f") + sent("F"), 0);
        assert!(widget.daily.open);

        // A key no command holds is the field's.
        assert!(widget.focus_address_input());
        press(
            &mut widget,
            "J",
            ModifiersState::ALT | ModifiersState::SHIFT,
        );
        assert_eq!(sent("J"), 0);
    }
}

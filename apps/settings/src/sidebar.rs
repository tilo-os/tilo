//! The settings sidebar: a cohesive navigation surface, not a stack of
//! free-floating buttons.
//!
//! ## Why a custom row widget
//!
//! `ui::widget::Button` is a fixed 120×40 pill that centres its label. A
//! navigation sidebar needs the opposite: a *full-width* row that left-aligns
//! an icon + the category's complete label, sizes itself to the text so
//! nothing is ever clipped, and carries an unmistakable active highlight. So
//! [`SidebarRow`] is a small purpose-built widget. It still routes its click
//! through the same CRDT mechanism the rest of the app uses — pressing a row
//! writes `settings.select.<slug> = true`, which [`crate::SettingsApp::sync`]
//! reads to switch the active category. No private message bus; the selection
//! lives in the document like every other piece of state.
//!
//! ## What makes it a sidebar
//!
//! [`build`] assembles the rows into one [`Container`] surface tinted a step
//! below the content background, with the app title, per-[`Section`] headers,
//! and the rows stacked inside a [`Scrollable`] so a short window can still
//! reach every category. The surface — not the individual rows — is what reads
//! as "the sidebar".

use std::collections::HashMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Column, Container, Scrollable, Text, Widget};

use crate::categories::{Category, Section};

/// Width (logical px) of the docked sidebar. Sized so the widest row
/// ("Permissions & Roles" with its glyph, plus the row's horizontal padding)
/// fits without clipping, with margin to spare.
pub const SIDEBAR_WIDTH: f64 = 264.0;

/// Height (logical px) of a single category row. Generous enough to read as a
/// touch target and to seat the icon + label comfortably.
const ROW_HEIGHT: f32 = 40.0;

/// Horizontal inset (logical px) of a row's content from the row edge.
const ROW_PAD_X: f64 = 14.0;

/// Inset (logical px) of the selection/hover pill from the row's left/right
/// edges, so the highlight reads as a chip with margin to the panel border.
const ROW_INSET_X: f64 = 8.0;

/// Inset (logical px) of the pill from the row's top/bottom edges.
const ROW_INSET_Y: f64 = 3.0;

/// Font size (logical px) of a category label.
const ROW_TEXT_SIZE: f32 = 14.0;

/// One selectable category row in the sidebar.
///
/// Fills the available width, left-aligns its icon + full label, and paints an
/// accent background + bar when it is the active category. Clicking it writes
/// `settings.select.<slug> = true` into the document — the same key
/// [`crate::SettingsApp::sync`] consumes to change panes.
pub struct SidebarRow {
    category: Category,
    /// Whether this row is the currently-selected category — drives the active
    /// highlight. Passed in at build time from the app's selection.
    active: bool,
    /// CRDT key this row writes `true` to on click.
    select_key: String,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
}

impl SidebarRow {
    pub fn new(category: Category, active: bool) -> Self {
        Self {
            category,
            active,
            select_key: format!("settings.select.{}", category.slug()),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
        }
    }
}

impl Widget for SidebarRow {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Full width, fixed height — the row is a leaf that draws itself, so it
        // needs no taffy children.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ROW_HEIGHT),
                },
                ..Default::default()
            })
            .unwrap();
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Active row: a filled accent pill. Hover/press: a subtle raised tint.
        // Otherwise nothing, so inactive rows read flat against the sidebar
        // surface. The pill is inset from the panel edges and given a little
        // vertical breathing room so the highlight reads as a deliberate
        // selection chip rather than a full-bleed band.
        let radius = theme.tokens.radii.md as f64;
        let pill_rect = Rect::new(
            self.rect.x0 + ROW_INSET_X,
            self.rect.y0 + ROW_INSET_Y,
            self.rect.x1 - ROW_INSET_X,
            self.rect.y1 - ROW_INSET_Y,
        );
        let highlight = if self.active {
            Some(theme.accent_primary)
        } else if self.is_pressed {
            Some(theme.bg_active)
        } else if self.is_hovered {
            Some(theme.bg_hover)
        } else {
            None
        };
        if let Some(color) = highlight {
            let pill = RoundedRect::from_rect(pill_rect, radius);
            scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &pill);
        }

        // Icon + label, left-aligned, vertically centred. The active row uses
        // a bright on-accent foreground; inactive rows use the primary text
        // colour so they stay legible against the sidebar surface.
        let fg = if self.active {
            theme.tokens.colors.text_primary
        } else {
            theme.text_primary
        };

        let glyph = self.category.glyph();
        let icon_x = self.rect.x0 + ROW_PAD_X;
        let icon_y = self.rect.y0 + (self.rect.height() - ROW_TEXT_SIZE as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            glyph,
            ROW_TEXT_SIZE,
            fg,
            transform * Affine::translate((icon_x, icon_y)),
            None,
        );

        // Place the label a fixed gutter past the icon column so every label
        // starts at the same x regardless of glyph advance width.
        let label = self.category.label();
        let label_x = self.rect.x0 + ROW_PAD_X + 28.0;
        let label_y = self.rect.y0 + (self.rect.height() - ROW_TEXT_SIZE as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            label,
            ROW_TEXT_SIZE,
            fg,
            transform * Affine::translate((label_x, label_y)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *handled = true;
                        // Fire selection on release (matches Button semantics):
                        // write the category's select key so the app's `sync`
                        // switches the active pane.
                        if !pressed {
                            let _ = state.put(root, self.select_key.as_str(), true);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SidebarRow"
    }

    /// Addressable by the category's display label so interaction tests can
    /// resolve a row's on-screen rect and click it.
    fn test_id(&self) -> Option<&str> {
        Some(self.category.label())
    }

    /// Expose the active state so a rebuilt tree can be inspected — used by
    /// the interaction tests to confirm the highlight followed the click. The
    /// `Widget::get_state` channel (ephemeral, non-persisted) is the natural
    /// place for this read-only snapshot.
    fn get_state(&self) -> Option<String> {
        Some(if self.active { "active".into() } else { "inactive".into() })
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
}

/// Build the full sidebar surface: app title, then one labelled section per
/// [`Section`], each listing its categories as [`SidebarRow`]s. `selected`
/// drives which row shows the active highlight.
///
/// The whole thing is one [`Container`] tinted a step below the content
/// background so it reads as a single cohesive sidebar pane, with the rows
/// inside a [`Scrollable`] so every category stays reachable on short windows.
pub fn build(selected: Category) -> Box<dyn Widget> {
    let mut col = Column::new()
        .with_intrinsic_height()
        .push(Box::new(Text::new("Settings").with_size(20.0)));

    for section in Section::ALL {
        col = col.push(Box::new(
            Text::new(&section.label().to_uppercase())
                .with_size(10.0)
                .with_color(Theme::dark().text_dim),
        ));
        for category in Category::in_section(*section) {
            col = col.push(Box::new(SidebarRow::new(category, category == selected)));
        }
    }

    let scroll = Scrollable::new_vertical().with_child(Box::new(col));
    // Tint the sidebar surface with the app's base colour — one step below the
    // raised content cards — so the split reads as sidebar | content.
    let surface = Theme::dark().tokens.colors.surfaces.base;
    Box::new(
        Container::new()
            .filling()
            .with_fill(surface)
            .with_child(Box::new(scroll)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_is_addressable_by_category_label() {
        let row = SidebarRow::new(Category::MouseKeyboard, false);
        assert_eq!(Widget::test_id(&row), Some("Mouse & Keyboard"));
    }

    #[test]
    fn row_writes_select_key_on_release() {
        let mut row = SidebarRow::new(Category::About, false);
        // Seat a rect and pretend the cursor is inside it.
        row.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
        row.is_hovered = true;

        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();

        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let release = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        for ev in [&press, &release] {
            let mut handled = false;
            let mut focus = None;
            row.handle_event(ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &taffy);
        }

        use automerge::{ReadDoc, Value};
        let got = doc.get(&root, "settings.select.about");
        assert!(
            matches!(got, Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))),
            "releasing a click on the row writes its select key true"
        );
    }
}

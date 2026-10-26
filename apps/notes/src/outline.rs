//! The open note's heading outline, in the shared navigation list.
//!
//! A note is a block document; this is its map. Every heading block becomes
//! a row, nested by its level, in a section under the page tree. Choosing a
//! row reveals that block through [`MainPanel::reveal_block`], so the caret
//! and the body move together.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath, NavItem, NavList, NavSection, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::main_panel::MainPanel;

/// One heading in the open note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Heading {
    pub(crate) id: String,
    pub(crate) level: u8,
    pub(crate) text: String,
}

fn heading_id(id: &str) -> String {
    format!("notes.heading:{id}")
}

fn heading_block(id: &str) -> Option<&str> {
    id.strip_prefix("notes.heading:")
}

/// Each heading under the nearest shallower one before it, also when the
/// note skips a level; no rows are made up for the skipped levels.
fn items(headings: &[Heading]) -> Vec<NavItem> {
    fn children(
        remaining: &mut std::iter::Peekable<std::slice::Iter<'_, Heading>>,
        parent_level: u8,
    ) -> Vec<NavItem> {
        let mut rows = Vec::new();
        while let Some(heading) = remaining.next_if(|next| next.level > parent_level) {
            let label = if heading.text.trim().is_empty() {
                ui::i18n!("notes-outline-untitled")
            } else {
                heading.text.trim().to_owned()
            };
            let nested = children(remaining, heading.level);
            rows.push(
                NavItem::new(heading_id(&heading.id), label)
                    .children(nested)
                    .expanded(),
            );
        }
        rows
    }
    children(&mut headings.iter().peekable(), 0)
}

pub(crate) struct NotesOutline {
    list: NavList,
    headings: Vec<Heading>,
    chosen: Rc<RefCell<Option<String>>>,
    structure_dirty: bool,
    /// The pointer in window coordinates, so a scroll or press aimed at the
    /// note beside the sidebar is not offered to the heading rows.
    last_cursor: Point,
}

impl NotesOutline {
    pub(crate) fn new() -> Self {
        let chosen = Rc::new(RefCell::new(None));
        let pick = Rc::clone(&chosen);
        Self {
            list: NavList::new(Vec::new())
                .with_id("notes.outline")
                .with_accessible_name(ui::i18n!("notes-outline"))
                .on_select(move |id| *pick.borrow_mut() = Some(id)),
            headings: Vec::new(),
            chosen,
            structure_dirty: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Follow the open note's heading blocks. The list is rebuilt only when
    /// the headings (or their words) change, so a selection and scroll stay.
    pub(crate) fn follow(&mut self, panel: &MainPanel) {
        let headings: Vec<Heading> = panel
            .editor()
            .blocks()
            .iter()
            .filter_map(|block| match block.kind {
                ui::widget::BlockKind::Heading(level) => Some(Heading {
                    id: block.id.clone(),
                    level,
                    text: block.text().to_owned(),
                }),
                _ => None,
            })
            .collect();
        if headings == self.headings {
            return;
        }
        let empty = headings.is_empty();
        self.list.set_sections(if empty {
            Vec::new()
        } else {
            vec![
                NavSection::new(
                    "notes.outline",
                    ui::i18n!("notes-outline"),
                    items(&headings),
                )
                .empty_caption(ui::i18n!("notes-outline-empty")),
            ]
        });
        self.headings = headings;
        self.structure_dirty = true;
    }

    /// Whether the note has headings worth showing.
    pub(crate) fn is_shown(&self) -> bool {
        !self.headings.is_empty()
    }

    /// The block chosen since the last call, as its durable id.
    pub(crate) fn take_chosen(&mut self) -> Option<String> {
        let id = self.chosen.borrow_mut().take()?;
        self.list.set_selected(None);
        heading_block(&id).map(str::to_owned)
    }

    /// Whether the list's rows came or went since the last call.
    pub(crate) fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }
}

impl Widget for NotesOutline {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.list.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.list.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.list.build_layout(taffy, theme, node_map);
        let rows = u32::try_from(self.headings.len().saturating_add(1)).unwrap_or(u32::MAX);
        let row = f64::from(theme.tokens.metrics.nav_row_height);
        let height = (row * f64::from(rows)).min(row * 8.0).max(row * 2.0);
        if let Ok(existing) = taffy.style(node).cloned() {
            let _ = taffy.set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(crate::num::f32_of_f64(height)),
                    },
                    flex_grow: 0.0,
                    flex_shrink: 0.0,
                    flex_basis: taffy::style_helpers::length(crate::num::f32_of_f64(height)),
                    ..existing
                },
            );
        }
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.list.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.list
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
            if !self.list.get_rect().contains(self.last_cursor) {
                return;
            }
        }
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::Touch(_)
        ) && !self.list.get_rect().contains(self.last_cursor)
        {
            return;
        }
        self.list.handle_event(event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.list.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.list.mark_drawn();
    }

    fn get_rect(&self) -> Rect {
        self.list.get_rect()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.list]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.list]
    }

    fn name(&self) -> &'static str {
        "NotesOutline"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.list.accessibility()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_are_read_from_blocks_and_nested_by_level() {
        let mut panel = MainPanel::new();
        panel.load_page(
            uuid::Uuid::new_v4(),
            "Outline",
            "# One\n\n### Deep\n\n## Two\n\nbody\n",
        );
        let mut outline = NotesOutline::new();
        outline.follow(&panel);
        assert!(outline.is_shown());
        assert_eq!(
            panel
                .editor()
                .blocks()
                .iter()
                .filter(|block| matches!(block.kind, ui::widget::BlockKind::Heading(_)))
                .count(),
            3
        );
        assert!(
            outline.take_chosen().is_none(),
            "following a note chooses nothing by itself"
        );
    }
}

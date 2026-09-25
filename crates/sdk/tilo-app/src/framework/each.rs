//! A keyed list of rows that follows a collection in the resource.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use tilo_resource::{Id, Resource};
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::widget::{Element, TaffyTreeExt, Widget};
use ui_core::winit::event::{Modifiers, WindowEvent};
use ui_core::{Theme, taffy};

use super::live::Live;

struct Row {
    id: Id,
    /// Dropping it retires every binding the row registered.
    _alive: Rc<()>,
    widget: Element,
}

type Ids<R> = Box<dyn Fn(&<R as Resource>::State) -> Vec<Id>>;
type BuildRow = Box<dyn Fn(&Id, &Rc<()>) -> Element>;

/// Rows for the items of a collection, in collection order, built by
/// [`ViewCx::each`](super::ViewCx::each). A row is kept for as long as its
/// item exists; only added items build rows and only removed items drop them.
pub struct Each<R: Resource> {
    live: Live<R>,
    ids: Ids<R>,
    build: BuildRow,
    rows: Vec<Row>,
    built: u64,
    nodes: Vec<NodeId>,
    rect: Rect,
    structure_dirty: bool,
}

impl<R: Resource> Each<R> {
    pub(crate) fn new(live: Live<R>, ids: Ids<R>, build: BuildRow) -> Self {
        let mut each = Self {
            built: live.revision(),
            live,
            ids,
            build,
            rows: Vec::new(),
            nodes: Vec::new(),
            rect: Rect::ZERO,
            structure_dirty: false,
        };
        each.reconcile();
        each
    }

    fn reconcile(&mut self) {
        let ids = self.live.with(|state| (self.ids)(state));
        let unchanged = ids.len() == self.rows.len()
            && ids.iter().zip(&self.rows).all(|(id, row)| *id == row.id);
        if unchanged {
            return;
        }
        let mut old: HashMap<Id, Row> = self
            .rows
            .drain(..)
            .map(|row| (row.id.clone(), row))
            .collect();
        self.rows = ids
            .into_iter()
            .map(|id| {
                old.remove(&id).unwrap_or_else(|| {
                    let alive = Rc::new(());
                    let widget = (self.build)(&id, &alive);
                    Row {
                        id,
                        _alive: alive,
                        widget,
                    }
                })
            })
            .collect();
        self.structure_dirty = true;
    }

    /// Ids of the rows currently shown, in order.
    pub fn row_ids(&self) -> impl Iterator<Item = &Id> {
        self.rows.iter().map(|row| &row.id)
    }
}

impl<R: Resource> Widget for Each<R> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.live.revision() != self.built {
            self.built = self.live.revision();
            self.reconcile();
        }
        for row in &mut self.rows {
            row.widget.sync(state, path);
        }
    }

    // The per-frame update pass drives retained models; every row sees it.
    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            changed |= row.widget.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.nodes = self
            .rows
            .iter_mut()
            .map(|row| row.widget.build_layout(taffy, theme, node_map))
            .collect();
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Grid,
                align_content: Some(taffy::AlignContent::Start),
                gap: taffy::Size {
                    width: length(0.0_f32),
                    height: length(theme.gap_standard),
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &self.nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        let inner = Point::new(x, y);
        for (row, child) in self.rows.iter_mut().zip(&self.nodes) {
            row.widget.apply_layout(taffy, *child, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for row in &mut self.rows {
            row.widget.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        _node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| row.widget.wants_pointer_capture())
        {
            if let (Some(row), Some(&child)) = (self.rows.get_mut(index), self.nodes.get(index)) {
                row.widget.handle_event(
                    event, scale, modifiers, doc, path, handled, focus, child, taffy,
                );
            }
            return;
        }
        for (row, &child) in self.rows.iter_mut().zip(&self.nodes) {
            row.widget.handle_event(
                event, scale, modifiers, doc, path, handled, focus, child, taffy,
            );
            if *handled {
                break;
            }
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.rows
            .iter()
            .any(|row| row.widget.wants_pointer_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for row in &mut self.rows {
            dirty |= row.widget.take_structure_dirty();
        }
        dirty
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.rows.iter().map(|row| row.widget.as_ref()).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.rows
            .iter_mut()
            .map(|row| row.widget.as_mut() as &mut dyn Widget)
            .collect()
    }

    fn name(&self) -> &'static str {
        "Each"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

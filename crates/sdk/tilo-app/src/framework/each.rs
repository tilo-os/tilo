//! A keyed list of rows that follows a collection in the resource, with each
//! row's item actions in its context menu.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use ui_core::widget::{AppModel, ModelPath};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use tilo_resource::{Id, Resource};
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::widget::{ContextMenu, Element, MenuItem, TaffyTreeExt, Widget};
use ui_core::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use ui_core::winit::keyboard::{Key, NamedKey};
use ui_core::{Theme, taffy};

use super::dispatch::Dispatcher;
use super::live::Live;

/// One entry of a row's context menu: an action published on the row's item.
struct MenuEntry {
    address: tilo_actions::ActionAddress,
    /// Whether the action still takes arguments once the item is bound; those
    /// open in the action palette, which asks for them.
    prompts: bool,
}

/// The context menu of a row: the actions offered on its item.
#[derive(Default)]
struct RowMenu {
    menu: ContextMenu,
    entries: Vec<MenuEntry>,
    cursor: Point,
    /// An entry that needs arguments, for the host to open in the palette.
    palette: Option<tilo_actions::ActionAddress>,
}

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
    dispatch: Dispatcher<R>,
    menu: RowMenu,
    ids: Ids<R>,
    build: BuildRow,
    rows: Vec<Row>,
    /// Set when the collection's membership may have changed since the rows
    /// were last reconciled with it.
    stale: Rc<Cell<bool>>,
    nodes: Vec<NodeId>,
    rect: Rect,
    structure_dirty: bool,
}

impl<R: Resource> Each<R> {
    pub(crate) fn new(
        live: Live<R>,
        dispatch: Dispatcher<R>,
        ids: Ids<R>,
        stale: Rc<Cell<bool>>,
        build: BuildRow,
    ) -> Self {
        let mut each = Self {
            stale,
            live,
            dispatch,
            menu: RowMenu::default(),
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

    /// Open the context menu of the row for `item` at `at`: every action the
    /// resource offers on that item. Returns whether there was any.
    fn open_menu(&mut self, item: &Id, at: Point) -> bool {
        let children = self.live.with(|state| R::children(state));
        let Some(resource) = children.iter().find(|child| {
            tilo_resource::items::item_of_resource::<R>(child.id.as_str())
                .is_some_and(|(_, id)| &id == item)
        }) else {
            return false;
        };
        let read_only = self.dispatch.read_only();
        let mut menu = ContextMenu::new();
        let mut entries = Vec::new();
        for spec in &resource.actions {
            let entry = if spec.contract.risk == tilo_actions::RiskLevel::Destructive {
                MenuItem::destructive_action(spec.label.clone(), || {})
            } else {
                MenuItem::action(spec.label.clone(), || {})
            };
            menu.push(entry.with_enabled(!read_only));
            entries.push(MenuEntry {
                address: tilo_actions::ActionAddress::new(resource.id.as_str(), spec.id.as_str()),
                prompts: !spec.params().is_empty(),
            });
        }
        if entries.is_empty() {
            return false;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a menu anchor is a logical pixel position on screen"
        )]
        menu.open_at([at.x as f32, at.y as f32]);
        self.menu.menu = menu;
        self.menu.entries = entries;
        true
    }

    /// Act on the menu entry the user chose: invoke it, or hand it to the
    /// palette when it still needs arguments.
    fn choose(&mut self, index: usize) {
        let Some(entry) = self.menu.entries.get(index) else {
            return;
        };
        if entry.prompts {
            self.menu.palette = Some(entry.address.clone());
        } else {
            self.dispatch
                .invoke_at(entry.address.clone(), serde_json::Map::new());
        }
    }

    /// The row holding keyboard focus.
    fn focused_row(&self) -> Option<&Row> {
        fn focused(widget: &dyn Widget) -> bool {
            let mut found = widget.is_focused();
            widget.for_each_child(&mut |child| found |= focused(child));
            found
        }
        self.rows.iter().find(|row| focused(row.widget.as_ref()))
    }

    /// Route an event to the row menu. Returns whether it was consumed.
    fn menu_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.menu.cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if self.menu.menu.is_open() {
            self.menu.menu.handle_event(event, ctx);
            if let Some(index) = self.menu.menu.take_selection() {
                self.choose(index);
            }
            return *ctx.handled;
        }
        let target = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self
                .rows
                .iter()
                .find(|row| row.widget.get_rect().contains(self.menu.cursor))
                .map(|row| (row.id.clone(), self.menu.cursor)),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                || (matches!(logical_key, Key::Named(NamedKey::F10))
                    && ctx.modifiers.state().shift_key()) =>
            {
                self.focused_row().map(|row| {
                    let rect = row.widget.get_rect();
                    (row.id.clone(), Point::new(rect.x0, rect.y1))
                })
            }
            _ => None,
        };
        let Some((item, at)) = target else {
            return false;
        };
        if self.open_menu(&item, at) {
            *ctx.handled = true;
            return true;
        }
        false
    }
}

impl<R: Resource> Widget for Each<R> {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.stale.replace(false) {
            self.reconcile();
        }
        for row in &mut self.rows {
            row.widget.sync(state, path);
        }
    }

    // The per-frame update pass drives retained models; every row sees it.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
        let style = Style {
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
        };
        let node = taffy.parent_node(style, &self.nodes);
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) {
        if self.menu_event(event, ctx) {
            return;
        }
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| row.widget.wants_pointer_capture())
        {
            if let (Some(row), Some(&child)) = (self.rows.get_mut(index), self.nodes.get(index)) {
                row.widget
                    .handle_event(event, &mut ctx.reborrow(ctx.path, child));
            }
            return;
        }
        for (row, &child) in self.rows.iter_mut().zip(&self.nodes) {
            row.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, child));
            if *ctx.handled {
                break;
            }
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.menu.is_open()
            || self
                .rows
                .iter()
                .any(|row| row.widget.wants_pointer_capture())
    }

    fn modal_capture(&self) -> Option<Rect> {
        let mut capture = self.menu.menu.modal_capture();
        for row in &self.rows {
            if capture.is_none() {
                capture = row.widget.modal_capture();
            }
        }
        capture
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for row in &mut self.rows {
            row.widget
                .draw_overlay(scene, transform, text, theme, presence);
        }
        if self.menu.menu.is_open() {
            self.menu
                .menu
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.menu.palette.take().or_else(|| {
            self.rows
                .iter_mut()
                .find_map(|row| row.widget.take_action_palette_request())
        })
    }

    fn take_search_request(&mut self) -> bool {
        self.rows
            .iter_mut()
            .any(|row| row.widget.take_search_request())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for row in &mut self.rows {
            dirty |= row.widget.take_structure_dirty();
        }
        dirty
    }

    // An open row menu is part of the tree, so assistive technology and
    // inspection see it; it lays itself out where it opened.
    fn children(&self) -> Vec<&dyn Widget> {
        let open = self
            .menu
            .menu
            .is_open()
            .then_some(&self.menu.menu as &dyn Widget);
        self.rows
            .iter()
            .map(|row| row.widget.as_ref())
            .chain(open)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let open = self.menu.menu.is_open();
        self.rows
            .iter_mut()
            .map(|row| row.widget.as_mut() as &mut dyn Widget)
            .chain(open.then_some(&mut self.menu.menu as &mut dyn Widget))
            .collect()
    }

    fn name(&self) -> &'static str {
        "Each"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

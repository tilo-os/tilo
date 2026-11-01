//! The menu of the window in front: what the desktop offers to do to it, at
//! the window itself. The tab-menu shortcut (Super+F10, or a controller's
//! Menu button) opens it there, and every row is a command the surface
//! already answers ([`tilo_desktop_surface::surface::window_menu`]), so
//! choosing one does exactly what the same command does from anywhere else.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::TabId;
use tilo_desktop_surface::surface::WindowMenuRow;
use ui::theme::Theme;
use ui::widget::{ContextMenu, MenuItem, TaffyTreeExt as _, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

/// The window menu, for the window whose tab is `tab`.
#[derive(Default)]
pub struct WindowMenu {
    menu: ContextMenu,
    /// The window the rows are for.
    tab: Option<TabId>,
    /// The command each row means, `None` for a row that commands nothing.
    rows: Vec<Option<String>>,
    /// What the person chose, for the host to carry out.
    chosen: Option<(TabId, String)>,
    rect: Rect,
}

impl WindowMenu {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the menu for `tab` with `rows`, its top edge under `anchor` (the
    /// window's corner) within `viewport`, or close it when it is already
    /// open.
    pub fn toggle(&mut self, tab: TabId, rows: &[WindowMenuRow], anchor: Rect, viewport: Rect) {
        if self.menu.is_open() {
            self.close();
            return;
        }
        let mut items = Vec::with_capacity(rows.len());
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let mut item = MenuItem::selection(row.label.clone());
            if !row.enabled {
                item = item.with_enabled(false);
                if let Some(reason) = &row.reason {
                    item = item.with_detail(reason.clone());
                }
            }
            items.push(item);
            ids.push(Some(row.id.clone()));
        }
        self.menu.set_items(items);
        self.rows = ids;
        self.tab = Some(tab);
        self.menu.open_anchored(anchor, viewport);
        self.menu.focus_first_action();
    }

    pub fn close(&mut self) {
        self.menu.close();
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// The window the open menu is for.
    pub fn tab(&self) -> Option<TabId> {
        if self.menu.is_open() { self.tab } else { None }
    }

    /// Where row `index` is painted, while the menu is open.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        self.menu.item_rect(index)
    }

    /// The labels of the rows shown, in order; nothing while it is closed.
    pub fn labels(&self) -> Vec<Option<String>> {
        if !self.menu.is_open() {
            return Vec::new();
        }
        self.menu
            .items()
            .iter()
            .map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Choose the row labelled `label`, as a click on it would.
    pub fn choose(&mut self, label: &str) -> bool {
        let Some(index) = self
            .menu
            .items()
            .iter()
            .position(|item| item.label() == Some(label))
        else {
            return false;
        };
        self.menu.click_item(index);
        self.settle();
        true
    }

    /// What the person chose since the last call; the menu has closed.
    pub fn take_choice(&mut self) -> Option<(TabId, String)> {
        self.settle();
        self.chosen.take()
    }

    fn settle(&mut self) {
        let Some(index) = self.menu.take_selection() else {
            return;
        };
        self.menu.close();
        let Some(tab) = self.tab else {
            return;
        };
        if let Some(Some(row)) = self.rows.get(index) {
            self.chosen = Some((tab, row.clone()));
        }
    }
}

impl Widget for WindowMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let menu = self.menu.build_layout(taffy, theme, map);
        // Over the whole host, out of its flow, like the other menus.
        let style = Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                bottom: taffy::LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[menu], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [menu] = taffy.children(node).unwrap_or_default()[..] {
            self.menu.apply_layout(taffy, menu, inner);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [menu] = nodes[..] else {
            return;
        };
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        }
        self.settle();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.menu]
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    fn name(&self) -> &'static str {
        "WindowMenu"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

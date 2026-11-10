//! The window menu on the page: what the desktop can do to one window, opened by a secondary click
//! on that window's bar.
//!
//! The rows and what choosing one does are the desk's own ([`DesktopHandle::window_menu`],
//! [`DesktopHandle::choose_window_menu`]): a native session shows them through its window-menu
//! view, and the page, which has no session around its desk, shows them here, in the toolkit's
//! menu, where the click was.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::TabId;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::surface::WindowMenuRow;
use ui::Theme;
use ui::kurbo::{Point, Rect};
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::widget::{ContextMenu, MenuItem, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;

/// The window menu the page shows: the desk's rows for the window whose bar was clicked.
pub(crate) struct PageWindowMenu {
    menu: ContextMenu,
    /// The window the open menu is for.
    tab: Option<TabId>,
    /// The row the person chose since the menu last settled, by its id.
    chosen: Rc<RefCell<Option<String>>>,
    rect: Rect,
}

impl PageWindowMenu {
    pub(crate) fn new() -> Self {
        Self {
            menu: ContextMenu::new(),
            tab: None,
            chosen: Rc::new(RefCell::new(None)),
            rect: Rect::ZERO,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Open the menu a window's bar asked for since the last pass, where it was pressed, and do
    /// the row the person chose. Whether anything changed.
    pub(crate) fn follow(&mut self, handle: &DesktopHandle) -> bool {
        let mut changed = false;
        if let Some((tab, at)) = handle.take_tab_menus().pop() {
            changed |= self.open(handle, tab, at);
        }
        let chosen = self.chosen.borrow_mut().take();
        if let (Some(id), Some(tab)) = (chosen, self.tab) {
            self.menu.close();
            self.tab = None;
            if let Err(error) = handle.choose_window_menu(tab, &id, None) {
                tracing::warn!(%error, row = id, "the window menu's row was refused");
            }
            changed = true;
        }
        changed
    }

    /// Show the desk's rows for the window showing `tab`, at `at` on the desk.
    fn open(&mut self, handle: &DesktopHandle, tab: TabId, at: Point) -> bool {
        let rows = handle.window_menu(tab);
        if rows.is_empty() {
            return false;
        }
        let items = rows.iter().map(|row| self.item(row)).collect();
        self.menu.set_items(items);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a point on the desk, a few thousand logical pixels at most"
        )]
        self.menu.open_at([at.x as f32, at.y as f32]);
        self.tab = Some(tab);
        true
    }

    /// `row` as the toolkit's menu draws it: a rule, a submenu of the places it offers, or a row
    /// that, chosen, is remembered by its id. A row the window cannot take now is shown with why.
    fn item(&self, row: &WindowMenuRow) -> MenuItem {
        if row.separator {
            return MenuItem::separator();
        }
        let item = if row.children.is_empty() {
            let chosen = Rc::clone(&self.chosen);
            let id = row.id.clone();
            MenuItem::action(row.label.clone(), move || {
                *chosen.borrow_mut() = Some(id.clone());
            })
        } else {
            MenuItem::submenu(
                row.label.clone(),
                row.children.iter().map(|child| self.item(child)).collect(),
            )
        };
        let item = match row.icon {
            Some(icon) => item.with_icon(icon),
            None => item,
        };
        let item = match &row.reason {
            Some(reason) if !row.enabled => item.with_detail(reason.clone()),
            _ => item,
        };
        item.with_enabled(row.enabled)
    }
}

impl Widget for PageWindowMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let menu = self.menu.build_layout(taffy, theme, map);
        // Over the whole page, out of its flow: the menu stands where the click was.
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
        if let Some(&menu) = taffy.children(node).unwrap_or_default().first() {
            self.menu
                .apply_layout(taffy, menu, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Some(&menu) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() else {
            return;
        };
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.menu]
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn name(&self) -> &'static str {
        "PageWindowMenu"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

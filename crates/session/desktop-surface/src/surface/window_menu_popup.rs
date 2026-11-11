//! The window menu as a menu of its own, for a host with no session around its desk: what the
//! desktop can do to one window, opened where a secondary click on that window's bar landed.
//!
//! The rows and what choosing one does are the desk's ([`DesktopHandle::window_menu`],
//! [`DesktopHandle::choose_window_menu`]): a native session shows them through its window-menu
//! view, and a host without one (the web page) mounts this beside its desk, in the toolkit's menu.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::TabId;
use ui::Theme;
use ui::kurbo::{Point, Rect};
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::widget::{ContextMenu, MenuItem, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;

use super::Row;
use crate::DesktopHandle;

/// What [`WindowMenuPopup::follow`] did.
#[derive(Debug, Default)]
pub struct Followed {
    /// The menu opened, closed or did a row: the host draws again.
    pub changed: bool,
    /// The row the person chose, by its id.
    pub chosen: Option<String>,
    /// Why the desk refused that row, as the menu says beside it.
    pub refused: Option<String>,
}

/// The window menu a host mounts over its desk: the desk's rows for the window whose bar asked.
pub struct WindowMenuPopup {
    handle: DesktopHandle,
    menu: ContextMenu,
    /// The window the open menu is for.
    tab: Option<TabId>,
    /// The row the person chose since the menu last settled, by its id.
    chosen: Rc<RefCell<Option<String>>>,
    rect: Rect,
}

impl WindowMenuPopup {
    #[must_use]
    pub fn new(handle: DesktopHandle) -> Self {
        Self {
            handle,
            menu: ContextMenu::new(),
            tab: None,
            chosen: Rc::new(RefCell::new(None)),
            rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Open the menu a window's bar asked for since the last call, where it was pressed, and do
    /// the row the person chose: what happened, for the host to redraw and to say.
    pub fn follow(&mut self) -> Followed {
        let mut followed = Followed::default();
        if let Some((tab, at)) = self.handle.take_tab_menus().pop() {
            followed.changed |= self.open(tab, at);
        }
        let chosen = self.chosen.borrow_mut().take();
        if let (Some(id), Some(tab)) = (chosen, self.tab) {
            self.tab = None;
            self.menu.close();
            followed.refused = self.handle.choose_window_menu(tab, &id, None).err();
            followed.chosen = Some(id);
            followed.changed = true;
        }
        followed
    }

    /// Show the desk's rows for the window showing `tab`, at `at` on the desk.
    fn open(&mut self, tab: TabId, at: Point) -> bool {
        let rows = self.handle.window_menu(tab);
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
    /// that, chosen, is remembered by its id. A row the window cannot take now says why.
    fn item(&self, row: &Row) -> MenuItem {
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

impl Widget for WindowMenuPopup {
    /// A bar that asked for its menu, or a row the person chose, is taken up between frames.
    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let followed = self.follow().changed;
        self.menu.update(state, path) | followed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let menu = self.menu.build_layout(taffy, theme, map);
        // Over the whole desk, out of its flow: the menu stands where the click was.
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
        "WindowMenuPopup"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
#[path = "window_menu_popup_tests.rs"]
mod tests;

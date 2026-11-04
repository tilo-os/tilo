//! The menu a [`VirtualList`]'s rows open: one context menu the list owns, as a tree's rows
//! have.
//!
//! A row's menu (a secondary click, the Menu key, Shift+F10, or assistive technology) is built
//! from what the model says of that row; the list itself has one too, for a press on the space
//! its rows leave. The chosen item comes back with the row's key, `None` for the list's own.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::{ListModel, VirtualList};
use crate::a11y::AccessNode;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::{Edits, Widget};

/// The prefix a list's menu targets carry, namespacing them into the list's own semantics.
pub(super) const MENU_TARGET: &str = "menu:";

type MenuItems<M> = Box<dyn FnMut(&M, Option<&<M as ListModel>::Key>) -> Vec<MenuItem>>;
type MenuChosen<K> = Box<dyn FnMut(Option<K>, usize)>;

/// The list's open menu: what its rows offer, the one it shows, and what it is about.
pub(super) struct ListMenu<M: ListModel> {
    items: MenuItems<M>,
    chosen: MenuChosen<M::Key>,
    menu: ContextMenu,
    /// The row the menu is about: its row's key, or `None` for the list itself.
    target: Option<M::Key>,
}

impl<M: ListModel> VirtualList<M> {
    /// The menu a press, the Menu key or assistive technology opens: `items` says what a row
    /// (`Some`) or the list's empty space (`None`) offers, and `chosen` hears the item the
    /// person took, with the row it was for.
    #[must_use]
    pub fn with_context_menu(
        mut self,
        items: impl FnMut(&M, Option<&M::Key>) -> Vec<MenuItem> + 'static,
        chosen: impl FnMut(Option<M::Key>, usize) + 'static,
    ) -> Self {
        self.menu = Some(ListMenu {
            items: Box::new(items),
            chosen: Box::new(chosen),
            menu: ContextMenu::new(),
            target: None,
        });
        self
    }

    /// Whether the rows' menu is open.
    #[must_use]
    pub fn context_menu_is_open(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.menu.is_open())
    }

    /// Close the rows' menu, its chosen item unheard.
    pub fn close_context_menu(&mut self) {
        if let Some(menu) = &mut self.menu {
            menu.menu.close();
            menu.target = None;
        }
    }

    /// Open the menu for `key`'s row (or the list, `None`) with its corner at `at`. The row is
    /// chosen and given the keyboard first, as a secondary click does. Whether a menu opened:
    /// none is offered, or it has no items for that row.
    pub fn open_context_menu(&mut self, key: Option<&M::Key>, at: Point) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        let items = (menu.items)(&self.model, key);
        if items.is_empty() {
            return false;
        }
        let mut opened = ContextMenu::new();
        for item in items {
            opened.push(item);
        }
        opened.open_at([crate::num::f32_of_f64(at.x), crate::num::f32_of_f64(at.y)]);
        menu.menu = opened;
        menu.target = key.cloned();
        true
    }

    /// Open the menu on the row of `key`, with its corner at `at`: what a host's row action
    /// does, and what a secondary click does. The row becomes the selection and the focus
    /// first. Whether a menu opened.
    pub fn open_row_menu(&mut self, key: M::Key, at: Point) -> bool {
        if !self.selection.is_selected(&key) {
            self.apply_selection(key.clone(), false, false);
        }
        self.focus_row(key.clone());
        self.open_context_menu(Some(&key), at)
    }

    /// Route `event` to the open menu. Returns whether it consumed it; a press outside closes
    /// the menu and falls through to the rows.
    pub(super) fn route_menu(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        if !menu.menu.is_open() {
            return false;
        }
        let mut consumed = false;
        menu.menu.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut consumed,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        self.drain_menu_selection();
        if !self.context_menu_is_open()
            && let Some(menu) = &mut self.menu
        {
            menu.target = None;
        }
        if consumed {
            *ctx.handled = true;
        }
        consumed
    }

    /// Report the item the person took, if the menu closed on one.
    pub(super) fn drain_menu_selection(&mut self) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        let Some(index) = menu.menu.take_selection() else {
            return;
        };
        let target = menu.target.clone();
        if !menu.menu.is_open() {
            menu.target = None;
        }
        (menu.chosen)(target, index);
    }

    /// Paint the open menu over the rows.
    pub(super) fn paint_menu(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(menu) = &mut self.menu {
            menu.menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Whether the open menu needs a frame: it arrives, leaves, or a key moved its highlight.
    /// A closed menu paints nothing, so it asks for none.
    pub(super) fn menu_needs_redraw(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.menu.is_open() && menu.menu.needs_redraw())
    }

    /// The open menu's semantics, its targets namespaced into the list's.
    pub(super) fn menu_node(&self) -> Option<AccessNode> {
        let menu = self.menu.as_ref()?;
        if !menu.menu.is_open() {
            return None;
        }
        let mut node = Widget::accessibility(&menu.menu)?;
        namespace(&mut node);
        Some(node)
    }

    /// Handle a semantic action addressed to the open menu's item, namespaced into the list's.
    /// `None` when the target is not one of the menu's.
    pub(super) fn perform_menu_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> Option<bool> {
        let target = target.strip_prefix(MENU_TARGET)?;
        let menu = self.menu.as_mut()?;
        let performed = menu
            .menu
            .perform_semantic_action(target, action, state, path);
        if performed {
            self.drain_menu_selection();
        }
        Some(performed)
    }

    /// The room the open menu takes, as the modal capture a host checks before it places
    /// anything over the list.
    pub(super) fn menu_capture(&self) -> Option<Rect> {
        self.menu
            .as_ref()
            .and_then(|menu| menu.menu.modal_capture())
    }
}

fn namespace(node: &mut AccessNode) {
    if let Some(target) = node.action_target.take() {
        let target = format!("{MENU_TARGET}{target}");
        node.semantic_key = Some(target.clone());
        node.action_target = Some(target);
    }
    for child in &mut node.children {
        namespace(child);
    }
}

impl<M: ListModel> ListMenu<M> {
    /// The menu was drawn: its arrival, its highlight and its layout are on screen.
    pub(super) fn menu_drawn(&mut self) {
        crate::widget::Widget::mark_drawn(&mut self.menu);
    }
}

//! The menu a [`VirtualList`]'s rows open: one context menu the list owns, as a tree's rows
//! have, or the report of it to the host, which opens and paints its own.
//!
//! A list owns one of the two ([`RowMenus`]): a row's menu (a secondary click, the Menu key,
//! Shift+F10, or assistive technology) is either built from what the model says of that row —
//! the list itself has one too, for a press on the space its rows leave, and the chosen item
//! comes back with the row's key, `None` for the list's own — or reported to the host with the
//! row and the point the host anchors its menu at.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::Choosing;
use super::{ListModel, VirtualList};
use crate::a11y::AccessNode;
use crate::presence::PresenceCtx;
use crate::shared_selection::SharedSelection;
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
    /// What a row is about, as its own record names it: the app says, the list only knows keys.
    about: Option<RowAbout<M::Key>>,
    menu: ContextMenu,
    /// The row the menu is about: its row's key, or `None` for the list itself.
    target: Option<M::Key>,
}

/// What a row is about: the selection the app names for the row's key, so the send row of the
/// menu opened on it opens a box about the row.
pub(super) type RowAbout<K> = Box<dyn Fn(&K) -> Option<SharedSelection>>;

impl<M: ListModel> ListMenu<M> {
    /// The menu this list paints, as it stands.
    #[cfg(test)]
    pub(super) fn context_menu(&self) -> &ContextMenu {
        &self.menu
    }

    /// The menu this list paints, to choose one of its rows.
    #[cfg(test)]
    pub(super) fn context_menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }
}

/// Where a host opens its own menu for a row: the row's key, its box on screen, and the point
/// the menu opens at.
pub(super) type RowMenuReport<K> = Box<dyn FnMut(K, Rect, Point)>;

/// What a list offers when a row's menu is asked for: a menu it owns and paints, or a report to
/// the host, which opens its own. One list has one of them.
pub(super) enum RowMenus<M: ListModel> {
    /// No row offers a menu.
    None,
    /// The list builds one menu from its model and paints it over the rows
    /// ([`VirtualList::with_context_menu`]).
    Owned(ListMenu<M>),
    /// The host opens its own menu where the list reports a row
    /// ([`VirtualList::with_row_menu_reporter`]).
    Reported(RowMenuReport<M::Key>),
    /// A [`ContextMenuArea`](crate::widget::ContextMenuArea) around the list opens the menu
    /// where a secondary click lands; the list says which rows offer one
    /// ([`VirtualList::with_menu_around`]).
    Around(RowOffersMenu<M>),
}

/// Whether the row of a key offers a menu, read off the model.
pub(super) type RowOffersMenu<M> = Box<dyn Fn(&M, &<M as ListModel>::Key) -> bool>;

impl<M: ListModel> RowMenus<M> {
    /// Whether a row offers a menu: the list owns one, or reports it to the host.
    pub(super) fn offers_row_menu(&self) -> bool {
        matches!(self, Self::Owned(_) | Self::Reported(_))
    }

    /// Whether the row `key` of `model` offers a menu, whoever opens it: assistive technology and
    /// a tool may ask that row for it.
    pub(super) fn row_offers(&self, model: &M, key: &M::Key) -> bool {
        match self {
            Self::None => false,
            Self::Owned(_) | Self::Reported(_) => true,
            Self::Around(offers) => offers(model, key),
        }
    }

    /// The menu the list owns, while it owns one.
    pub(super) fn owned(&self) -> Option<&ListMenu<M>> {
        match self {
            Self::Owned(menu) => Some(menu),
            _ => None,
        }
    }

    /// The menu the list owns, while it owns one.
    pub(super) fn owned_mut(&mut self) -> Option<&mut ListMenu<M>> {
        match self {
            Self::Owned(menu) => Some(menu),
            _ => None,
        }
    }
}

impl<M: ListModel> VirtualList<M> {
    /// The menu a press, the Menu key or assistive technology opens: `items` says what a row
    /// (`Some`) or the list's empty space (`None`) offers, and `chosen` hears the item the
    /// person took, with the row it was for. A list offers its rows one menu: this replaces the
    /// menu the host opens ([`Self::with_row_menu_reporter`]).
    #[must_use]
    pub fn with_context_menu(
        mut self,
        items: impl FnMut(&M, Option<&M::Key>) -> Vec<MenuItem> + 'static,
        chosen: impl FnMut(Option<M::Key>, usize) + 'static,
    ) -> Self {
        self.menus = RowMenus::Owned(ListMenu {
            items: Box::new(items),
            chosen: Box::new(chosen),
            about: None,
            menu: ContextMenu::new(),
            target: None,
        });
        self
    }

    /// Name what each row is about: `about` answers a row's key with the selection the app keeps
    /// for it (a task's issue, a note, a file), so a menu opened on that row carries it as its
    /// subject — the send row's box starts with it, and stands at the row.
    /// The list only knows the row's key, so the app names the record.
    #[must_use]
    pub fn with_row_about(
        mut self,
        about: impl Fn(&M::Key) -> Option<SharedSelection> + 'static,
    ) -> Self {
        if let Some(menu) = self.menus.owned_mut() {
            menu.about = Some(Box::new(about));
        }
        self
    }

    /// Report a row's menu to the host, which opens and paints its own: `report` hears the
    /// row's key, the row's box on screen and the point its menu opens at, when a secondary
    /// click, the Menu key, Shift+F10 or assistive technology asks for the row's menu. The row
    /// takes the keyboard first, and the list changes no selection: what the menu acts on is
    /// the host's. A list offers its rows one menu: this replaces the menu the list builds
    /// ([`Self::with_context_menu`]).
    #[must_use]
    pub fn with_row_menu_reporter(
        mut self,
        report: impl FnMut(M::Key, Rect, Point) + 'static,
    ) -> Self {
        self.menus = RowMenus::Reported(Box::new(report));
        self
    }

    /// Say which rows offer the menu a [`ContextMenuArea`](crate::widget::ContextMenuArea)
    /// around the list opens: the list opens none itself, and a row's menu asked for by assistive
    /// technology or a tool is a secondary click where the row stands, as a person's is.
    #[must_use]
    pub fn with_menu_around(mut self, offers: impl Fn(&M, &M::Key) -> bool + 'static) -> Self {
        self.menus = RowMenus::Around(Box::new(offers));
        self
    }

    /// Whether the rows' menu is open.
    #[must_use]
    pub fn context_menu_is_open(&self) -> bool {
        self.menus.owned().is_some_and(|menu| menu.menu.is_open())
    }

    /// Whether the box that took the menu's place stands open.
    #[must_use]
    pub fn send_to_alone_open(&self) -> bool {
        self.menus
            .owned()
            .is_some_and(|menu| menu.menu.send_to_alone_open())
    }

    /// Close the rows' menu, its chosen item unheard. A list that reports its row menus to the
    /// host has none of its own to close.
    pub fn close_context_menu(&mut self) {
        if let Some(menu) = self.menus.owned_mut() {
            menu.menu.close();
            menu.target = None;
        }
    }

    /// Open the list's own menu for `key`'s row (or the list, `None`) with its corner at `at`.
    /// The row is chosen and given the keyboard first, as a secondary click does. Whether a
    /// menu opened: none is offered, or it has no items for that row.
    pub fn open_context_menu(&mut self, key: Option<&M::Key>, at: Point) -> bool {
        // What the menu is about, and where that stands on screen, read before the menu is
        // taken: the subject's box is the row's, so the send row's box stands at the row.
        let row = key
            .and_then(|key| self.model.index_of(key))
            .and_then(|index| self.row_box(index));
        let subject = key.and_then(|key| {
            let menu = self.menus.owned()?;
            let about = menu.about.as_ref()?;
            let mut subject = about(key)?;
            subject.bounds = subject.bounds.or(row);
            Some(subject)
        });
        if key.is_some() && subject.is_none() {
            tracing::debug!("this row's menu is about nothing: the app named no subject for it");
        }
        let Some(menu) = self.menus.owned_mut() else {
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
        opened.set_about(subject);
        opened.open_at([crate::num::f32_of_f64(at.x), crate::num::f32_of_f64(at.y)]);
        menu.menu = opened;
        menu.target = key.cloned();
        true
    }

    /// Open the menu on the row of `key`, with its corner at `at`: what a host's row action
    /// does, and what a secondary click does. A list that owns its menu makes the row its
    /// selection and focus first; one that reports its row menus gives the row the keyboard and
    /// hands its host the box and the point. Whether a menu opened.
    pub fn open_row_menu(&mut self, key: M::Key, at: Point) -> bool {
        if matches!(self.menus, RowMenus::Reported(_)) {
            let Some(row) = self
                .model
                .index_of(&key)
                .and_then(|index| self.row_box(index))
            else {
                return false;
            };
            self.focus_row(key.clone());
            if let RowMenus::Reported(report) = &mut self.menus {
                report(key, row, at);
            }
            return true;
        }
        if self.menus.owned().is_none() {
            return false;
        }
        if !self.selection.is_selected(&key) {
            self.apply_selection(key.clone(), false, false, Choosing::ByHand);
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
        let Some(menu) = self.menus.owned_mut() else {
            return false;
        };
        if !menu.menu.is_showing() {
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
            && let Some(menu) = self.menus.owned_mut()
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
        let Some(menu) = self.menus.owned_mut() else {
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
        if let Some(menu) = self.menus.owned_mut() {
            menu.menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Whether the open menu needs a frame: it arrives, leaves, or a key moved its highlight.
    /// A closed menu paints nothing, so it asks for none.
    pub(super) fn menu_needs_redraw(&self) -> bool {
        self.menus.owned().is_some_and(|menu| {
            (menu.menu.is_open() && menu.menu.needs_redraw()) || menu.menu.send_to().is_drawn()
        })
    }

    /// The open menu's semantics, its targets namespaced into the list's.
    pub(super) fn menu_node(&self) -> Option<AccessNode> {
        let menu = self.menus.owned()?;
        if !menu.menu.is_open() && !menu.menu.send_to_alone_drawn() {
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
        let menu = self.menus.owned_mut()?;
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
        self.menus
            .owned()
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

//! A context menu's query field: what typing into the menu does.
//!
//! A menu the context-menu gesture opened (a secondary click, a long press,
//! the Menu key: [`super::note_input`]) takes typing when it has many rows or
//! when a sentence can be asked of something: a query field at its top
//! narrows its entries with the ranking every filtered list shares
//! (`tilo_ui_base::text_match::ranked_rows`), and Enter runs the best match. Its last
//! row, "Send to…", stands there before anything is typed: it opens the
//! send-to box in the menu's place ([`crate::widget::SendToBox`]), carrying what
//! was typed into its message, and the box sends it to whom the person names.

use std::cell::Cell;
use tilo_ui_base::{i18n::widget_format, i18n::widget_text};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::{
    CONTEXT_GESTURE, ContextMenu, MenuItem, PADDING, QUERY_GAP, QUERY_MIN_ROWS, label_inset,
};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::text_selection::Selection;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{
    query_text::QueryComposition, query_text::QueryEventChange, query_text::QueryInk,
    query_text::QueryView,
};

/// A menu's query field: the typed text and its editing state.
#[derive(Default)]
pub(super) struct MenuQuery {
    pub(super) text: String,
    pub(super) selection: Selection,
    pub(super) composition: QueryComposition,
    /// The search index the rows are ranked with.
    pub(super) index: tilo_ui_base::text_match::ListIndex,
}

impl ContextMenu {
    /// The owner answers a sentence typed into the menu itself, draining it
    /// with [`Self::take_ask`], and the menu takes typing however it was
    /// opened. For a host with its own composer (the shell's menus); an
    /// app's menus open the send-to box with no code of their own.
    pub fn set_owner_asks(&mut self, asks: bool) {
        if self.owner_asks != asks {
            self.owner_asks = asks;
            self.dirty = true;
        }
    }

    /// The owner delivers what its box sends itself ([`Self::set_owner_sends`]): the menu then
    /// offers the row that sends what it is about, and opens the box with no app runner behind
    /// it.
    pub fn set_owner_sends(&mut self, sends: bool) {
        if self.owner_sends != sends {
            self.owner_sends = sends;
            self.dirty = true;
        }
    }

    /// The sentence the person asked, exactly once, for an owner that
    /// answers it ([`Self::set_owner_asks`]). The menu has closed.
    pub fn take_ask(&mut self) -> Option<String> {
        self.asked.take()
    }

    /// What is typed into the menu's query field; empty on a menu that
    /// takes no typing.
    pub fn query(&self) -> &str {
        self.query.as_ref().map_or("", |query| query.text.as_str())
    }

    /// Whether the menu has a query field: it was opened by the
    /// context-menu gesture, or its owner answers what is typed.
    pub fn takes_typing(&self) -> bool {
        self.query.is_some()
    }

    /// Replace what is typed, as the person typing it would.
    pub fn set_query(&mut self, text: &str) {
        let Some(query) = self.query.as_mut() else {
            return;
        };
        text.clone_into(&mut query.text);
        query.selection = Selection::collapsed(query.text.len());
        query.composition.clear();
        self.query_edited();
    }

    /// The rows shown, in the order shown.
    pub fn shown_rows(&self) -> &[usize] {
        &self.shown
    }

    /// Give a menu the context-menu gesture opened (or one whose owner
    /// answers typing) its query field, empty, and the row that asks what is
    /// typed when something can answer it; replaces what an earlier opening
    /// added. A menu of no more than [`QUERY_MIN_ROWS`] rows that nothing can
    /// answer a sentence for takes none: a short menu of commands is read at a
    /// glance, and the field would only push them down.
    pub(super) fn prepare_query(&mut self) {
        if let Some(own) = self.own_rows.take() {
            self.items.truncate(own);
        }
        self.pinned_top = None;
        self.asked = None;
        self.send_to
            .set_about(crate::shared_selection::menu_subject(self.about.clone()));
        let long = self.items.len() > QUERY_MIN_ROWS;
        let takes_typing =
            self.owner_asks || (CONTEXT_GESTURE.with(Cell::get) && (long || self.can_ask()));
        self.query = takes_typing.then(Box::<MenuQuery>::default);
        if self.query.is_some() && self.can_ask() {
            // Set off from the owner's rows, after all of them: the indices
            // the owner hears its choices by stay its own.
            self.own_rows = Some(self.items.len());
            self.items.push(MenuItem::separator());
            self.items
                .push(MenuItem::action(widget_text("menu-send"), || {}).with_icon(Icon::Send));
        }
        self.refilter();
        self.dirty = true;
    }

    /// The owner's rows: every row but the one the menu adds itself.
    pub(super) fn owner_items(&self) -> &[MenuItem] {
        let owned = self
            .own_rows
            .unwrap_or(self.items.len())
            .min(self.items.len());
        self.items.get(..owned).unwrap_or_default()
    }

    /// Whether a sentence typed into the menu can be sent from it.
    pub(super) fn can_ask(&self) -> bool {
        self.owner_asks || self.sends()
    }

    /// Whether what the box sends reaches somewhere: an app runner under this same process hands
    /// it to the shell, or the owner hands it to the session itself.
    fn sends(&self) -> bool {
        self.owner_sends || crate::shared_selection::send_host_available()
    }

    /// The row that opens the send-to box, when the menu has one: the last,
    /// after the rule that sets it off.
    pub(super) fn ask_row(&self) -> Option<usize> {
        self.own_rows
            .filter(|&own| own < self.items.len())
            .map(|_| self.items.len() - 1)
    }

    /// The query's text, trimmed; empty without one.
    pub(super) fn needle(&self) -> String {
        self.query
            .as_ref()
            .map(|query| query.text.trim().to_lowercase())
            .unwrap_or_default()
    }

    /// Work out [`Self::shown`] for the query: every row of the owner's
    /// without one; else its entries that match, best first (a nested menu's
    /// row by its own label); and last, set off by a rule, the send row. A
    /// separator with nothing to set off on either side is not shown: the
    /// rows the menu draws are the rows listed, one for one.
    pub(super) fn refilter(&mut self) {
        self.rank_rows();
        self.shown
            .retain(|&index| super::item_shown(&self.items, index));
        self.sync_rows();
    }

    /// Work out [`Self::shown`] for the query, without showing it.
    fn rank_rows(&mut self) {
        let owned = self
            .own_rows
            .unwrap_or(self.items.len())
            .min(self.items.len());
        let needle = self.needle();
        if needle.is_empty() {
            self.shown = (0..owned).collect();
            if let Some(ask) = self.ask_row().filter(|_| self.sends()) {
                if let Some(MenuItem::Action { label, .. }) = self.items.get_mut(ask) {
                    *label = widget_text("menu-send");
                }
                self.shown.extend([ask - 1, ask]);
            }
            return;
        }
        let Some(query) = self.query.as_ref() else {
            return;
        };
        // Ranked as every list search ranks; separators and captions are
        // never a match.
        self.shown = tilo_ui_base::text_match::ranked_rows(
            &query.index,
            query.text.trim(),
            self.items.iter().take(owned).map(|item| {
                let label = match item {
                    MenuItem::Action { label, .. } => Some(label.as_str()),
                    MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
                };
                label
                    .into_iter()
                    .map(|label| (label, tilo_ui_base::text_match::FieldWeight::TITLE))
            }),
        );
        if let Some(ask) = self.ask_row() {
            // The row carries what is typed into the box, so it names the
            // sentence it sends.
            let text = self
                .query
                .as_ref()
                .map(|query| query.text.trim().to_owned())
                .unwrap_or_default();
            if let Some(MenuItem::Action { label, .. }) = self.items.get_mut(ask) {
                *label = widget_format("menu-ask-typed", &[("text", text.into())]);
            }
            if !self.shown.is_empty() {
                self.shown.push(ask - 1);
            }
            self.shown.push(ask);
        }
    }

    /// The query changed: narrow the rows, put the keyboard on the best
    /// match (else on asking it), and keep the field where it is.
    pub(super) fn query_edited(&mut self) {
        let typed = !self.needle().is_empty();
        if typed && self.pinned_top.is_none() && self.open {
            self.pinned_top = Some(self.rect.y0);
        }
        if !typed {
            self.pinned_top = None;
        }
        self.close_submenu();
        self.refilter();
        self.list.reset_scroll();
        self.opened_under = Some(self.last_cursor);
        let first = if typed {
            self.shown
                .iter()
                .copied()
                .find(|&index| self.items.get(index).is_some_and(MenuItem::is_enabled))
        } else {
            None
        };
        self.set_hover(first);
        self.layout_at_position();
        self.dirty = true;
        tilo_ui_base::widget::note_resource_presentation_changed();
    }

    /// Choose the row that sends what the menu is about: the menu goes, as
    /// for any row, and the send-to box takes its place where it was opened,
    /// holding the sentence typed into the menu, and takes the keyboard. The
    /// box is the menu's, so every host that shows and routes the open menu
    /// shows and routes the box with no code of its own.
    pub(super) fn ask_typed(&mut self) {
        if self.ask_row().is_none() {
            return;
        }
        let text = self
            .query
            .as_ref()
            .map(|query| query.text.trim().to_owned())
            .unwrap_or_default();
        if self.owner_asks && !(text.is_empty() && self.sends()) {
            if text.is_empty() {
                return;
            }
            self.asked = Some(text);
            self.close();
            return;
        }
        let anchor = self.anchor.unwrap_or(self.rect);
        self.close_rows();
        self.alone = Some(anchor);
        self.send_to
            .open((!text.is_empty()).then_some(text.as_str()), &[]);
        self.send_to.place(anchor, self.viewport);
    }

    /// The row marked while its nested menu stands beside the menu.
    pub(super) fn marked_row(&self) -> Option<usize> {
        self.submenu.as_ref().map(|(row, _)| *row)
    }

    /// The hovered row, when it opens a nested menu.
    pub(super) fn hovered_submenu(&self) -> Option<usize> {
        self.hovered.filter(|&idx| {
            self.items
                .get(idx)
                .and_then(MenuItem::submenu_items)
                .is_some()
        })
    }

    /// A press or tap at `point` in the query field puts its caret there.
    /// Whether it was in the field.
    pub(super) fn place_query_caret(&mut self, point: Point) -> bool {
        if !self.query_rect().is_some_and(|field| field.contains(point)) {
            return false;
        }
        if let Some(query) = self.query.as_mut()
            && let Some(selection) = query.composition.pointer_selection(&query.text, point)
        {
            query.selection = selection;
            // The caret moved.
            self.dirty = true;
        }
        true
    }

    /// The query field's height, with the air under it; none on a menu
    /// that takes no typing.
    pub(super) fn query_height(&self) -> f64 {
        if self.query.is_some() {
            self.row_height + QUERY_GAP
        } else {
            0.0
        }
    }

    /// Where the query field is drawn.
    pub(super) fn query_rect(&self) -> Option<Rect> {
        self.query.as_ref()?;
        let inset = PADDING;
        Some(Rect::new(
            self.rect.x0 + inset,
            self.rect.y0 + PADDING,
            self.rect.x1 - inset,
            self.rect.y0 + PADDING + self.row_height,
        ))
    }

    /// Feed a key or IME event to the query field. Whether it took it.
    pub(super) fn edit_query(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let command = self.platform.command_modifier_down(modifiers.state());
        let shift = modifiers.state().shift_key();
        let Some(query) = self.query.as_mut() else {
            return false;
        };
        let change = query.composition.handle_event(
            event,
            &mut query.text,
            &mut query.selection,
            shift,
            command,
        );
        match change {
            QueryEventChange::Ignored => false,
            QueryEventChange::Display => {
                // The composition or the caret changed under the field.
                self.dirty = true;
                true
            }
            QueryEventChange::Edited => {
                self.query_edited();
                true
            }
        }
    }

    /// The query field at the panel's top: a field on the panel, concentric
    /// with it, showing what is typed or what typing does.
    pub(super) fn paint_query(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        panel_radius: f64,
    ) {
        let Some(field) = self.query_rect() else {
            return;
        };
        let placeholder = tilo_ui_base::i18n::I18nManager::lookup_plain("menu-query-placeholder");
        let Some(query) = self.query.as_mut() else {
            return;
        };
        let tokens = &theme.tokens;
        // Concentric with the panel it is inset in, never under 4.
        let radius = (panel_radius - PADDING).max(4.0);
        // A field well is the raised level, which needs no edge.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &vello::kurbo::RoundedRect::from_rect(field, radius),
        );
        let inset = label_inset(theme) - PADDING;
        query.composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &query.text,
                selection: &query.selection,
                placeholder: &placeholder,
                show_caret: self.open,
            },
            Rect::new(field.x0 + inset, field.y0, field.x1 - inset, field.y1),
            &theme.text(TextRole::Body),
            QueryInk::of(&tokens.colors),
        );
    }

    /// The query field's caret while the menu is open.
    pub(super) fn query_caret(&self) -> Option<Rect> {
        self.query
            .as_ref()
            .filter(|_| self.open)
            .and_then(|query| query.composition.ime_cursor_rect())
    }

    /// What the input method may compare the query by.
    pub(super) fn query_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let query = self.query.as_ref().filter(|_| self.open)?;
        query
            .composition
            .ime_edit_fingerprint(&query.text, &query.selection, key)
    }

    /// The query field as assistive technology reads it: a text field whose
    /// value is what is typed, first, as it is drawn.
    pub(super) fn query_access_node(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let query = self.query.as_ref()?;
        Some(
            AccessNode::new(Role::TextInput)
                .with_name(tilo_ui_base::i18n::I18nManager::lookup_plain(
                    "menu-query-name",
                ))
                .with_optional_value(Some(query.text.clone()))
                .with_action_target("context-menu:query".to_owned())
                .with_bounds(self.query_rect().unwrap_or(Rect::ZERO))
                .with_state(AccessState {
                    focusable: true,
                    hidden: !self.open,
                    ..Default::default()
                }),
        )
    }
}

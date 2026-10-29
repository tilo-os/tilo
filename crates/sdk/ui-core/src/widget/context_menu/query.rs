//! A context menu's query field: what typing into the menu does.
//!
//! A menu the context-menu gesture opened (a secondary click, a long press,
//! the Menu key: [`super::note_input`]) takes typing: a query field at its
//! top narrows its entries with the ranking every filtered list shares
//! (`crate::search::ranked_rows`), Enter runs the best match, and a sentence none of
//! them answers is asked of an agent about what the menu is on
//! ([`ContextMenu::set_about`]): through the desktop
//! ([`crate::shared_selection::ask_agent_at`]), or through the menu's owner
//! when it answers itself ([`ContextMenu::set_owner_asks`]).

use std::cell::Cell;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::{CONTEXT_GESTURE, ContextMenu, MenuItem, PADDING, QUERY_GAP, label_inset};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// A menu's query field: the typed text and its editing state.
#[derive(Default)]
pub(super) struct MenuQuery {
    pub(super) text: String,
    pub(super) selection: Selection,
    pub(super) composition: QueryComposition,
    /// The search index the rows are ranked with.
    pub(super) index: crate::search::ListIndex,
}

impl ContextMenu {
    /// The owner answers a sentence typed into the menu itself, draining it
    /// with [`Self::take_ask`], and the menu takes typing however it was
    /// opened. For a host with its own agent composer (the desktop's menus);
    /// an app's menus ask through the desktop with no code of their own.
    pub fn set_owner_asks(&mut self, asks: bool) {
        self.owner_asks = asks;
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
    /// added.
    pub(super) fn prepare_query(&mut self, at: Point) {
        if let Some(own) = self.own_rows.take() {
            self.items.truncate(own);
        }
        self.pinned_top = None;
        self.asked = None;
        self.ask_at = at;
        self.query =
            (self.owner_asks || CONTEXT_GESTURE.with(Cell::get)).then(Box::<MenuQuery>::default);
        if self.query.is_some() && self.can_ask() {
            self.own_rows = Some(self.items.len());
            self.items
                .push(MenuItem::action(String::new(), || {}).with_icon(Icon::Sparkles));
        }
        self.refilter();
    }

    /// The owner's rows: every row but the one the menu adds itself.
    pub(super) fn owner_items(&self) -> &[MenuItem] {
        let owned = self
            .own_rows
            .unwrap_or(self.items.len())
            .min(self.items.len());
        self.items.get(..owned).unwrap_or_default()
    }

    /// Whether a sentence typed into the menu can be asked of an agent.
    pub(super) fn can_ask(&self) -> bool {
        self.owner_asks || crate::shared_selection::agent_host_available()
    }

    /// The row asking the typed sentence, when the menu has one.
    pub(super) fn ask_row(&self) -> Option<usize> {
        self.own_rows.filter(|&own| own < self.items.len())
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
    /// row by its own label), and last the row that asks the sentence.
    pub(super) fn refilter(&mut self) {
        let owned = self
            .own_rows
            .unwrap_or(self.items.len())
            .min(self.items.len());
        let needle = self.needle();
        if needle.is_empty() {
            self.shown = (0..owned).collect();
            return;
        }
        let Some(query) = self.query.as_ref() else {
            return;
        };
        // Ranked as every list search ranks; separators and captions are
        // never a match.
        self.shown = crate::search::ranked_rows(
            &query.index,
            query.text.trim(),
            self.items.iter().take(owned).map(|item| {
                let label = match item {
                    MenuItem::Action { label, .. } => Some(label.as_str()),
                    MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
                };
                label
                    .into_iter()
                    .map(|label| (label, crate::search::FieldWeight::TITLE))
            }),
        );
        if let Some(ask) = self.ask_row() {
            let text = self
                .query
                .as_ref()
                .map(|query| query.text.trim().to_owned())
                .unwrap_or_default();
            if let Some(MenuItem::Action { label, .. }) = self.items.get_mut(ask) {
                *label = crate::i18n!("menu-ask-typed", "text" => text);
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
        self.scroll_offset = 0.0;
        self.opened_under = Some(self.last_cursor);
        self.hovered = if typed {
            self.shown
                .iter()
                .copied()
                .find(|&index| self.items.get(index).is_some_and(MenuItem::is_enabled))
        } else {
            None
        };
        self.layout_at_position();
        crate::widget::note_resource_presentation_changed();
    }

    /// Ask an agent the typed sentence about what the menu is on, and close.
    pub(super) fn ask_typed(&mut self) {
        let text = self
            .query
            .as_ref()
            .map(|query| query.text.trim().to_owned())
            .unwrap_or_default();
        // Only a menu that offered to ask, when it opened, asks.
        if text.is_empty() || self.ask_row().is_none() {
            return;
        }
        if self.owner_asks {
            self.asked = Some(text);
        } else {
            crate::shared_selection::ask_agent_at(self.about.clone(), self.ask_at, &text);
        }
        self.close();
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
            QueryEventChange::Display => true,
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
        let placeholder = crate::i18n::I18nManager::lookup_plain(if self.can_ask() {
            "menu-query-placeholder-ask"
        } else {
            "menu-query-placeholder"
        });
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
    pub(super) fn query_access_node(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let query = self.query.as_ref()?;
        Some(
            AccessNode::new(Role::TextInput)
                .with_name(crate::i18n::I18nManager::lookup_plain("menu-query-name"))
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

//! Slides' controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the slide list's toggle in the sidebar slot, the deck's title as the
//! editable location, Editor and Overview as the views, and Insert, Present
//! and the properties' toggle as actions. Each runs the deck's published
//! action for it, so the bar, assistive technology and automation go through
//! one door. The editor draws no header row of its own.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::editor_chrome::insert_tools;
use super::{ElementKind, Hit, Mode, SlidesApp, SlidesEffect, tr};

/// The bar's menu of insert tools.
pub(crate) const INSERT: &str = "slides.insert";
/// The bar action that starts the presentation.
pub(crate) const PRESENT: &str = "slides.present";
/// The bar action that shows and hides the properties.
pub(crate) const PROPERTIES: &str = "slides.properties";
/// The bar's views: the slide being edited, or every slide.
pub(crate) const EDITOR: &str = "slides.view:editor";
pub(crate) const OVERVIEW: &str = "slides.view:overview";

/// The id of the Insert menu's item for `kind`.
pub(crate) fn insert_id(kind: ElementKind) -> String {
    format!("slides.insert:{}", kind.as_str())
}

impl SlidesApp {
    /// The window bar's slots for what the editor shows now; none while it
    /// presents.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        if self.mode == Mode::Present {
            return WindowToolbar::default();
        }
        let editing = self.mode == Mode::Edit;
        let may_edit = !self.dispatch.read_only();
        let mut actions = Vec::new();
        if editing {
            let items = insert_tools()
                .into_iter()
                .map(|(label, icon, kind)| ToolbarAction::new(insert_id(kind), icon.name(), label))
                .collect();
            actions.push(
                ToolbarAction::new(INSERT, "plus", tr("slides-insert"))
                    .with_items(items)
                    .enabled(may_edit && self.slide().is_some()),
            );
        }
        actions.push(
            ToolbarAction::new(PRESENT, "play", tr("slides-a11y-start-presentation"))
                .enabled(!self.deck.slides.is_empty()),
        );
        if editing {
            let open = self.inspector_open();
            actions.push(
                ToolbarAction::new(
                    PROPERTIES,
                    "panel-right",
                    tr(if open {
                        "slides-hide-properties"
                    } else {
                        "slides-show-properties"
                    }),
                )
                .on(open),
            );
        }
        let title = self.deck.title.trim();
        WindowToolbar {
            sidebar: editing.then(|| self.rail_open()),
            location: Some(ToolbarLocation {
                label: if title.is_empty() {
                    tr("slides-untitled")
                } else {
                    title.to_owned()
                },
                value: self.deck.title.clone(),
                placeholder: tr("slides-untitled"),
                editable: may_edit,
                name: tr("slides-deck-name"),
                ..ToolbarLocation::default()
            }),
            views: Some(ToolbarViews {
                name: tr("slides-view"),
                choices: vec![
                    ToolbarView {
                        id: EDITOR.into(),
                        label: tr("slides-view-editor"),
                    },
                    ToolbarView {
                        id: OVERVIEW.into(),
                        label: tr("slides-view-overview"),
                    },
                ],
                chosen: if editing { EDITOR } else { OVERVIEW }.into(),
            }),
            actions,
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the editor shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar, through the deck's
    /// published action for it.
    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        if self.mode == Mode::Present {
            return;
        }
        match event {
            ToolbarEvent::ToggleSidebar => self.dispatch.effect(&SlidesEffect::ShowSlideList {
                shown: !self.rail_open(),
            }),
            ToolbarEvent::Commit { text } => self.rename(text),
            ToolbarEvent::Action { id } => self.run_bar_action(id),
            _ => {}
        }
    }

    fn run_bar_action(&mut self, id: &str) {
        match id {
            EDITOR => self.dispatch.effect(&SlidesEffect::Editor),
            OVERVIEW => self.dispatch.effect(&SlidesEffect::Overview),
            PRESENT => self.dispatch.effect(&SlidesEffect::Present),
            PROPERTIES => self.dispatch.effect(&SlidesEffect::ShowProperties {
                shown: !self.inspector_open(),
            }),
            _ => {
                let tool = insert_tools()
                    .into_iter()
                    .map(|(_, _, kind)| kind)
                    .find(|kind| insert_id(*kind) == id);
                if let Some(kind) = tool
                    && self.mode == Mode::Edit
                {
                    self.apply_hit(Hit::Insert(kind));
                }
            }
        }
    }

    /// Rename the deck to `text`, as the deck's title field edit does; a
    /// blank or unchanged title keeps the deck's.
    fn rename(&self, text: &str) {
        let title = text.trim();
        if title.is_empty() || title == self.deck.title {
            return;
        }
        self.submit(&crate::actions::set_deck_title_action(title));
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;

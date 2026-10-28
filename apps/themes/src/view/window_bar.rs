//! The studio's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the shell's sidebar toggle and Back, what the
//! content shows as the location (the presets, or the theme's name), and
//! the theme's commands: Apply and Copy as file while it is legible, with
//! Duplicate, Reset and Delete theme in the bar's menu. The window draws no
//! header row and no row of commands of its own.

use std::cell::RefCell;
use std::rc::Rc;

use ui::prelude::*;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{Editing, Page, Ui, t};
use crate::effects::ThemesEffect;
use crate::studio::Studio;

pub(crate) const APPLY: &str = "theme-apply";
pub(crate) const EXPORT: &str = "theme-export";
pub(crate) const DUPLICATE: &str = "theme-duplicate";
pub(crate) const RESET: &str = "theme-reset";
pub(crate) const DELETE: &str = "theme-delete";
pub(crate) const IMPORT: &str = "theme-import";

/// The theme the editor shows, for the bar's commands; none in the gallery.
pub(super) type Shown = Rc<RefCell<Option<Editing>>>;

/// The navigation shell, with the studio's slots in the window's bar.
pub(super) struct StudioWindow {
    pub(super) shell: NavShell,
    pub(super) ui: Ui,
}

impl StudioWindow {
    fn editing(&self) -> Option<Editing> {
        self.ui.shown.borrow().clone()
    }

    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let sidebar_in_front =
            self.shell.geometry().stack.len() > 1 && self.shell.front_pane() == NavPane::Sidebar;
        let editing = self
            .editing()
            .filter(|_| self.ui.page.get() != Page::Gallery);
        let label = match &editing {
            _ if sidebar_in_front => None,
            None => Some(t("themes-presets")),
            Some(editing) => Some(
                self.ui
                    .live
                    .with(|studio| editing.read(studio, |theme| theme.name.trim().to_owned()))
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| t("themes-untitled")),
            ),
        };
        toolbar.location = label.map(|label| ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        let mut actions = vec![ToolbarAction::new(IMPORT, "upload", t("themes-import")).in_menu()];
        if let Some(editing) = editing {
            let raw = editing.id.as_str().to_owned();
            let legible = self
                .ui
                .live
                .with(|studio| editing.read(studio, |theme| theme.findings(&raw).is_empty()))
                .unwrap_or(false);
            actions.push(ToolbarAction::new(APPLY, "check", t("themes-apply")).enabled(legible));
            actions
                .push(ToolbarAction::new(EXPORT, "download", t("themes-export")).enabled(legible));
            actions.push(ToolbarAction::new(DUPLICATE, "copy", t("themes-duplicate")).in_menu());
            actions
                .push(ToolbarAction::new(RESET, "rotate-ccw", t("themes-reset-button")).in_menu());
            actions
                .push(ToolbarAction::new(DELETE, "trash-2", t("themes-delete-button")).in_menu());
        }
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then the theme's commands. Returns whether anything was asked.
    fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            if let ToolbarEvent::Action { id } = event {
                if id == IMPORT {
                    self.ui.dispatcher.effect(&ThemesEffect::Import);
                } else if let Some(editing) = self.editing() {
                    self.run(&id, &editing);
                }
            }
        }
        asked
    }

    /// One of the theme's commands, as its control in the editor ran it.
    fn run(&self, id: &str, editing: &Editing) {
        let ui = &self.ui;
        let legible = || {
            let raw = editing.id.as_str().to_owned();
            ui.live
                .with(|studio| editing.read(studio, |theme| theme.findings(&raw).is_empty()))
                .unwrap_or(false)
        };
        match id {
            IMPORT => {
                ui.dispatcher.effect(&ThemesEffect::Import);
            }
            APPLY if legible() => ui.apply(editing.reference()),
            EXPORT if legible() => ui.export(editing.reference()),
            DUPLICATE => ui.start_from(&editing.reference()),
            RESET => {
                ui.dispatcher
                    .submit(editing.invoke(Studio::RESET.with(editing.id.clone())));
            }
            DELETE => {
                // A draft never sent has nothing to delete: leaving it is
                // enough.
                if editing.sent() {
                    ui.dispatcher
                        .submit(Studio::DELETE.with(editing.id.clone()));
                }
                ui.open(Page::Gallery);
            }
            _ => {}
        }
    }
}

impl ui::widget::Decorator for StudioWindow {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.take_window_toolbar_events();
        ui::window_toolbar::declare(Some(self.window_toolbar()));
        Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let asked = self.take_window_toolbar_events();
        ui::window_toolbar::declare(Some(self.window_toolbar()));
        Widget::update(&mut self.shell, state, path) || asked
    }

    fn name(&self) -> &'static str {
        "StudioWindow"
    }
}

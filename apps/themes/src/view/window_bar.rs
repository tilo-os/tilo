//! The studio's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the shell's sidebar toggle and Back, what the
//! content shows as the location (the presets, or the theme's name), and
//! the theme's commands: Apply and Copy as file while it is legible, with
//! Duplicate, Reset and Delete theme in the bar's menu. The window draws no
//! header row and no row of commands of its own.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{BarActions, BarCommands};
use ui::prelude::*;
use ui::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};

use super::{Editing, Page, Ui, t};
use crate::effects::ThemesEffect;
use crate::studio::Studio;

/// What the bar's entries do that is no effect of the studio: they act on
/// the theme the editor shows when pressed.
#[derive(Clone, Copy, PartialEq)]
enum Command {
    Duplicate,
    Reset,
    Delete,
}

/// The theme the editor shows, for the bar's commands; none in the gallery.
pub(super) type Shown = Rc<RefCell<Option<Editing>>>;

/// The navigation shell, with the studio's slots in the window's bar.
pub(super) struct StudioWindow {
    shell: NavShell,
    ui: Ui,
    actions: BarActions<Studio>,
    commands: BarCommands<Command>,
}

impl StudioWindow {
    pub(super) fn new(shell: NavShell, ui: Ui) -> Self {
        Self {
            shell,
            actions: BarActions::new(ui.dispatcher.clone()),
            commands: BarCommands::new(),
            ui,
        }
    }

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
        let bar = &self.actions;
        let commands = &self.commands;
        let mut actions = vec![
            ToolbarAction {
                name: t("themes-import"),
                icon: "upload".into(),
                ..bar.effect(&ThemesEffect::Import)
            }
            .in_menu(),
        ];
        if let Some(editing) = editing {
            let raw = editing.id.as_str().to_owned();
            let legible = self
                .ui
                .live
                .with(|studio| editing.read(studio, |theme| theme.findings(&raw).is_empty()))
                .unwrap_or(false);
            let theme = editing.reference();
            let apply = bar.effect(&ThemesEffect::ApplyTheme {
                theme: theme.clone(),
            });
            let enabled = apply.enabled && legible;
            actions.push(
                ToolbarAction {
                    name: t("themes-apply"),
                    icon: "check".into(),
                    ..apply
                }
                .enabled(enabled),
            );
            let export = bar.effect(&ThemesEffect::Export { theme });
            let enabled = export.enabled && legible;
            actions.push(
                ToolbarAction {
                    name: t("themes-export"),
                    icon: "download".into(),
                    ..export
                }
                .enabled(enabled),
            );
            actions.push(
                commands
                    .entry("copy", t("themes-duplicate"), Command::Duplicate)
                    .in_menu(),
            );
            actions.push(
                commands
                    .entry("rotate-ccw", t("themes-reset-button"), Command::Reset)
                    .in_menu(),
            );
            actions.push(
                commands
                    .entry("trash-2", t("themes-delete-button"), Command::Delete)
                    .in_menu(),
            );
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
            if self.actions.press(&event) {
                continue;
            }
            if let Some(command) = self.commands.pressed(&event)
                && let Some(editing) = self.editing()
            {
                self.run(command, &editing);
            }
        }
        asked
    }

    /// One of the theme's commands, as its control in the editor ran it.
    fn run(&self, command: Command, editing: &Editing) {
        let ui = &self.ui;
        match command {
            Command::Duplicate => ui.start_from(&editing.reference()),
            Command::Reset => {
                ui.dispatcher
                    .submit(editing.invoke(Studio::RESET.with(editing.id.clone())));
            }
            Command::Delete => {
                // A draft never sent has nothing to delete: leaving it is
                // enough.
                if editing.sent() {
                    ui.dispatcher
                        .submit(Studio::DELETE.with(editing.id.clone()));
                }
                ui.open(Page::Gallery);
            }
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

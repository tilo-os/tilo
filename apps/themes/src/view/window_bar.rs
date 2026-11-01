//! The studio's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the shell's sidebar toggle and Back, what the
//! content shows as the location (the presets, or the theme's name), and
//! the theme's commands: Apply and Copy as file while it is legible, with
//! Duplicate, Reset and Delete theme in the bar's menu. Each is declared as
//! a binding of the studio's effects or of what the editor shows
//! ([`ui::framework::WindowBar`]). The window draws no header row and no row
//! of commands of its own.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::WindowBar;
use ui::prelude::*;
use ui::window_toolbar::ToolbarLocation;

use super::{Editing, Page, Ui, t};
use crate::effects::ThemesEffect;
use crate::studio::Studio;

/// The theme the editor shows, for the bar's commands; none in the gallery.
pub(super) type Shown = Rc<RefCell<Option<Editing>>>;

/// The navigation shell, with the studio's slots in the window's bar.
pub(super) struct StudioWindow {
    shell: NavShell,
    ui: Ui,
    /// The bar as it was declared last: the press heard now names one of its
    /// slots, then the window declares the bar for what it shows now.
    bar: WindowBar<Studio>,
}

impl StudioWindow {
    pub(super) fn new(shell: NavShell, ui: Ui) -> Self {
        let bar = WindowBar::new(ui.dispatcher.clone());
        Self { shell, ui, bar }
    }

    fn editing(&self) -> Option<Editing> {
        self.ui.shown.borrow().clone()
    }

    /// The window bar's slots for what the window shows now.
    fn window_bar(&self) -> WindowBar<Studio> {
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
        let mut bar = WindowBar::new(self.ui.dispatcher.clone())
            .over(&self.shell)
            .place(label.map(|label| ToolbarLocation {
                label,
                ..ToolbarLocation::default()
            }))
            .effect(&ThemesEffect::Import)
            .named("upload", t("themes-import"))
            .in_menu();
        let Some(editing) = editing else {
            return bar;
        };
        let raw = editing.id.as_str().to_owned();
        let legible = self
            .ui
            .live
            .with(|studio| editing.read(studio, |theme| theme.findings(&raw).is_empty()))
            .unwrap_or(false);
        let theme = editing.reference();
        bar = bar
            .effect(&ThemesEffect::ApplyTheme {
                theme: theme.clone(),
            })
            .named("check", t("themes-apply"))
            .enabled(legible)
            .effect(&ThemesEffect::Export { theme })
            .named("download", t("themes-export"))
            .enabled(legible)
            .effect(&ThemesEffect::DuplicateTheme {
                theme: editing.reference(),
            })
            .named("copy", t("themes-duplicate"))
            .in_menu()
            .action(Studio::RESET.with(editing.id.clone()))
            .named("rotate-ccw", t("themes-reset-button"))
            .in_menu()
            .effect(&ThemesEffect::DeleteTheme {
                theme: editing.id.as_str().to_owned(),
                sent: editing.sent(),
            })
            .named("trash-2", t("themes-delete-button"))
            .in_menu();
        bar
    }

    /// Declare the slots of what the window shows now.
    fn declare(&mut self) {
        self.bar = self.window_bar();
        self.bar.declare();
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
        self.bar.take_events();
        self.declare();
        Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let asked = self.bar.take_events();
        self.declare();
        Widget::update(&mut self.shell, state, path) || asked
    }

    fn name(&self) -> &'static str {
        "StudioWindow"
    }
}

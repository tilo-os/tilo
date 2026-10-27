//! The window's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the shell's sidebar toggle and Back, the open
//! page as the location, New shortcut and Reload shortcuts, and the open
//! page's own: a saved shortcut's Edit (Delete in the menu), a draft's
//! Review and save (Discard draft in the menu). The window draws no header
//! row; a saved shortcut's Run stays with the inputs it is run with.

use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::{DRAFT, OVERVIEW, SAVED, Ui, draft_label, save_blocker};
use crate::door::DoorEffect;
use crate::model::Workbench;

pub(crate) const NEW_SHORTCUT: &str = "new-shortcut";
pub(crate) const RELOAD: &str = "reload";
pub(crate) const EDIT: &str = "edit-shortcut";
pub(crate) const DUPLICATE: &str = "duplicate-shortcut";
pub(crate) const DELETE: &str = "delete-shortcut";
pub(crate) const REVIEW: &str = "review-save";
pub(crate) const DISCARD: &str = "discard-draft";

/// What the open page is, for the bar.
enum Open {
    Overview,
    Draft(Id),
    Saved(String),
}

/// The navigation shell, with the window's slots in its bar.
pub(super) struct ShortcutsWindow {
    pub(super) shell: NavShell,
    pub(super) ui: Ui,
}

impl ShortcutsWindow {
    fn open(&self) -> Open {
        let selected = self
            .ui
            .selected
            .get()
            .unwrap_or_else(|| OVERVIEW.to_owned());
        if let Some(id) = selected.strip_prefix(DRAFT) {
            Open::Draft(Id::from_raw(id))
        } else if let Some(id) = selected.strip_prefix(SAVED) {
            Open::Saved(id.to_owned())
        } else {
            Open::Overview
        }
    }

    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let sidebar_in_front =
            self.shell.geometry().stack.len() > 1 && self.shell.front_pane() == NavPane::Sidebar;
        let open = self.open();
        let label = match &open {
            _ if sidebar_in_front => None,
            Open::Overview => Some(String::from("Overview")),
            Open::Draft(id) => self.ui.live.with(|workbench| {
                workbench
                    .drafts
                    .get(id)
                    .map(|draft| draft_label(&draft.name))
            }),
            Open::Saved(id) => self.ui.door.with(|door| {
                door.library.ready().and_then(|library| {
                    library
                        .iter()
                        .find(|shortcut| &shortcut.id == id)
                        .map(|shortcut| shortcut.name.clone())
                })
            }),
        };
        toolbar.location = label.map(|label| ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        let lookup = I18nManager::lookup_plain;
        let mut actions = vec![
            ToolbarAction::new(NEW_SHORTCUT, "plus", "New shortcut"),
            ToolbarAction::new(RELOAD, "refresh", "Reload shortcuts"),
        ];
        match open {
            Open::Saved(_) => {
                actions.push(ToolbarAction::new(EDIT, "pencil", lookup("shortcuts-edit")));
                actions.push(ToolbarAction::new(
                    DUPLICATE,
                    "copy",
                    lookup("shortcuts-duplicate"),
                ));
                actions.push(
                    ToolbarAction::new(DELETE, "trash-2", lookup("shortcuts-delete")).in_menu(),
                );
            }
            Open::Draft(_) => {
                let savable = self.ui.editor.with(|input| save_blocker(input).is_none());
                actions.push(
                    ToolbarAction::new(REVIEW, "check", lookup("shortcuts-review"))
                        .enabled(savable),
                );
                actions.push(ToolbarAction::new(
                    DUPLICATE,
                    "copy",
                    lookup("shortcuts-duplicate"),
                ));
                actions.push(
                    ToolbarAction::new(DISCARD, "trash-2", lookup("shortcuts-discard-button"))
                        .in_menu(),
                );
            }
            Open::Overview => {}
        }
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then the window's. Returns whether anything was asked.
    fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            if let ToolbarEvent::Action { id } = event {
                self.run(&id);
            }
        }
        asked
    }

    fn run(&self, id: &str) {
        let ui = &self.ui;
        match (id, self.open()) {
            (NEW_SHORTCUT, _) => ui
                .dispatcher
                .submit(Workbench::NEW_DRAFT.with(String::from("New shortcut"))),
            (RELOAD, _) => ui.dispatcher.effect(&DoorEffect::Refresh),
            (EDIT, Open::Saved(id)) => {
                let shortcut = ui.door.with(|door| {
                    door.library
                        .ready()
                        .and_then(|library| library.iter().find(|s| s.id == id).cloned())
                });
                if let Some(json) = shortcut.and_then(|s| serde_json::to_value(s).ok()) {
                    ui.dispatcher.submit(Workbench::OPEN_SHORTCUT.with(json));
                }
            }
            (DELETE, Open::Saved(id)) => ui.dispatcher.effect(&DoorEffect::Delete { shortcut: id }),
            (DUPLICATE, Open::Draft(id)) => {
                ui.dispatcher.submit(Workbench::DUPLICATE_DRAFT.with(id))
            }
            (DUPLICATE, Open::Saved(id)) => {
                let shortcut = ui.door.with(|door| {
                    door.library
                        .ready()
                        .and_then(|library| library.iter().find(|s| s.id == id).cloned())
                });
                if let Some(json) = shortcut.and_then(|s| serde_json::to_value(s).ok()) {
                    ui.dispatcher
                        .submit(Workbench::DUPLICATE_SHORTCUT.with(json));
                }
            }
            (REVIEW, Open::Draft(_)) => {
                if ui.editor.with(|input| save_blocker(input).is_none()) {
                    ui.sheet.set(true);
                }
            }
            (DISCARD, Open::Draft(id)) => ui.dispatcher.submit(Workbench::DISCARD.with(id)),
            _ => {}
        }
    }
}

impl ui::widget::Decorator for ShortcutsWindow {
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
        "ShortcutsWindow"
    }
}

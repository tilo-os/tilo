//! The window's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the shell's sidebar toggle and Back, the open
//! page as the location, New shortcut and Reload shortcuts, and the open
//! page's own: a saved shortcut's Edit (Delete in the menu), a draft's
//! Review and save (Discard draft in the menu). The window draws no header
//! row; a saved shortcut's Run stays with the inputs it is run with.

use ui::framework::{BarActions, BarCommands};
use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};

use super::{DRAFT, OVERVIEW, SAVED, Ui, draft_label, save_blocker};
use crate::door::DoorEffect;
use crate::model::Workbench;

/// What the open page is, for the bar.
enum Open {
    Overview,
    Draft(Id),
    Saved(String),
}

/// What a draft's entries do in the window itself.
#[derive(Clone, Copy, PartialEq)]
enum Command {
    /// Edit the draft as blocks or as its program text.
    ToggleText,
    /// Open the save sheet.
    Review,
}

/// The navigation shell, with the window's slots in its bar.
pub(super) struct ShortcutsWindow {
    shell: NavShell,
    ui: Ui,
    actions: BarActions<Workbench>,
    commands: BarCommands<Command>,
}

impl ShortcutsWindow {
    pub(super) fn new(shell: NavShell, ui: Ui) -> Self {
        Self {
            actions: BarActions::new(ui.dispatcher.clone()),
            commands: BarCommands::new(),
            shell,
            ui,
        }
    }

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

    /// The saved shortcut `id` as the workbench's verbs take it.
    fn saved_json(&self, id: &str) -> Option<serde_json::Value> {
        let shortcut = self.ui.door.with(|door| {
            door.library
                .ready()
                .and_then(|library| library.iter().find(|s| s.id == id).cloned())
        });
        shortcut.and_then(|s| serde_json::to_value(s).ok())
    }

    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let sidebar_in_front =
            self.shell.geometry().stack.len() > 1 && self.shell.front_pane() == NavPane::Sidebar;
        let open = self.open();
        let label = if sidebar_in_front {
            None
        } else {
            self.label(&open)
        };
        toolbar.location = label.map(|label| ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        let actions = self.page_actions(open);
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    /// What the open page is called.
    fn label(&self, open: &Open) -> Option<String> {
        match open {
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
        }
    }

    /// The bar's actions for the open page.
    fn page_actions(&self, open: Open) -> Vec<ToolbarAction> {
        let lookup = I18nManager::lookup_plain;
        let bar = &self.actions;
        let named = |icon: &str, name: String, entry: ToolbarAction| ToolbarAction {
            icon: icon.into(),
            name,
            ..entry
        };
        let mut actions = vec![
            named(
                "plus",
                "New shortcut".into(),
                bar.action(Workbench::NEW_DRAFT.with(String::from("New shortcut"))),
            ),
            named(
                "refresh",
                "Reload shortcuts".into(),
                bar.effect(&DoorEffect::Refresh),
            ),
        ];
        match open {
            Open::Saved(id) => {
                if let Some(json) = self.saved_json(&id) {
                    actions.push(named(
                        "pencil",
                        lookup("shortcuts-edit"),
                        bar.action(Workbench::OPEN_SHORTCUT.with(json.clone())),
                    ));
                    actions.push(named(
                        "copy",
                        lookup("shortcuts-duplicate"),
                        bar.action(Workbench::DUPLICATE_SHORTCUT.with(json)),
                    ));
                }
                actions.push(
                    named(
                        "trash-2",
                        lookup("shortcuts-delete"),
                        bar.effect(&DoorEffect::Delete { shortcut: id }),
                    )
                    .in_menu(),
                );
            }
            Open::Draft(id) => {
                let savable = self.ui.editor.with(|input| save_blocker(input).is_none());
                let as_text = self.ui.as_text.get();
                actions.push(
                    self.commands
                        .entry(
                            if as_text { "list" } else { "code" },
                            lookup(if as_text {
                                "shortcuts-show-blocks"
                            } else {
                                "shortcuts-show-text"
                            }),
                            Command::ToggleText,
                        )
                        .on(as_text),
                );
                actions.push(
                    self.commands
                        .entry("check", lookup("shortcuts-review"), Command::Review)
                        .enabled(savable),
                );
                actions.push(named(
                    "copy",
                    lookup("shortcuts-duplicate"),
                    bar.action(Workbench::DUPLICATE_DRAFT.with(id.clone())),
                ));
                actions.push(
                    named(
                        "trash-2",
                        lookup("shortcuts-discard-button"),
                        bar.action(Workbench::DISCARD.with(id)),
                    )
                    .in_menu(),
                );
            }
            Open::Overview => {}
        }
        actions
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then the window's. Returns whether anything was asked.
    fn take_window_toolbar_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) || self.actions.press(&event) {
                continue;
            }
            let ui = &self.ui;
            match self.commands.pressed(&event) {
                Some(Command::ToggleText) => ui.as_text.set(!ui.as_text.get()),
                Some(Command::Review) if ui.editor.with(|input| save_blocker(input).is_none()) => {
                    ui.sheet.set(true);
                }
                Some(Command::Review) | None => {}
            }
        }
        asked
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

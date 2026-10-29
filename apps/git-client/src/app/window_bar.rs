//! The git client's controls, as slots in the window's bar
//! ([`ui::window_toolbar`]): the repository as the location, Changes, History
//! and Branches as the views, the list column's toggle in the sidebar slot,
//! Fetch, Pull and Push as actions, opening another repository in the bar's
//! menu on every screen, and on the Changes screen the diff's layout and
//! syntax highlighting there too. History is the bar's own control; the
//! client draws no header row.

use ui::framework::RunMode;
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use super::{GitClientState, Screen};
use crate::actions::GitClientEffect;
use crate::i18n::t;
use crate::panels::RemoteAction;
use crate::settings::DiffLayout;

/// What an entry of the window bar does.
#[derive(Clone, PartialEq)]
pub(crate) enum BarCommand {
    /// Fetch, Pull or Push.
    Remote(RemoteAction),
    /// Show the diff in this layout.
    Layout(DiffLayout),
    /// The client's published effect.
    Effect(GitClientEffect),
}

impl GitClientState {
    /// The window bar's slots for what the window shows now; none of the
    /// client's while no repository is open.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        if self.awaiting_repo {
            return WindowToolbar::default();
        }
        let bar = &self.bar;
        let remote = self.can_run_remote();
        let mut actions: Vec<ToolbarAction> = RemoteAction::ALL
            .into_iter()
            .map(|action| {
                bar.entry(
                    action.icon().name(),
                    action.name(),
                    BarCommand::Remote(action),
                )
                .enabled(remote)
            })
            .collect();
        let effect = |icon: &str, name: &str, effect: GitClientEffect| {
            bar.entry(icon, t(name), BarCommand::Effect(effect))
                .in_menu()
        };
        // Every screen has a menu, so the views stand where they stood when
        // the screen changes.
        actions.push(
            effect(
                "folder-open",
                "git-open-title",
                GitClientEffect::ChooseRepository,
            )
            .enabled(self.mode != RunMode::Past),
        );
        if self.screen == Screen::Changes {
            let layout = self.diff_layout();
            let unified = BarCommand::Layout(DiffLayout::Unified);
            let split = BarCommand::Layout(DiffLayout::SideBySide);
            actions.extend([
                bar.entry("list", t("git-diff-unified-name"), unified)
                    .on(layout == DiffLayout::Unified)
                    .in_menu(),
                bar.entry("columns-2", t("git-diff-split-name"), split)
                    .on(layout == DiffLayout::SideBySide)
                    .in_menu(),
                effect("code", "git-syntax-name", GitClientEffect::SyntaxHighlight)
                    .on(self.settings.syntax_highlight),
                // The two controls that make an unreadable diff readable sit
                // beside the layout, not behind them in the palette.
                effect(
                    "ruler",
                    "git-whitespace-name",
                    GitClientEffect::ShowWhitespace,
                )
                .on(self.diff_preference(|settings| settings.show_whitespace)),
                effect("arrow-up-down", "git-wrap-name", GitClientEffect::WrapLines)
                    .on(self.diff_preference(|settings| settings.wrap_lines)),
            ]);
        }
        WindowToolbar {
            // Branches has no list column: its toggle holds its place, so
            // the location stands where it does on the other screens.
            sidebar: Some(self.screen.has_list() && self.drawer.is_open()),
            sidebar_unavailable: !self.screen.has_list(),
            location: Some(self.location()),
            views: Some(ToolbarViews {
                name: t("git-view"),
                choices: Screen::ALL
                    .into_iter()
                    .map(|screen| ToolbarView {
                        id: screen.target().into(),
                        label: screen.label(),
                    })
                    .collect(),
                chosen: self.screen.target().into(),
            }),
            actions,
            ..WindowToolbar::default()
        }
    }

    /// The repository, by name; an editable path but on a past version, so
    /// typing another one opens that repository.
    fn location(&self) -> ToolbarLocation {
        let live = self.mode != RunMode::Past;
        ToolbarLocation {
            label: self.model.name.clone(),
            value: self
                .repo_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            placeholder: if live {
                t("git-repository-path")
            } else {
                String::new()
            },
            name: t("git-repository-path"),
            go_name: if live { t("git-open") } else { String::new() },
            editable: live,
            ..ToolbarLocation::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for what the window shows now.
    pub(super) fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar.
    pub(crate) fn run_window_bar_event(&mut self, event: &ToolbarEvent) {
        if self.awaiting_repo || self.confirm.is_some() {
            return;
        }
        if let Some(command) = self.bar.pressed(event) {
            self.run_bar_command(command);
            return;
        }
        match event {
            ToolbarEvent::ToggleSidebar if self.screen.has_list() => self.drawer.toggle(),
            ToolbarEvent::Action { id } => {
                if let Some(screen) = Screen::ALL.into_iter().find(|screen| screen.target() == id) {
                    let effect = match screen {
                        Screen::Changes => GitClientEffect::ShowChanges,
                        Screen::History => GitClientEffect::ShowHistory,
                        Screen::Panels => GitClientEffect::ShowBranches,
                    };
                    self.dispatch_or(&effect, |app| app.switch_screen(screen));
                }
            }
            ToolbarEvent::Commit { text } if self.mode != RunMode::Past => {
                if let Ok(path) = self.typed_repository(text) {
                    self.dispatch_or(
                        &GitClientEffect::OpenRepository { path: path.clone() },
                        |app| {
                            let _ = app.open_repository_at(&path);
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn run_bar_command(&mut self, command: BarCommand) {
        match command {
            BarCommand::Remote(action) => {
                if self.can_run_remote() {
                    self.apply_remote_action(action);
                }
            }
            BarCommand::Layout(want) => {
                if self.diff_layout() != want {
                    self.dispatch_or(&GitClientEffect::DiffLayout, Self::toggle_diff_layout);
                }
            }
            BarCommand::Effect(GitClientEffect::ChooseRepository) if self.mode == RunMode::Past => {
            }
            BarCommand::Effect(effect) => self.dispatch_or(&effect, |app| {
                if let Some(Err(error)) = app.apply_diff_preference(&effect) {
                    app.last_error = Some(error);
                }
            }),
        }
    }

    /// Run `effect` as the client's published action, or `fallback` where
    /// nothing publishes it.
    fn dispatch_or(&mut self, effect: &GitClientEffect, fallback: impl FnOnce(&mut Self)) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        } else {
            fallback(self);
        }
    }

    /// Whether Fetch, Pull and Push can act: not on a past version, nor
    /// while a transfer runs.
    fn can_run_remote(&self) -> bool {
        self.mode != RunMode::Past && self.remote_job.is_none()
    }

    /// A diff preference as stored, which a press may have changed since the
    /// last sync.
    fn diff_preference(&self, of: impl Fn(&crate::settings::Settings) -> bool) -> bool {
        self.preferences
            .as_ref()
            .map_or_else(|| of(&self.settings), |preferences| of(&preferences.get()))
    }

    /// The diff layout as stored, which a press may have changed since the
    /// last sync.
    fn diff_layout(&self) -> DiffLayout {
        self.preferences
            .as_ref()
            .map_or(self.settings.diff_layout, |preferences| {
                preferences.get().diff_layout
            })
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;

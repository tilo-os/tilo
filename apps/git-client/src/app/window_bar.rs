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

use super::{GitClientApp, Screen};
use crate::actions::GitClientEffect;
use crate::i18n::t;
use crate::panels::RemoteAction;
use crate::settings::DiffLayout;

/// The bar's menu items that choose the diff's layout.
pub(crate) const UNIFIED: &str = "git.diff-layout.unified";
pub(crate) const SPLIT: &str = "git.diff-layout.split";
/// The bar's menu item that turns syntax highlighting on and off.
pub(crate) const SYNTAX: &str = "git.syntax-highlight";
/// The bar's menu item that shows or hides whitespace.
pub(crate) const WHITESPACE: &str = "git.show-whitespace";
/// The bar's menu item that wraps long lines or stops wrapping them.
pub(crate) const WRAP: &str = "git.wrap-lines";
/// The bar's menu item that opens another repository.
pub(crate) const OPEN: &str = "git.choose-repository";

pub(crate) fn remote_id(action: RemoteAction) -> &'static str {
    match action {
        RemoteAction::Fetch => "git.remote.fetch",
        RemoteAction::Pull => "git.remote.pull",
        RemoteAction::Push => "git.remote.push",
    }
}

impl GitClientApp {
    /// The window bar's slots for what the window shows now; none of the
    /// client's while no repository is open.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        if self.awaiting_repo {
            return WindowToolbar::default();
        }
        let remote = self.can_run_remote();
        let mut actions: Vec<ToolbarAction> = RemoteAction::ALL
            .into_iter()
            .map(|action| {
                ToolbarAction::new(remote_id(action), action.icon().name(), action.name())
                    .enabled(remote)
            })
            .collect();
        // Every screen has a menu, so the views stand where they stood when
        // the screen changes.
        actions.push(
            ToolbarAction::new(OPEN, "folder-open", t("git-open-title"))
                .enabled(self.mode != RunMode::Past)
                .in_menu(),
        );
        if self.screen == Screen::Changes {
            let layout = self.diff_layout();
            actions.extend([
                ToolbarAction::new(UNIFIED, "list", t("git-diff-unified-name"))
                    .on(layout == DiffLayout::Unified)
                    .in_menu(),
                ToolbarAction::new(SPLIT, "columns-2", t("git-diff-split-name"))
                    .on(layout == DiffLayout::SideBySide)
                    .in_menu(),
                ToolbarAction::new(SYNTAX, "code", t("git-syntax-name"))
                    .on(self.settings.syntax_highlight)
                    .in_menu(),
                // The two controls that make an unreadable diff readable sit
                // beside the layout, not behind them in the palette.
                ToolbarAction::new(WHITESPACE, "ruler", t("git-whitespace-name"))
                    .on(self.diff_preference(|settings| settings.show_whitespace))
                    .in_menu(),
                ToolbarAction::new(WRAP, "arrow-up-down", t("git-wrap-name"))
                    .on(self.diff_preference(|settings| settings.wrap_lines))
                    .in_menu(),
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
        match event {
            ToolbarEvent::ToggleSidebar if self.screen.has_list() => self.drawer.toggle(),
            ToolbarEvent::Action { id } => self.run_bar_action(id),
            ToolbarEvent::Commit { text } if self.mode != RunMode::Past => {
                self.dispatch_or(
                    &GitClientEffect::OpenRepository { path: text.clone() },
                    |app| {
                        let _ = app.open_repository_at(text);
                    },
                );
            }
            _ => {}
        }
    }

    fn run_bar_action(&mut self, id: &str) {
        if let Some(screen) = Screen::ALL.into_iter().find(|screen| screen.target() == id) {
            let effect = match screen {
                Screen::Changes => GitClientEffect::ShowChanges,
                Screen::History => GitClientEffect::ShowHistory,
                Screen::Panels => GitClientEffect::ShowBranches,
            };
            self.dispatch_or(&effect, |app| app.switch_screen(screen));
            return;
        }
        if let Some(action) = RemoteAction::ALL
            .into_iter()
            .find(|action| remote_id(*action) == id)
        {
            if self.can_run_remote() {
                self.apply_remote_action(action);
            }
            return;
        }
        match id {
            OPEN if self.mode != RunMode::Past => {
                self.dispatch_or(&GitClientEffect::ChooseRepository, |_| {});
            }
            UNIFIED | SPLIT => {
                let want = if id == SPLIT {
                    DiffLayout::SideBySide
                } else {
                    DiffLayout::Unified
                };
                if self.diff_layout() != want {
                    self.dispatch_or(&GitClientEffect::DiffLayout, Self::toggle_diff_layout);
                }
            }
            SYNTAX => {
                self.dispatch_or(
                    &GitClientEffect::SyntaxHighlight,
                    Self::toggle_syntax_highlight,
                );
            }
            WHITESPACE => self.dispatch_or(&GitClientEffect::ShowWhitespace, |app| {
                let before = app.settings.show_whitespace;
                if let Err(error) = app.change_preferences(|settings| {
                    settings.show_whitespace = !before;
                }) {
                    app.last_error = Some(error);
                }
            }),
            WRAP => self.dispatch_or(&GitClientEffect::WrapLines, |app| {
                let before = app.settings.wrap_lines;
                if let Err(error) = app.change_preferences(|settings| {
                    settings.wrap_lines = !before;
                }) {
                    app.last_error = Some(error);
                }
            }),
            _ => {}
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

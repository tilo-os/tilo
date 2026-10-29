//! Music's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the library's toggle, Back while there is somewhere to go
//! back to, the destination (or the album, artist or playlist) as the
//! window's location, and the one action of the page: on Home open an
//! audio file, on Playlists New playlist. Search is the bar's own.

use ui::window_toolbar::{ToolbarLocation, WindowToolbar};

use super::{Action, Music, View};

/// What an entry of the window's bar asks for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BarCommand {
    Back,
    /// Open an audio file, through the window's controls.
    OpenFile,
    /// The window's own action.
    Run(Action),
}

impl Music {
    /// The window bar's slots for what the window shows now.
    fn window_toolbar(&self) -> WindowToolbar {
        // The shell's own: the sidebar's toggle, Back in a narrow window.
        let mut toolbar = self
            .bar
            .as_ref()
            .map(ui::widget::WindowBarLink::slots)
            .unwrap_or_default();
        toolbar.location = Some(ToolbarLocation {
            label: self.view_title(),
            ..ToolbarLocation::default()
        });
        let entries = &self.bar_entries;
        if self.nav.can_go_back() {
            let back = entries.entry("arrow-left", "Back", BarCommand::Back);
            toolbar.leading.push(back.navigating());
        }
        if self.view == View::Home {
            let name = crate::i18n::t("music-open-audio-file");
            toolbar
                .actions
                .push(entries.entry("folder-open", name, BarCommand::OpenFile));
        }
        if self.view == View::Playlists && !self.spotify_catalogue {
            let new = BarCommand::Run(Action::BeginNewPlaylist);
            toolbar
                .actions
                .push(entries.entry("plus", "New playlist", new));
        }
        toolbar
    }

    /// Declare the window bar's slots.
    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run what the person did in the window's bar: the shell's own buttons
    /// through the shell, Back here, Open audio file as the library's action, through the
    /// window's controls, New playlist as the window's own action.
    pub(super) fn take_window_toolbar_events(&mut self, state: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            if self.bar.as_ref().is_some_and(|bar| bar.event(&event)) {
                self.dirty = true;
                continue;
            }
            match self.bar_entries.pressed(&event) {
                Some(BarCommand::Back) => self.back_requested.set(true),
                Some(BarCommand::OpenFile) => {
                    if let Some(invocation) = Self::open_file_invocation() {
                        self.bar_controls.push_back(invocation);
                    }
                }
                Some(BarCommand::Run(action)) => self.dispatch(action, state),
                None => continue,
            }
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Action, Music, NamePrompt, View};

    #[test]
    fn new_playlist_is_the_window_bars_action_on_playlists() {
        let mut app = Music::new();
        let mut doc = crate::seed::sample_document();
        app.reload(&doc);
        app.dispatch(
            Action::Nav(View::Playlists),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.declare_window_toolbar();
        let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
        let new = toolbar
            .actions
            .iter()
            .find(|action| action.name == "New playlist")
            .expect("Playlists offers New playlist in the window bar");
        ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
            id: new.id.clone(),
        });
        app.take_window_toolbar_events(&mut ui::widget::Edits::lend(&mut doc));
        assert!(
            matches!(app.name_prompt, Some(NamePrompt::NewPlaylist)),
            "the bar's New playlist asks for the playlist's name"
        );
    }
}

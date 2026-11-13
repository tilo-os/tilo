//! Music's slots in the window's bar: the library toggle, Back, the location and the page's
//! one action.

use ui::window_toolbar::{ToolbarLocation, WindowToolbar};

use super::{Action, Music, View};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum BarCommand {
    Back,
    /// Open an audio file, through the window's controls.
    OpenFile,
    Run(Action),
}

impl Music {
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
            let back = entries.entry("arrow-left", crate::i18n::t("music-back"), BarCommand::Back);
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
                .push(entries.entry("plus", crate::i18n::t("music-new-playlist"), new));
        }
        toolbar
    }

    pub(super) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run what the person did in the bar.
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

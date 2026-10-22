//! Music in the window's shell (`resource.rs`): the shell keeps the
//! sidebar and the transport, and this window says what they show.
//!
//! The library is the one truth. Each sync this window declares the
//! sidebar's places (the destinations, the person's kept views and the
//! playlists) through a [`NavFeed`], and what plays through a
//! [`TransportFeed`]; what the person chose in either comes back as the
//! window's own actions, never stored on the shell's side.

use ui::LocalState;
use ui::icons::Icon;
use ui::widget::{
    NavAction, NavFeed, NavItem, NavPane, NavSection, TransportArt, TransportEvent, TransportFeed,
    TransportRepeat, TransportState, WindowBarLink,
};

use super::{Action, MusicApp, View};
use crate::model::RepeatMode;

/// The library's destinations in sidebar order: their icon, label and the
/// view they open.
const DESTINATIONS: [(Icon, &str, View); 8] = [
    (Icon::Home, "Home", View::Home),
    (Icon::LayoutGrid, "Browse", View::Browse),
    (Icon::Music, "Songs", View::Songs),
    (Icon::GalleryVerticalEnd, "Albums", View::Albums),
    (Icon::Users, "Artists", View::Artists),
    (Icon::Heart, "Liked songs", View::Liked),
    (Icon::Download, "Downloads", View::Downloads),
    (Icon::ListMusic, "Queue", View::Queue),
];

const PLAYLISTS: &str = "playlists";
const NEW_PLAYLIST: &str = "new-playlist";

/// The sidebar row id of the place that opens `view`, if one does.
pub(super) fn place_id(view: &View) -> Option<String> {
    match view {
        View::Kept(id) => Some(format!("music.kept:{id}")),
        View::Playlist(id) => Some(format!("music.playlist:{id}")),
        view => DESTINATIONS
            .iter()
            .position(|(_, _, candidate)| candidate == view)
            .map(|index| format!("music.place:{index}")),
    }
}

/// The view the sidebar row `id` opens.
pub(super) fn place_view(id: &str) -> Option<View> {
    if let Some(kept) = id.strip_prefix("music.kept:") {
        return Some(View::Kept(kept.to_owned()));
    }
    if let Some(playlist) = id.strip_prefix("music.playlist:") {
        return Some(View::Playlist(playlist.to_owned()));
    }
    let index: usize = id.strip_prefix("music.place:")?.parse().ok()?;
    DESTINATIONS.get(index).map(|(_, _, view)| view.clone())
}

impl MusicApp {
    /// This window in a shell: its sidebar shows what `places` shows, its
    /// band's transport what `transport` shows, the shell's own buttons
    /// join the window bar through `bar`, and `pane` is the shell's front
    /// screen in a narrow window.
    #[must_use]
    pub fn in_shell(
        mut self,
        places: &NavFeed,
        transport: &TransportFeed,
        bar: WindowBarLink,
        pane: &LocalState<NavPane>,
    ) -> Self {
        self.places = places.clone();
        self.transport = transport.clone();
        self.bar = Some(bar);
        self.pane = Some(pane.clone());
        self
    }

    /// Declare the sidebar's places and what plays. A page this window
    /// opened by itself (an album from a song, a search) comes to the
    /// front of a narrow window, as one the sidebar opened does.
    pub(super) fn show_shell(&mut self) {
        if self.shown_view.as_ref() != Some(&self.view) {
            if self.shown_view.is_some()
                && let Some(pane) = &self.pane
            {
                pane.set(NavPane::Content);
            }
            self.shown_view = Some(self.view.clone());
        }
        self.show_places();
        self.show_transport();
    }

    /// Hand the shell's sidebar the destinations, the person's kept views
    /// and the playlists, with the one that is open chosen.
    fn show_places(&self) {
        let destinations = DESTINATIONS
            .iter()
            .enumerate()
            .map(|(index, (icon, label, _))| {
                NavItem::new(format!("music.place:{index}"), *label).icon(*icon)
            })
            .collect();
        let mut sections = vec![NavSection::untitled("library", destinations)];
        let kept: Vec<NavItem> = self
            .kept_views()
            .kept_views
            .into_iter()
            .filter_map(|view| {
                let id = place_id(&View::Kept(view.id))?;
                Some(NavItem::new(id, view.name).icon(Icon::Bookmark))
            })
            .collect();
        if !kept.is_empty() {
            sections.push(NavSection::new("kept", "Your views", kept));
        }
        let playlists = self
            .lib
            .playlists()
            .filter_map(|playlist| {
                let id = place_id(&View::Playlist(playlist.id.clone()))?;
                let count = u32::try_from(playlist.len()).unwrap_or(u32::MAX);
                Some(NavItem::new(id, playlist.name.clone()).count(count))
            })
            .collect();
        sections.push(
            NavSection::new(PLAYLISTS, "Playlists", playlists).trailing_action(NavAction::new(
                NEW_PLAYLIST,
                Icon::Plus,
                "New playlist",
            )),
        );
        self.places.show(sections, place_id(&self.view));
    }

    /// What plays, as the transport in the shell's band shows it.
    fn show_transport(&self) {
        let pb = &self.pb;
        let idle = pb.current.is_none() && pb.queue.is_empty();
        let track = pb.current.as_deref().and_then(|id| self.playback_track(id));
        let mut state = TransportState {
            title: "Nothing playing".to_owned(),
            playing: pb.playing,
            idle,
            position_ms: self.playhead_ms,
            duration_ms: f64_of(self.current_duration_ms()),
            volume: f64::from(pb.volume) / 100.0,
            shuffle: Some(pb.shuffle),
            repeat: Some(match pb.repeat {
                RepeatMode::Off => TransportRepeat::Off,
                RepeatMode::All => TransportRepeat::All,
                RepeatMode::One => TransportRepeat::One,
            }),
            lyrics: Some(self.view == View::Lyrics),
            queue: Some(self.view == View::Queue),
            more: true,
            ..TransportState::default()
        };
        if let Some(track) = track {
            state.title.clone_from(&track.title);
            state.subtitle = self
                .playback_error
                .clone()
                .unwrap_or_else(|| self.lib.artist_name(&track.artist_id).to_owned());
            state.art_name = self.lib.album_title(&track.album_id).to_owned();
            state.art = self
                .art_image(&self.best_cover_id(&track.id, &track.album_id))
                .map_or(TransportArt::Placeholder, TransportArt::Image);
            if !self.is_local_file(&track.id) {
                state.liked = Some(self.lib.is_liked(&track.id));
            }
        }
        self.transport.show(state);
    }

    /// Run what the person chose in the shell's sidebar and its transport
    /// as the window's own actions.
    pub(super) fn take_shell_events(&mut self, state: &mut ui::widget::Edits<'_>) {
        for chosen in self.places.take_chosen() {
            if let Some(view) = place_view(&chosen) {
                self.dispatch(Action::Nav(view), state);
            }
        }
        for (section, action) in self.places.take_section_actions() {
            if section == PLAYLISTS && action == NEW_PLAYLIST {
                self.dispatch(Action::BeginNewPlaylist, state);
            }
        }
        for event in self.transport.take_events() {
            let action = match event {
                TransportEvent::TogglePlay => Action::TogglePlay,
                TransportEvent::Previous => Action::Prev,
                TransportEvent::Next => Action::Next,
                TransportEvent::Seek(ms) => {
                    let duration = f64_of(self.current_duration_ms());
                    if duration <= 0.0 {
                        continue;
                    }
                    Action::SeekFrac((ms / duration).clamp(0.0, 1.0))
                }
                TransportEvent::Volume(volume) => Action::VolumeFrac(volume.clamp(0.0, 1.0)),
                TransportEvent::ToggleMute => Action::ToggleMute,
                TransportEvent::ToggleShuffle => Action::ToggleShuffle,
                TransportEvent::CycleRepeat => Action::CycleRepeat,
                TransportEvent::ToggleLike => match self.pb.current.clone() {
                    Some(track) => Action::ToggleLike(track),
                    None => continue,
                },
                TransportEvent::Lyrics => Action::Nav(View::Lyrics),
                TransportEvent::Queue => Action::ToggleQueue,
                TransportEvent::More(anchor) => Action::OpenPlayerMenu(anchor),
            };
            self.dispatch(action, state);
        }
        self.show_shell();
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a song's length in milliseconds, far inside f64's exact range"
)]
fn f64_of(ms: u64) -> f64 {
    ms as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_opens_the_view_it_names() {
        let views = DESTINATIONS
            .iter()
            .map(|(_, _, view)| view.clone())
            .chain([View::Kept("k1".into()), View::Playlist("p1".into())]);
        for view in views {
            let id = place_id(&view).expect("a place");
            assert_eq!(place_view(&id), Some(view));
        }
        assert_eq!(place_id(&View::Search), None, "search is no place");
    }
}

//! Catalogue selection never changes how an existing track is resolved.
use super::{
    AlbumPage, ArtistPage, BackendError, HomeContent, MusicBackend, PlaylistPage, ResolveOutcome,
    SearchBundle, SearchOutcome, spotify,
};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Sources {
    youtube: Box<dyn MusicBackend>,
    spotify: Option<spotify::SpotifyBackend>,
    selected: AtomicBool,
}
impl Sources {
    pub fn new(youtube: Box<dyn MusicBackend>) -> Self {
        Self {
            youtube,
            spotify: spotify::SpotifyBackend::new().ok(),
            selected: AtomicBool::new(
                !cfg!(test) && crate::storage::load_device_profile().spotify_catalogue,
            ),
        }
    }
    fn selected(&self) -> &dyn MusicBackend {
        if self.selected.load(Ordering::Relaxed)
            && let Some(spotify) = &self.spotify
        {
            return spotify;
        }
        self.youtube.as_ref()
    }
    fn for_id(&self, id: &str) -> Result<&dyn MusicBackend, BackendError> {
        if id.starts_with("spotify:") {
            self.spotify
                .as_ref()
                .map(|spotify| spotify as &dyn MusicBackend)
                .ok_or_else(|| BackendError("Spotify is unavailable".into()))
        } else {
            Ok(self.youtube.as_ref())
        }
    }
}
impl MusicBackend for Sources {
    fn spotify_operation(
        &self,
        operation: &super::SpotifyOperation,
    ) -> Result<super::SpotifyReply, BackendError> {
        self.spotify
            .as_ref()
            .ok_or_else(|| BackendError("Spotify unavailable".into()))?
            .operation(operation)
    }
    fn name(&self) -> &str {
        self.selected().name()
    }
    fn is_live(&self) -> bool {
        self.selected().is_live()
    }
    fn select_source(&self, spotify: bool, sign_in: bool) -> Result<(), BackendError> {
        if spotify {
            let source = self
                .spotify
                .as_ref()
                .ok_or_else(|| BackendError("Spotify is unavailable".into()))?;
            if sign_in {
                source.sign_in()?;
            }
            source.check_connected()?;
        }
        self.selected.store(spotify, Ordering::Relaxed);
        Ok(())
    }
    fn connect_spotify_read_only(&self) -> Result<(), BackendError> {
        let source = self
            .spotify
            .as_ref()
            .ok_or_else(|| BackendError("Spotify is unavailable".into()))?;
        source.sign_in_read_only()?;
        source.check_connected()?;
        self.selected.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn disconnect_spotify(&self) -> Result<(), BackendError> {
        self.spotify
            .as_ref()
            .ok_or_else(|| BackendError("Spotify is unavailable".into()))?
            .disconnect()?;
        self.selected.store(false, Ordering::Relaxed);
        Ok(())
    }
    fn spotify_playback(&self, sign_in: bool) -> Result<String, BackendError> {
        let account = self
            .spotify
            .as_ref()
            .ok_or_else(|| BackendError("Spotify unavailable".into()))?
            .playback_account(sign_in)?;
        if sign_in {
            self.selected.store(true, Ordering::Relaxed);
        }
        Ok(account)
    }
    fn search_tracks(&self, query: &str) -> SearchOutcome {
        self.selected().search_tracks(query)
    }
    fn search_all(&self, query: &str) -> Result<SearchBundle, BackendError> {
        self.selected().search_all(query)
    }
    fn search_quick(&self, query: &str) -> Result<SearchBundle, BackendError> {
        self.selected().search_quick(query)
    }
    fn warm(&self) {
        self.selected().warm();
    }
    fn catalogue_label(&self) -> &'static str {
        if self.selected.load(Ordering::Relaxed) && self.spotify.is_some() {
            "Spotify"
        } else {
            "YouTube Music"
        }
    }
    fn resolve_audio(&self, id: &str) -> ResolveOutcome {
        self.for_id(id)?.resolve_audio(id)
    }
    fn home(&self) -> Result<HomeContent, BackendError> {
        self.selected().home()
    }
    fn album(&self, id: &str) -> Result<AlbumPage, BackendError> {
        self.for_id(id)?.album(id)
    }
    fn artist(&self, id: &str) -> Result<ArtistPage, BackendError> {
        self.for_id(id)?.artist(id)
    }
    fn playlist(&self, id: &str) -> Result<PlaylistPage, BackendError> {
        self.for_id(id)?.playlist(id)
    }
}

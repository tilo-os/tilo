//! Catalogue selection never changes how an existing track is resolved.
use super::{
    AlbumPage, ArtistPage, BackendError, HomeContent, MusicBackend, PlaylistPage, ResolveOutcome,
    SearchBundle, SearchOutcome, spotify,
};

pub(super) struct Sources {
    youtube: Box<dyn MusicBackend>,
    spotify: Option<spotify::SpotifyBackend>,
}
impl Sources {
    pub fn new(youtube: Box<dyn MusicBackend>) -> Self {
        Self {
            youtube,
            spotify: spotify::SpotifyBackend::new().ok(),
        }
    }
    fn spotify(&self) -> Result<&spotify::SpotifyBackend, BackendError> {
        self.spotify
            .as_ref()
            .ok_or_else(|| BackendError("Spotify is unavailable".into()))
    }

    fn for_id(&self, id: &str) -> Result<&dyn MusicBackend, BackendError> {
        if id.starts_with("spotify:") {
            self.spotify_catalogue()
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
        self.spotify()?.operation(operation)
    }
    fn name(&self) -> &str {
        self.youtube.name()
    }
    fn is_live(&self) -> bool {
        self.youtube.is_live()
    }
    fn spotify_catalogue(&self) -> Result<&dyn MusicBackend, BackendError> {
        Ok(self.spotify()?)
    }
    fn connect_spotify_library(&self) -> Result<(), BackendError> {
        let source = self.spotify()?;
        source.sign_in()?;
        source.check_connected()
    }
    fn connect_spotify_read_only(&self) -> Result<(), BackendError> {
        let source = self.spotify()?;
        source.sign_in_read_only()?;
        source.check_connected()
    }
    fn disconnect_spotify(&self) -> Result<(), BackendError> {
        self.spotify()?.disconnect()
    }
    fn spotify_playback(&self, sign_in: bool) -> Result<String, BackendError> {
        self.spotify()?.playback_account(sign_in)
    }
    fn search_tracks(&self, query: &str) -> SearchOutcome {
        self.youtube.search_tracks(query)
    }
    fn search_all(&self, query: &str) -> Result<SearchBundle, BackendError> {
        self.youtube.search_all(query)
    }
    fn search_quick(&self, query: &str) -> Result<SearchBundle, BackendError> {
        self.youtube.search_quick(query)
    }
    fn warm(&self) {
        self.youtube.warm();
    }
    fn resolve_audio(&self, id: &str) -> ResolveOutcome {
        self.for_id(id)?.resolve_audio(id)
    }
    fn home(&self) -> Result<HomeContent, BackendError> {
        self.youtube.home()
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

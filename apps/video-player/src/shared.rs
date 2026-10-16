//! The shared library: playlists, timestamp annotations and where playback
//! stands for everyone watching a video together.
//!
//! Media capabilities stay device-private: shared rows hold only an opaque
//! media reference and human metadata, never a path. Renaming a playlist,
//! editing or moving an annotation and reordering or removing a playlist
//! item are field edits at their paths; the methods below are the
//! operations that are more than setting a field.

use std::collections::BTreeMap;

use ui::tilo_resource::{self, ActionCx, Collection, Id, Model, Refusal, resource};

/// How many videos' playback the library remembers; the one left longest
/// ago makes room.
pub const MAX_TRANSPORTS: usize = 32;
/// How much text a name, a title or a note may hold.
const MAX_TEXT_BYTES: usize = 8 * 1024;
/// The id of the shared Watch later playlist, the same on every replica so
/// two people adding to it at once add to one playlist.
pub const WATCH_LATER: &str = "watch-later";

/// One video in a playlist.
#[derive(Model, Clone, Debug, PartialEq, Eq)]
#[model(check = PlaylistItem::valid)]
pub struct PlaylistItem {
    /// Playlist
    #[model(readonly)]
    pub playlist: Id,
    /// Video
    ///
    /// The opaque, shareable identity of the video; never a device path.
    #[model(readonly)]
    pub media_ref: String,
    /// Title
    #[model(label)]
    pub title: String,
}

impl PlaylistItem {
    fn valid(&self) -> Result<(), Refusal> {
        if !valid_ref(&self.media_ref) {
            return Err(Refusal::new("a playlist item names its video"));
        }
        if self.title.trim().is_empty() || self.title.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new("a playlist item has a title"));
        }
        Ok(())
    }
}

/// A playlist everyone in the room sees. Its videos are the library's
/// playlist items that name it, in their order.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
#[model(check = Playlist::valid)]
pub struct Playlist {
    /// Name
    #[model(label)]
    pub name: String,
}

impl Playlist {
    fn valid(&self) -> Result<(), Refusal> {
        if self.name.trim().is_empty() || self.name.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new("a playlist has a name"));
        }
        Ok(())
    }
}

/// A review note, or a bookmark when it says nothing, at a time in a video.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
#[model(check = TimestampAnnotation::valid)]
pub struct TimestampAnnotation {
    /// Video
    ///
    /// The opaque, shareable identity of the video; never a device path.
    #[model(readonly)]
    pub media_ref: String,
    /// Time
    ///
    /// Milliseconds from the start of the video.
    pub time_ms: u64,
    /// Note
    #[model(label)]
    pub text: String,
}

impl TimestampAnnotation {
    fn valid(&self) -> Result<(), Refusal> {
        if !valid_ref(&self.media_ref) || self.text.len() > MAX_TEXT_BYTES {
            return Err(Refusal::new(
                "an annotation names its video and its note is bounded",
            ));
        }
        Ok(())
    }

    /// What its row says: the note, or "Bookmark".
    pub fn title(&self) -> &str {
        if self.text.is_empty() {
            "Bookmark"
        } else {
            &self.text
        }
    }
}

/// Where playback of one video stands for everyone watching it together:
/// the play state and the playhead at a wall-clock moment, which every
/// participant projects to its own now.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct SharedTransport {
    /// Playing
    pub playing: bool,
    /// Position
    pub position_ms: u64,
    /// Speed
    pub rate: f64,
    /// Said at
    ///
    /// Unix milliseconds at which `position_ms` held.
    pub epoch_ms: u64,
}

impl SharedTransport {
    /// The transport as the media engine follows it.
    pub fn state(&self) -> media::TransportState {
        media::TransportState {
            playing: self.playing,
            position_ms: self.position_ms,
            rate: self.rate as f32,
            epoch_ms: self.epoch_ms,
        }
    }
}

/// The library everyone in the room shares.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Library {
    /// Playlists
    pub playlists: Collection<Playlist>,
    /// Playlist items
    ///
    /// Every playlist's videos, in order. Kept beside the playlists rather
    /// than inside them, so two people adding to one playlist they both
    /// just made (Watch later) keep both videos.
    pub items: Collection<PlaylistItem>,
    /// Annotations
    pub annotations: Collection<TimestampAnnotation>,
    /// Playback watched together, by video.
    #[model(readonly)]
    pub transports: BTreeMap<String, SharedTransport>,
}

fn valid_ref(media_ref: &str) -> bool {
    !media_ref.trim().is_empty()
        && media_ref.len() <= 256
        && !media_ref.chars().any(char::is_control)
}

#[resource(kind = "video-player.library", label = "Video Player")]
impl Library {
    /// Create shared playlist.
    ///
    /// A playlist everyone in the room sees.
    #[action]
    pub fn create_playlist(
        &mut self,
        cx: &mut ActionCx,
        #[param("Playlist name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a playlist has a name"));
        }
        self.playlists.push(
            cx.id(),
            Playlist {
                name: name.to_owned(),
            },
        );
        Ok(())
    }

    /// Delete playlist.
    ///
    /// Its videos go with it.
    #[action(destructive, item = playlists)]
    pub fn delete_playlist(&mut self, playlist: &Id) -> Result<(), Refusal> {
        self.playlists
            .remove(playlist)
            .ok_or_else(|| Refusal::new("no such playlist"))?;
        self.items.retain(|_, item| &item.playlist != playlist);
        Ok(())
    }

    /// Add media to playlist.
    #[action(item = playlists)]
    pub fn add_to_playlist(
        &mut self,
        cx: &mut ActionCx,
        playlist: &Id,
        #[param("Opaque shareable media identity; never a device path")] media_ref: String,
        #[param("Media title")] title: String,
    ) -> Result<(), Refusal> {
        if !self.playlists.contains(playlist) {
            return Err(Refusal::new("no such playlist"));
        }
        self.items.push(
            cx.id(),
            PlaylistItem {
                playlist: playlist.clone(),
                media_ref,
                title: title.trim().to_owned(),
            },
        );
        Ok(())
    }

    /// Add to Watch later.
    ///
    /// Adds a video to the shared Watch later playlist, making the playlist
    /// when there is none; a video already on it stays where it is.
    #[action]
    pub fn add_to_watch_later(
        &mut self,
        cx: &mut ActionCx,
        #[param("Opaque shareable media identity; never a device path")] media_ref: String,
        #[param("Media title")] title: String,
    ) {
        let id = Id::from_raw(WATCH_LATER);
        if !self.playlists.contains(&id) {
            self.playlists.push(
                id.clone(),
                Playlist {
                    name: "Watch later".into(),
                },
            );
        }
        if !self.on_watch_later(&media_ref) {
            self.items.push(
                cx.id(),
                PlaylistItem {
                    playlist: id,
                    media_ref,
                    title: title.trim().to_owned(),
                },
            );
        }
    }

    /// Add timestamp annotation.
    ///
    /// A review note at a time in a shared video; without a note it is a
    /// bookmark.
    #[action]
    pub fn add_annotation(
        &mut self,
        cx: &mut ActionCx,
        #[param("Opaque shareable media identity; never a device path")] media_ref: String,
        #[param("Absolute timestamp in milliseconds")] time_ms: u64,
        #[param("Annotation text; omit for a bookmark")] text: Option<String>,
    ) {
        self.annotations.push(
            cx.id(),
            TimestampAnnotation {
                media_ref,
                time_ms,
                text: text.unwrap_or_default(),
            },
        );
    }

    /// Delete annotation.
    #[action(destructive, item = annotations)]
    pub fn delete_annotation(&mut self, annotation: &Id) -> Result<(), Refusal> {
        self.annotations
            .remove(annotation)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such annotation"))
    }

    /// Say where playback stands.
    ///
    /// Everyone watching the video follows. Not an undo step: playing,
    /// pausing and seeking are not edits. The library remembers the
    /// playback of the last few videos.
    #[action(housekeeping)]
    pub fn set_transport(
        &mut self,
        #[param("Opaque shareable media identity; never a device path")] media_ref: String,
        #[param("Whether it plays")] playing: bool,
        #[param("The playhead in milliseconds")] position_ms: u64,
        #[param("Playback speed from 0.25 to 4")] rate: f64,
        #[param("Unix milliseconds at which the playhead held")] epoch_ms: u64,
    ) -> Result<(), Refusal> {
        if !valid_ref(&media_ref) || !(0.25..=4.0).contains(&rate) {
            return Err(Refusal::new(
                "a valid media identity and a playback rate from 0.25 to 4 are required",
            ));
        }
        self.transports.insert(
            media_ref,
            SharedTransport {
                playing,
                position_ms,
                rate,
                epoch_ms,
            },
        );
        while self.transports.len() > MAX_TRANSPORTS {
            let Some(oldest) = self
                .transports
                .iter()
                .min_by_key(|(_, transport)| transport.epoch_ms)
                .map(|(media_ref, _)| media_ref.clone())
            else {
                break;
            };
            self.transports.remove(&oldest);
        }
        Ok(())
    }
}

impl Library {
    /// Where playback of the video `media_ref` stands, if anyone played it.
    pub fn transport_for(&self, media_ref: &str) -> Option<&SharedTransport> {
        self.transports.get(media_ref)
    }

    /// The annotations by video, then by time.
    pub fn annotations_in_order(&self) -> Vec<(&Id, &TimestampAnnotation)> {
        let mut annotations: Vec<_> = self.annotations.iter().collect();
        annotations.sort_by(|(a_id, a), (b_id, b)| {
            a.media_ref
                .cmp(&b.media_ref)
                .then(a.time_ms.cmp(&b.time_ms))
                .then_with(|| a_id.as_str().cmp(b_id.as_str()))
        });
        annotations
    }

    /// The videos of the playlist `playlist`, in order.
    pub fn items_of<'a>(
        &'a self,
        playlist: &'a Id,
    ) -> impl Iterator<Item = (&'a Id, &'a PlaylistItem)> + 'a {
        self.items
            .iter()
            .filter(move |(_, item)| &item.playlist == playlist)
    }

    /// Whether the video `media_ref` is on the Watch later playlist.
    pub fn on_watch_later(&self, media_ref: &str) -> bool {
        self.items_of(&Id::from_raw(WATCH_LATER))
            .any(|(_, item)| item.media_ref == media_ref)
    }
}

#[cfg(test)]
mod tests;

music-open-audio-file = Open audio file
music-connect-spotify = Connect Spotify
music-connect-spotify-library = Enable full controls
music-spotify-playback = Authorize playback
music-spotify-playback-authorized = Spotify: playback authorized; Premium checked before audio
music-spotify-devices = Spotify devices
music-browse-spotify = Browse Spotify
music-browse-youtube = YouTube Music
music-source-connecting = Connecting… Finish sign-in in your browser.
music-disconnect-spotify = Disconnect Spotify
music-spotify-updating = Updating Spotify…
music-spotify-output = Spotify output:
music-spotify-device-help = Move Spotify playback to this device. Spotify manages its queue.
music-spotify-library = Your Spotify library
music-spotify-liked = Spotify Liked Songs
music-spotify-liked-description = Songs saved to your Spotify account
music-sources = Music sources
music-sources-heading = Music sources
music-spotify-active = active
music-spotify-save = Save to Spotify Liked Songs
music-spotify-unsave = Remove from Spotify Liked Songs
music-spotify-signin-expired = Spotify sign-in expired. Connect again to refresh your library; your local files remain available.
music-spotify-queue-changed = The queue changed during the Spotify transition. Choose a song to continue.
music-stream-reconnecting = The stream stopped at { $position }. Reconnecting…
music-stream-cut-short = This song’s stream keeps stopping at { $position }.
music-spotify-request-timeout = Spotify did not finish this request in time. A change may already have been applied; refresh before retrying.
music-spotify-select-device = Select { $device }
music-spotify-pause-device = Pause { $device }
music-spotify-play-device = Play on { $device }
music-spotify-playlist-choice = Spotify: { $name }
music-spotify-remove-playlist = Remove all occurrences from this Spotify playlist
music-spotify-rename-playlist = Rename Spotify playlist…
music-spotify-save-playlist = Save Spotify playlist
music-spotify-unsave-playlist = Remove Spotify playlist from your library
music-spotify-create-playlist = New Spotify playlist
music-spotify-create = Create
music-spotify-save-name = Save name
music-spotify-private-help = Creates an empty private playlist in Spotify. Add songs after creation is confirmed.
music-spotify-ownership-help = Only the owner can rename this playlist. Adding or removing songs requires owner or collaborator access.
music-spotify-select-first = Choose Spotify as your catalogue first.
music-spotify-albums = Saved Spotify albums
music-spotify-playlists = Spotify playlists
music-spotify-queue-refresh = Refresh Spotify queue
music-spotify-library-more = More collections
music-spotify-library-page-unavailable = No next page is ready. Wait for loading, or refresh your Spotify library. At most 2,000 collections per type are supported.
music-spotify-library-page-invalid = Spotify returned an invalid library page. Your current library has been kept.
music-spotify-remote-queue = Spotify remote queue
music-spotify-remote-queue-help = A read-only Spotify snapshot. This device’s queue stays separate. Refresh to check for changes.
music-spotify-remote-queue-empty = Spotify reported no upcoming items.
music-spotify-remote-queue-unloaded = Refresh to read Spotify’s remote queue. No remote snapshot is loaded.
music-spotify-remote-queue-age = Snapshot captured { $seconds } seconds ago
music-spotify-remote-queue-truncated = Showing the first 50 remote items.
music-spotify-local-queue = This device’s queue
music-spotify-artists = Followed artists
music-spotify-following = Following on Spotify
music-spotify-follow-artist = Follow artist on Spotify
music-spotify-unfollow-artist = Unfollow artist on Spotify
music-spotify-playlist-actions = Spotify playlist actions
music-recommendations-title = Made from your library
music-recommendations-missing-track = This song is no longer in the library.
music-recommendations-help = Suggestions from songs already in this library. Each one names the choice that brought it here.
music-recommendations-privacy = Uses the inputs you enable and artists you prefer. No listening history or provider requests. Feedback shares with this library; History can undo it.
music-recommendations-tune = Tune suggestions
music-recommendations-input-likes = Use liked songs and their genre tags
music-recommendations-input-saved-albums = Use saved albums
music-recommendations-input-followed-artists = Use followed artists
music-recommendations-empty = Nothing tailored yet. Add music, then like a song, save an album or follow an artist to find suggestions.
music-recommendations-no-match = No eligible songs match. Adjust inputs, restore a song or add music. An account connection isn’t needed.
music-recommendations-library = Open songs
music-recommendations-song = { $title } by { $artist }
music-recommendations-play = Play
music-recommendations-dismiss = Hide this suggestion
music-recommendations-restore = Show again
music-recommendations-prefer = More from this artist
music-recommendations-unprefer = Stop preferring this artist
music-recommendations-feedback = Your recommendation choices
music-recommendations-feedback-help = Preferences affect suggestions only, without following artists on a service. Change them below or Undo in History.
music-recommendations-feedback-empty = No hidden songs or preferred artists.
music-recommendations-preferred-artist = Preferred: { $name }
music-recommendations-hidden-song = Hidden: { $title }
music-recommendations-reason-preferred = Because you asked for more from { $name }.
music-recommendations-reason-liked = Because you liked “{ $name }” by this artist.
music-recommendations-reason-saved = Because you saved “{ $name }” by this artist.
music-recommendations-reason-followed = Because you follow { $name } in this library.
music-recommendations-reason-genre = Its library genre tag, { $genre }, matches { $artist }, whose song you liked. Genre tags can be imperfect.
music-recommendations-enabled = Enabled
music-recommendations-disabled = Disabled
music-recommendations-enable = Enable input
music-recommendations-disable = Disable input

## History: what changed in the music library
music-create-playlist = Created the playlist { $name }
music-delete-playlist = Deleted the playlist { $item }
music-import-track = Added a song to the library
music-library-liked = Changed the liked songs
music-library-saved-albums = Changed the saved albums
music-library-followed-artists = Changed the followed artists
music-library-playlists = Changed the playlists
music-playlist-name = Renamed the playlist { $item }
music-playlist-description = Changed the description of { $item }
music-playlist-cover = Changed the cover of { $item }
music-playlist-tracks = Changed the songs of { $item }
music-preferences-likes = { $new ->
    [true] Explored recommendations from liked songs
   *[false] Stopped exploring recommendations from liked songs
}
music-preferences-saved-albums = { $new ->
    [true] Explored recommendations from saved albums
   *[false] Stopped exploring recommendations from saved albums
}
music-preferences-followed-artists = { $new ->
    [true] Explored recommendations from followed artists
   *[false] Stopped exploring recommendations from followed artists
}
music-preferences-preferred-artists = Changed the preferred artists
music-preferences-dismissed = Changed the hidden recommendations

## What the window and its player are called for assistive technology

music-title = Music
music-a11y-player = Music player
music-a11y-search = Search music
music-a11y-search-description = Search songs, albums, artists and playlists
music-a11y-search-description-simple = Search for a song, album, or artist
music-a11y-choose-source = Choose where your music comes from
music-a11y-empty = Empty music library
music-a11y-description = Music library and playback controls

## Window and accessibility copy
music-search-this-app = In this app
music-a11y-songs = Songs
music-a11y-navigation = Music navigation

## Facts, property labels and section titles
music-a11y-search-genre = Search for { $genre }
music-a11y-move-down = Move { $track } down
music-a11y-move-up = Move { $track } up
music-a11y-remove-from-playlist = Remove { $track } from { $playlist }
music-a11y-delete-playlist = Delete { $playlist }
music-a11y-rename-playlist = Rename { $playlist }
music-a11y-artist = { $genre }; { $listeners ->
    [one] one monthly listener
   *[other] { $listeners } monthly listeners
}
music-a11y-play = Play { $track }
music-a11y-like = Like { $track }
music-kind-album = Album
music-kind-artist = Artist
music-kind-playlist = Playlist
music-detail-load-failed = Couldn’t load this { $kind }
music-view-playlist = Playlist

## What the window shows
music-spotify-protected = Spotify protected audio cannot be downloaded through Tilo
music-already-first = This item is already first in the queue
music-already-last = This item is already last in the queue
music-choose-track-playback = Choose a track before using playback controls
music-choose-track-like = Choose a track to like
music-resource-library = Music
music-resource-player = Player
music-a11y-loading-source = Loading audio source
music-a11y-waiting-output = Waiting for audio output
music-a11y-playback-controls = Playback controls
music-view-home = Home
music-view-songs = Songs
music-view-albums = Albums
music-view-artists = Artists
music-view-playlists = Playlists
music-view-downloads = Downloads
music-view-liked = Liked songs
music-view-browse = Browse
music-view-queue = Queue
music-view-lyrics = Lyrics
music-view-now-playing = Now playing
music-view-search = Search
music-view-playlist-fallback = Playlist
music-no-songs = No songs yet
music-no-albums = No albums yet
music-no-artists = No artists yet
music-keep-search = Keep search
music-kept = Kept
music-forget-view = Remove from your views
music-like = Like
music-unlike = Unlike
music-track-actions = Track actions

music-empty-queue = There are no queued tracks
music-shuffle-off = Turn shuffle off
music-shuffle-on = Turn shuffle on

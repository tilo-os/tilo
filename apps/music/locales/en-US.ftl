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

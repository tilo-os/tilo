app so.tilo.music "Music" (
    icon: .music,
    category: .multimedia,
    tagline: "The music on your devices, and the catalogues that have more",
    keywords: ["music", "musik", "song", "songs", "titel", "track", "tracks", "album", "alben", "artist", "artists", "künstler", "kuenstler", "playlist", "playlists", "wiedergabe", "player", "audio", "listen", "hören", "hoeren", "spotify", "youtube music", "lyrics", "songtext", "liedtext"],
)

use so.tilo.playback as playback

/// A song: one the person opened from a device, or one the catalogue named. The music origin
/// keeps the catalogue's own fields in step; what the person did with the song, that they like
/// it, that they kept it offline, is theirs, and no answer writes over it.
type Track (origin: music.track, untitled: "Unknown song") {
    key: Text
    title: Text
    artist: Text = ""
    artist_key: Text = ""
    album: Text = ""
    album_key: Text = ""
    /// How long it runs, in seconds; 0 until the player or the catalogue says.
    seconds: Int = 0
    plays: Int = 0
    cover: Text = ""
    /// The search that found it, so a query's rows are its own.
    found_for: Text = ""
    /// Where the bytes play from, as the catalogue answered.
    stream: Text = ""
    /// Where kept bytes are, as this device holds them (`tilo-blob://…`); empty until kept.
    offline: Text = ""
    /// Its words, as LRC; `lyrics_state` says whether they are timed.
    lyrics: Text = ""
    lyrics_state: Text = ""
    liked: Bool = false

    search {
        filter liked (icon: .heart)
        sort title a_z
    }
}

/// An album the catalogue named.
type Album (origin: music.album, untitled: "Unknown album") {
    key: Text
    title: Text
    artist: Text = ""
    artist_key: Text = ""
    year: Int = 0
    /// `album`, `single`, `ep` or `release`.
    release_kind: Text = ""
    cover: Text = ""
    found_for: Text = ""
    /// Whether the person keeps it in their library.
    in_library: Bool = false

    search {
        filter in_library (icon: .bookmark)
    }
}

/// An artist the catalogue named.
type Artist (origin: music.artist, untitled: "Unknown artist") {
    key: Text
    name: Text
    subscribers: Int = 0
    avatar: Text = ""
    found_for: Text = ""
    followed: Bool = false

    search {
        filter followed (icon: .bell)
    }
}

/// A playlist the catalogue publishes.
type Playlist (origin: music.playlist, untitled: "Untitled playlist") {
    key: Text
    title: Text
    subtitle: Text = ""
    track_count: Int = 0
    cover: Text = ""
    found_for: Text = ""
}

/// One titled section of the catalogue's home page.
type Section (origin: music.section, untitled: "Section") {
    key: Text
    title: Text
    order: Int = 0
}

/// One row of the home page: what the catalogue suggests, under the section it put it in.
type Suggestion (origin: music.suggestion, untitled: "Suggestion") {
    key: Text
    section: Text = ""
    /// What the row names: `track`, `album`, `artist` or `playlist`.
    kind: Text = ""
    /// The key of the thing it names.
    target: Text = ""
    title: Text = ""
    subtitle: Text = ""
    cover: Text = ""
}

/// A playlist the person made: their songs, in their order, with a cover they chose. Its songs
/// are the shared session's items, so a mixtape plays through the same queue as everything else.
type Mixtape (untitled: "New playlist") {
    name: Text
    tracks: Collection<playback.Item>
    cover: Text = ""

    search {
        sort name a_z
    }
}

/// What the person likes and what the recommendations may draw on, shared by the workspace.
type Taste {
    /// Ask for albums by artists the person already likes.
    similar_artists: Bool = true
    /// Ask for more by the artists followed.
    followed_artists: Bool = true
    /// Ask for songs like the ones liked.
    liked_songs: Bool = true
    /// Ask for albums saved.
    saved_albums: Bool = true
    /// Artists the person said to leave out.
    dismissed: Set<Text>
    /// Artists the person said to bring more of.
    preferred: Set<Text>
}

/// The workspace's music: the songs and the catalogue's pages, what the person made, what they
/// like, what plays, and how the catalogue's fetch stands.
type Library (title: name) {
    name: Text = "Music"
    tracks: Collection<Track>
    albums: Collection<Album>
    artists: Collection<Artist>
    playlists: Collection<Playlist>
    suggestions: Collection<Suggestion>
    sections: Collection<Section>
    mixtapes: Collection<Mixtape>
    taste: Taste
    /// What plays, and where it stands: the shared session Music and Video keep, once this app
    /// played. The player and the shell's live card read the same record.
    playing: Option<playback.Playback>
    /// Every session this app made, kept as the resources they are; `playing` names the one.
    sessions: Collection<playback.Playback>
    /// `youtube` or `off`: which catalogue the app asks.
    catalogue: Text = "youtube"
    /// What the catalogue's last fetch is doing, and why it failed.
    search_state: Text = ""
    search_trouble: Text = ""
    home_state: Text = ""
    home_trouble: Text = ""
    trouble: Text = ""
    online: Bool = true
}

root library: Library

/// What the library starts with: the recommendations' switches on. The session is made the
/// first time something plays, so an empty library opens on its catalogue and no player.
initial action start(lib: Library) {
    lib.taste = Taste()
}

/// The session this app plays through: the one it has, or one made now. It is kept both in
/// `playing`, which a view reads, and in `sessions`, so the record is a resource of its own.
action session(lib: Library) -> playback.Playback {
    if let held = lib.playing {
        return held
    }
    let made = lib.sessions.insert(playback.Playback())
    lib.playing = .some(made)
    made
}

default view library for Library

default view track for Track

default view album for Album

default view artist for Artist

default view playlist for Playlist

default view mixtape for Mixtape

/// The app's window: the places of the library, the songs, the catalogue's pages, and the
/// player under them.
view library(lib: Library) {
    state place: Text = "home"
    let downloads = lib.tracks.filter(t -> t.offline != "")
    let liked = lib.tracks.filter(.liked)
    ui.NavShell(content_kind: .list, sidebar_title: "Music") {
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("home", "Home", icon: .home) {
                    on select: set(place, "home")
                }
                ui.NavItem("search", "Search", icon: .search) {
                    on select: set(place, "search")
                }
                ui.NavItem("songs", "Songs", icon: .list, count: lib.tracks.len()) {
                    on select: set(place, "songs")
                }
                ui.NavItem("albums", "Albums", icon: .music, count: lib.albums.len()) {
                    on select: set(place, "albums")
                }
                ui.NavItem("artists", "Artists", icon: .users, count: lib.artists.len()) {
                    on select: set(place, "artists")
                }
                ui.NavSection("Yours", collapsible: true) {
                    ui.NavItem("mixtapes", "Playlists", icon: .list, count: lib.mixtapes.len()) {
                        on select: set(place, "mixtapes")
                    }
                    ui.NavItem("liked", "Liked", icon: .heart, count: liked.len()) {
                        on select: set(place, "liked")
                    }
                    ui.NavItem("downloads", "Downloads", icon: .download, count: downloads.len()) {
                        on select: set(place, "downloads")
                    }
                    ui.NavItem("taste", "Recommendations", icon: .sparkles) {
                        on select: set(place, "taste")
                    }
                }
            }
        }
        band: {
            band(lib)
        }
        actions: {
            std.header
        }
        content: {
            scroll {
                page(full) {
                    std.service_banner(
                        "YouTube Music",
                        lib.online,
                        lib.trouble,
                        retry: refresh(lib),
                    )
                    if place == "search" {
                        search_page(lib)
                    } else if place == "songs" {
                        track_list(lib, lib.tracks.sort_by(.title, asc), "")
                    } else if place == "albums" {
                        album_grid(lib, lib.albums.sort_by(.title, asc))
                    } else if place == "artists" {
                        artist_grid(lib, lib.artists.sort_by(.name, asc))
                    } else if place == "mixtapes" {
                        mixtape_list(lib)
                    } else if place == "liked" {
                        track_list(lib, liked.sort_by(.title, asc), "Nothing liked yet")
                    } else if place == "downloads" {
                        track_list(lib, downloads, "Nothing kept for offline")
                    } else if place == "taste" {
                        taste_page(lib)
                    } else {
                        home_page(lib)
                    }
                }
            }
        }
    }
}

/// The player: what plays, and the controls that steer it. Its source is the kept bytes where
/// the song is kept offline and the catalogue's stream otherwise; the band plays it on the
/// window's engine, so its place and length are the sound's own.
view band(lib: Library) {
    if let p = lib.playing {
        ui.MediaTransport(
            title: if p.title == "" { "Nothing playing" } else { p.title },
            subtitle: if lib.trouble != "" { lib.trouble } else { p.artist },
            error: lib.trouble != "",
            art: p.cover,
            playing: p.state == .playing,
            idle: p.track == "",
            position: p.at,
            duration: p.length,
            volume: p.volume,
            shuffle: p.shuffle,
            repeat: if p.repeat == "all" {
                .all
            } else if p.repeat == "one" { .one } else { .off },
            liked: if let s = lib.tracks.find(t -> t.id == p.track) { s.liked } else { false },
            lyrics: false,
            queue: true,
            source: p.source,
            fill,
        ) {
            on act_play_pause: playback.toggle(p)
            on act_previous: playback.previous(p)
            on act_next: playback.next(p)
            on act_shuffle: playback.shuffle_toggle(p)
            on act_repeat: playback.cycle_repeat(p)
            on act_like: like_playing(
                p,
                liked: not (lib.tracks.find(t -> t.id == p.track)?.liked ?? false),
            )
            on act_mute: playback.mute(p)
            on act_seek(place: Float): playback.seek(p, place: place)
            on act_volume(place: Float): playback.set_volume(p, level: place)
            on act_queue: open(p, view: queue)
            on act_lyrics: open(p, view: lyrics)
            on ended: playback.next(p)
            on progress(share: Float): playback.set_share(p, share)
            on length(seconds: Float): playback.set_length(p, seconds)
            on fail(reason: Text): note_failure(lib, reason: reason)
        }
    } else {
        ui.MediaTransport(title: "Nothing playing", idle: true, queue: false, fill)
    }
}

/// A song's page: its player line, what it is, and what may be done with it.
view track(tr: Track) {
    let lib = root Library
    std.page(tr) {
        header: {
            button(play_song(tr)) as play_song
            button(keep_offline(tr)) as keep_song
            button(like(tr)) as like_song
        }
        band(lib)
        stack(gap: .related) {
            text(tr.title, role: .title)
            text(tr.artist, role: .subheading)
            ui.Facts {
                if tr.album != "" {
                    ui.Fact(tr.album, icon: .music)
                }
                if tr.seconds > 0 {
                    ui.Fact(clock(tr.seconds), icon: .clock)
                }
                if tr.offline != "" {
                    ui.Fact("Kept offline", icon: .download)
                }
            }
            row(gap: .related, wrap: true) {
                for m in lib.mixtapes {
                    ui.Button(m.name, icon: .list) {
                        on press: mixtape_add(m, tr)
                    }
                }
                ui.Button("Add to queue", icon: .list) {
                    on press: add_to_queue(lib, tr)
                }
                ui.Button("Play next", icon: .forward) {
                    on press: play_next(lib, tr)
                }
                if tr.offline != "" {
                    ui.Button("Remove download", icon: .trash) {
                        on press: remove_offline(tr)
                    }
                }
                ui.Button("Lyrics", icon: .quote) {
                    on press: lyrics_now(lib)
                }
            }
            if tr.lyrics != "" {
                ui.Section("Lyrics") {
                    text(tr.lyrics, role: .reading)
                }
            }
        }
    }
}

/// An album's page: its songs, played from the top.
view album(a: Album) {
    let lib = root Library
    let songs = lib.tracks.filter(t -> t.album_key == a.key)
    std.page(a) {
        header: {
            button(play_album(a)) as play_album_button
            button(save_album(a)) as save_album_button
        }
        stack(gap: .related) {
            text(a.title, role: .title)
            text(a.artist, role: .subheading)
            track_list(lib, songs, "This album has no songs yet")
        }
    }
}

/// An artist's page: their songs and their albums.
view artist(a: Artist) {
    let lib = root Library
    let songs = lib.tracks.filter(t -> t.artist_key == a.key)
    let albums = lib.albums.filter(x -> x.artist_key == a.key)
    std.page(a) {
        header: {
            button(follow_artist(a)) as follow_artist_button
            button(refresh_artist(a)) as refresh_button
        }
        stack(gap: .related) {
            text(a.name, role: .title)
            if a.subscribers > 0 {
                text(listeners(a.subscribers), role: .caption, secondary: true)
            }
            ui.Section("Songs") {
                track_list(lib, songs, "This artist has no songs yet")
            }
            ui.Section("Albums") {
                album_grid(lib, albums)
            }
        }
    }
}

/// A catalogue playlist's page: its songs, played from the top.
view playlist(pl: Playlist) {
    let lib = root Library
    let songs = lib.tracks.filter(t -> t.found_for == pl.found_for)
    std.page(pl) {
        header: {
            button(open_playlist(pl)) as refresh_button
        }
        stack(gap: .related) {
            text(pl.title, role: .title)
            text(pl.subtitle, role: .subheading)
            track_list(lib, songs, "This playlist has no songs yet")
        }
    }
}

/// A mixtape's page: the person's own songs, in their order.
view mixtape(m: Mixtape) {
    let lib = root Library
    let songs = m.tracks.flat_map(item -> lib.tracks.filter(t -> t.id == item.ref))
    std.page(m) {
        header: {
            button(play_mixtape(m)) as play_mixtape_button
            ui.Button("Cover", icon: .image) {
                on press: file.choose(
                    pick_cover(m),
                    accept: ["image/png", "image/jpeg", "image/webp"],
                )
            }
        }
        stack(gap: .related) {
            text(m.name, role: .title)
            track_list(lib, songs, "Add songs to this playlist")
        }
    }
}

/// The home page: the catalogue's suggestions, in their sections.
view home_page(lib: Library) {
    if lib.suggestions.is_empty() {
        ui.EmptyState(
            "Find music",
            message: "Search the catalogue, or open an audio file from this device.",
            icon: .music,
            action: "Open audio file",
        ) {
            on act: file.choose(open_audio_file(lib), accept: audio_kinds())
        }
        ui.Button("Refresh", icon: .refresh) {
            on press: refresh(lib)
        }
    } else {
        stack(gap: .section) {
            row(gap: .tight) {
                ui.Button("Open audio file", icon: .file_plus) {
                    on press: file.choose(open_audio_file(lib), accept: audio_kinds())
                }
                ui.Button("Refresh", icon: .refresh) {
                    on press: refresh(lib)
                }
            }
            for section in lib.sections.sort_by(.order, asc) {
                ui.Section(section.title) {
                    ui.MediaGrid(size: .small) {
                        for row in lib.suggestions.filter(s -> s.section == section.title) {
                            ui.MediaCard(
                                row,
                                title: row.title,
                                picture: row.cover,
                                meta: [row.subtitle],
                            ) {
                                on activate: open_suggestion(lib, row)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The search page: what the catalogue found for the words typed. The field asks for a query;
/// a search that the system's own search or an agent ran shows its results here too.
view search_page(lib: Library) {
    state typed: Text = ""
    stack(gap: .related) {
        ui.TextInput(bind: typed, placeholder: "Search YouTube Music", autofocus: true, fill) {
            on submit: search(lib, typed)
        }
        if lib.search_state == "" {
            ui.EmptyState(
                "Search the catalogue",
                message: "Find songs, albums and artists with the system's search.",
                icon: .search,
            )
        } else if lib.search_state == "searching" {
            ui.EmptyState("Searching…")
        } else if lib.search_trouble != "" {
            ui.EmptyState("The catalogue did not answer", message: lib.search_trouble, icon: .info)
        } else {
            track_list(lib, lib.tracks.filter(t -> t.found_for != ""), "")
            let albums = lib.albums.filter(a -> a.found_for != "")
            let artists = lib.artists.filter(a -> a.found_for != "")
            let playlists = lib.playlists.filter(p -> p.found_for != "")
            if not albums.is_empty() {
                ui.Section("Albums") {
                    album_grid(lib, albums)
                }
            }
            if not artists.is_empty() {
                ui.Section("Artists") {
                    artist_grid(lib, artists)
                }
            }
            if not playlists.is_empty() {
                ui.Section("Catalogue playlists") {
                    ui.MediaGrid(size: .small) {
                        for p in playlists {
                            ui.MediaCard(p, title: p.title, picture: p.cover, meta: [p.subtitle]) {
                                on activate: open(p)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The songs as rows a person plays: a row plays the songs from where it stands, and the row's
/// mark says what the person did with the song.
view track_list(lib: Library, songs: List<Track>, empty: Text) {
    if songs.is_empty() {
        ui.EmptyState(empty)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: false) {
            on activate(x: Track): play_song(x)
            for song in songs {
                ui.ListItem(item: song) {
                    ui.ListRow(
                        title: song.title,
                        secondary: row_line(song),
                        meta: .none,
                        icon: if now_playing(lib) == song.id { .speaker } else { .music },
                    )
                }
            }
        }
    }
}

/// A row's line: who sings the song, then its album and its length when they are known.
fn row_line(song: Track) -> Text {
    if song.artist == "" {
        song.album
    } else if song.album != "" { "{song.artist}, {song.album}" } else { song.artist }
}

/// The albums as cards.
view album_grid(lib: Library, albums: List<Album>) {
    if albums.is_empty() {
        ui.EmptyState("No albums yet")
    } else {
        ui.MediaGrid(size: .regular) {
            for a in albums {
                ui.MediaCard(a, title: a.title, picture: a.cover, meta: [a.artist]) {
                    on activate: open(a)
                }
            }
        }
    }
}

/// The artists as cards.
view artist_grid(lib: Library, artists: List<Artist>) {
    if artists.is_empty() {
        ui.EmptyState("No artists yet")
    } else {
        ui.MediaGrid(size: .small) {
            for a in artists {
                ui.MediaCard(a, title: a.name, picture: a.avatar) {
                    on activate: open(a)
                }
            }
        }
    }
}

/// The person's playlists, made from the library.
view mixtape_list(lib: Library) {
    stack(gap: .related) {
        if lib.mixtapes.is_empty() {
            ui.EmptyState("No playlists of your own yet", action: "New playlist") {
                on act: new_mixtape(lib, "New playlist")
            }
        } else {
            ui.MediaGrid(size: .regular) {
                for m in lib.mixtapes.sort_by(.name, asc) {
                    ui.MediaCard(
                        m,
                        title: m.name,
                        picture: m.cover,
                        meta: [count_text(m.tracks.len())],
                    ) {
                        on activate: open(m)
                    }
                }
            }
        }
    }
}

/// What the recommendations may draw on, and the artists the person ruled out.
view taste_page(lib: Library) {
    let taste = lib.taste
    std.page(lib) {
        stack(gap: .related) {
            ui.Section("Recommendations") {
                ui.Button("Similar artists", toggled: taste.similar_artists) {
                    on press: update_taste(lib, "similar_artists", not taste.similar_artists)
                }
                ui.Button("Artists followed", toggled: taste.followed_artists) {
                    on press: update_taste(lib, "followed_artists", not taste.followed_artists)
                }
                ui.Button("More like the songs liked", toggled: taste.liked_songs) {
                    on press: update_taste(lib, "liked_songs", not taste.liked_songs)
                }
                ui.Button("More like the albums saved", toggled: taste.saved_albums) {
                    on press: update_taste(lib, "saved_albums", not taste.saved_albums)
                }
            }
            ui.Section("Catalogue") {
                ui.Button("YouTube Music", toggled: lib.catalogue == "youtube") {
                    on press: set_catalogue(lib, "youtube")
                }
                ui.Button("Off") {
                    on press: set_catalogue(lib, "off")
                }
            }
        }
    }
}

/// The kinds of file that open the music player. The compiler writes them into the app's
/// manifest, so Files and the person's own file system offer the app for exactly these.
fn audio_kinds() -> List<Text> {
    [
        "audio/wav",
        "audio/x-wav",
        "audio/wave",
        "audio/mpeg",
        "audio/flac",
        "audio/x-flac",
        "audio/ogg",
        "audio/opus",
        "audio/mp4",
        "audio/aac",
        "audio/aiff",
        "audio/x-aiff",
    ]
}

/// Opens an audio file: the file a person picked, or one Files handed over, and plays it. The
/// song it already made opens instead of a second copy, so its place in history stays one.
primary action open_audio_file(lib: Library, file: File) -> Track (
    icon: .file_plus,
    did: "added a song",
    key: "Ctrl+O",
    opens: [
    "audio/wav",
    "audio/x-wav",
    "audio/wave",
    "audio/mpeg",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
    "audio/opus",
    "audio/mp4",
    "audio/aac",
    "audio/aiff",
    "audio/x-aiff",
    .wav,
    .mp3,
    .flac,
    .ogg,
    .oga,
    .opus,
    .m4a,
    .aac,
    .aif,
    .aiff,
],
) {
    let song = if let existing = lib.tracks.find(t -> t.stream == file.url) {
        existing
    } else {
        lib.tracks.insert(Track(title: stem(file.name), stream: file.url))
    }
    play_song(song)
    song
}

/// The item a song queues as: its reference, what it is called and who sings it, its cover,
/// where its bytes play from, and how long it is.
fn item_of(t: Track) -> playback.Item {
    playback.Item(
        ref: t.id,
        title: t.title,
        artist: t.artist,
        cover: t.cover,
        source: if t.offline != "" { t.offline } else { t.stream },
        length: t.seconds.to_float(),
    )
}

/// Plays one song, alone: what a row's press does. The place is the shared session's own
/// record, so the song continues on this device or another.
action play_song(t: Track) (icon: .play) {
    let lib = root Library
    playback.play_list(session(lib), [item_of(t)], at: 0)
}

/// Plays an album's songs from the top.
action play_album(a: Album) (icon: .play) {
    let lib = root Library
    let songs = lib.tracks.filter(t -> t.album_key == a.key)
    playback.play_list(session(lib), songs.map(t -> item_of(t)), at: 0)
}

/// Plays a mixtape's songs in the person's order.
action play_mixtape(m: Mixtape) (icon: .play) {
    let lib = root Library
    playback.play_list(session(lib), m.tracks.map(item -> item), at: 0)
}

/// The queue of what plays: the songs it holds, in order. A row dropped in another place moves
/// its item there, and the session's own action keeps the order.
view queue(p: playback.Playback) {
    let lib = root Library
    let songs = p.queue.flat_map(item -> lib.tracks.filter(t -> t.id == item.ref))
    std.page(p) {
        stack(gap: .related) {
            text("Queue", role: .title)
            if songs.is_empty() {
                ui.EmptyState("Nothing queued")
            } else {
                ui.VirtualList(selection_follows_focus: true, in_column: false) {
                    on reorder(ids: List<Text>): queue_moved(p, ids)
                    on activate(x: Track): play_song(x)
                    for song in songs {
                        ui.ListItem(item: song) {
                            ui.ListRow(
                                title: song.title,
                                secondary: song.artist,
                                icon: if p.track == song.id { .speaker } else { .music },
                            )
                        }
                    }
                }
            }
        }
    }
}

/// The words of what plays: the song's own lyrics, or what it takes to ask for them.
view lyrics(p: playback.Playback) {
    let lib = root Library
    std.page(p) {
        stack(gap: .related) {
            if let song = lib.tracks.find(t -> t.id == p.track) {
                text(song.title, role: .title)
                text(song.artist, role: .subheading)
                if song.lyrics != "" {
                    text(song.lyrics, role: .reading)
                } else {
                    ui.EmptyState(
                        "No lyrics yet",
                        message: "Ask the catalogue for the words of what plays.",
                        icon: .quote,
                        action: "Find lyrics",
                    ) {
                        on act: lyrics_of(lib, p)
                    }
                }
            } else {
                ui.EmptyState("Nothing playing", icon: .quote)
            }
        }
    }
}

/// Adds a song to what follows.
action add_to_queue(lib: Library, t: Track) (icon: .list) {
    let p = session(lib)
    playback.queue_add(p, item_of(t))
}

/// Queues a song right after what plays.
action play_next(lib: Library, t: Track) (icon: .forward, label: "Play next") {
    let p = session(lib)
    playback.queue_play_next(p, item_of(t))
}

/// Asks for the words of what plays, keeping the session it asks for.
action lyrics_now(lib: Library) (icon: .quote) {
    let p = session(lib)
    lyrics_of(lib, p)
}

/// The song that plays, while it plays, so a row marks it.
fn now_playing(lib: Library) -> Text {
    if let p = lib.playing {
        if p.state == .playing { p.track } else { "" }
    } else { "" }
}

/// Moves the dragged row where it was dropped: the two ids its reorder carries.
action queue_moved(p: playback.Playback, ids: List<Text>) {
    if ids.len() < 2 {
        return
    }
    playback.queue_move(p, ids.at(0) ?? "", ids.at(1) ?? "")
}

/// Likes a song, or takes the like back.
action like(t: Track, liked: Bool = not t.liked) (icon: .heart) {
    t.liked = liked
}

/// Likes what plays.
action like_playing(p: playback.Playback, liked: Bool = true) (icon: .heart) {
    let lib = root Library
    if let song = lib.tracks.find(t -> t.id == p.track) {
        song.liked = liked
    }
}

/// Keeps a song on this device, for playing with no network.
action keep_offline(t: Track) (icon: .download) {
    if t.offline != "" {
        return
    }
    if t.stream == "" {
        outside("music.stream", t.key)
        return
    }
    outside("music.pin", [t.stream, t.title])
}

/// Takes the kept copy away; the song still plays from the catalogue.
action remove_offline(t: Track) (icon: .trash, label: "Remove download") {
    t.offline = ""
}

/// Keeps an album in the library, or takes it out.
action save_album(a: Album, keep: Bool = not a.in_library) (icon: .bookmark) {
    a.in_library = keep
}

/// Follows an artist.
action follow_artist(a: Artist, followed: Bool = not a.followed) (icon: .bell) {
    a.followed = followed
}

/// Asks the catalogue for the songs, albums, artists and playlists a query finds.
action search(lib: Library, query: Text) uses time (icon: .search) {
    require query != "" "Type something to search for"
    lib.search_state = "searching"
    lib.search_trouble = ""
    outside("music.search", query)
}

/// Asks the catalogue for its home page.
action refresh(lib: Library) uses time (icon: .refresh) {
    lib.home_state = "loading"
    lib.home_trouble = ""
    outside("music.home", "")
}

/// Asks the catalogue for an artist's page.
action refresh_artist(a: Artist) (icon: .refresh) {
    outside("music.artist", a.key)
}

/// Asks the catalogue for a playlist's songs.
action open_playlist(p: Playlist) (icon: .refresh, label: "Refresh") {
    outside("music.playlist", p.key)
}

/// Does what a suggestion on the home page names.
action open_suggestion(lib: Library, row: Suggestion) uses time {
    if row.kind == "track" {
        if let song = lib.tracks.find(t -> t.key == row.target) { play_song(song) }
    } else if row.kind == "album" {
        outside("music.album", row.target)
    } else if row.kind == "artist" {
        outside("music.artist", row.target)
    } else if row.kind == "playlist" { outside("music.playlist", row.target) }
}

/// Asks for what plays to be sung, into the song's own record.
action lyrics_of(lib: Library, p: playback.Playback) uses time (icon: .quote) {
    if let song = lib.tracks.find(t -> t.id == p.track) {
        if song.lyrics == "" {
            outside("music.lyrics", [song.title, song.artist])
        }
    }
}

/// Says why the player stopped.
action note_failure(lib: Library, reason: Text) {
    lib.trouble = reason
}

/// Makes a playlist of the person's own.
action new_mixtape(lib: Library, name: Text) (icon: .plus) {
    lib.mixtapes.insert(Mixtape(name: name))
}

/// Adds a song to a mixtape.
action mixtape_add(m: Mixtape, t: Track) (icon: .list) {
    if not m.tracks.any(i -> i.ref == t.id) { m.tracks.insert(item_of(t)) }
}

/// Takes a song out of a mixtape.
action mixtape_remove(m: Mixtape, t: Track) (icon: .trash) {
    if let item = m.tracks.find(i -> i.ref == t.id) { m.tracks.remove(item) }
}

/// Gives a mixtape the cover the person picks.
action pick_cover(m: Mixtape, file: File) (icon: .image) {
    m.cover = file.url
}

/// Sets a mixtape's cover to the picture the person chose.
action set_cover(m: Mixtape, file: File) {
    m.cover = file.url
}

/// Chooses which catalogue the app asks.
action set_catalogue(lib: Library, which: Text) {
    lib.catalogue = which
    if which == "youtube" { outside("music.home", "") }
}

/// Turns one of the recommendations' switches.
action update_taste(lib: Library, which: Text, on: Bool) {
    if which == "similar_artists" {
        lib.taste.similar_artists = on
    } else if which == "followed_artists" {
        lib.taste.followed_artists = on
    } else if which == "liked_songs" {
        lib.taste.liked_songs = on
    } else if which == "saved_albums" {
        lib.taste.saved_albums = on
    }
}

/// Leaves an artist out of the recommendations, or brings them back.
action dismiss_artist(lib: Library, a: Artist) {
    if lib.taste.dismissed.contains(a.key) {
        lib.taste.dismissed.remove(a.key)
    } else {
        lib.taste.dismissed.add(a.key)
        lib.taste.preferred.remove(a.key)
    }
}

/// Asks for more of an artist.
action prefer_artist(lib: Library, a: Artist) {
    if lib.taste.preferred.contains(a.key) {
        lib.taste.preferred.remove(a.key)
    } else {
        lib.taste.preferred.add(a.key)
        lib.taste.dismissed.remove(a.key)
    }
}

/// A file's name without its extension: what a song opened from a device is called.
fn stem(name: Text) -> Text {
    name.split(".").first() ?? name
}

/// A song's length as people read it.
fn clock(seconds: Int) -> Text {
    let m = seconds / 60
    let s = seconds % 60
    if s < 10 { "{m}:0{s}" } else { "{m}:{s}" }
}

/// How many songs a list holds.
fn count_text(n: Int) -> Text {
    if n == 1 { "1 song" } else { "{n} songs" }
}

/// An artist's listeners as people read them.
fn listeners(n: Int) -> Text {
    if n >= 1000000 {
        "{n / 1000000}M monthly listeners"
    } else if n >= 1000 { "{n / 1000}K monthly listeners" } else { "{n} monthly listeners" }
}

/// A song playing, as a person's first listen leaves it: one song of their own in the library,
/// playing on this device, with nothing of the catalogue fetched.
scenario "a song playing" {
    let lib = root Library
    let song = lib.tracks.insert(
        Track(
            key: "first-light",
            title: "First Light",
            artist: "Ava",
            album: "Morning",
            seconds: 214,
            stream: "https://music.example/first-light.wav",
        ),
    )
    play_song(song)
}

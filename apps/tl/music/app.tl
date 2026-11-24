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
    /// The kinds of music the catalogue calls theirs: empty where it names none, as YouTube
    /// Music names none.
    genres: List<Text>

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
    /// What the playlist says about itself; empty when it says nothing.
    description: Text = ""
}

/// A place Spotify plays on: a phone, a computer, a speaker or the person's browser. What plays
/// can be moved to it, and steered while it is there.
type SpotifyDevice (origin: music.device, untitled: "Unknown device") {
    /// The device's id, as Spotify names it.
    key: Text
    name: Text
    /// `computer`, `smartphone`, `speaker`, `tv`, `cast` or another Spotify names.
    kind: Text = ""
    /// Whether what plays plays here.
    active: Bool = false
    /// How loud it is, `0..=100`, when Spotify says.
    volume: Int = 0
}

/// A song in what plays next on Spotify, in its own order: a snapshot the person refreshes,
/// never a queue this app edits.
type SpotifyQueueItem (origin: music.spotify_queue, untitled: "Unknown song") {
    /// The song's Spotify address, which is what playing it asks for.
    key: Text
    title: Text
    artist: Text = ""
    album: Text = ""
    cover: Text = ""
    seconds: Int = 0
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
    /// What the playlist says about itself, for a reader.
    description: Text = ""
    /// The Spotify playlist it is, once the person's catalogue is Spotify: every song added
    /// here is added there, and its name and description are kept in step.
    spotify: Text = ""

    search {
        sort name a_z
    }
}

/// What the person likes and what the recommendations may draw on, shared by the workspace.
///
/// A suggestion is derived from the library alone: a song the person liked, an album they kept,
/// an artist they follow. There is no listening history and no request to a catalogue, so what
/// the person listens to elsewhere never shapes what is suggested here.
type Taste {
    /// Ask for more by the artists followed.
    followed_artists: Bool = true
    /// Ask for other songs by an artist the person liked a song of.
    liked_songs: Bool = true
    /// Ask for other songs from an album the person kept.
    saved_albums: Bool = true
    /// Ask for songs by artists whose kind of music is one the person already listens to.
    similar_artists: Bool = true
    /// Artists the person said to leave out.
    dismissed: Set<Text>
    /// Artists the person said to bring more of: their songs come first.
    preferred: Set<Text>
    /// Songs the person hid from the suggestions.
    hidden: Set<Text>
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
    /// The places Spotify plays on, and the person's sign-in to it.
    devices: Collection<SpotifyDevice>
    /// What plays next on Spotify, as its own queue had it when last read.
    spotify_queue: Collection<SpotifyQueueItem>
    /// `off`, `connecting`, `connected`, `expired` or `trouble`.
    spotify_state: Text = "off"
    spotify_account: Text = ""
    /// `premium`, `free`, `open` or empty, as Spotify says.
    spotify_product: Text = ""
    spotify_trouble: Text = ""
    /// The page the person signs in on, while `spotify_state` says `connecting`.
    connect_url: Text = ""
    /// The device Spotify plays on, as its key; empty when nothing does.
    spotify_active: Text = ""
    /// What plays there, as people read it.
    spotify_now: Text = ""
    spotify_playing: Bool = false
    /// Where the next page of each of the person's Spotify collections starts; 0 is none.
    spotify_next_tracks: Int = 0
    spotify_next_albums: Int = 0
    spotify_next_playlists: Int = 0
    spotify_next_artists: Int = 0
    /// `youtube`, `spotify` or `off`: which catalogue the app asks.
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

default view device for SpotifyDevice

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
                if lib.catalogue == "spotify"
                    and lib.spotify_state == "connected"
                    and tr.stream.starts_with("spotify:") {
                    ui.Button("Add to Spotify's queue", icon: .forward) {
                        on press: queue_on_spotify(lib, tr)
                    }
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
            ui.Button(
                "More of this artist",
                icon: .sparkles,
                toggled: lib.taste.preferred.contains(a.key),
            ) {
                on press: prefer_artist(lib, a)
            }
            ui.Button("Not this artist", icon: .ban, toggled: lib.taste.dismissed.contains(a.key)) {
                on press: dismiss_artist(lib, a)
            }
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
            if not a.genres.is_empty() {
                ui.Facts {
                    for g in a.genres.take(4) {
                        ui.Fact(g, icon: .music)
                    }
                }
            }
            ui.Section("Albums") {
                album_grid(lib, albums)
            }
            let kindred = similar_artists(lib, a)
            if not kindred.is_empty() {
                ui.Section("Artists like them") {
                    artist_grid(lib, kindred)
                }
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

/// A place Spotify plays on: what it is, and the way to move what plays there.
view device(d: SpotifyDevice) {
    let lib = root Library
    std.page(d) {
        header: {
            ui.Button(
                if d.active { "Playing here" } else { "Play here" },
                icon: .play,
                toggled: d.active,
            ) {
                on press: transfer_playback(lib, d)
            }
        }
        stack(gap: .related) {
            text(d.name, role: .title)
            text(device_line(d), role: .subheading)
        }
    }
}

/// A mixtape's page: the person's own songs, in their order, and what it is called and says
/// about itself, at Spotify too when it is a Spotify playlist.
view mixtape(m: Mixtape) {
    state typed: Text = m.name
    state about: Text = m.description
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
            if m.spotify != "" {
                text("On Spotify", role: .caption, secondary: true)
            }
            ui.TextInput(bind: typed, placeholder: "Playlist name", fill) {
                on submit: rename_mixtape(m, typed)
            }
            ui.TextInput(bind: about, placeholder: "What this playlist is for", lines: 2, fill) {
                on submit: describe_mixtape(m, about)
            }
            track_list(lib, songs, "Add songs to this playlist")
        }
    }
}

/// The home page: the catalogue's suggestions, in their sections.
view home_page(lib: Library) {
    let recs = recommendations(lib)
    stack(gap: .section) {
        if lib.suggestions.is_empty() and recs.is_empty() {
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
            row(gap: .tight) {
                ui.Button("Open audio file", icon: .file_plus) {
                    on press: file.choose(open_audio_file(lib), accept: audio_kinds())
                }
                ui.Button("Refresh", icon: .refresh) {
                    on press: refresh(lib)
                }
            }
            // What the library itself suggests, before the catalogue's own rows: each song names
            // the choice of the person's that brought it here.
            if not recs.is_empty() {
                ui.Section("Made from your library") {
                    stack(gap: .tight) {
                        for t in recs {
                            row(gap: .tight, align: .center) {
                                ui.ListRow(
                                    title: t.title,
                                    secondary: rec_reason(lib, t),
                                    icon: .music,
                                    item: t,
                                ) {
                                    on activate: play_rec(t)
                                }
                                ui.Button("Play", icon: .play, icon_only: true) {
                                    on press: play_rec(t)
                                }
                                ui.Button("Hide", icon: .eye_off, icon_only: true) {
                                    on press: hide_rec(lib, t)
                                }
                            }
                        }
                    }
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
        // What Spotify keeps that this page does not show yet: one more page of each, where
        // Spotify said there is one.
        if lib.catalogue == "spotify" and lib.spotify_state == "connected" {
            row(gap: .tight, wrap: true) {
                if lib.spotify_next_tracks > 0 {
                    ui.Button("More liked songs", icon: .chevron_down) {
                        on press: spotify_more(lib, "tracks")
                    }
                }
                if lib.spotify_next_albums > 0 {
                    ui.Button("More saved albums", icon: .chevron_down) {
                        on press: spotify_more(lib, "albums")
                    }
                }
                if lib.spotify_next_playlists > 0 {
                    ui.Button("More playlists", icon: .chevron_down) {
                        on press: spotify_more(lib, "playlists")
                    }
                }
                if lib.spotify_next_artists > 0 {
                    ui.Button("More followed artists", icon: .chevron_down) {
                        on press: spotify_more(lib, "artists")
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
            ui.Section("What suggestions may draw on") {
                ui.Button("Artists followed", toggled: taste.followed_artists) {
                    on press: update_taste(lib, "followed_artists", not taste.followed_artists)
                }
                ui.Button("Songs liked", toggled: taste.liked_songs) {
                    on press: update_taste(lib, "liked_songs", not taste.liked_songs)
                }
                ui.Button("Albums kept", toggled: taste.saved_albums) {
                    on press: update_taste(lib, "saved_albums", not taste.saved_albums)
                }
                ui.Button("Artists like these", toggled: taste.similar_artists) {
                    on press: update_taste(lib, "similar_artists", not taste.similar_artists)
                }
                ui.Text(
                    "Suggestions come from these choices alone: no listening history, and nothing asked of a catalogue.",
                    role: .caption,
                    secondary: true,
                )
            }
            let preferred = lib.artists.filter(a -> taste.preferred.contains(a.key))
            let hidden = lib.tracks.filter(t -> taste.hidden.contains(t.id))
            if not preferred.is_empty() {
                ui.Section("More of these artists") {
                    for a in preferred {
                        row(gap: .tight, align: .center) {
                            ui.ListRow(title: a.name, icon: .users, item: a)
                            ui.Button("Stop preferring", icon: .x, icon_only: true) {
                                on press: prefer_artist(lib, a)
                            }
                        }
                    }
                }
            }
            if not hidden.is_empty() {
                ui.Section("Hidden songs") {
                    for t in hidden {
                        row(gap: .tight, align: .center) {
                            ui.ListRow(title: t.title, secondary: t.artist, icon: .music, item: t)
                            ui.Button("Suggest this again", icon: .eye, icon_only: true) {
                                on press: restore_rec(lib, t)
                            }
                        }
                    }
                }
            }
            ui.Section("Catalogue") {
                ui.Button("YouTube Music", toggled: lib.catalogue == "youtube") {
                    on press: set_catalogue(lib, "youtube")
                }
                ui.Button("Spotify", toggled: lib.catalogue == "spotify") {
                    on press: set_catalogue(lib, "spotify")
                }
                ui.Button("Off") {
                    on press: set_catalogue(lib, "off")
                }
            }
            ui.Section("Spotify") {
                if lib.spotify_state == "connecting" {
                    ui.Link(lib.connect_url, label: "Sign in on Spotify")
                    ui.Button("I signed in", icon: .check) {
                        on press: finish_spotify(lib)
                    }
                } else if lib.spotify_state == "connected" {
                    text(spotify_account_line(lib), role: .caption, secondary: true)
                    if lib.spotify_now != "" {
                        text(lib.spotify_now, role: .subheading)
                    }
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.Button("Previous", icon: .skip_back) {
                            on press: remote_previous(lib)
                        }
                        ui.Button(
                            if lib.spotify_playing { "Pause" } else { "Play" },
                            icon: if lib.spotify_playing { .pause } else { .play },
                        ) {
                            on press: if lib.spotify_playing {
                                remote_pause(lib)
                            } else { remote_play(lib) }
                        }
                        ui.Button("Next", icon: .skip_forward) {
                            on press: remote_next(lib)
                        }
                        ui.Button("Refresh", icon: .refresh) {
                            on press: spotify_devices(lib)
                        }
                        ui.Button("Disconnect", icon: .x) {
                            on press: disconnect_spotify(lib)
                        }
                    }
                    for d in lib.devices {
                        ui.ListItem(item: d) {
                            ui.ListRow(
                                title: d.name,
                                secondary: device_line(d),
                                icon: if d.active { .speaker } else { .monitor },
                            )
                        }
                        ui.Button(
                            if d.active { "Playing here" } else { "Play here" },
                            toggled: d.active,
                        ) {
                            on press: transfer_playback(lib, d)
                        }
                    }
                } else if lib.spotify_state == "expired" {
                    text(
                        "Spotify ended the sign-in. Connect again to keep using what you keep there.",
                        role: .caption,
                        secondary: true,
                    )
                    ui.Button("Reconnect Spotify", icon: .link) {
                        on press: connect_spotify(lib)
                    }
                } else if lib.spotify_state == "trouble" {
                    text(lib.spotify_trouble, role: .caption, secondary: true)
                    ui.Button("Connect Spotify", icon: .link) {
                        on press: connect_spotify(lib)
                    }
                } else {
                    ui.Button("Connect Spotify", icon: .link) {
                        on press: connect_spotify(lib)
                    }
                }
            }
            if lib.spotify_state == "connected" {
                ui.Section("What plays next there") {
                    ui.Button("Read Spotify's queue", icon: .refresh) {
                        on press: spotify_queue(lib)
                    }
                    if lib.spotify_queue.is_empty() {
                        text(
                            "Spotify's queue has not been read yet.",
                            role: .caption,
                            secondary: true,
                        )
                    } else {
                        for q in lib.spotify_queue {
                            row(gap: .tight, align: .center) {
                                ui.ListRow(title: q.title, secondary: queue_line(q), icon: .music)
                                ui.Button("Play", icon: .play, icon_only: true) {
                                    on press: remote_play(lib, q.key)
                                }
                            }
                        }
                    }
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
    if t.stream.starts_with("spotify:") {
        outside("music.spotify", ["play", t.stream])
    } else {
        playback.play_list(session(lib), [item_of(t)], at: 0)
    }
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

/// Why the library suggests `t`, in the person's own words, or empty where nothing they chose
/// connects the song to the library.
fn rec_reason(lib: Library, t: Track) -> Text {
    let taste = lib.taste
    if t.liked
        or t.stream == ""
        or taste.hidden.contains(t.id)
        or taste.dismissed.contains(t.artist_key) {
        return ""
    }
    if t.artist_key == "" {
        return ""
    }
    if taste.followed_artists and lib.artists.any(a -> a.key == t.artist_key and a.followed) {
        return "Because you follow {t.artist}."
    }
    if taste.saved_albums
        and t.album_key != ""
        and lib.albums.any(a -> a.key == t.album_key and a.in_library) {
        return "From an album you kept."
    }
    if taste.liked_songs {
        if let seed = lib.tracks.find(
                x -> x.liked and x.artist_key == t.artist_key and x.id() != t.id(),
            ) {
            return "Because you liked {seed.title}."
        }
    }
    if taste.similar_artists {
        let kind = taste_genre(lib, t.artist_key)
        if kind != "" {
            return "Because you listen to {kind}."
        }
    }
    ""
}

/// The artists in the library who play a kind of music `a` plays: what the catalogue's genres
/// say is like them.
fn similar_artists(lib: Library, a: Artist) -> List<Artist> {
    lib.artists.filter(x -> x.key != a.key and shared_genre(a, x) != "")
}

/// A kind of music both artists play, as the catalogue lists them; empty when they share none.
fn shared_genre(a: Artist, b: Artist) -> Text {
    a.genres.find(g -> b.genres.contains(g)) ?? ""
}

/// Whether the person listens to `a`: they follow them, like a song of theirs, or keep one of
/// their albums, if that is a choice their taste still draws on.
fn listened(lib: Library, a: Artist) -> Bool {
    let taste = lib.taste
    if taste.followed_artists and a.followed {
        return true
    }
    if taste.liked_songs and lib.tracks.any(t -> t.liked and t.artist_key == a.key) {
        return true
    }
    taste.saved_albums and lib.albums.any(x -> x.in_library and x.artist_key == a.key)
}

/// A kind of music the person already listens to that the artist `song_by` plays, as the
/// catalogue names it: how a song by an artist they have not heard is still a suggestion.
fn taste_genre(lib: Library, song_by: Text) -> Text {
    if song_by == "" {
        return ""
    }
    if let artist = lib.artists.find(x -> x.key == song_by) {
        for a in lib.artists {
            if a.key != song_by and listened(lib, a) {
                let shared = shared_genre(artist, a)
                if shared != "" {
                    return shared
                }
            }
        }
    }
    ""
}

/// The songs the library suggests: what the person's likes, kept albums and followed artists
/// connect to, the artists they asked more of first, at most eight. Every one names the choice
/// that brought it, as [`rec_reason`] says it.
fn recommendations(lib: Library) -> List<Track> {
    let connected = lib.tracks.filter(t -> rec_reason(lib, t) != "")
    let preferred = connected.filter(t -> lib.taste.preferred.contains(t.artist_key))
    let rest = connected.filter(t -> not lib.taste.preferred.contains(t.artist_key))
    [preferred, rest].flat_map(x -> x).take(8)
}

/// Plays a suggested song.
action play_rec(t: Track) (icon: .play) {
    play_song(t)
}

/// Hides a song from the suggestions. History can undo it, and the taste page brings it back.
action hide_rec(lib: Library, t: Track) (icon: .eye_off, label: "Hide this suggestion") {
    lib.taste.hidden.add(t.id)
}

/// Brings a hidden song back into the suggestions.
action restore_rec(lib: Library, t: Track) (icon: .eye, label: "Suggest this again") {
    lib.taste.hidden.remove(t.id)
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

/// Likes a song, or takes the like back. Where the song is Spotify's and the person's Spotify
/// is connected, their Spotify library keeps it too, and what Spotify says is what the song
/// shows.
action like(t: Track, liked: Bool = not t.liked) (icon: .heart) {
    t.liked = liked
    let lib = root Library
    if lib.catalogue == "spotify"
        and lib.spotify_state == "connected"
        and t.key.starts_with("spotify:") {
        outside("music.spotify", ["like", t.key, told(liked)])
    }
}

/// Likes what plays.
action like_playing(p: playback.Playback, liked: Bool = true) (icon: .heart) {
    let lib = root Library
    if let song = lib.tracks.find(t -> t.id == p.track) { like(song, liked) }
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

/// Keeps an album in the library, or takes it out: at Spotify too, when it is a Spotify album
/// and the person's Spotify is connected.
action save_album(a: Album, keep: Bool = not a.in_library) (icon: .bookmark) {
    a.in_library = keep
    let lib = root Library
    if lib.catalogue == "spotify"
        and lib.spotify_state == "connected"
        and a.key.starts_with("spotify:") {
        outside("music.spotify", ["save_album", a.key, told(keep)])
    }
}

/// Follows an artist, or stops: at Spotify too, when they are a Spotify artist and the person's
/// Spotify is connected.
action follow_artist(a: Artist, followed: Bool = not a.followed) (icon: .bell) {
    a.followed = followed
    let lib = root Library
    if lib.catalogue == "spotify"
        and lib.spotify_state == "connected"
        and a.key.starts_with("spotify:") {
        outside("music.spotify", ["follow", a.key, told(followed)])
    }
}

/// Asks the catalogue for the songs, albums, artists and playlists a query finds.
action search(lib: Library, query: Text) uses time (icon: .search) {
    require query != "" "Type something to search for"
    lib.search_state = "searching"
    lib.search_trouble = ""
    if lib.catalogue == "spotify" {
        outside("music.spotify", ["search", query])
    } else { outside("music.search", query) }
}

/// Asks the catalogue for its home page.
action refresh(lib: Library) uses time (icon: .refresh) {
    lib.home_state = "loading"
    lib.home_trouble = ""
    if lib.catalogue == "spotify" {
        outside("music.spotify", ["home", ""])
    } else { outside("music.home", "") }
}

/// Asks the catalogue for an artist's page.
action refresh_artist(a: Artist) (icon: .refresh) {
    if a.key.starts_with("spotify:") {
        let lib = root Library
        if lib.catalogue == "spotify" {
            outside("music.spotify", ["artist", a.key])
        }
    } else { outside("music.artist", a.key) }
}

/// Asks the catalogue for a playlist's songs.
action open_playlist(p: Playlist) (icon: .refresh, label: "Refresh") {
    if p.key.starts_with("spotify:") {
        outside("music.spotify", ["playlist", p.key])
    } else { outside("music.playlist", p.key) }
}

/// Does what a suggestion on the home page names.
action open_suggestion(lib: Library, row: Suggestion) uses time {
    if row.kind == "track" {
        if let song = lib.tracks.find(t -> t.key == row.target) { play_song(song) }
    } else if row.kind == "album" {
        if row.target.starts_with("spotify:") {
            outside("music.spotify", ["album", row.target])
        } else { outside("music.album", row.target) }
    } else if row.kind == "artist" {
        if row.target.starts_with("spotify:") {
            outside("music.spotify", ["artist", row.target])
        } else { outside("music.artist", row.target) }
    } else if row.kind == "playlist" {
        if row.target.starts_with("spotify:") {
            outside("music.spotify", ["playlist", row.target])
        } else { outside("music.playlist", row.target) }
    }
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

/// Makes a playlist of the person's own: at Spotify too, when it is their catalogue and their
/// Spotify is connected, so the playlist plays there as it does here.
action new_mixtape(lib: Library, name: Text) (icon: .plus) {
    let made = lib.mixtapes.insert(Mixtape(name: name))
    if lib.catalogue == "spotify" and lib.spotify_state == "connected" {
        outside("music.spotify", ["create_playlist", name, made.id])
    }
}

/// Adds a song to a mixtape, and to the Spotify playlist it is.
action mixtape_add(m: Mixtape, t: Track) (icon: .list) {
    if not m.tracks.any(i -> i.ref == t.id) { m.tracks.insert(item_of(t)) }
    if m.spotify != "" and t.stream.starts_with("spotify:") {
        outside("music.spotify", ["playlist_add", m.spotify, t.stream])
    }
}

/// Takes a song out of a mixtape, and out of the Spotify playlist it is.
action mixtape_remove(m: Mixtape, t: Track) (icon: .trash) {
    if let item = m.tracks.find(i -> i.ref == t.id) { m.tracks.remove(item) }
    if m.spotify != "" and t.stream.starts_with("spotify:") {
        outside("music.spotify", ["playlist_remove", m.spotify, t.stream])
    }
}

/// Gives a playlist of the person's another name, at Spotify too when it is a Spotify one.
action rename_mixtape(m: Mixtape, name: Text) (icon: .pencil) {
    m.name = name
    send_playlist(m)
}

/// Sets what a playlist says about itself, at Spotify too when it is a Spotify one.
action describe_mixtape(m: Mixtape, description: Text) {
    m.description = description
    send_playlist(m)
}

/// Sends a playlist's name and description to Spotify, which is the copy its person reads on
/// their other devices; a playlist that is not Spotify's says nothing.
action send_playlist(m: Mixtape) (label: "Send playlist to Spotify") {
    if m.spotify != "" {
        outside("music.spotify", ["edit_playlist", m.spotify, m.name, m.description])
    }
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
    if which == "youtube" {
        outside("music.home", "")
    } else if which == "spotify" {
        outside("music.spotify", ["status", ""])
        outside("music.spotify", ["devices", ""])
    }
}

/// Connects the person's Spotify: the page they sign in on is shown in the Spotify section.
action connect_spotify(lib: Library) (icon: .link, label: "Connect Spotify") {
    lib.spotify_state = "connecting"
    lib.spotify_trouble = ""
    outside("music.spotify", ["connect", ""])
}

/// Takes the sign-in the person gave on Spotify's page.
action finish_spotify(lib: Library) (icon: .check, label: "I signed in") {
    outside("music.spotify", ["finish", ""])
}

/// Forgets this device's Spotify sign-in.
action disconnect_spotify(lib: Library) (icon: .x, label: "Disconnect Spotify") {
    outside("music.spotify", ["disconnect", ""])
}

/// Reads the places Spotify plays on.
action spotify_devices(lib: Library) (icon: .speaker, label: "Refresh devices") {
    outside("music.spotify", ["devices", ""])
}

/// Moves what plays to `d`, from the beginning.
action transfer_playback(lib: Library, d: SpotifyDevice) (icon: .play) {
    outside("music.spotify", ["transfer", d.key])
}

/// Plays `uri` on Spotify, where it plays: empty plays what was playing.
action remote_play(lib: Library, uri: Text = "") (icon: .play, label: "Play on Spotify") {
    outside("music.spotify", ["play", uri])
}

/// Pauses what plays on Spotify.
action remote_pause(lib: Library) (icon: .pause, label: "Pause on Spotify") {
    outside("music.spotify", ["pause", ""])
}

/// Plays what follows on Spotify.
action remote_next(lib: Library) (icon: .skip_forward, label: "Next on Spotify") {
    outside("music.spotify", ["next", ""])
}

/// Plays what played before on Spotify.
action remote_previous(lib: Library) (icon: .skip_back, label: "Previous on Spotify") {
    outside("music.spotify", ["previous", ""])
}

/// Reads what plays on Spotify now.
action spotify_now(lib: Library) (icon: .music, label: "What plays on Spotify") {
    outside("music.spotify", ["now", ""])
}

/// Reads what plays next on Spotify: its own queue, which is shown here and never edited here.
action spotify_queue(lib: Library) (icon: .list, label: "Read Spotify's queue") {
    outside("music.spotify", ["queue", ""])
}

/// Puts a song after what plays on Spotify.
action queue_on_spotify(lib: Library, t: Track) (icon: .forward, label: "Add to Spotify's queue") {
    if t.stream.starts_with("spotify:") {
        outside("music.spotify", ["add_to_queue", t.stream])
    }
}

/// Asks Spotify for one more page of what the person keeps there.
action spotify_more(lib: Library, kind: Text) (icon: .chevron_down, label: "More from Spotify") {
    let at = next_of(lib, kind)
    if at > 0 {
        outside("music.spotify", ["saved_page", kind, "{at}"])
    }
}

/// Turns one of the recommendations' switches.
action update_taste(lib: Library, which: Text, on: Bool) {
    if which == "followed_artists" {
        lib.taste.followed_artists = on
    } else if which == "liked_songs" {
        lib.taste.liked_songs = on
    } else if which == "saved_albums" {
        lib.taste.saved_albums = on
    } else if which == "similar_artists" {
        lib.taste.similar_artists = on
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

/// Who is signed in to Spotify, and what it lets them do.
fn spotify_account_line(lib: Library) -> Text {
    let account = if lib.spotify_account == "" { "Spotify" } else { lib.spotify_account }
    if lib.spotify_product == "" { account } else { "{account} · {lib.spotify_product}" }
}

/// A device, as a row says it: playing here, or what it is.
fn device_line(d: SpotifyDevice) -> Text {
    if d.active { "Playing here" } else if d.kind != "" { d.kind } else { "Spotify device" }
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

/// A switch as the outside effect carries it.
fn told(flag: Bool) -> Text {
    if flag { "true" } else { "false" }
}

/// Where the next page of what the person keeps in Spotify starts; 0 is none.
fn next_of(lib: Library, kind: Text) -> Int {
    if kind == "tracks" {
        lib.spotify_next_tracks
    } else if kind == "albums" {
        lib.spotify_next_albums
    } else if kind == "playlists" {
        lib.spotify_next_playlists
    } else if kind == "artists" { lib.spotify_next_artists } else { 0 }
}

/// A Spotify queue row's line: who sings it, then the show it is an episode of.
fn queue_line(q: SpotifyQueueItem) -> Text {
    if q.artist != "" and q.album != "" and q.artist != q.album {
        "{q.artist}, {q.album}"
    } else if q.artist != "" { q.artist } else { q.album }
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

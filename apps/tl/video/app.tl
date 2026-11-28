app so.tilo.video "Video" (
    icon: .film,
    category: .multimedia,
    tagline: "Plays the videos on your devices",
    keywords: ["video", "videos", "player", "movie", "movies", "film", "films", "play", "watch", "clip", "clips", "media", "mp4", "mkv", "webm", "mov", "avi", "video abspielen", "videoplayer", "film ansehen", "filme", "video", "videos", "abspielen", "anschauen", "wiedergabe", "medienspieler", "klipp"],
)

use so.tilo.playback as playback
use so.tilo.session as session

/// A video a person opened: what it is called and where its bytes play from. It stands in the
/// library so opening the same file again opens the clip it already made, with the history it
/// has, rather than a second one.
type Clip (untitled: "Untitled video") {
    /// The video's name: the file it was opened as, extension included, so the player reads
    /// the container it holds.
    title: Text
    /// The address of its bytes: a file's content (`tilo-blob://…`), or a stream's URL.
    source: Text
    /// The audio tracks the player found in it, in the order it indexes them.
    audio_tracks: List<Text>
    /// The audio track the person chose, by its index; none leaves the player's own choice.
    audio: Option<Int> = .none
    /// The subtitle tracks the player found in it.
    subtitle_tracks: List<Text>
    /// The subtitle track the person chose, by its index; `-1` shows none.
    subtitle: Int = -1
    /// The chapters the player found in it, in the order the scrubber marks them.
    chapters: List<Text>

    search {
        sort title a_z
    }
}

/// A playlist everyone in the workspace sees; its videos are the entries that name it.
type Playlist (title: name) {
    name: Text
}

/// A video of a playlist.
type PlaylistEntry {
    playlist: Playlist
    clip: Clip
}

/// A moment of a video, a bookmark when it says nothing.
type Bookmark (untitled: "Bookmark") {
    clip: Clip
    /// Where in the video it stands, in seconds.
    at: Float
    text: Text = ""
}

/// The videos opened in this workspace. The app's root, and the record a file is opened into.
type Library {
    clips: Collection<Clip>
    playlists: Collection<Playlist>
    entries: Collection<PlaylistEntry>
    /// Videos kept to watch later.
    watch_later: Collection<Clip>
    bookmarks: Collection<Bookmark>
    /// What plays, and where it stands: the shared session Video and Music keep, once this app
    /// played. The player page and the shell's live card read the same record.
    playing: Option<playback.Playback>
    /// Every session this app made, kept as the resources they are; `playing` names the one.
    sessions: Collection<playback.Playback>
}

root library: Library

/// A video this device played, and where this device stopped watching it.
type Recent {
    clip: Clip
    /// Where watching stopped, in seconds.
    at: Float = 0.0
    /// When it was last watched, so the list reads newest first.
    watched: Time
}

/// The videos this device played, newest first. A device keeps its own list: it is this
/// computer's viewing, not something every member of the workspace sees.
type Recents {
    videos: Collection<Recent>
}

device root recent: Recents

/// How this device plays: what a freshly opened video does, the speed it plays at and whether
/// it begins again when it ends.
type PlayerSetup {
    /// A video that is opened begins playing.
    autoplay: Bool = true
    /// The speed it plays at, from `0.25` to `4`; `1` is normal.
    rate: Float = 1.0
    /// A video begins again when it ends.
    repeat_video: Bool = false
}

device root set_up: PlayerSetup

default view library for Library

default view video for Clip

/// A place in a video as a player reads it: `m:ss`, or `h:mm:ss` past an hour.
fn clock_text(at: Float) -> Text {
    let whole = at.max(0.0).to_int()
    let seconds = pad2(whole % 60)
    let minutes = whole / 60 % 60
    if whole >= 3600 {
        "{whole / 3600}:{pad2(minutes)}:{seconds}"
    } else { "{minutes}:{seconds}" }
}

fn pad2(number: Int) -> Text {
    if number < 10 { "0{number}" } else { "{number}" }
}

/// What kinds of file open the video player. The compiler writes them into the app's manifest,
/// so Files and the person's own file system offer the app for exactly these and for no other.
fn video_kinds() -> List<Text> {
    ["video/mp4", "video/webm", "video/x-matroska", "video/quicktime", "video/x-msvideo"]
}

/// Opens a video file: the file a person picked, or one Files handed over, and plays it where it
/// was left. The clip it already made opens instead of a second copy, so its place in history
/// stays one.
primary action open_video(lib: Library, file: File) -> Clip uses time (
    icon: .film,
    did: "opened a video",
    key: "Ctrl+O",
    opens: [
    "video/mp4",
    "video/webm",
    "video/x-matroska",
    "video/quicktime",
    "video/x-msvideo",
    .mp4,
    .webm,
    .mkv,
    .mov,
    .avi,
],
) {
    if let existing = lib.clips.find(c -> c.source == file.url) {
        play_clip(lib, existing)
        opened(lib, existing)
        return existing
    }
    let made = lib.clips.insert(Clip(title: file.name, source: file.url))
    play_clip(lib, made)
    opened(lib, made)
    made
}

/// Takes an opened video in: it is this device's most recent, and it waits paused where it was
/// left when the person chose not to have videos start on their own.
action opened(lib: Library, c: Clip) uses time {
    watched(recent, c, 0.0)
    if not set_up.autoplay { playback.pause(player(lib)) }
}

/// Starts an empty playlist.
action create_playlist(lib: Library, name: Text) -> Playlist (icon: .plus) {
    lib.playlists.insert(Playlist(name: name))
}

/// Deletes a playlist and the entries that name it.
destructive action delete_playlist(lib: Library, pl: Playlist) (icon: .trash) {
    lib.entries.remove_where(e -> e.playlist == pl)
    lib.playlists.remove(pl)
}

/// Adds `c` to `pl`, once.
action add_to_playlist(lib: Library, pl: Playlist, c: Clip) (icon: .list) {
    if lib.entries.find(e -> e.playlist == pl and e.clip == c) == .none {
        lib.entries.insert(PlaylistEntry(playlist: pl, clip: c))
    }
}

/// Keeps `c` to watch later, once.
action add_to_watch_later(lib: Library, c: Clip) (icon: .bookmark) {
    if lib.watch_later.find(k -> k == c) == .none { lib.watch_later.insert(c) }
}

/// Bookmarks the moment `at` of `c`, with `text` when the person wrote something.
action add_bookmark(lib: Library, c: Clip, at: Float, text: Text = "") -> Bookmark (
    icon: .bookmark,
) {
    lib.bookmarks.insert(Bookmark(clip: c, at: at, text: text))
}

/// Deletes a bookmark or note.
destructive action delete_bookmark(lib: Library, b: Bookmark) (icon: .trash) {
    lib.bookmarks.remove(b)
}

/// Jumps to a bookmark: its video plays from the moment the bookmark marks.
action jump_to_bookmark(lib: Library, b: Bookmark) uses time (
    icon: .bookmark,
    label: "Jump to bookmark",
) {
    play_clip(lib, b.clip)
    playback.set_position(player(lib), b.at)
}

/// Renames a playlist.
action rename_playlist(pl: Playlist, name: Text) (
    icon: .pencil,
    label: "Rename",
    did: "renamed a playlist",
) {
    pl.name = name
}

/// Plays a queued item of the session, keeping the rest of the queue as it stands.
action play_queued(lib: Library, item: playback.Item) (icon: .play, label: "Play queued video") {
    let p = player(lib)
    if let at = p.queue.index_of(item) { playback.select(p, at) }
}

/// Removes a queued item from the session.
action remove_queued(lib: Library, item: playback.Item) (icon: .x, label: "Remove from queue") {
    let p = player(lib)
    if let at = p.queue.index_of(item) { playback.queue_remove(p, index: at) }
}

/// Keeps where the player got to in `c`: the session's place, and this device's recent row.
action record_progress(lib: Library, c: Clip, share: Float) uses time {
    let p = player(lib)
    playback.set_share(p, share)
    watched(recent, c, share * p.length)
}

/// Records that this device watched `c` to `at`, newest first, keeping the last 32.
action watched(r: Recents, c: Clip, at: Float) uses time {
    if let held = r.videos.find(v -> v.clip == c) {
        held.at = at
        held.watched = now
        return
    }
    r.videos.insert(Recent(clip: c, at: at, watched: now))
    // The newest is kept and the one watched longest ago goes when the list is full.
    if r.videos.len() > 32 {
        if let oldest = r.videos.sort_by(v -> v.watched).first() { r.videos.remove(oldest) }
    }
}

/// Clears this device's recent videos; the clips themselves stay in the library.
destructive action clear_recent(r: Recents) (
    icon: .trash,
    label: "Clear recent videos",
    did: "cleared the recent videos",
) {
    for v in r.videos.map(v -> v) {
        r.videos.remove(v)
    }
}

/// Remembers the audio tracks, subtitles and chapters the player found in `c`, so the person
/// can choose among them and the scrubber marks where a chapter begins.
action found_tracks(c: Clip, audio: List<Text>, subtitles: List<Text>, chapters: List<Text>) (
    label: "Found the video's tracks",
) {
    c.audio_tracks = audio
    c.subtitle_tracks = subtitles
    c.chapters = chapters
}

/// Uses the audio track at `at` of this video.
action choose_audio(c: Clip, name: Text) (icon: .speaker, label: "Use this audio track") {
    c.audio = .some(c.audio_tracks.index_of(name) ?? 0)
}

/// Shows the subtitle track called `name`, or none when it is "Off".
action choose_subtitles(c: Clip, name: Text) (icon: .captions, label: "Show subtitles") {
    c.subtitle = if name == "Off" { -1 } else { c.subtitle_tracks.index_of(name) ?? -1 }
}

/// Opens Settings, where more video and audio formats are installed.
action open_formats() (icon: .download, label: "Play more video and audio formats") {
    session.open_app(app: "so.tilo.settings")
}

/// Plays videos that are opened from now on, or leaves them where they were left when they open.
action set_autoplay(s: PlayerSetup, on: Bool) (icon: .play, label: "Play a video when it opens") {
    s.autoplay = on
}

/// The speed videos play at on this device, from `0.25` to `4`.
action set_speed(s: PlayerSetup, rate: Float = 1.0) (icon: .gauge, label: "Playback speed") {
    s.rate = rate.max(0.25).min(4.0)
}

/// Whether a video begins again when it ends.
action set_repeat(s: PlayerSetup, on: Bool) (icon: .repeat, label: "Repeat the video") {
    s.repeat_video = on
}

/// Opens the videos a person dropped on the window: each file becomes its clip, and the first
/// plays in the shared session.
action open_dropped(lib: Library, files: List<File>) uses time (
    icon: .film,
    did: "opened a video",
) {
    require not files.is_empty() "Drop a video file"
    for file in files {
        if lib.clips.find(c -> c.source == file.url) == .none {
            lib.clips.insert(Clip(title: file.name, source: file.url))
        }
    }
    if let first = files.first() {
        if let clip = lib.clips.find(c -> c.source == first.url) {
            play_clip(lib, clip)
            opened(lib, clip)
        }
    }
}

/// The session this app plays through: the one it has, or one made now. It is kept both in
/// `playing`, which a view reads, and in `sessions`, so the record is a resource of its own.
action player(lib: Library) -> playback.Playback {
    if let held = lib.playing {
        return held
    }
    let made = lib.sessions.insert(playback.Playback())
    lib.playing = .some(made)
    made
}

/// Plays or pauses the session when it is showing `c`: what the P key and the window's player
/// control run, without needing the root passed in.
action toggle_playing(c: Clip) (icon: .play, key: "P") {
    let lib = root Library
    let p = player(lib)
    if p.track == c.id {
        playback.set_playing(p, p.state != .playing)
    } else { play_clip(lib, c) }
}

/// Plays `c` in the shared session, from where it was left (the beginning the first time):
/// what the window's Play button, the palette and an agent's `play` run. The place is the
/// shared session's own record, so the video continues on this device or another.
action play_clip(lib: Library, c: Clip) (icon: .play, did: "played a video") {
    let p = player(lib)
    playback.play_list(p, [playback.Item(ref: c.id, title: c.title, source: c.source)], at: 0)
    playback.resume(p, c.id)
}

/// Plays `c` from its beginning, as a person who watched it before chooses.
action restart_clip(lib: Library, c: Clip) uses time (
    icon: .rotate_ccw,
    label: "Play from the beginning",
) {
    let p = player(lib)
    playback.play_list(p, [playback.Item(ref: c.id, title: c.title, source: c.source)], at: 0)
    playback.remember_position(p, 0.0, now)
}

/// The window with no video open: what to do, and nothing else. What the person watched is the
/// system's history, not a second list the app keeps.
view library(lib: Library) {
    std.page(lib) {
        ui.DropZone("Drop a video", message: "Any video file", icon: .film) {
            on drop(files: List<File>): open_dropped(lib, files)
        }
        if lib.clips.is_empty() {
            ui.EmptyState(
                "Play a video",
                message: "Open a video file from your device.",
                icon: .film,
                action: "Open video",
            ) {
                on act: file.choose(open_video(lib), accept: video_kinds())
            }
        } else {
            stack(gap: .tight) {
                for clip in lib.clips {
                    ui.ListRow(
                        title: clip.title,
                        icon: .film,
                        badge: if clip.source.starts_with("file:") { "Local" } else { "" },
                    ) {
                        on activate: open(clip)
                    }
                }
                ui.Button("Open video", icon: .folder_open) {
                    on press: file.choose(open_video(lib), accept: video_kinds())
                }
            }
        }
    }
}

/// A video's window: the picture plays at the top, as wide as the page gives it, 16:9 until the
/// decoder answers with the video's own shape. The player's controls are the shared ones, so
/// play, pause, seeking, the volume and fullscreen are the system's, not this app's; what it
/// plays is the workspace's session, so the bar, the lock screen and every device show it.
view video(c: Clip) {
    state sheet: Bool = false
    state tab: Text = "queue"
    state moment: Float = 0.0
    state jump: Float = 0.0
    state jump_seq: Int = 0
    state trouble: Text = ""
    let lib = root Library
    let setup = set_up
    std.page(c, canvas: true) {
        header: {
            ui.Button("Open video", icon: .folder_open) {
                on press: file.choose(open_video(lib), accept: video_kinds())
            }
            ui.Button("Library", icon: .eye, toggled: sheet) {
                on press: set(sheet, not sheet)
            }
            ui.Button("Play from the beginning", icon: .rotate_ccw) {
                on press: restart_clip(lib, c)
            }
            ui.Button("Bookmark this moment", icon: .bookmark) {
                on press: add_bookmark(lib, c, moment)
            }
            ui.Button("Watch later", icon: .clock) {
                on press: add_to_watch_later(lib, c)
            }
        }
        if sheet {
            ui.FloatingPanel(width: 360.0) {
                row(gap: .tight) {
                    ui.Button("Queue", icon: .list_music, toggled: tab == "queue") {
                        on press: set(tab, "queue")
                    }
                    ui.Button("Recent", icon: .history, toggled: tab == "recent") {
                        on press: set(tab, "recent")
                    }
                    ui.Button("Shared", icon: .users, toggled: tab == "shared") {
                        on press: set(tab, "shared")
                    }
                }
                if tab == "queue" {
                    if let p = lib.playing {
                        if p.queue.is_empty() {
                            ui.EmptyState("Nothing queued", icon: .list_music, inline: true)
                        } else {
                            stack(gap: .tight) {
                                for item in p.queue {
                                    row(gap: .tight, align: .center) {
                                        ui.ListRow(title: item.title, icon: .film) {
                                            on activate: play_queued(lib, item)
                                        }
                                        ui.Button("Remove from queue", icon: .x, icon_only: true) {
                                            on press: remove_queued(lib, item)
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        ui.EmptyState("Nothing queued", icon: .list_music, inline: true)
                    }
                } else if tab == "recent" {
                    let r = recent
                    if r.videos.is_empty() {
                        ui.EmptyState("No recent videos", icon: .history, inline: true)
                    } else {
                        stack(gap: .tight) {
                            for v in r.videos.sort_by(one -> one.watched, desc) {
                                ui.ListRow(
                                    title: v.clip.title,
                                    secondary: "Stopped at {clock_text(v.at)}",
                                    icon: .film,
                                ) {
                                    on activate: play_clip(lib, v.clip)
                                }
                            }
                            ui.Button("Clear recent videos", icon: .trash) {
                                on press: clear_recent(r)
                            }
                        }
                    }
                } else {
                    stack(gap: .related) {
                        text("Playlists", role: .subheading)
                        for pl in lib.playlists {
                            row(gap: .tight, align: .center) {
                                ui.ListRow(pl.name, icon: .list_music) {
                                    on activate: add_to_playlist(lib, pl, c)
                                }
                                ui.Button("Delete shared playlist", icon: .trash, icon_only: true) {
                                    on press: delete_playlist(lib, pl)
                                }
                            }
                        }
                        ui.Button("New playlist", icon: .plus) {
                            on press: create_playlist(lib, "New playlist")
                        }
                        text("Watch later", role: .subheading)
                        if lib.watch_later.is_empty() {
                            ui.EmptyState("Nothing kept for later", icon: .clock, inline: true)
                        } else {
                            for kept in lib.watch_later {
                                ui.ListRow(title: kept.title, icon: .film) {
                                    on activate: play_clip(lib, kept)
                                }
                            }
                        }
                        text("Bookmarks", role: .subheading)
                        if lib.bookmarks.is_empty() {
                            ui.EmptyState("No bookmarks", icon: .bookmark, inline: true)
                        } else {
                            for b in lib.bookmarks {
                                row(gap: .tight, align: .center) {
                                    ui.ListRow(
                                        title: if b.text == "" { b.clip.title } else { b.text },
                                        secondary: b.clip.title,
                                        icon: .bookmark,
                                    ) {
                                        on activate: {
                                            set(jump, b.at)
                                            set(jump_seq, jump_seq + 1)
                                            jump_to_bookmark(lib, b)
                                        }
                                    }
                                    ui.Button(
                                        "Delete timestamp annotation",
                                        icon: .trash,
                                        icon_only: true,
                                    ) {
                                        on press: delete_bookmark(lib, b)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        layers(fill, aspect: 16.0 / 9.0) {
            if let p = lib.playing {
                if p.track == c.id {
                    ui.VideoPlayer(
                        title: c.title,
                        source: c.source,
                        playing: p.state == .playing,
                        position: p.at,
                        seek_to: if jump_seq > 0 { .some(jump) } else { .none },
                        seek_to_seq: jump_seq,
                        volume: p.volume,
                        muted: p.volume <= 0.0,
                        rate: setup.rate,
                        repeat: setup.repeat_video,
                        audio_track: c.audio,
                        subtitle_track: c.subtitle,
                        fill,
                    ) {
                        on set_playing(now_playing: Bool): playback.set_playing(p, now_playing)
                        on set_volume(level: Float): playback.set_volume(p, level)
                        on set_rate(rate: Float): set_speed(setup, rate)
                        on progress(share: Float): record_progress(lib, c, share)
                        on length(seconds: Float): playback.set_length(p, seconds)
                        on seek(place: Float): playback.seek(p, place)
                        on audio_tracks(labels: List<Text>): found_tracks(
                            c,
                            audio: labels,
                            subtitles: c.subtitle_tracks,
                            chapters: c.chapters,
                        )
                        on subtitle_tracks(labels: List<Text>): found_tracks(
                            c,
                            audio: c.audio_tracks,
                            subtitles: labels,
                            chapters: c.chapters,
                        )
                        on chapters(labels: List<Text>): found_tracks(
                            c,
                            audio: c.audio_tracks,
                            subtitles: c.subtitle_tracks,
                            chapters: labels,
                        )
                        on fail(what: Text, at: Float): set(
                            trouble,
                            "Stopped after {at.max(0.0).to_int()} seconds: {what}",
                        )
                    }
                } else {
                    row(fill, align: .center, distribute: .center) {
                        ui.Button("Play", icon: .play, role: .primary) {
                            on press: play_clip(lib, c)
                        }
                    }
                }
            } else {
                row(fill, align: .center, distribute: .center) {
                    ui.Button("Play", icon: .play, role: .primary) {
                        on press: play_clip(lib, c)
                    }
                }
            }
        }
        if trouble != "" {
            ui.EmptyState(
                "Can't play this video yet",
                message: trouble,
                icon: .circle_alert,
                error: true,
                action: "Play more video and audio formats",
                inline: true,
            ) {
                on act: open_formats()
            }
        }
        stack(gap: .related) {
            text(c.title, role: .title)
            row(gap: .related, align: .center, wrap: true) {
                ui.Toggle(shown: setup.autoplay, label: "Play a video when it opens") {
                    on check(on): set_autoplay(setup, on)
                }
                ui.Toggle(shown: setup.repeat_video, label: "Repeat the video") {
                    on check(on): set_repeat(setup, on)
                }
            }
            if c.audio_tracks.len() > 1 {
                ui.Dropdown(
                    value: c.audio_tracks.at(c.audio ?? 0) ?? "",
                    options: c.audio_tracks,
                    caption: "Audio track",
                ) {
                    on change(name): choose_audio(c, name)
                }
            }
            if not c.subtitle_tracks.is_empty() {
                ui.Dropdown(
                    value: if c.subtitle < 0 {
                        "Off"
                    } else {
                        c.subtitle_tracks.at(c.subtitle) ?? "Off"
                    },
                    options: [["Off"], c.subtitle_tracks].flat_map(x -> x),
                    caption: "Subtitles",
                ) {
                    on change(name): choose_subtitles(c, name)
                }
            }
            if not c.chapters.is_empty() {
                text(c.chapters.join("  ·  "), role: .caption, secondary: true)
            }
        }
    }
}

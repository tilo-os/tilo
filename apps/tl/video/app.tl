app so.tilo.video "Video" (
    icon: .film,
    category: .multimedia,
    tagline: "Plays the videos on your devices",
    keywords: ["video", "videos", "player", "movie", "movies", "film", "films", "play", "watch", "clip", "clips", "media", "mp4", "mkv", "webm", "mov", "avi", "video abspielen", "videoplayer", "film ansehen", "filme", "video", "videos", "abspielen", "anschauen", "wiedergabe", "medienspieler", "klipp"],
)

use so.tilo.playback as playback

/// A video a person opened: what it is called and where its bytes play from. It stands in the
/// library so opening the same file again opens the clip it already made, with the history it
/// has, rather than a second one.
type Clip (untitled: "Untitled video") {
    /// The video's name: the file it was opened as, extension included, so the player reads
    /// the container it holds.
    title: Text
    /// The address of its bytes: a file's content (`tilo-blob://…`), or a stream's URL.
    source: Text

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

default view library for Library

default view video for Clip

/// What kinds of file open the video player. The compiler writes them into the app's manifest,
/// so Files and the person's own file system offer the app for exactly these and for no other.
fn video_kinds() -> List<Text> {
    ["video/mp4", "video/webm", "video/x-matroska", "video/quicktime", "video/x-msvideo"]
}

/// Opens a video file: the file a person picked, or one Files handed over, and plays it where it
/// was left. The clip it already made opens instead of a second copy, so its place in history
/// stays one.
primary action open_video(lib: Library, file: File) -> Clip (
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
        return existing
    }
    let made = lib.clips.insert(Clip(title: file.name, source: file.url))
    play_clip(lib, made)
    made
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

/// Opens the videos a person dropped on the window: each file becomes its clip, and the first
/// plays in the shared session.
action open_dropped(lib: Library, files: List<File>) (icon: .film, did: "opened a video") {
    require not files.is_empty() "Drop a video file"
    for file in files {
        if lib.clips.find(c -> c.source == file.url) == .none {
            lib.clips.insert(Clip(title: file.name, source: file.url))
        }
    }
    if let first = files.first() {
        if let clip = lib.clips.find(c -> c.source == first.url) { play_clip(lib, clip) }
    }
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

/// Plays or pauses the session when it is showing `c`: what the P key and the window's player
/// control run, without needing the root passed in.
action toggle_playing(c: Clip) (icon: .play, key: "P") {
    let lib = root Library
    let p = session(lib)
    if p.track == c.id {
        playback.set_playing(p, p.state != .playing)
    } else { play_clip(lib, c) }
}

/// Plays `c` in the shared session, from where it was left (the beginning the first time):
/// what the window's Play button, the palette and an agent's `play` run. The place is the
/// shared session's own record, so the video continues on this device or another.
action play_clip(lib: Library, c: Clip) (icon: .play, did: "played a video") {
    let p = session(lib)
    playback.play_list(p, [playback.Item(ref: c.id, title: c.title, source: c.source)], at: 0)
    playback.resume(p, c.id)
}

/// Plays `c` from its beginning, as a person who watched it before chooses.
action restart_clip(lib: Library, c: Clip) uses time (
    icon: .rotate_ccw,
    label: "Play from the beginning",
) {
    let p = session(lib)
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
                    ui.ListRow(title: clip.title, icon: .film) {
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
    state trouble: Text = ""
    let lib = root Library
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
                                    ui.ListRow(title: item.title, icon: .film)
                                }
                            }
                        }
                    } else {
                        ui.EmptyState("Nothing queued", icon: .list_music, inline: true)
                    }
                } else {
                    stack(gap: .related) {
                        text("Playlists", role: .subheading)
                        if lib.playlists.is_empty() {
                            ui.Button("New playlist", icon: .plus) {
                                on press: create_playlist(lib, "New playlist")
                            }
                        } else {
                            for pl in lib.playlists {
                                ui.ListRow(title: pl.name, icon: .list_music) {
                                    on activate: add_to_playlist(lib, pl, c)
                                }
                            }
                            ui.Button("New playlist", icon: .plus) {
                                on press: create_playlist(lib, "New playlist")
                            }
                        }
                        text("Watch later", role: .subheading)
                        if lib.watch_later.is_empty() {
                            ui.EmptyState("Nothing kept for later", icon: .clock, inline: true)
                        } else {
                            for kept in lib.watch_later {
                                ui.ListRow(title: kept.title, icon: .film)
                            }
                        }
                        text("Bookmarks", role: .subheading)
                        if lib.bookmarks.is_empty() {
                            ui.EmptyState("No bookmarks", icon: .bookmark, inline: true)
                        } else {
                            for b in lib.bookmarks {
                                ui.ListRow(
                                    title: if b.text == "" { b.clip.title } else { b.text },
                                    secondary: b.clip.title,
                                    icon: .bookmark,
                                )
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
                        fill,
                    ) {
                        on set_playing(now_playing: Bool): playback.set_playing(p, now_playing)
                        on progress(share: Float): playback.set_share(p, share)
                        on length(seconds: Float): playback.set_length(p, seconds)
                        on seek(place: Float): playback.seek(p, place)
                        on fail(what: Text, at: Float): set(
                            trouble,
                            "Stopped after " + at.max(0.0).to_int().to_text() + " seconds: " + what,
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
                "Can't play this video",
                message: trouble,
                icon: .circle_alert,
                error: true,
                inline: true,
            )
        }
        stack(gap: .related) {
            text(c.title, role: .title)
        }
    }
}

app so.tilo.tube "YouTube" (
    icon: .film,
    category: .multimedia,
    tagline: "Browses and plays YouTube videos",
    keywords: ["youtube", "video", "videos", "streaming", "watch", "channel", "channels", "subscriptions", "creators", "trending", "filme", "videos ansehen", "kanäle", "kanaele", "kanal", "schauen", "mediathek", "anschauen", "abos", "videoportal"],
    schemes: "tube",
)

/// A channel someone publishes videos on. The service writes what it says (its name, handle and
/// numbers); the person's own relation to it is a `Subscription`, so subscribing never depends on
/// what the service last answered.
type Channel (origin: youtube.channel) {
    key: Text merge register
    name: Text
    handle: Text
    subscribers: Int
    avatar: Text
    banner: Text
    description: Text
    verified: Bool
    /// The search that found it, empty for a channel the person's subscriptions or a page
    /// brought in.
    found_for: Text = ""

    search {
        sort name a_z
        hide avatar
        hide banner
        hide found_for
    }
}

/// A video, as the service says it is, with what the person did with it: liked, put on the watch
/// queue, and where they left off (the history entry says that).
type Video (origin: youtube.video) {
    key: Text merge register
    title: Text
    channel_key: Text merge register
    channel_name: Text
    channel_avatar: Text
    views: Int
    published: Time
    /// How long it runs, in seconds.
    duration: Int
    description: Text
    likes: Int
    thumbnail: Text
    /// The address its bytes play from, as the service answered: empty until it is asked.
    stream: Text
    /// The whole-file qualities it offers, best first; empty for a live stream, which has one.
    stream_qualities: List<Text>
    /// Where each of its qualities plays from, by the label the menu shows.
    streams: Map<Text>
    /// The caption tracks it offers, by language code: the label each shows.
    caption_tracks: Map<Text>
    /// The language of the captions on, empty for off.
    caption_language: Text = ""
    /// The words of the chosen caption track, as the service fetched them.
    captions: Text
    /// The quality it plays at, empty for the best.
    quality: Text = ""
    /// Whether it is a live stream rather than a recording.
    live: Bool
    /// The person liked it.
    liked: Bool
    /// The trending feed listed it.
    trending: Bool = false
    /// The search that found it, empty for a video a feed or a page brought in.
    found_for: Text = ""

    search {
        filter liked "Liked" (icon: .thumbs_up)
        filter trending "Trending" (icon: .flame)
        sort published newest
        hide thumbnail
        hide stream
        hide stream_qualities
        hide streams
        hide caption_tracks
        hide caption_language
        hide captions
        hide quality
        hide channel_avatar
        hide found_for
    }
}

/// A playlist: videos in an order its maker chose. The videos are the client's own, held in the
/// order the playlist lists them, so a page plays and shows them as its maker put them.
type Playlist (origin: youtube.playlist) {
    key: Text merge register
    title: Text
    channel_key: Text merge register
    channel_name: Text
    video_count: Int
    thumbnail: Text
    /// The keys of the videos it holds, in its maker's order: none until the service answered
    /// what it holds.
    video_keys: List<Text>
    /// The search that found it.
    found_for: Text = ""

    search {
        sort title a_z
        hide thumbnail
        hide video_keys
        hide found_for
    }
}

/// A channel the person subscribed to, by its key: membership, so two people subscribing at once
/// keep it once, and the feed reads the channels' videos.
type Subscription {
    /// The channel's key, as the service names it.
    key: Text merge register
    name: Text

    search {
        sort name a_z
    }
}

/// One video someone put on the workspace's watch queue. A collection keeps the order its items
/// were placed in, so the queue is played in the order the videos were put on it, oldest first.
type QueuedVideo {
    /// The video's key, as the service names it.
    key: Text merge register
}

/// How one member likes to watch. Each member's own record: it follows them to every device, and
/// a view reads the record of the person looking at it.
type PlaybackPrefs {
    member: Actor
    /// Play the next video when this one ends.
    autoplay: Bool = false
    /// How fast it plays: 0.25×, 0.5×, 1×, 1.5×, 2× or 3×.
    speed: Float = 1.0
    /// The tallest picture they want, as YouTube names it (`1080p`, `720p`, `480p`), or `auto`
    /// for the best that fits.
    quality: Text = "auto"
    /// Show the video's captions when it has them.
    captions: Bool = false
}

/// One video the person watched: when, and where they left off. Watched again, it moves to the
/// front rather than appearing twice. Each entry names the member who watched it, so a member
/// reads their own history and nobody else's.
type WatchEntry {
    /// The video's key, as the service names it.
    key: Text merge register
    title: Text
    channel_name: Text
    /// Who watched it. Empty for an entry the native app's history carried before this one
    /// recorded members: it stays the workspace's, and watching the video again makes it theirs.
    member: Option<Actor> = .none
    watched: Time
    /// How far in they were, as a share of its length; zero when they watched it to the end.
    progress: Float = 0.0

    search {
        sort watched newest
        hide progress
        hide member
    }
}

/// What the client holds: the channels, videos and playlists the service answered with, the
/// person's subscriptions, the workspace's watch queue, each member's watch history and playback
/// preferences, and how the last search stands.
type YouTube {
    /// What the client is called. A root with no text of its own is named by the first text it
    /// holds, so without this the client would be called after the first channel it holds: the
    /// fetch actions and the scenarios write it once, and it names the client from then on.
    title: Text = "YouTube"
    channels: Collection<Channel>
    videos: Collection<Video>
    playlists: Collection<Playlist>
    subscriptions: Collection<Subscription>
    /// The workspace's queue of videos to watch, oldest first.
    queue: Collection<QueuedVideo>
    history: Collection<WatchEntry>
    /// Each member's own playback preferences.
    playback: Collection<PlaybackPrefs>
    /// What the last search asks for, as it was typed.
    search_text: Text = ""
    /// How the search of `search_text` stands: `searching`, `done`, `failed` or empty.
    search_state: Text = ""
    /// Why the last search failed; empty while it went through.
    search_trouble: Text = ""
    /// How the subscriptions feed stands: `loading`, `done`, `failed` or empty.
    feed_state: Text = ""
    /// Why the client is not in step now: YouTube cannot be reached, or what it said could not
    /// be saved; empty while it is.
    trouble: Text = ""
    /// Whether YouTube answered the last time it was asked.
    online: Bool = true
    /// The speed videos play at: 1.0 is normal.
    rate: Float = 1.0
}

root YouTube

default view feed for YouTube

default view channel_page for Channel

default view video_page for Video

default view playlist_page for Playlist

default view watched_page for WatchEntry

row view video_row for Video

row view channel_row for Channel

row view watch_row for WatchEntry

/// A count as people read one: 1.9M, 840K, 512. The tenth is left out when it is zero: 210K,
/// not 210.0K.
fn count_text(n: Int) -> Text {
    if n >= 1000000000 {
        tenths(n / 100000000, "B")
    } else if n >= 1000000 {
        tenths(n / 100000, "M")
    } else if n >= 1000 { tenths(n / 100, "K") } else { "{n}" }
}

/// `tenths` tenths of a unit, as a whole number and its tenth: 19 is "1.9", 21 is "2.1", and
/// 210 is "21" rather than "21.0".
fn tenths(tenths: Int, suffix: Text) -> Text {
    if tenths % 10 == 0 { "{tenths / 10}{suffix}" } else { "{tenths / 10}.{tenths % 10}{suffix}" }
}

/// How many people watched it.
fn views_text(n: Int) -> Text {
    if n == 1 { "1 view" } else { "{count_text(n)} views" }
}

/// How many people follow the channel.
fn subscribers_text(n: Int) -> Text {
    if n == 1 { "1 subscriber" } else { "{count_text(n)} subscribers" }
}

/// A number as two digits: the minutes and seconds of a length.
fn two(n: Int) -> Text {
    if n < 10 { "0{n}" } else { "{n}" }
}

/// How long a video runs, as a player reads it: 4:32, 1:02:14.
fn clock(secs: Int) -> Text {
    if secs >= 3600 {
        "{secs / 3600}:{two(secs % 3600 / 60)}:{two(secs % 60)}"
    } else { "{secs / 60}:{two(secs % 60)}" }
}

/// Whether the person subscribed to the channel.
fn subscribed(yt: YouTube, c: Channel) -> Bool {
    yt.subscriptions.any(s -> s.key == c.key)
}

/// `who`'s own history: the entries they watched, and the ones carried from before this app
/// recorded members, newest first.
fn my_history(yt: YouTube, who: Actor) -> List<WatchEntry> {
    yt.history.filter(h -> h.member == .some(who) or h.member == .none).sort_by(.watched, desc)
}

/// The entry of `who`'s own history that says when they watched the video, if they did.
fn watched_before(yt: YouTube, who: Actor, v: Video) -> Option<WatchEntry> {
    yt.history.find(h -> h.key == v.key and (h.member == .some(who) or h.member == .none))
}

/// Whether the video is on the workspace's watch queue.
fn queued(yt: YouTube, key: Text) -> Bool {
    yt.queue.any(q -> q.key == key)
}

/// The queue's videos, in the order they were put on it.
fn queue_videos(yt: YouTube) -> List<Video> {
    yt.queue.flat_map(q -> yt.videos.filter(v -> v.key == q.key))
}

/// How `who` likes to watch, when they said: a member who set nothing reads nothing here, and
/// every default holds.
fn prefs_for(yt: YouTube, who: Actor) -> Option<PlaybackPrefs> {
    yt.playback.find(p -> p.member == who)
}

/// The playlist's videos, in its maker's order: the ones the client holds, by the keys it lists.
fn playlist_videos(yt: YouTube, p: Playlist) -> List<Video> {
    p.video_keys.flat_map(key -> yt.videos.filter(v -> v.key == key))
}

/// The channels the person follows, as the client holds them, by name: a subscription whose
/// channel has not been fetched yet is not listed here.
fn subscribed_channels(yt: YouTube) -> List<Channel> {
    yt.channels.filter(c -> yt.subscriptions.any(s -> s.key == c.key)).sort_by(.name)
}

/// The videos of the channels the person subscribed to, newest first: the subscriptions feed.
fn subscribed_videos(yt: YouTube) -> List<Video> {
    yt.videos
        .filter(v -> yt.subscriptions.any(s -> s.key == v.channel_key))
        .sort_by(.published, desc)
}

/// The videos the trending feed listed, most watched first.
fn trending_videos(yt: YouTube) -> List<Video> {
    yt.videos.filter(.trending).sort_by(.views, desc)
}

/// What the last search found, of the kind `wanted`: `video`, `channel` or `playlist`.
fn found(yt: YouTube) -> List<Video> {
    yt.videos
        .filter(v -> v.found_for != "" and v.found_for == yt.search_text)
        .sort_by(.published, desc)
}

fn found_channels(yt: YouTube) -> List<Channel> {
    yt.channels.filter(c -> c.found_for != "" and c.found_for == yt.search_text)
}

fn found_playlists(yt: YouTube) -> List<Playlist> {
    yt.playlists.filter(p -> p.found_for != "" and p.found_for == yt.search_text)
}

/// Subscribes to the channel, or unsubscribes: the button says which it will do.
action subscribe(c: Channel, subscribed: Bool = true) (icon: .bell) {
    let yt = root YouTube
    if subscribed {
        if not yt.subscriptions.any(s -> s.key == c.key) {
            yt.subscriptions.insert(Subscription(key: c.key, name: c.name))
        }
    } else {
        yt.subscriptions.remove_where(s -> s.key == c.key)
    }
}

/// Subscribes to the channel, or takes the subscription back.
action toggle_subscription(c: Channel) (
    label: if subscribed(root YouTube, c) { "Unsubscribe" } else { "Subscribe" },
    icon: .bell,
    on: subscribed(root YouTube, c),
) {
    subscribe(c, not subscribed(root YouTube, c))
}

/// Likes the video, or takes the like back.
action like(v: Video, liked: Bool = not v.liked) (
    label: if v.liked { "Unlike" } else { "Like" },
    icon: .thumbs_up,
    on: v.liked,
) {
    v.liked = liked
}

/// Puts the video on the workspace's watch queue, or takes it off. A video put on it goes after
/// the ones already waiting, so the queue plays in the order people added to it.
action watch_later(
    v: Video,
    saved: Bool (label: "On the watch queue") = not queued(root YouTube, v.key),
) (
    label: if queued(root YouTube, v.key) { "Remove from the queue" } else { "Watch later" },
    icon: .bookmark,
    on: queued(root YouTube, v.key),
) {
    let yt = root YouTube
    if saved {
        if not queued(yt, v.key) { yt.queue.insert(QueuedVideo(key: v.key)) }
    } else {
        yt.queue.remove_where(q -> q.key == v.key)
    }
}

/// Plays the queue from its oldest video: what was put on it first plays first.
action play_queue(yt: YouTube) uses time (icon: .play, label: "Play all") {
    if let first = queue_videos(yt).first() { progress(first, 0.0) }
}

/// Plays the video, whether or not it was played before.
primary action play(v: Video) uses time (icon: .play) {
    progress(v, 0.0)
}

/// Plays the video from where the person left off, when they did not finish it.
action resume(v: Video) uses time (icon: .play, label: "Resume") {
    let yt = root YouTube
    progress(v, watched_before(yt, invoker, v)?.progress ?? 0.0)
}

/// Records how far into the video the person is, and that they watched it now: it moves to the
/// front of the history, and asks the service for the address its bytes play from when that is
/// not known yet. Starting a video, resuming it and moving the playhead as it plays are one
/// action, so a video has one place it is played from.
action progress(v: Video, at: Float) uses time {
    let yt = root YouTube
    let mine = invoker
    let watched = now
    // A video left within twenty seconds of its end is watched whole, not left at its last
    // second: resuming it starts it again rather than at the credits.
    let kept = if v.duration > 0 and at + 20.0 / v.duration.to_float() >= 1.0 { 0.0 } else { at }
    if let entry = yt.history.find(
            h -> h.key == v.key and (h.member == .some(mine) or h.member == .none),
        ) {
        // An entry the native app's history carried becomes theirs when they watch it again.
        entry.member = .some(mine)
        entry.progress = kept
        entry.watched = watched
        entry.title = v.title
        entry.channel_name = v.channel_name
    } else {
        yt.history.insert(
            WatchEntry(
                key: v.key,
                title: v.title,
                channel_name: v.channel_name,
                member: .some(mine),
                watched: watched,
                progress: kept,
            ),
        )
    }
    if v.stream == "" { outside("youtube.stream", v.key) }
}

/// Takes one video out of the person's own history.
///
/// Plays videos at `speed`: a slower read for a dense talk, a quicker one for a long report.
action set_rate(yt: YouTube, speed: Float) (label: "Playback speed") {
    yt.rate = speed
}

/// Plays the video at another of its qualities, from the place it was at: the view plays the
/// address that quality has.
action play_at_quality(v: Video, chosen: Text) (label: "Quality") {
    v.quality = chosen
}

/// Shows the video's words in `language`, or turns them off when it is empty: the service fetches
/// the track's words, which the player draws as the video reaches them.
action show_captions(v: Video, language: Text) (label: "Captions") {
    v.caption_language = language
    if language != "" {
        outside("youtube.captions", [v.key, language])
    }
}

/// Takes one video out of the history.
destructive action forget(v: Video) (icon: .trash, label: "Delete from history") {
    let yt = root YouTube
    let mine = invoker
    yt.history.remove_where(h -> h.key == v.key and (h.member == .some(mine) or h.member == .none))
}

/// Deletes the person's own watch history, and the entries carried from before it recorded
/// members, which this history shows them; nobody else's.
destructive action delete_history(yt: YouTube) (icon: .trash, label: "Delete history") {
    let mine = invoker
    yt.history.remove_where(h -> h.member == .some(mine) or h.member == .none)
}

/// Asks the service for where the video's bytes play from again, after the player could not show
/// them: the address the client holds is the service's alone to write, and the answer it gives
/// now is what the player opens.
action retry_playback(v: Video) (icon: .refresh, label: "Try again") {
    outside("youtube.stream", v.key)
}

/// Asks the service for what the playlist holds, in its maker's order.
action refresh_playlist(p: Playlist) (icon: .refresh, label: "Refresh") {
    outside("youtube.playlist", p.key)
}

/// Plays the playlist from its first video.
action play_playlist(p: Playlist) uses time (icon: .play, label: "Play all") {
    let yt = root YouTube
    if let first = playlist_videos(yt, p).first() { progress(first, 0.0) }
}

/// Each of these keeps one of the person's own playback preferences: the record is theirs, made
/// the first time they set one, and every other member reads their own.
action set_autoplay(yt: YouTube, on: Bool) (icon: .play, label: "Autoplay next video") {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.autoplay = on
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.autoplay = on
    }
}

/// How fast videos play for the person: 0.25 to 3, 1 for the usual speed.
action set_speed(yt: YouTube, speed: Float) (icon: .gauge, label: "Playback speed") {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.speed = speed
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.speed = speed
    }
}

/// The tallest picture the person wants, as YouTube names it; `auto` takes the best that fits.
action set_quality(yt: YouTube, quality: Text) (icon: .settings, label: "Preferred quality") {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.quality = quality
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.quality = quality
    }
}

/// Whether videos show their captions for the person.
action set_captions(yt: YouTube, on: Bool) (icon: .message_square, label: "Captions") {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.captions = on
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.captions = on
    }
}

/// Searches YouTube: the service answers with channels, playlists and videos, written where the
/// search view lists them. The answer arrives through the platform's network, never the app's.
action search(yt: YouTube, query: Text = yt.search_text) (icon: .search) {
    require query != "" "Type something to search for"
    if yt.title == "" {
        yt.title = "YouTube"
    }
    yt.search_text = query
    yt.search_state = "searching"
    yt.search_trouble = ""
    outside("youtube.search", query)
}

/// Leaves the search the feed is showing for the place the person chose: the results stay where
/// the service wrote them (found again by the query that found them), and the client shows the
/// place chosen.
action end_search(yt: YouTube) (label: "Leave search") {
    applies yt.search_state != "" "No search is showing"
    yt.search_state = ""
}

/// Asks the service for the newest videos of each subscribed channel: what the subscriptions feed
/// shows.
action refresh_feed(yt: YouTube) (icon: .refresh, label: "Refresh") {
    if yt.title == "" {
        yt.title = "YouTube"
    }
    yt.feed_state = "loading"
    outside("youtube.subscriptions", yt.subscriptions.map(s -> s.key))
}

/// Asks the service for the channel's videos.
action refresh_channel(c: Channel) (icon: .refresh, label: "Refresh") {
    outside("youtube.channel", c.key)
}

/// Asks the service for the videos it lists as trending.
action load_trending(yt: YouTube) (icon: .flame, label: "Refresh trending") {
    if yt.title == "" {
        yt.title = "YouTube"
    }
    outside("youtube.trending", "")
}

/// What the native Tube kept before this app took its id over, carried into this one once: the
/// channels subscribed to, the videos liked, the watch queue, and the watch history with when
/// each video was watched. The native app kept a video by its key alone, so each one carried is
/// that key until the service answers with the rest. The history carried came before this app
/// recorded who watched what, so its entries name no member and stay the workspace's until
/// someone watches one of the videos again. Its comment likes and replies stay where
/// they are: this app shows no comments. The native app's own documents are left as they are.
migration action carry_native_tube(yt: YouTube) {
    for kept in earlier(yt).filter(e -> e.of == "") {
        for key in kept.lists.get("subscriptions") ?? [] {
            if not yt.subscriptions.any(s -> s.key == key) {
                yt.subscriptions.insert(Subscription(key: key, name: ""))
            }
        }
        for key in kept.lists.get("likes") ?? [] {
            if let v = yt.videos.find(x -> x.key == key) {
                v.liked = true
            } else {
                yt.videos.insert(Video(key: key, liked: true))
            }
        }
        for key in kept.lists.get("queue") ?? [] {
            if not yt.queue.any(q -> q.key == key) {
                // The queue reads a video's name from the video the service answers with; one it
                // has not answered about yet is carried as its key, and the queue still holds it.
                if not yt.videos.any(x -> x.key == key) { yt.videos.insert(Video(key: key)) }
                yt.queue.insert(QueuedVideo(key: key))
            }
        }
        // When each video was watched, in milliseconds since 1970 as the native app kept it.
        let watched = kept.entries.get("watched") ?? Map<Text>()
        for key in watched.keys() {
            let ms = (watched.get(key) ?? "").to_int() ?? 0
            if not yt.history.any(h -> h.key == key) {
                yt.history.insert(
                    WatchEntry(
                        key: key,
                        title: "",
                        channel_name: "",
                        watched: 1970-01-01T00:00Z + Duration(ms / 1000),
                    ),
                )
            }
        }
    }
}

/// The app's window: the person's subscriptions, trending, what they kept and what they watched,
/// with search over the service's catalogue.
view feed(yt: YouTube) {
    state place: Text = "subscriptions"
    let subs = subscribed_videos(yt)
    let trending = trending_videos(yt)
    let later = queue_videos(yt)
    let liked = yt.videos.filter(.liked).sort_by(.published, desc)
    let mine = my_history(yt, workspace.me)
    // A search shows its own place until the person chooses another: choosing a place closes the
    // search (`end_search`), so the feed they pick is the one that shows.
    let searched = yt.search_state != ""
    let here = if searched { "search" } else { place }
    ui.NavShell(content_kind: .grid, sidebar_title: "YouTube") {
        sidebar: {
            ui.NavList(selected: here) {
                ui.NavItem("search", "Search YouTube", icon: .search) {
                    on select: set(place, "search")
                }
                ui.NavItem("subscriptions", "Subscriptions", icon: .rss, count: subs.len()) {
                    on select: set(place, "subscriptions").then(end_search(yt))
                }
                ui.NavItem("trending", "Trending", icon: .flame, count: trending.len()) {
                    on select: set(place, "trending").then(end_search(yt))
                }
                ui.NavSection("You", collapsible: true) {
                    ui.NavItem("later", "Watch later", icon: .bookmark, count: later.len()) {
                        on select: set(place, "later").then(end_search(yt))
                    }
                    ui.NavItem("liked", "Liked videos", icon: .thumbs_up, count: liked.len()) {
                        on select: set(place, "liked").then(end_search(yt))
                    }
                    ui.NavItem("history", "History", icon: .history, count: mine.len()) {
                        on select: set(place, "history").then(end_search(yt))
                    }
                }
                if not yt.subscriptions.is_empty() {
                    ui.NavSection("Subscriptions", collapsible: true) {
                        for c in subscribed_channels(yt) {
                            ui.NavItem(
                                "channel/{c.key}",
                                c.name,
                                icon: .users,
                                count: yt.videos.count(v -> v.channel_key == c.key),
                            ) {
                                on select: set(place, "channel").then(end_search(yt)).then(open(c))
                            }
                        }
                    }
                }
            }
        }
        actions: {
            std.header
        }
        content: {
            scroll {
                page(full) {
                    if here == "search" {
                        search_results(yt)
                    } else if here == "history" {
                        history_list(yt, in_column: false)
                    } else if here == "later" {
                        if later.is_empty() {
                            ui.EmptyState(
                                "Nothing on the watch queue",
                                message: "Videos you keep for later wait here, oldest first",
                                icon: .bookmark,
                            )
                        } else {
                            // The queue is played from what was put on it first, and the heading
                            // says how many wait, so a video added later is never lost out of
                            // sight at the end of a long grid.
                            stack(gap: .group) {
                                if later.len() > 1 {
                                    text("{later.len()} videos, oldest first", role: .caption)
                                }
                                if let first = later.first() {
                                    row(gap: .related, align: .center) {
                                        ui.Button("Play all", icon: .play, role: .primary) {
                                            on press: play_queue(yt).then(open(first))
                                        }
                                    }
                                }
                                video_grid(later, "")
                            }
                        }
                    } else if here == "liked" {
                        video_grid(liked, "Nothing liked yet")
                    } else if here == "trending" {
                        if trending.is_empty() {
                            ui.EmptyState(
                                "Nothing trending yet",
                                icon: .flame,
                                action: "Refresh trending",
                            ) {
                                on act: load_trending(yt)
                            }
                        } else {
                            video_grid(trending, "")
                        }
                    } else if subs.is_empty() {
                        if yt.subscriptions.is_empty() {
                            ui.EmptyState("No subscriptions yet", icon: .rss)
                        } else {
                            ui.EmptyState("Nothing new yet", icon: .rss, action: "Refresh") {
                                on act: refresh_feed(yt)
                            }
                        }
                    } else {
                        // The videos under it are what was last fetched: the notice says why
                        // YouTube does not stand well, in YouTube's own words, and `Try again`
                        // reads the feed again. The column's own gap holds the notice over the
                        // grid, so the card keeps its distance above and below alike.
                        stack(gap: .group) {
                            std.service_banner(
                                "YouTube",
                                yt.online,
                                yt.trouble,
                                retry: refresh_feed(yt),
                            )
                            video_grid(subs, "")
                        }
                    }
                }
            }
        }
    }
}

/// The videos as cards in a grid, or what the place says while it holds none.
view video_grid(videos: List<Video>, empty: Text) {
    if videos.is_empty() {
        ui.EmptyState(empty)
    } else {
        ui.MediaGrid(size: .regular) {
            for v in videos {
                ui.MediaCard(
                    v,
                    title: v.title,
                    picture: v.thumbnail,
                    meta: [v.channel_name, "{views_text(v.views)} · {item_clock(v)}"],
                    aspect: .wide,
                )
            }
        }
    }
}

/// How long the video runs, as a card says it.
fn item_clock(v: Video) -> Text {
    if v.live { std.say("Live") } else { clock(v.duration) }
}

/// What the last search found: the channels, the playlists, then the videos. The field at the
/// top asks YouTube for what is typed; a query typed into the system's search reaches the same
/// place (`search_text` and `search`), so an agent or the palette finds the catalogue too.
///
/// The field starts at `search_text`, the query the results under it were found for: a search
/// run by the system's own search, or by an agent, shows the words it asked with, as every
/// search field shows what it searched for. Typing changes the field alone; Enter searches.
view search_results(yt: YouTube) {
    state typed: Text = yt.search_text
    stack(gap: .section) {
        ui.TextInput(bind: typed, placeholder: "Search YouTube", autofocus: true, fill) {
            on submit: search(yt, typed)
        }
        search_found(yt)
    }
}

/// What the search asked for, found: the state it stands in, then the channels, the playlists and
/// the videos.
view search_found(yt: YouTube) {
    if yt.search_state == "searching" {
        ui.ProgressBar(label: "Searching YouTube")
    } else if yt.search_trouble != "" {
        ui.Banner("The search failed", message: yt.search_trouble, kind: .warning)
    } else if yt.search_text == "" {
        ui.EmptyState(
            "Search YouTube",
            message: "Channels, playlists and videos, by name",
            icon: .search,
        )
    } else {
        let channels = found_channels(yt)
        let playlists = found_playlists(yt)
        let videos = found(yt)
        if channels.is_empty() and playlists.is_empty() and videos.is_empty() {
            ui.EmptyState(
                "Nothing found",
                message: "Nothing matches {yt.search_text}",
                icon: .search,
            )
        } else {
            stack(gap: .section) {
                if not channels.is_empty() {
                    ui.Section("Channels") {
                        for c in channels {
                            channel_row(c)
                        }
                    }
                }
                if not playlists.is_empty() {
                    ui.Section("Playlists") {
                        ui.MediaGrid(size: .small) {
                            for p in playlists {
                                ui.MediaCard(
                                    p,
                                    title: p.title,
                                    picture: p.thumbnail,
                                    meta: [p.channel_name, "{p.video_count} videos"],
                                    aspect: .wide,
                                )
                            }
                        }
                    }
                }
                if not videos.is_empty() {
                    ui.Section("Videos") {
                        video_grid(videos, "")
                    }
                }
            }
        }
    }
}

/// How much of a video was watched, as a percentage.
fn share_text(share: Float) -> Text {
    "{share * 100.0}%"
}

/// What the person watched, newest first, with how far they got. This is the history's place in
/// the feed, and a watched video's own page: the same list, whether the person opened History or
/// the video they watched.
view watched_page(h: WatchEntry) {
    let yt = root YouTube
    let entries = my_history(yt, workspace.me)
    ui.NavShell(content_kind: .list, title: "History", count: entries.len()) {
        actions: {
            std.header {
                button(delete_history(yt)) as clear
            }
        }
        content: {
            history_list(yt, in_column: true, fill)
        }
    }
}

/// The history in the feed's content: the same rows as its own place draws. The feed scrolls its
/// page, so the list stands as tall as its rows there; its own place gives it the column.
view history_list(yt: YouTube, in_column: Bool) {
    let entries = my_history(yt, workspace.me)
    if entries.is_empty() {
        ui.EmptyState("Nothing watched yet")
    } else {
        std.list(entries, in_column: in_column) { (entry, at) ->
            ui.ListItem(item: entry, presence: at) {
                watch_row(entry)
            }
        }
    }
}

/// Plays the video a history entry names, from where the person left off.
action play_again(yt: YouTube, h: WatchEntry) uses time {
    if let v = yt.videos.find(x -> x.key == h.key) { progress(v, h.progress) }
}

/// A channel's page: its banner, its avatar and name, whether the person follows it, and its
/// videos, newest first.
view channel_page(c: Channel) {
    let yt = root YouTube
    let videos = yt.videos.filter(v -> v.channel_key == c.key).sort_by(.published, desc)
    std.page(c, canvas: true) {
        header: {
            if subscribed(yt, c) {
                ui.Button("Unsubscribe", icon: .check) {
                    on press: toggle_subscription(c)
                }
            } else {
                ui.Button("Subscribe", icon: .bell, role: .primary) {
                    on press: toggle_subscription(c)
                }
            }
            ui.Button("Refresh", icon: .refresh, icon_only: true) {
                on press: refresh_channel(c)
            }
        }
        if c.banner != "" {
            ui.Image(c.banner, alt: "{c.name}'s banner", fit: .cover, aspect: 4.0)
        }
        row(gap: .section, align: .center) {
            if c.avatar != "" {
                ui.Image(c.avatar, alt: c.name, fit: .cover, aspect: 1.0, min: 4rem, max: 4rem)
            } else {
                ui.Avatar(name: c.name)
            }
            stack(gap: .tight) {
                row(gap: .tight, align: .center) {
                    text(c.name, role: .large_title)
                    if c.verified {
                        ui.Icon(.circle_check, tone: .info)
                    }
                }
                ui.Facts {
                    ui.Fact(c.handle)
                    ui.Fact(subscribers_text(c.subscribers))
                    ui.Fact("{videos.len()} videos")
                }
            }
        }
        if c.description != "" {
            ui.Section("About") {
                text(c.description, role: .reading)
            }
        }
        ui.Section("Videos") {
            video_grid(videos, "No videos here yet")
        }
    }
}

/// A video's page: it plays at the top, then its name, who made it, what people did with it, what
/// it says, and what YouTube plays next.
view video_page(v: Video) {
    state playing: Bool = false
    // Why the player could not show the video, in its own words; empty while it plays or waits.
    state failed: Text = ""
    let yt = root YouTube
    let channel = yt.channels.find(c -> c.key == v.channel_key)
    let left = watched_before(yt, workspace.me, v)?.progress ?? 0.0
    std.page(v, canvas: true) {
        header: {
            button(like(v)) as like
            button(watch_later(v)) as later
        }
        // The player is the picture: 16:9, as wide as the page's column gives it, and the page
        // scrolls under it. The page is not a filling canvas (`fill: true`), which lays its own
        // parts out from the page gutter and drops the side gutters the reading column has;
        // the ratio gives the box its height, so the canvas height `fill` was there for is not
        // needed. Until the service answered with where the bytes play from, the poster and
        // Play stand in the same box.
        layers(fill, aspect: 16.0 / 9.0) {
            if v.stream == "" {
                if v.thumbnail != "" {
                    ui.Image(v.thumbnail, alt: v.title, fit: .cover, fill)
                }
                // Play stands centred over the poster, not stretched across the box: the layer
                // that fills the cell is the picture, and a control keeps its own size.
                row(fill, align: .center, distribute: .center) {
                    ui.Button("Play", icon: .play, role: .primary) {
                        on press: progress(v, left)
                    }
                }
            } else {
                ui.VideoPlayer(
                    title: v.title,
                    poster: v.thumbnail,
                    source: if v.quality == "" {
                        v.stream
                    } else { v.streams.get(v.quality) ?? v.stream },
                    playing: playing,
                    position: left * v.duration.to_float(),
                    duration: v.duration.to_float(),
                    rate: yt.rate,
                    qualities: v.stream_qualities,
                    playing_quality: v.quality,
                    caption_tracks: v.caption_tracks,
                    caption_language: v.caption_language,
                    captions: if v.caption_language == "" { "" } else { v.captions },
                    fill,
                ) {
                    on set_playing(now_playing: Bool): set(playing, now_playing)
                    on seek(place: Float): progress(v, place)
                    // The player reports where it got to every five seconds and once when it
                    // stops, so a video watched without touching the playhead is remembered
                    // where it was left.
                    on progress(at: Float): progress(v, at)
                    on fail(reason: Text): set(failed, reason)
                    on set_rate(speed: Float): set_rate(yt, speed)
                    on select_quality(label: Text): play_at_quality(v, label)
                    on select_captions(language: Text): show_captions(v, language)
                }
            }
        }
        // The player could not show the video: what the engine said, and one press to ask the
        // service for the address again.
        if failed != "" {
            ui.Banner(
                "This video won't play here",
                message: failed,
                kind: .warning,
                action: "Try again",
            ) {
                on act: set(failed, "").then(retry_playback(v))
            }
        }
        // What the video is: its name, who made it and what people did with it, under the
        // player. The page scrolls as a whole (std.page's own scroll), so this is a plain
        // column: a `scroll` of its own would have no height to fill in a page that grows with
        // what it holds.
        stack(gap: .related) {
            text(v.title, role: .title)
            row(gap: .related, align: .center, wrap: true) {
                if let c = channel {
                    ui.Avatar(name: c.name)
                    ui.Button(c.name, icon: .users) {
                        on press: open(c)
                    }
                    ui.Button(
                        if subscribed(yt, c) { "Unsubscribe" } else { "Subscribe" },
                        icon: if subscribed(yt, c) { .check } else { .bell },
                    ) {
                        on press: toggle_subscription(c)
                    }
                } else {
                    ui.Avatar(name: v.channel_name)
                    text(v.channel_name, role: .subheading)
                }
                spacer()
                ui.Button(
                    if v.liked { "Liked" } else { "Like" },
                    icon: .thumbs_up,
                    toggled: v.liked,
                    count: v.likes,
                ) {
                    on press: like(v)
                }
                ui.Button(
                    "Watch later",
                    icon: .bookmark,
                    toggled: queued(yt, v.key),
                    icon_only: false,
                ) {
                    on press: watch_later(v)
                }
            }
        }
        stack(gap: .group) {
            ui.Facts {
                ui.Fact(views_text(v.views), icon: .eye)
                ui.Fact(v.published, icon: .calendar)
                if v.live {
                    ui.Fact("Live", icon: .rss)
                }
                ui.Fact(clock(v.duration), icon: .clock)
            }
            ui.Section("Description") {
                text(v.description, role: .reading)
            }
            if v.found_for != "" {
                ui.Chip("Found for {v.found_for}", icon: .search)
            }
        }
    }
}

/// A playlist's page: what it holds, in the order its maker put it in, and the way to play it
/// from the first.
view playlist_page(p: Playlist) {
    let yt = root YouTube
    let videos = playlist_videos(yt, p)
    std.page(p, canvas: true) {
        header: {
            ui.Button("Refresh", icon: .refresh, icon_only: true) {
                on press: refresh_playlist(p)
            }
        }
        row(gap: .section, align: .center) {
            if p.thumbnail != "" {
                ui.Image(p.thumbnail, alt: p.title, fit: .cover, aspect: 16.0 / 9.0, max: 20rem)
            }
            stack(gap: .tight) {
                text(p.title, role: .title)
                text(p.channel_name, role: .caption)
                text("{p.video_count} videos", role: .caption)
                if let first = videos.first() {
                    row(gap: .tight) {
                        ui.Button("Play all", icon: .play, role: .primary) {
                            on press: play_playlist(p).then(open(first))
                        }
                    }
                }
            }
        }
        if videos.is_empty() {
            ui.EmptyState(
                "Nothing shown from this playlist yet",
                message: "Its videos appear once YouTube says what it holds",
                icon: .list_video,
            )
        } else {
            video_grid(videos, "")
        }
    }
}

/// A video in a list: its name, who made it and how many people watched it.
view video_row(v: Video) {
    row(
        v.title,
        secondary: v.channel_name,
        meta: v.published,
        mark: if queued(root YouTube, v.key) { .bookmark } else if v.liked { .thumbs_up },
    )
}

/// A channel in a list: its name, its handle and how many people follow it.
view channel_row(c: Channel) {
    ui.ListRow(title: c.name, secondary: c.handle, value: subscribers_text(c.subscribers)) {
        avatar: {
            ui.Avatar(name: c.name)
        }
        on activate: open(c)
    }
}

/// A video in the history: its name, who made it and when the person watched it.
view watch_row(h: WatchEntry) {
    row(
        h.title,
        secondary: h.channel_name,
        value: if h.progress > 0.0 { "{share_text(h.progress)}" } else { "" },
        meta: h.watched,
    )
}

/// A subscriptions feed as a person's first afternoon on the client leaves it: five channels
/// followed, their newest videos, two videos kept for later, two liked, and three watched.
///
/// The channels, videos and playlists are the service's, which this stands in for: nothing here
/// is invented as if YouTube had answered it.
scenario "a subscriptions feed" {
    let yt = root YouTube
    yt.title = "YouTube"
    let mkbhd = yt.channels.insert(
        Channel(
            key: "UCmkbhd",
            name: "MKBHD",
            handle: "@mkbhd",
            subscribers: 19800000,
            verified: true,
            description: "Videos about technology, reviewed by someone who uses it every day.",
        ),
    )
    let veritasium = yt.channels.insert(
        Channel(
            key: "UCveritasium",
            name: "Veritasium",
            handle: "@veritasium",
            subscribers: 16400000,
            verified: true,
            description: "An element of truth: science and engineering explained.",
        ),
    )
    let cpu = yt.channels.insert(
        Channel(
            key: "UCcpu",
            name: "Computers Explained",
            handle: "@computers.explained",
            subscribers: 412000,
            description: "How the machines we use every day actually work.",
        ),
    )
    let kitchen = yt.channels.insert(
        Channel(
            key: "UCkitchen",
            name: "Small Kitchen",
            handle: "@smallkitchen",
            subscribers: 96000,
            description: "Cooking in a very small kitchen, one pan at a time.",
        ),
    )
    let brass = yt.channels.insert(
        Channel(
            key: "UCbrass",
            name: "The Brass Section",
            handle: "@brass.section",
            subscribers: 218000,
            description: "Brass bands, rehearsals and the people who play in them.",
        ),
    )
    yt.subscriptions.insert(Subscription(key: mkbhd.key, name: mkbhd.name))
    yt.subscriptions.insert(Subscription(key: veritasium.key, name: veritasium.name))
    yt.subscriptions.insert(Subscription(key: cpu.key, name: cpu.name))
    yt.subscriptions.insert(Subscription(key: kitchen.key, name: kitchen.name))
    yt.subscriptions.insert(Subscription(key: brass.key, name: brass.name))
    let phone = yt.videos.insert(
        Video(
            key: "v-phone",
            title: "The phone that finally lasts a week",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            views: 1840000,
            published: 2026-10-28T14:00Z,
            duration: 921,
            description: "A week of use, three days of battery, and what that changes about the way you carry a phone.",
            likes: 96000,
            live: false,
            trending: true,
        ),
    )
    let magnet = yt.videos.insert(
        Video(
            key: "v-magnet",
            title: "Why every magnet you own is lying to you",
            channel_key: veritasium.key,
            channel_name: veritasium.name,
            views: 4310000,
            published: 2026-10-27T16:30Z,
            duration: 1342,
            description: "The field lines people draw are not what is there. Here is what is.",
            likes: 310000,
            live: false,
            trending: true,
        ),
    )
    let cache = yt.videos.insert(
        Video(
            key: "v-cache",
            title: "A cache is a lie your computer tells you",
            channel_key: cpu.key,
            channel_name: cpu.name,
            views: 210000,
            published: 2026-10-27T09:00Z,
            duration: 745,
            description: "Why the fastest memory is the memory you forgot you had, and what happens when it is wrong.",
            likes: 14200,
            live: false,
        ),
    )
    let bread = yt.videos.insert(
        Video(
            key: "v-bread",
            title: "Bread in a one-metre kitchen",
            channel_key: kitchen.key,
            channel_name: kitchen.name,
            views: 48000,
            published: 2026-10-26T18:15Z,
            duration: 611,
            description: "Four ingredients, one bowl, and the smallest counter in the world.",
            likes: 3900,
            live: false,
        ),
    )
    let rehearsal = yt.videos.insert(
        Video(
            key: "v-rehearsal",
            title: "Twenty minutes before the contest",
            channel_key: brass.key,
            channel_name: brass.name,
            views: 91000,
            published: 2026-10-26T07:45Z,
            duration: 1180,
            description: "A band warms up in a car park. Then the crowd comes in.",
            likes: 8800,
            live: false,
        ),
    )
    let watch = yt.videos.insert(
        Video(
            key: "v-watch",
            title: "The watch that needs no winding",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            views: 2740000,
            published: 2026-10-24T15:00Z,
            duration: 688,
            description: "Six months with a watch that charges itself from the way you move.",
            likes: 148000,
            live: false,
        ),
    )
    let wires = yt.videos.insert(
        Video(
            key: "v-wires",
            title: "Why undersea cables do not break",
            channel_key: veritasium.key,
            channel_name: veritasium.name,
            views: 6120000,
            published: 2026-10-22T12:00Z,
            duration: 1594,
            description: "The internet runs on cables on the sea floor. People repair them by hand.",
            likes: 402000,
            live: false,
        ),
    )
    // Two videos on the watch queue, oldest first, two liked, and three the person watched.
    let mine = invoker
    let watched = now
    yt.queue.insert(QueuedVideo(key: rehearsal.key))
    yt.queue.insert(QueuedVideo(key: bread.key))
    magnet.liked = true
    wires.liked = true
    yt.history.insert(
        WatchEntry(
            key: phone.key,
            title: phone.title,
            channel_name: phone.channel_name,
            member: .some(mine),
            watched: watched,
            progress: 0.35,
        ),
    )
    yt.history.insert(
        WatchEntry(
            key: cache.key,
            title: cache.title,
            channel_name: cache.channel_name,
            member: .some(mine),
            watched: watched,
            progress: 0.0,
        ),
    )
    yt.history.insert(
        WatchEntry(
            key: wires.key,
            title: wires.title,
            channel_name: wires.channel_name,
            member: .some(mine),
            watched: watched,
            progress: 0.72,
        ),
    )
    // A playlist holds its own videos in its maker's order, which here is the older first.
    yt.playlists.insert(
        Playlist(
            key: "pl-science",
            title: "Science, explained",
            channel_name: veritasium.name,
            channel_key: veritasium.key,
            video_count: 2,
            video_keys: [wires.key, magnet.key],
        ),
    )
    yt
}

/// One channel's page on its own: a channel with videos, followed, as a place on a canvas shows
/// it.
scenario "a channel" -> Channel {
    let yt = root YouTube
    yt.title = "YouTube"
    let c = yt.channels.insert(
        Channel(
            key: "UCmkbhd",
            name: "MKBHD",
            handle: "@mkbhd",
            subscribers: 19800000,
            verified: true,
            description: "Videos about technology, reviewed by someone who uses it every day.",
        ),
    )
    yt.subscriptions.insert(Subscription(key: c.key, name: c.name))
    yt.videos.insert(
        Video(
            key: "v-phone",
            title: "The phone that finally lasts a week",
            channel_key: c.key,
            channel_name: c.name,
            views: 1840000,
            published: 2026-10-28T14:00Z,
            duration: 921,
            description: "A week of use, three days of battery, and what that changes about the way you carry a phone.",
            likes: 96000,
            live: false,
        ),
    )
    yt.videos.insert(
        Video(
            key: "v-watch",
            title: "The watch that needs no winding",
            channel_key: c.key,
            channel_name: c.name,
            views: 2740000,
            published: 2026-10-24T15:00Z,
            duration: 688,
            description: "Six months with a watch that charges itself from the way you move.",
            likes: 148000,
            live: false,
        ),
    )
    c
}

/// A video's page on its own: a video whose stream YouTube has answered with, with its channel,
/// what people did with it, and what it says.
scenario "a video playing" -> Video {
    let yt = root YouTube
    yt.title = "YouTube"
    let c = yt.channels.insert(
        Channel(
            key: "UCveritasium",
            name: "Veritasium",
            handle: "@veritasium",
            subscribers: 16400000,
            verified: true,
            description: "An element of truth: science and engineering explained.",
        ),
    )
    yt.subscriptions.insert(Subscription(key: c.key, name: c.name))
    let v = yt.videos.insert(
        Video(
            key: "v-magnet",
            title: "Why every magnet you own is lying to you",
            channel_key: c.key,
            channel_name: c.name,
            views: 4310000,
            published: 2026-10-27T16:30Z,
            duration: 1342,
            description: "The field lines people draw are not what is there. Here is what is, and why it took four hundred years to draw it right.",
            likes: 310000,
            live: false,
            stream: "https://media.example/v-magnet.mp4",
            trending: true,
        ),
    )
    yt.history.insert(
        WatchEntry(
            key: v.key,
            title: v.title,
            channel_name: v.channel_name,
            member: .some(invoker),
            watched: now,
            progress: 0.4,
        ),
    )
    v
}

/// What a search answers with: three channels, two playlists and videos, the way YouTube lists
/// them.
scenario "search results" -> YouTube {
    let yt = root YouTube
    yt.title = "YouTube"
    let mkbhd = yt.channels.insert(
        Channel(
            key: "UCmkbhd",
            name: "MKBHD",
            handle: "@mkbhd",
            subscribers: 19800000,
            verified: true,
            found_for: "phone reviews",
        ),
    )
    let studio = yt.channels.insert(
        Channel(
            key: "UCstudio",
            name: "Studio Phone Reviews",
            handle: "@studioreviews",
            subscribers: 148000,
            found_for: "phone reviews",
        ),
    )
    yt.channels.insert(
        Channel(
            key: "UCrepair",
            name: "Phone Repair Shop",
            handle: "@repairshop",
            subscribers: 31000,
            found_for: "phone reviews",
        ),
    )
    yt.videos.insert(
        Video(
            key: "v-phone",
            title: "The phone that finally lasts a week",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            views: 1840000,
            published: 2026-10-28T14:00Z,
            duration: 921,
            description: "A week of use, three days of battery.",
            likes: 96000,
            live: false,
            found_for: "phone reviews",
        ),
    )
    yt.videos.insert(
        Video(
            key: "v-budget",
            title: "The best phone under 300",
            channel_key: studio.key,
            channel_name: studio.name,
            views: 86000,
            published: 2026-10-27T11:00Z,
            duration: 1042,
            description: "Four phones, one week, and a decision.",
            likes: 4200,
            live: false,
            found_for: "phone reviews",
        ),
    )
    yt.playlists.insert(
        Playlist(
            key: "pl-phones",
            title: "Every phone reviewed this year",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            video_count: 41,
            found_for: "phone reviews",
        ),
    )
    yt.search_text = "phone reviews"
    yt.search_state = "done"
    yt
}

/// A client nobody has set up yet: what a first start shows.
scenario "no subscriptions yet" {
    let yt = root YouTube
    yt.title = "YouTube"
}

/// YouTube that cannot be reached: the feed under the notice is what was last fetched, and the
/// notice says why in YouTube's words.
scenario "YouTube cannot be reached" -> YouTube {
    let yt = root YouTube
    yt.title = "YouTube"
    let c = yt.channels.insert(
        Channel(
            key: "UCmkbhd",
            name: "MKBHD",
            handle: "@mkbhd",
            subscribers: 19800000,
            verified: true,
        ),
    )
    yt.subscriptions.insert(Subscription(key: c.key, name: c.name))
    yt.videos.insert(
        Video(
            key: "v-phone",
            title: "The phone that finally lasts a week",
            channel_key: c.key,
            channel_name: c.name,
            views: 1840000,
            published: 2026-10-28T14:00Z,
            duration: 921,
            likes: 96000,
        ),
    )
    yt.videos.insert(
        Video(
            key: "v-camera",
            title: "Three cameras, one question",
            channel_key: c.key,
            channel_name: c.name,
            views: 963000,
            published: 2026-10-25T14:00Z,
            duration: 1148,
            likes: 51000,
        ),
    )
    yt.online = false
    yt.trouble = "connection refused"
    yt
}

/// The watch history on its own: three videos watched, two of them left part way, as a place on
/// a canvas shows it.
scenario "a watch history" -> WatchEntry {
    let yt = root YouTube
    yt.title = "YouTube"
    let mkbhd = yt.channels.insert(
        Channel(
            key: "UCmkbhd",
            name: "MKBHD",
            handle: "@mkbhd",
            subscribers: 19800000,
            verified: true,
        ),
    )
    let phone = yt.videos.insert(
        Video(
            key: "v-phone",
            title: "The phone that finally lasts a week",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            views: 1840000,
            published: 2026-10-28T14:00Z,
            duration: 921,
            likes: 96000,
        ),
    )
    let watch = yt.videos.insert(
        Video(
            key: "v-watch",
            title: "The watch that needs no winding",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            views: 2740000,
            published: 2026-10-24T15:00Z,
            duration: 688,
            likes: 148000,
        ),
    )
    let laptop = yt.videos.insert(
        Video(
            key: "v-laptop",
            title: "A week with the quiet laptop",
            channel_key: mkbhd.key,
            channel_name: mkbhd.name,
            views: 910000,
            published: 2026-10-20T11:00Z,
            duration: 1104,
            likes: 41000,
        ),
    )
    let mine = invoker
    let watched = now
    yt.history.insert(
        WatchEntry(
            key: phone.key,
            title: phone.title,
            channel_name: phone.channel_name,
            member: .some(mine),
            watched: watched,
            progress: 0.35,
        ),
    )
    yt.history.insert(
        WatchEntry(
            key: watch.key,
            title: watch.title,
            channel_name: watch.channel_name,
            member: .some(mine),
            watched: watched,
            progress: 0.0,
        ),
    )
    let entry = yt.history.insert(
        WatchEntry(
            key: laptop.key,
            title: laptop.title,
            channel_name: laptop.channel_name,
            member: .some(mine),
            watched: watched,
            progress: 0.72,
        ),
    )
    entry
}

app so.tilo.tube "YouTube" (
    icon: .film,
    category: .multimedia,
    tagline: "Browses and plays YouTube videos",
    keywords: ["youtube", "video", "videos", "streaming", "watch", "channel", "channels", "subscriptions", "creators", "trending", "filme", "videos ansehen", "kanäle", "kanaele", "kanal", "schauen", "mediathek", "anschauen", "abos", "videoportal"],
    splash_budget: 250,
    schemes: "tube",
)

use so.tilo.playback as playback
use so.tilo.session as session

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
    /// How many people disliked it, as Return YouTube Dislike estimates: zero until it says.
    dislikes: Int = 0
    thumbnail: Text
    /// The community's title for it, when DeArrow names it better than its maker did.
    dearrow_title: Text = ""
    /// The picture the community frames it with, as this device's content address.
    dearrow_thumbnail: Text = ""
    /// The address its bytes play from, as the service answered: empty until it is asked.
    stream: Text
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
        hide channel_avatar
        hide dearrow_title
        hide dearrow_thumbnail
        hide dislikes
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

/// A comment someone left on a video, as the service says it is. The origin writes what YouTube
/// answered, keyed by the comment's own id, so asking again refreshes the same rows.
type Comment (origin: youtube.comment) {
    key: Text merge register
    /// The video it was left on, as the service names it.
    video_key: Text merge register
    author: Text
    avatar: Text
    text: Text
    likes: Int
    /// When it was written.
    published: Time
    /// The video's maker pinned it to the top.
    pinned: Bool
    /// The video's maker marked it with a heart.
    hearted: Bool
    /// How many replies it has on YouTube.
    replies: Int

    search {
        sort likes newest
        hide key
    }
}

/// One `SponsorBlock` segment of a video: where it starts and ends, so the player can skip it.
/// The origin writes what the community placed, keyed by where it stands.
type Segment (origin: youtube.segment) {
    key: Text merge register
    /// The video it belongs to, as the service names it.
    video_key: Text merge register
    /// Its kind, as SponsorBlock names it: `sponsor`, `selfpromo`, `interaction`, `intro`,
    /// `outro`, `preview`, `music_offtopic` or `filler`.
    category: Text
    /// Where it starts, in whole seconds.
    start: Int
    /// Where it ends, in whole seconds.
    end: Int
    /// The community votes that place it.
    votes: Int

    search {
        sort start oldest
        hide key
    }
}

/// A reply a member wrote to a comment: the workspace's own, kept beside the comment it answers,
/// since YouTube does not let an app post as the person. Each names who wrote it, so only they
/// take it back.
type Reply {
    /// The comment it answers, as the service names it.
    comment: Text merge register
    /// The video the comment is on.
    video: Text
    text: Text
    /// Who wrote it. Empty for a reply the native app kept, which stays the workspace's.
    member: Option<Actor> = .none
    said: Time
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
/// a view reads the record of the person looking at it. What a player sets for one window — the
/// speed it plays at and the captions it shows — is the window's own state, not this record.
type PlaybackPrefs {
    member: Actor
    /// Play the next video the session holds when this one ends.
    autoplay: Bool = false
    /// The tallest picture they want, as YouTube names it (`1080p`, `720p`, `480p`), or `auto`
    /// for the best that fits.
    quality: Text = "auto"
    /// Skip the segments SponsorBlock marks, as they play.
    sponsorblock: Bool = true
    /// Show the community's titles and pictures where DeArrow has them.
    dearrow: Bool = true
    /// Show how many people disliked a video, as Return YouTube Dislike estimates it.
    dislikes: Bool = true
    /// Show the comments under a video.
    comments: Bool = true
    /// How fast the player here plays a video, kept with the member so the next video starts at
    /// the same speed.
    speed: Float = 1.0
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
/// person's subscriptions, their watch history, and how the last search stands.
///
/// It keeps the workspace's watch queue, and each member's playback preferences.
type YouTube (label: "YouTube", noun: "channel") {
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
    /// Whether a person deleted their history: an earlier history is not imported over it, since
    /// the delete is theirs to keep.
    history_cleared: Bool = false
    /// The comments the service answered with, by the comment's own id.
    comments: Collection<Comment>
    /// The SponsorBlock segments the service answered with, by where each stands.
    segments: Collection<Segment>
    /// The replies members wrote to comments: the workspace's own, since YouTube lets no app post
    /// as the person.
    replies: Collection<Reply>
    /// Each member's own playback preferences.
    playback: Collection<PlaybackPrefs>
    /// What plays, and where it stands: the shared session, which the player, the shell's live
    /// card and every device read. Tube's watch page and the player's own controls act on it.
    playing: Option<playback.Playback>
    /// Every session this app made, kept as the resources they are; `playing` names the one.
    sessions: Collection<playback.Playback>
    /// What the last search asks for, as it was typed.
    search_text: Text = ""
    /// Which kinds the last search shows: `all`, `video`, `channel` or `playlist`.
    search_kind: Text = "all"
    /// How long the videos it finds run: `any`, `short` (under four minutes), `medium` (four to
    /// twenty) or `long` (over twenty).
    search_length: Text = "any"
    /// How recent they are: `any`, `hour`, `today`, `week`, `month` or `year`.
    search_published: Text = "any"
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

/// The tallest picture `who` wants a video played at, as YouTube names it: `auto` when they
/// chose nothing.
fn quality_of(yt: YouTube, who: Actor) -> Text {
    prefs_for(yt, who)?.quality ?? "auto"
}

/// The video's title as the person reads it: the community's (DeArrow), when they show it and it
/// has one, else its maker's.
fn shown_title(yt: YouTube, v: Video, who: Actor) -> Text {
    let pref = prefs_for(yt, who)
    if (pref?.dearrow ?? true) and v.dearrow_title != "" { v.dearrow_title } else { v.title }
}

/// The picture the video is drawn with: the community's when the person shows one and it has
/// one, else its maker's still.
fn shown_picture(yt: YouTube, v: Video, who: Actor) -> Text {
    let pref = prefs_for(yt, who)
    if (pref?.dearrow ?? true) and v.dearrow_thumbnail != "" {
        v.dearrow_thumbnail
    } else { v.thumbnail }
}

/// The video's comments, most liked first.
fn comments_of(yt: YouTube, key: Text) -> List<Comment> {
    yt.comments.filter(c -> c.video_key == key).sort_by(.likes, desc)
}

/// The replies members wrote to `comment`, oldest first.
fn replies_to(yt: YouTube, comment: Text) -> List<Reply> {
    yt.replies.filter(r -> r.comment == comment).sort_by(.said)
}

/// The SponsorBlock segment of the video that `at` (a share of its length) falls in, when the
/// person skips the segments the community marks: the one that plays there, if any.
fn segment_at(yt: YouTube, v: Video, at: Float, who: Actor) -> Option<Segment> {
    let pref = prefs_for(yt, who)
    if not (pref?.sponsorblock ?? true) {
        return .none
    }
    let seconds = at * v.duration.to_float()
    yt.segments.find(
        s -> s.video_key == v.key
            and s.category != "filler"
            and seconds >= s.start.to_float()
            and seconds < s.end.to_float(),
    )
}

/// Which kinds a search shows, as the person narrowed it: `all` shows every kind.
fn shows_kind(yt: YouTube, kind: Text) -> Bool {
    yt.search_kind == "all" or yt.search_kind == kind
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

/// Plays the queue from its oldest video: what was put on it first plays first, and the rest
/// follow in that order, so autoplay goes on down the queue.
action play_queue(yt: YouTube) uses time (icon: .play, label: "Play all") {
    let videos = queue_videos(yt)
    if let first = videos.first() {
        progress(first, 0.0)
        play_items(yt, videos, first.key)
    }
}

/// Plays the video, whether or not it was played before: the session takes it as its one item,
/// so the player, the live card and the media keys all act on it, and it continues where the
/// person left it.
primary action play(v: Video) uses time (icon: .play) {
    let yt = root YouTube
    progress(v, 0.0)
    play_items(yt, [v], v.key)
}

/// Plays the video from where the person left off, when they did not finish it.
action resume(v: Video) uses time (icon: .play, label: "Resume") {
    let yt = root YouTube
    progress(v, watched_before(yt, invoker, v)?.progress ?? 0.0)
    play_items(yt, [v], v.key)
}

/// The session this app plays through: the one it has, or one made now. It is kept both in
/// `playing`, which a view reads, and in `sessions`, so the record is a resource of its own.
action session(yt: YouTube) -> playback.Playback {
    if let held = yt.playing {
        return held
    }
    let made = yt.sessions.insert(playback.Playback())
    yt.playing = .some(made)
    made
}

/// What the session shows of `v`: the app's reading of it, so the live card and another device
/// name the video without reading this app's records.
fn item_of(yt: YouTube, v: Video, who: Actor) -> playback.Item {
    playback.Item(
        ref: v.key,
        title: shown_title(yt, v, who),
        artist: v.channel_name,
        cover: shown_picture(yt, v, who),
        source: v.stream,
        length: v.duration.to_float(),
    )
}

/// Plays `videos` in the session, beginning with `at`'s place: what plays and what follows, so
/// a shared next, the live card and another device move through the same list. The video at
/// `at` continues where it was left.
action play_items(yt: YouTube, videos: List<Video>, at: Text) {
    let p = session(yt)
    let mine = invoker
    let items = videos.map(v -> item_of(yt, v, mine))
    let index = if let chosen = videos.find(v -> v.key == at) {
        videos.index_of(chosen) ?? 0
    } else { 0 }
    playback.play_list(p, items, at: index)
    playback.resume(p, at)
}

/// Plays the video at `quality`, from where it stands: the player's own quality menu, which asks
/// the service for a stream of that picture. The session keeps the item and the place it stood
/// at, so the picture changes without the playhead moving; the address YouTube answers with is
/// the one the player opens next, and `progress` writes it into the session's item.
action play_at_quality(v: Video, quality: Text) (icon: .settings, label: "Playback quality") {
    let yt = root YouTube
    set_quality(yt, quality)
    if v.stream != "" {
        outside("youtube.stream", (v.key, quality))
    }
}

/// When the video reaches its end and the person asked for the next one, the session moves on to
/// what follows it: the queue's next video is watched now, so its place in the history is kept
/// and its bytes are asked for when the service has not answered with them yet.
action autoplay_next(yt: YouTube, p: playback.Playback, share: Float) uses time {
    if share < 0.99 or p.queue.len() < 2 {
        return
    }
    if not (prefs_for(yt, invoker)?.autoplay ?? false) {
        return
    }
    playback.next(p)
    if let v = yt.videos.find(x -> x.key == p.track) { progress(v, 0.0) }
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
    if v.stream == "" {
        outside("youtube.stream", (v.key, quality_of(yt, mine)))
    }
    // The address YouTube answered with is what plays: when the person picked another quality,
    // the service writes a new one and the session follows it, so the live card names the stream
    // the player opened.
    if let p = yt.playing {
        if p.track == v.key and p.source != v.stream { playback.set_source(p, v.stream) }
    }
}

/// Skips the SponsorBlock segment the video plays into, at `at` (a share of its length), when the
/// person skips segments: the playhead moves to the segment's end, so a sponsor read or an outro
/// is never watched. The player reports where it got to as it plays, so this is what makes a
/// marked segment skip on its own.
action skip_segment(v: Video, at: Float) uses time (
    icon: .skip_forward,
    label: "Skip the segment",
) {
    let yt = root YouTube
    if let segment = segment_at(yt, v, at, invoker) {
        if v.duration > 0 {
            progress(v, segment.end.to_float() / v.duration.to_float())
        }
    }
}

/// Takes one video out of the person's own history.
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
    // An earlier history is not imported over the delete: it is theirs to keep.
    yt.history_cleared = true
}

/// Asks the service for where the video's bytes play from again, after the player could not show
/// them: the address the client holds is the service's alone to write, and the answer it gives
/// now is what the player opens.
action retry_playback(v: Video) (icon: .refresh, label: "Try again") {
    outside("youtube.stream", (v.key, quality_of(root YouTube, invoker)))
}

/// Asks the service for what the playlist holds, in its maker's order.
action refresh_playlist(p: Playlist) (icon: .refresh, label: "Refresh") {
    outside("youtube.playlist", p.key)
}

/// Plays the playlist from its first video, and the rest in its maker's order.
action play_playlist(p: Playlist) uses time (icon: .play, label: "Play all") {
    let yt = root YouTube
    let videos = playlist_videos(yt, p)
    if let first = videos.first() {
        progress(first, 0.0)
        play_items(yt, videos, first.key)
    }
}

/// Opening a playlist asks the service what it holds, when the client does not know yet: a
/// playlist a search found is known by its name and cover alone, and its videos appear once
/// YouTube says them, so a page is never empty until someone presses Refresh.
opened action fetch_on_open(p: Playlist) {
    if p.video_keys.is_empty() and p.video_count > 0 { outside("youtube.playlist", p.key) }
}

/// Opening a video asks the service for what its page shows and the client does not hold yet:
/// where its bytes play from at the person's preferred quality, its comments, the segments
/// SponsorBlock marks, the community's title and picture (DeArrow) and how many people disliked
/// it. Each answer is written by the origin, so one that comes late fills the page it is shown on.
opened action read_on_open(v: Video) {
    let yt = root YouTube
    let mine = invoker
    if v.stream == "" {
        outside("youtube.stream", (v.key, quality_of(yt, mine)))
    }
    outside("youtube.comments", v.key)
    if not yt.segments.any(s -> s.video_key == v.key) { outside("youtube.segments", v.key) }
    if v.dearrow_title == "" { outside("youtube.branding", v.key) }
    if v.dislikes == 0 { outside("youtube.votes", v.key) }
}

/// Asks the service for the video's comments again, for what people said since the page opened.
action refresh_comments(v: Video) (icon: .refresh, label: "Refresh comments") {
    outside("youtube.comments", v.key)
}

/// Replies to a comment: the workspace keeps the reply itself, beside the comment it answers,
/// since YouTube lets no app post as the person. Everyone who reads the comment reads the reply.
action reply_to_comment(c: Comment, text: Text) -> Reply uses time (
    icon: .message_square,
    label: "Reply",
) {
    require text != "" "Type something to reply with"
    let yt = root YouTube
    yt.replies.insert(
        Reply(comment: c.key, video: c.video_key, text: text, member: .some(invoker), said: now),
    )
}

/// Takes back a reply the person wrote. A reply the native app kept is the workspace's, and any
/// member may take it back.
destructive action retract_reply(r: Reply) (icon: .trash, label: "Delete reply") {
    require r.member == .some(invoker) or r.member == .none "Only the person who wrote a reply takes it back"
    let yt = root YouTube
    yt.replies.remove_where(x -> x.id() == r.id())
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

/// Whether the player skips the segments SponsorBlock marks, as they play.
action set_sponsorblock(yt: YouTube, on: Bool) (
    icon: .skip_forward,
    label: "Skip sponsor segments",
) {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.sponsorblock = on
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.sponsorblock = on
    }
}

/// Whether a video shows the community's title and picture where DeArrow has them.
action set_dearrow(yt: YouTube, on: Bool) (
    icon: .pen_tool,
    label: "Community titles and pictures",
) {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.dearrow = on
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.dearrow = on
    }
}

/// Whether a video shows how many people disliked it, as Return YouTube Dislike estimates it.
action set_dislikes(yt: YouTube, on: Bool) (icon: .thumbs_down, label: "Dislike counts") {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.dislikes = on
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.dislikes = on
    }
}

/// Whether a video's page shows its comments.
action set_comments(yt: YouTube, on: Bool) (icon: .message_square, label: "Show comments") {
    let mine = invoker
    if let held = yt.playback.find(p -> p.member == mine) {
        held.comments = on
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.comments = on
    }
}

/// How fast the person watches: every video here starts at this speed, on every device.
action set_speed(yt: YouTube, speed: Float) (icon: .gauge, label: "Playback speed") {
    let mine = invoker
    let chosen = speed.clamp(0.25, 2.0)
    if let held = yt.playback.find(p -> p.member == mine) {
        held.speed = chosen
    } else {
        let made = yt.playback.insert(PlaybackPrefs(member: mine))
        made.speed = chosen
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
    // The filters the person set travel with the search: YouTube answers narrowed, so a search for
    // a kind, a length or a period comes back as that, never as everything the words find.
    outside("youtube.search", (query, yt.search_kind, yt.search_length, yt.search_published))
}

/// Narrows the search to the kinds the person chose: `all`, `video`, `channel` or `playlist`.
/// What the service found is kept, and the page shows the kind chosen alone, as the old chips
/// did; asking again is the search field's, which carries the kind with it.
action filter_search(yt: YouTube, kind: Text) (icon: .list_filter, label: "Filter the search") {
    require kind == "all" or kind == "video" or kind == "channel" or kind == "playlist" "{kind} is not a kind of result"
    yt.search_kind = kind
}

/// Narrows the search to how long its videos run: `any`, `short`, `medium` or `long`.
action filter_length(yt: YouTube, length: Text) (icon: .clock, label: "Filter by length") {
    require length == "any" or length == "short" or length == "medium" or length == "long" "{length} is not a length"
    yt.search_length = length
    if yt.search_text != "" { search(yt, yt.search_text) }
}

/// Narrows the search to how recent its results are: `any`, `hour`, `today`, `week`, `month` or
/// `year`.
action filter_published(yt: YouTube, published: Text) (icon: .calendar, label: "Filter by date") {
    require published == "any"
        or published == "hour"
        or published == "today"
        or published == "week"
        or published == "month"
        or published == "year" "{published} is not a period"
    yt.search_published = published
    if yt.search_text != "" { search(yt, yt.search_text) }
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

/// What a `tube:` link names after `prefix` (`tube:watch?v=`), or empty when it names something
/// else.
fn link_after(link: Text, prefix: Text) -> Text {
    if link.starts_with(prefix) { link.slice(prefix.len(), link.len()) } else { "" }
}

/// Opens a link of the scheme this client claims: the video (`tube:watch?v=<key>`), channel
/// (`tube:channel?key=<key>`) or playlist (`tube:playlist?key=<key>`) it names, or the search it
/// asks for (`tube:search?q=<query>`). A shell hands a link here whether or not a window of the
/// client is open, and what this returns opens in the window. The host publishes it as the
/// system's `open-link` on the client's root, which is what a shell hands a link to.
action open_link(yt: YouTube, link: Text) -> host.Resource {
    let video = link_after(link, "tube:watch?v=")
    if video != "" {
        if let v = yt.videos.find(x -> x.key == video) {
            return v
        }
    }
    let channel = link_after(link, "tube:channel?key=")
    if channel != "" {
        if let c = yt.channels.find(x -> x.key == channel) {
            return c
        }
    }
    let playlist = link_after(link, "tube:playlist?key=")
    if playlist != "" {
        if let p = yt.playlists.find(x -> x.key == playlist) {
            return p
        }
    }
    let query = link_after(link, "tube:search?q=")
    if query != "" { search(yt, query) }
    // A link that names nothing the client holds shows the client itself rather than a window
    // that took nothing.
    yt
}

/// The text a row's value holds, when it is a string: read off the JSON it is written as, since
/// a value an action is handed is not one of `json.Value`'s cases until it is parsed.
fn row_text(value: json.Value) -> Option<Text> {
    let written = json.encode(value)
    if written.starts_with("\"") {
        .some(written.slice(1, written.len() - 1))
    } else { .none }
}

/// The milliseconds a row's value names, when it is a number. The value is written as JSON
/// first, so a whole number reads the same whether it came in as one or as a float with a
/// trailing `.0`.
fn row_ms(value: json.Value) -> Option<Int> {
    json.encode(value).split(".").at(0)?.to_int()
}

/// One imported video as a history entry of `mine`: its name and channel when the client holds
/// them, when it was watched, and where it was left as a share of its length.
fn imported_entry(
    yt: YouTube,
    key: Text,
    ms: Int,
    position: Option<Int>,
    mine: Actor,
) -> WatchEntry {
    let held = yt.videos.find(v -> v.key == key)
    let duration = held?.duration ?? 0
    let progress = match position {
        .some(at) => if duration > 0 {
            at.to_float() / (duration.to_float() * 1000.0)
        } else { 0.0 },
        .none => 0.0,
    }
    WatchEntry(
        key: key,
        title: held?.title ?? "",
        channel_name: held?.channel_name ?? "",
        member: .some(mine),
        watched: 1970-01-01T00:00Z + Duration(ms / 1000),
        progress: progress,
    )
}

/// The entries of the workspace's history beyond its newest thousand, which the native app never
/// kept: an imported history leaves them out, so it never grows without end.
fn beyond_newest(yt: YouTube) -> List<WatchEntry> {
    let all = yt.history.sort_by(.watched, desc)
    let newest = all.take(1000)
    all.filter(old -> not newest.any(k -> k.id() == old.id()))
}

/// Imports a watch history another device kept, in the format the native app's own history and
/// its `import` call used: rows of `[a video's key, when it was watched in milliseconds since
/// 1970]`. A video the person already watched keeps its place; each one imported becomes part of
/// their own history, which keeps its newest thousand videos.
action import_history(yt: YouTube, watched: List<List<json.Value>>) (
    icon: .arrow_down,
    label: "Import watch history",
) {
    let mine = invoker
    for row in watched {
        let named = match row.at(0) {
            .some(value) => row_text(value),
            .none => .none,
        }
        if let key = named {
            let watched_at = match row.at(1) {
                .some(value) => row_ms(value) ?? 0,
                .none => 0,
            }
            if not yt.history.any(
                h -> h.key == key and (h.member == .some(mine) or h.member == .none),
            ) {
                yt.history.insert(imported_entry(yt, key, watched_at, .none, mine))
            }
        }
    }
    for old in beyond_newest(yt) {
        yt.history.remove_where(h -> h.id() == old.id())
    }
}

/// Imports the watch history an earlier version of this app kept for the person, in that
/// version's own format: rows of `[a video's key, when it was watched in milliseconds since 1970,
/// where it was left in milliseconds or nothing]`. Nothing is imported once the person deleted
/// their history, since the delete is theirs to keep; a video already in it keeps its place.
action import_previous(yt: YouTube, previous: List<List<json.Value>>) (
    icon: .arrow_down,
    label: "Import earlier history",
) {
    let mine = invoker
    if yt.history_cleared {
        return
    }
    for row in previous {
        let named = match row.at(0) {
            .some(value) => row_text(value),
            .none => .none,
        }
        if let key = named {
            if not yt.history.any(
                h -> h.key == key and (h.member == .some(mine) or h.member == .none),
            ) {
                let watched_at = match row.at(1) {
                    .some(value) => row_ms(value) ?? 0,
                    .none => 0,
                }
                let position = match row.at(2) {
                    .some(value) => row_ms(value),
                    .none => .none,
                }
                yt.history.insert(imported_entry(yt, key, watched_at, position, mine))
            }
        }
    }
    for old in beyond_newest(yt) {
        yt.history.remove_where(h -> h.id() == old.id())
    }
}

/// What the native Tube kept before this app took its id over, carried into this one once: the
/// channels subscribed to, the videos liked, the watch queue, the watch history with when each
/// video was watched, and the replies it wrote to comments. The native app kept a video by its key
/// alone, so each one carried is that key until the service answers with the rest. The history
/// carried came before this app recorded who watched what, so its entries name no member and stay
/// the workspace's until someone watches one of the videos again. Its replies are carried with no
/// member named, so any member may take one back. The native app's own documents are left as they
/// are.
migration action carry_native_tube(yt: YouTube) uses time {
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
        // A history the person deleted there stays deleted: an earlier history is not imported
        // over it here either.
        if (kept.texts.get("cleared") ?? "") == "true" {
            yt.history_cleared = true
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
    // The replies the native app kept, which it wrote to comments: the workspace's, with no
    // member named, so any member may take one back. What a reply said and the comment it answered
    // are kept; YouTube's comments themselves are read from the service.
    for kept in earlier(yt).filter(e -> e.of == "replies") {
        let comment = kept.texts.get("comment") ?? ""
        let text = kept.texts.get("text") ?? ""
        if comment != ""
            and text != ""
            and not yt.replies.any(r -> r.comment == comment and r.text == text) {
            yt.replies.insert(Reply(comment: comment, video: "", text: text, said: now))
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
    let mine = my_history(yt, invoker)
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
                    ui.NavItem("settings", "Settings", icon: .settings) {
                        on select: set(place, "settings").then(end_search(yt))
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
            std.header {
                // What was opened lately is the system's, as search is: the button opens the
                // shell's history narrowed to this app, and the app keeps no list of it.
                ui.Button("Recently opened", icon: .clock, icon_only: true, role: .plain) {
                    on press: session.recents(app: "so.tilo.tube")
                }
            }
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
                    } else if here == "settings" {
                        settings_page(yt)
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

/// The videos as cards in a grid, or what the place says while it holds none. A card shows the
/// community's title and picture where the person shows them and DeArrow has them.
view video_grid(videos: List<Video>, empty: Text) {
    let yt = root YouTube
    let mine = invoker
    if videos.is_empty() {
        ui.EmptyState(empty)
    } else {
        ui.MediaGrid(size: .regular) {
            for v in videos {
                ui.MediaCard(
                    v,
                    title: shown_title(yt, v, mine),
                    picture: shown_picture(yt, v, mine),
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
        // The filters the search narrows to: the kinds it shows, how long its videos run and how
        // recent they are. Each is an action, so an agent narrows the search as a person does.
        row(gap: .tight, wrap: true) {
            ui.Button("All", toggled: yt.search_kind == "all") {
                on press: filter_search(yt, "all")
            }
            ui.Button("Videos", icon: .film, toggled: yt.search_kind == "video") {
                on press: filter_search(yt, "video")
            }
            ui.Button("Channels", icon: .users, toggled: yt.search_kind == "channel") {
                on press: filter_search(yt, "channel")
            }
            ui.Button("Playlists", icon: .list_video, toggled: yt.search_kind == "playlist") {
                on press: filter_search(yt, "playlist")
            }
        }
        row(gap: .tight, wrap: true, align: .center) {
            text("Length", role: .caption)
            ui.Button("Any", toggled: yt.search_length == "any") {
                on press: filter_length(yt, "any")
            }
            ui.Button("Under 4 min", toggled: yt.search_length == "short") {
                on press: filter_length(yt, "short")
            }
            ui.Button("4 to 20 min", toggled: yt.search_length == "medium") {
                on press: filter_length(yt, "medium")
            }
            ui.Button("Over 20 min", toggled: yt.search_length == "long") {
                on press: filter_length(yt, "long")
            }
        }
        row(gap: .tight, wrap: true, align: .center) {
            text("Posted", role: .caption)
            ui.Button("Any time", toggled: yt.search_published == "any") {
                on press: filter_published(yt, "any")
            }
            ui.Button("Past hour", toggled: yt.search_published == "hour") {
                on press: filter_published(yt, "hour")
            }
            ui.Button("Today", toggled: yt.search_published == "today") {
                on press: filter_published(yt, "today")
            }
            ui.Button("This week", toggled: yt.search_published == "week") {
                on press: filter_published(yt, "week")
            }
            ui.Button("This month", toggled: yt.search_published == "month") {
                on press: filter_published(yt, "month")
            }
            ui.Button("This year", toggled: yt.search_published == "year") {
                on press: filter_published(yt, "year")
            }
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
        // What the kind filter shows: choosing videos shows the videos alone, and `all` shows
        // every kind the answer brought.
        let channels: List<Channel> = if shows_kind(yt, "channel") {
            found_channels(yt)
        } else { [] }
        let playlists: List<Playlist> = if shows_kind(yt, "playlist") {
            found_playlists(yt)
        } else { [] }
        let videos: List<Video> = if shows_kind(yt, "video") { found(yt) } else { [] }
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
    let entries = my_history(yt, invoker)
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
    let entries = my_history(yt, invoker)
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
    if let v = yt.videos.find(x -> x.key == h.key) {
        progress(v, h.progress)
        play_items(yt, [v], v.key)
        let p = session(yt)
        playback.remember_position(p, h.progress * p.length, now)
    }
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
    // Why the player could not show the video, in its own words; empty while it plays or waits.
    state failed: Text = ""
    // How fast the player here plays, and the captions it shows: the window's own choices, which
    // the player's gear menu sets and which outlast this page on this window. A rate of nought
    // means the member has not changed it here yet, so their own speed applies.
    state rate: Float = 0.0
    state caption_language: Text = ""
    // The comment the person is answering, when they are, and what they typed: the reply goes to
    // the workspace, so the composer stays beside the comment until it is sent.
    state reply_to: Option<Text> = .none
    state reply_text: Text = ""
    let yt = root YouTube
    let mine = invoker
    let p = yt.playing
    let held = if let playing = p { playing.track == v.key } else { false }
    let channel = yt.channels.find(c -> c.key == v.channel_key)
    let left = watched_before(yt, mine, v)?.progress ?? 0.0
    let speed = if rate > 0.0 { rate } else { prefs_for(yt, mine)?.speed ?? 1.0 }
    let shows_comments = prefs_for(yt, mine)?.comments ?? true
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
                let poster = shown_picture(yt, v, invoker)
                if poster != "" {
                    ui.Image(poster, alt: shown_title(yt, v, invoker), fit: .cover, fill)
                }
                // Play stands centred over the poster, not stretched across the box: the layer
                // that fills the cell is the picture, and a control keeps its own size.
                row(fill, align: .center, distribute: .center) {
                    ui.Button("Play", icon: .play, role: .primary) {
                        on press: resume(v)
                    }
                }
            } else if held {
                // The session is playing this video: the player is the session's own, so the
                // live card, the media keys and another device move the same playhead.
                if let playing = p {
                    ui.VideoPlayer(
                        title: shown_title(yt, v, mine),
                        poster: shown_picture(yt, v, mine),
                        source: v.stream,
                        playing: playing.state == .playing,
                        position: playing.at,
                        duration: v.duration.to_float(),
                        rate: speed,
                        caption_language: caption_language,
                        live: v.live,
                        fill,
                    ) {
                        on set_playing(now_playing: Bool): playback.set_playing(
                            playing,
                            now_playing,
                        )
                        // The playhead moved where a segment SponsorBlock marks is: the same skip
                        // as when playback reaches one on its own.
                        on seek(place: Float): playback
                            .seek(playing, place)
                            .then(skip_segment(v, place))
                        // The player reports where it got to every five seconds and once when it
                        // stops, so a video watched without touching the playhead is remembered
                        // where it was left. A report that lands in a segment SponsorBlock marks
                        // moves the playhead past it, so a sponsor read is not watched.
                        on progress(share: Float): playback
                            .set_share(playing, share)
                            .then(progress(v, share))
                            .then(skip_segment(v, share))
                            .then(autoplay_next(yt, playing, share))
                        on length(seconds: Float): playback.set_length(playing, seconds)
                        // The speed the person chose is kept with their preferences, so the
                        // next video and their other devices play at it too.
                        on set_rate(chosen: Float): set(rate, chosen).then(set_speed(yt, chosen))
                        on select_captions(language: Text): set(caption_language, language)
                        on select_quality(quality: Text): play_at_quality(v, quality)
                        on fail(reason: Text): set(failed, reason)
                    }
                }
            } else {
                let poster = shown_picture(yt, v, mine)
                if poster != "" {
                    ui.Image(poster, alt: shown_title(yt, v, mine), fit: .cover, fill)
                }
                row(fill, align: .center, distribute: .center) {
                    ui.Button("Play", icon: .play, role: .primary) {
                        on press: play(v)
                    }
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
            text(shown_title(yt, v, invoker), role: .title)
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
                if v.dislikes > 0 and (prefs_for(yt, invoker)?.dislikes ?? true) {
                    ui.Fact("{count_text(v.dislikes)}", icon: .thumbs_down)
                }
                ui.Fact(clock(v.duration), icon: .clock)
            }
            ui.Section("Description") {
                text(v.description, role: .reading)
            }
            if shows_comments {
                comments_section(yt, v, reply_to, reply_text)
            } else {
                text("Comments are turned off in Settings.", role: .caption, secondary: true)
            }
            if v.found_for != "" {
                ui.Chip("Found for {v.found_for}", icon: .search)
            }
        }
    }
}

/// A video's comments, most liked first, with the replies members wrote and the one the person is
/// answering: read them, reply to one, take back a reply of their own.
view comments_section(
    yt: YouTube,
    v: Video,
    reply_to: State<Option<Text>>,
    reply_text: State<Text>,
) {
    let comments = comments_of(yt, v.key)
    if comments.is_empty() {
        ui.EmptyState("No comments yet", icon: .message_square, action: "Refresh comments") {
            on act: refresh_comments(v)
        }
    } else {
        ui.Section("Comments") {
            stack(gap: .related) {
                for c in comments {
                    stack(gap: .tight) {
                        row(gap: .tight, align: .center) {
                            ui.Avatar(name: c.author)
                            text(c.author, role: .label)
                            if c.pinned {
                                ui.Chip("Pinned", icon: .pin)
                            }
                            spacer()
                            if c.likes > 0 {
                                text(count_text(c.likes), role: .caption)
                            }
                            ui.Button("Reply", icon: .reply, icon_only: true) {
                                on press: set(reply_to, c.key).then(set(reply_text, ""))
                            }
                        }
                        text(c.text, role: .reading)
                        // The workspace's replies under the comment: the person's own, and the
                        // ones carried from before the app recorded members.
                        for r in replies_to(yt, c.key) {
                            row(gap: .tight, align: .center) {
                                text(r.text, role: .caption)
                                spacer()
                                if r.member == .some(invoker) or r.member == .none {
                                    ui.Button("Delete", icon: .trash, icon_only: true) {
                                        on press: retract_reply(r)
                                    }
                                }
                            }
                        }
                        if let answering = reply_to {
                            if answering == c.key {
                                row(gap: .tight, align: .end) {
                                    ui.TextInput(bind: reply_text, placeholder: "Reply", fill)
                                    ui.Button("Send", icon: .send, role: .primary) {
                                        on press: reply_to_comment(c, reply_text)
                                            .then(
                                                set(reply_to, .none),
                                            )
                                    }
                                    ui.Button("Cancel") {
                                        on press: set(reply_to, .none)
                                    }
                                }
                            }
                        }
                    }
                }
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
            // Opening the page asks the service what the playlist holds; until it answers, the
            // page says it is coming rather than that it shows nothing.
            if p.video_count > 0 {
                ui.ProgressBar(label: "Loading the playlist")
            } else {
                ui.EmptyState(
                    "Nothing shown from this playlist yet",
                    message: "Its videos appear once YouTube says what it holds",
                    icon: .list_video,
                )
            }
        } else {
            video_grid(videos, "")
        }
    }
}

/// How this member likes to watch: what follows them to every device. Every switch and choice is
/// an action, so an agent sets one as a person does, and the same record is what the watch page
/// reads.
view settings_page(yt: YouTube) {
    let mine = invoker
    let prefs = prefs_for(yt, mine)
    let speed = prefs?.speed ?? 1.0
    let quality = quality_of(yt, mine)
    stack(gap: .section) {
        ui.Section("Playback") {
            ui.Toggle(shown: prefs?.autoplay ?? false, label: "Play the next video on its own") {
                on check(on): set_autoplay(yt, on)
            }
            ui.Toggle(shown: prefs?.comments ?? true, label: "Show comments") {
                on check(on): set_comments(yt, on)
            }
            ui.Toggle(shown: prefs?.dislikes ?? true, label: "Show dislike counts") {
                on check(on): set_dislikes(yt, on)
            }
            row(gap: .tight, align: .center, wrap: true) {
                text("Speed", role: .caption)
                for choice in [0.5, 0.75, 1.0, 1.25, 1.5, 2.0] {
                    ui.Button("{choice}×", toggled: speed == choice) {
                        on press: set_speed(yt, choice)
                    }
                }
            }
            row(gap: .tight, align: .center, wrap: true) {
                text("Preferred quality", role: .caption)
                for choice in ["auto", "1080p", "720p", "480p"] {
                    ui.Button(choice, toggled: quality == choice) {
                        on press: set_quality(yt, choice)
                    }
                }
            }
        }
        ui.Section("Community marks") {
            ui.Toggle(
                shown: prefs?.sponsorblock ?? true,
                label: "Skip segments SponsorBlock marks",
            ) {
                on check(on): set_sponsorblock(yt, on)
            }
            ui.Toggle(
                shown: prefs?.dearrow ?? true,
                label: "Community titles and pictures (DeArrow)",
            ) {
                on check(on): set_dearrow(yt, on)
            }
            text(
                "Dislike counts come from Return YouTube Dislike, as its users voted.",
                role: .caption,
                secondary: true,
            )
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
/// The channels, videos and playlists are YouTube's, which no action of this app writes: a seeded
/// workspace or a staged window takes them in from the sample YouTube through the app's origin,
/// as a real answer comes in, before this runs. What the person did with them is theirs, done
/// here through the app's own actions on what came in.
scenario "a subscriptions feed" {
    let yt = root YouTube
    for key in ["UCmkbhd", "UCveritasium", "UCcpu", "UCkitchen", "UCbrass"] {
        if let c = yt.channels.find(x -> x.key == key) { invoke(subscribe(c, true)) }
    }
    // Two videos on the watch queue, oldest first, two liked, and three the person watched.
    for key in ["v-rehearsal", "v-bread"] {
        if let v = yt.videos.find(x -> x.key == key) { invoke(watch_later(v, true)) }
    }
    for key in ["v-magnet", "v-wires"] {
        if let v = yt.videos.find(x -> x.key == key) { invoke(like(v, true)) }
    }
    if let v = yt.videos.find(x -> x.key == "v-phone") { invoke(progress(v, 0.35)) }
    if let v = yt.videos.find(x -> x.key == "v-cache") { invoke(progress(v, 0.0)) }
    if let v = yt.videos.find(x -> x.key == "v-wires") { invoke(progress(v, 0.72)) }
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

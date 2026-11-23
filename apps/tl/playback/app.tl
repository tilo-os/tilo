app so.tilo.playback "Playback" (icon: .play, category: .system)

/// One place a watching or listening session's sound and picture come out: this device, a
/// speaker in a room, or a cast target. A session holds as many as the person put it on, each
/// with its own volume and whether it plays in step with the rest. Every app that plays keeps
/// one Playback here, so the player, the live card and the system read the same session.
type Output {
    /// `local`, `speaker` or `cast`.
    kind: Text
    /// The device, speaker or target as the system names it; empty for this device.
    device: Text = ""
    /// What people read of it: "Kitchen", "Living room speaker".
    name: Text = ""
    volume: Float = 0.8
    /// Whether it plays in step with the session's place, or lags behind a cast target.
    synced: Bool = true
}

/// One item the session can play, with what a person reads of it. A queue holds these, so a
/// shared next fills the session's title, artist, cover, source and length without asking the
/// app that made it: Music builds one from its Track, Video from its Clip.
type Item {
    /// The app's reference to the item (Video's Clip, Music's Track).
    ref: Text
    title: Text
    artist: Text = ""
    cover: Text = ""
    /// Where its bytes play from: the kept content, else the source.
    source: Text = ""
    /// How long it runs, in seconds, when the app knows it.
    length: Float = 0.0
}

/// Where an item was left, so a later opening continues there. The session keeps one per item,
/// as Ebook keeps one per book, and each write is in the session's history under the member who
/// watched, named by the action's invocation.
type Progress {
    /// The app's reference to the item, as the session's `track` names it.
    ref: Text
    /// Where it stood, in seconds.
    position: Float = 0.0
    /// When the place was last written.
    updated: Time
}

/// Whether the player stands still, plays, or has nothing at all.
enum PlaybackState {
    stopped
    playing
    paused
}

/// What plays and where it stands: one session of the workspace, written only by actions, so
/// every device opens on the same thing at the same place and the system shows it as the
/// person's player. Its `live` view is what the shell draws. `track` is a reference the app
/// resolves (Video's Clip, Music's Track); the title, artist, cover, source and length are the
/// app's reading of it, kept here so the live card draws without reading another app's records.
type Playback (untitled: "Nothing playing") {
    /// The app's reference to what plays, empty when nothing plays.
    track: Text
    title: Text
    artist: Text
    cover: Text
    /// Where the sound or picture plays from: the kept bytes where they are, else the source.
    source: Text
    /// Whether it plays. Only this package's actions write it.
    state: PlaybackState = .stopped
    /// Every place it comes out, in the order it was put there: this device first, then the
    /// speakers and cast targets the person added.
    outputs: Collection<Output>
    /// Where it stands and how long it is, in seconds.
    at: Float = 0.0
    length: Float = 0.0
    volume: Float = 0.8
    shuffle: Bool = false
    /// `off`, `all` or `one`.
    repeat: Text = "off"
    /// The items that follow, in order, and which one plays.
    queue: Collection<Item>
    queue_at: Int = 0
    /// Where each item was left: what a later opening resumes from, one entry per item.
    progress: Collection<Progress>

    search {
        sort title a_z
    }
}

/// What the shell shows of what plays: a card a bar-pill tall, its controls the session's own
/// actions (play or pause, the item before and after, and the seek), so the media keys, the
/// lock screen and the bar press exactly what a person reaches in the window. The card says
/// which device plays it when a cast target or a speaker carries it.
// live when p.state == .playing
view live(p: Playback) {
    ui.MediaCard(
        p,
        title: if p.title == "" { "Nothing playing" } else { p.title },
        picture: p.cover,
        meta: [p.artist, first_output(p)],
    )
    row(gap: .tight, align: .center) {
        ui.Button("Previous", icon: .skip_back) {
            on press: previous(p)
        }
        ui.Button(
            if p.state == .playing { "Pause" } else { "Play" },
            icon: if p.state == .playing { .pause } else { .play },
            role: .primary,
        ) {
            on press: toggle(p)
        }
        ui.Button("Next", icon: .skip_forward) {
            on press: next(p)
        }
        ui.Button("Back 15 seconds", icon: .rotate_ccw) {
            on press: seek_by(p, seconds: -15)
        }
        ui.Button("Forward 15 seconds", icon: .rotate_cw) {
            on press: seek_by(p, seconds: 15)
        }
    }
}

/// What the first output is called: this device when it names none.
fn first_output(p: Playback) -> Text {
    if let output = p.outputs.at(0) {
        if output.name == "" { "This device" } else { output.name }
    } else { "" }
}

/// Plays the items `items` names, in the order given, from the one at `at`: the queue is what
/// follows, and the item at `at` plays. The app that calls this fills the title, artist, cover,
/// source and length it resolves from its own record; the shared actions below move the queue
/// without reading another app's records.
action play_list(p: Playback, items: List<Item>, at: Int = 0) {
    if items.is_empty() {
        return
    }
    let index = at.max(0).min(items.len() - 1)
    let held = p.queue.map(i -> i)
    for old in held {
        p.queue.remove(old)
    }
    for item in items {
        p.queue.insert(item)
    }
    p.queue_at = index
    if let item = p.queue.at(index) { set_current(p, item) }
    p.at = 0.0
    p.state = .playing
    if p.outputs.is_empty() { p.outputs.insert(Output(kind: "local")) }
}

/// The session shows `item`: its fields are the item's, so a shared next or previous never
/// leaves the title, cover or source of the one before it.
action set_current(p: Playback, item: Item) {
    p.track = item.ref
    p.title = item.title
    p.artist = item.artist
    p.cover = item.cover
    p.source = item.source
    p.length = item.length
}

/// Plays or pauses what plays, as a player's own control and a media key ask it.
action set_playing(p: Playback, on: Bool) {
    if p.track == "" {
        return
    }
    p.state = if on { .playing } else { .paused }
}

/// What the player reports of its length, once the decoder knows it.
action set_length(p: Playback, seconds: Float) {
    p.length = seconds.max(0.0)
    if let item = p.queue.find(i -> i.ref == p.track) {
        item.length = p.length
    }
}

/// Where `seconds` stands in what plays: within its length when the player knows it, and never
/// before the beginning.
fn place(p: Playback, seconds: Float) -> Float {
    let at = seconds.max(0.0)
    if p.length > 0.0 { at.min(p.length) } else { at }
}

/// Keeps the session's place and the item's as `when` records them. It has no effect of its own,
/// so an app's action that plays can call it inline: an action that needs a moment is a step of
/// its own, which a nested call only suggests.
action remember_position(p: Playback, seconds: Float, when: Time) {
    p.at = place(p, seconds)
    if p.track != "" {
        if let kept = p.progress.find(k -> k.ref == p.track) {
            kept.position = p.at
            kept.updated = when
        } else {
            p.progress.insert(Progress(ref: p.track, position: p.at, updated: when))
        }
    }
}

/// Where the player stands, as it reports it, so a later opening resumes there: the session's
/// place and the item's, kept so another device or a later opening continues from it.
action set_position(p: Playback, seconds: Float) uses time {
    remember_position(p, seconds, now)
}

/// Where the player stands, as the share of its length a player reports (`0..=1`), kept so a
/// later opening resumes there.
action set_share(p: Playback, share: Float) uses time {
    remember_position(p, share * p.length, now)
}

/// Where `ref` was left, in seconds; zero when nothing there was played yet.
fn position_of(p: Playback, ref: Text) -> Float {
    if let kept = p.progress.find(k -> k.ref == ref) { kept.position.max(0.0) } else { 0.0 }
}

/// Moves what plays to where `ref` was left, so a video or a song continues where the person
/// stopped instead of beginning again.
action resume(p: Playback, ref: Text) {
    if let kept = p.progress.find(k -> k.ref == ref) {
        p.at = place(p, kept.position)
    }
}

/// Plays what is chosen, or begins again where it stands.
action play(p: Playback) (icon: .play) {
    if p.track == "" {
        return
    }
    p.state = .playing
}

/// Plays or pauses what plays.
action toggle(p: Playback) (icon: .play) {
    if p.track == "" {
        return
    }
    p.state = if p.state == .playing { .paused } else { .playing }
}

/// Pauses what plays and starts it again where it stood ([`toggle`]'s own job).
action pause(p: Playback) (icon: .pause) {
    p.state = .paused
}

/// The item after this one: the queue's next, or the first when repeat is on.
action next(p: Playback) {
    if p.queue.is_empty() {
        return
    }
    let last = p.queue.len() - 1
    let at = if p.queue_at + 1 > last {
        if p.repeat == "all" { 0 } else { last }
    } else { p.queue_at + 1 }
    select(p, at)
    if p.repeat == "one" {
        p.at = 0.0
        p.state = .playing
    }
}

/// The item before this one.
action previous(p: Playback) {
    if p.queue.is_empty() {
        return
    }
    select(p, (p.queue_at - 1).max(0))
}

/// Plays the queued item at `at`, keeping the queue as it stands.
action select(p: Playback, at: Int) {
    if let item = p.queue.at(at) {
        p.queue_at = at
        set_current(p, item)
        p.at = 0.0
        p.state = .playing
    }
}

/// Moves what plays by `seconds`, for the live card's two seek controls.
action seek_by(p: Playback, seconds: Int = 0) uses time {
    remember_position(p, p.at + seconds.to_float(), now)
}

/// Moves what plays to `place`, a fraction of its length, and keeps the place so a later
/// opening continues there.
action seek(p: Playback, place: Float = 0.0) uses time {
    remember_position(p, place * p.length, now)
}

/// How loud the session is.
action set_volume(p: Playback, level: Float = 0.8) {
    p.volume = level.max(0.0).min(1.0)
}

/// Silences the session, or brings back the volume it had.
action mute(p: Playback) {
    p.volume = if p.volume > 0.0 { 0.0 } else { 0.8 }
}

/// Shuffles the queue, or puts its order back. The item that plays stays first.
action shuffle_toggle(p: Playback, shuffle: Bool = not p.shuffle) uses random {
    p.shuffle = shuffle
    if shuffle {
        let current = p.queue.find(i -> i.ref == p.track)
        let shuffled = random.shuffle(p.queue.map(i -> i))
        let held = p.queue.map(i -> i)
        for old in held {
            p.queue.remove(old)
        }
        if let playing = current {
            p.queue.insert(playing)
            for item in shuffled {
                if item.ref != playing.ref { p.queue.insert(item) }
            }
            p.queue_at = 0
        }
    }
}

/// Moves repeat on: off, all, one.
action cycle_repeat(p: Playback) {
    p.repeat = if p.repeat == "off" { "all" } else if p.repeat == "all" { "one" } else { "off" }
}

/// Adds `item` after what plays.
action queue_add(p: Playback, item: Item) (icon: .list_music) {
    p.queue.insert(item)
}

/// Adds `item` right after what plays.
action queue_play_next(p: Playback, item: Item) (icon: .skip_forward, label: "Play next") {
    let made = p.queue.insert(item)
    if let current = p.queue.at(p.queue_at) { p.queue.move(made, after: current) }
}

/// Takes the item at `index` out of the queue.
action queue_remove(p: Playback, index: Int, queue_at: Int = p.queue_at) {
    if let item = p.queue.at(index) {
        p.queue.remove(item)
        p.queue_at = if index < queue_at { (queue_at - 1).max(0) } else { queue_at }
    }
}

/// Moves the item called `from` to stand where `to` stands: the queue row a person dragged,
/// and the ids its reorder reports.
action queue_move(p: Playback, from: Text, to: Text) {
    let moved = p.queue.find(i -> i.ref == from)
    let target = p.queue.find(i -> i.ref == to)
    if let item = moved {
        if let before = target {
            p.queue.move(item, before: before)
            if let current = p.queue.find(i -> i.ref == p.track) {
                p.queue_at = p.queue.index_of(current) ?? 0
            }
        }
    }
}

/// Puts the sound on one more place: a speaker in a room, or a cast target.
action add_output(p: Playback, output: Output) (icon: .speaker) {
    p.outputs.insert(output)
}

/// Takes the place at `index` out of the session; this device is never taken out.
action remove_output(p: Playback, index: Int) (icon: .x) {
    if index <= 0 {
        return
    }
    if let output = p.outputs.at(index) { p.outputs.remove(output) }
}

/// Moves the place at `from` to where `to` is: "continue on the speaker" is moving the session
/// from this device to it.
action move_to(p: Playback, from: Int, to: Int) {
    if from <= 0 {
        return
    }
    let moved = p.outputs.at(from)
    let target = p.outputs.at(to)
    if let output = moved {
        if let before = target { p.outputs.move(output, before: before) }
    }
}

/// Sets how loud the place at `index` is.
action set_output_volume(p: Playback, index: Int, level: Float = 0.8) {
    if let output = p.outputs.at(index) {
        output.volume = level.max(0.0).min(1.0)
    }
}

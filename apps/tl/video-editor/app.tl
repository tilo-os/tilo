app so.tilo.video-editor "Video Editor" (
    icon: .film,
    category: .multimedia,
    tagline: "Cuts video and sound into a film",
    keywords: ["video editor", "video", "film", "timeline", "cut", "trim", "split", "transition", "title", "export", "videobearbeitung", "video schneiden", "film", "zeitleiste", "schnitt", "exportieren"],
    document: editor.projects,
)

use so.tilo.playback as playback

/// What a source is: a picture that moves, a sound, a still, or words this app draws itself.
enum Form {
    video
    audio
    image
    title
}

/// How a title's lines sit in its box.
enum Align {
    left
    centre
    right
}

/// One source in the bin: what it is, where its bytes are, and — for a title — the words it
/// draws.
///
/// A clip references its source; the bytes stay where the file was chosen from, so another
/// member resolves them on their own device.
type Media (title: name, untitled: "Untitled source") {
    name: Text
    form: Form = .video
    duration: Float = 0.0
    width: Int = 0
    height: Int = 0
    fps: Float = 0.0
    /// Its place in the palette, which tints its bin swatch and its clips.
    palette: Int = 0
    /// The address its bytes read at (`tilo-blob://…`); empty for a title.
    path: Text = ""
    has_audio: Bool = false
    /// Its picture as the bin shows it, kept once the engine drew it.
    still: Text = ""
    /// Its loudness across the whole file, a fraction of full scale, as the waveform draws it.
    peaks: List<Float>
    /// Whether this device cannot read the source's bytes: they were never brought here, or they
    /// live on another device of the workspace. Its clips draw nothing until it is relinked.
    missing: Bool = false
    /// A title's words.
    words: Text = ""
    /// A title's cap height, a fraction of the frame height.
    size: Float = 0.12
    red: Float = 1.0
    green: Float = 1.0
    blue: Float = 1.0
    alpha: Float = 1.0
    align: Align = .centre
    bold: Bool = true
    italic: Bool = false
    /// A title's backing plate, 0 for none.
    backdrop: Float = 0.0

    search {
        sort name a_z
    }
}

/// One stackable picture effect on a clip: what it is and how strong.
type Effect (title: form, untitled: "Effect") {
    form: Text
    amount: Float = 1.0
}

/// A row of the timeline. Video tracks stack above audio tracks.
type Track (title: name, untitled: "Track") {
    name: Text
    form: Form = .video
    index: Int = 1
    muted: Bool = false
    solo: Bool = false
    locked: Bool = false
    /// The fader, in decibels, 0 at unity.
    gain_db: Float = 0.0
    /// Where it sits across the stereo field: -1 hard left, 0 centre, 1 hard right.
    pan: Float = 0.0
}

/// One value a clip holds at a moment of its own, and how the curve leaves it.
type Key (title: param, untitled: "Key") {
    /// The field it drives, as the engine names it (`opacity`, `pos_x`, `effect:…`).
    param: Text
    /// Seconds from the clip's start, so a move or a split carries it.
    t: Float
    v: Float
    /// `linear`, `hold` or `ease`.
    interp: Text = "linear"
}

/// One placement of a source on a track: where it starts, which part of the source it shows,
/// and how it looks and sounds.
type Clip (title: name, untitled: "Clip") {
    name: Text
    track: Track
    media: Media
    /// Its left edge on the timeline, in seconds.
    start: Float = 0.0
    /// The source in-point, in seconds into the source.
    in_point: Float = 0.0
    /// The source out-point, in seconds into the source.
    out_point: Float = 0.0
    opacity: Float = 1.0
    volume: Float = 1.0
    /// The incoming transition: empty for a hard cut, else `dissolve`, `fade`, `dip` or `wipe`.
    transition: Text = ""
    /// How long that transition runs, in seconds; 0 for its default.
    transition_duration: Float = 0.0
    fade_in: Float = 0.0
    fade_out: Float = 0.0
    palette: Int = 0
    /// The playback rate; 1 is normal.
    speed_rate: Float = 1.0
    reverse: Bool = false
    brightness: Float = 0.5
    contrast: Float = 0.5
    saturation: Float = 0.5
    scale_x: Float = 1.0
    scale_y: Float = 1.0
    pos_x: Float = 0.0
    pos_y: Float = 0.0
    rotation: Float = 0.0
    /// The point of the picture that scaling and turning go around and that a position moves,
    /// as a fraction of it: 0.5, 0.5 is its middle, 0.0, 0.0 its top-left corner.
    anchor_x: Float = 0.5
    anchor_y: Float = 0.5
    crop_left: Float = 0.0
    crop_right: Float = 0.0
    crop_top: Float = 0.0
    crop_bottom: Float = 0.0
    effects: Collection<Effect>
    key_enabled: Bool = false
    key_tolerance: Float = 0.22
    key_softness: Float = 0.1
    key_spill: Float = 0.5
    /// Where a value of this clip changes over its own length: a curve on a field keeps the
    /// constant field's value where no key sits near.
    keys: Collection<Key>
    /// Frames of this clip as the inspector's filmstrip shows them, and the span they were drawn
    /// for, so a trim draws them again.
    strips: List<Text>
    strips_key: Text = ""
    /// The colour the key removes, in 0..1 RGB: what a sample from the frame sets.
    key_red: Float = 0.06
    key_green: Float = 0.72
    key_blue: Float = 0.2
    /// A mask that limits where the layer shows: empty for none, else `rect` or `ellipse`.
    mask_shape: Text = ""
    mask_x: Float = 0.0
    mask_y: Float = 0.0
    mask_w: Float = 0.6
    mask_h: Float = 0.6
    mask_rotation: Float = 0.0
    mask_feather: Float = 0.12
    mask_invert: Bool = false
    /// The link group this clip is one half of: its picture and its sound move together.
    link: Text = ""
    link_follows: Bool = true
    /// True when this picture's own sound was split off onto an audio clip.
    sound_split: Bool = false
}

/// A point on the timeline a person marked.
type Marker (title: name, untitled: "Marker") {
    name: Text
    /// Seconds on the timeline.
    t: Float = 0.0
    palette: Int = 3
    note: Text = ""
}

/// A cut: its shape, its sources, its tracks and clips, and what the engine last drew of it.
type Project (title: name, untitled: "Untitled project") {
    name: Text
    fps: Float = 30.0
    width: Int = 1920
    height: Int = 1080
    /// Whether the shape was chosen by a person rather than taken from the first source.
    explicit_format: Bool = false
    /// Where a render starts and ends on the timeline; negative for the head and the end.
    mark_in: Float = -1.0
    mark_out: Float = -1.0
    /// The container an export writes.
    format: Text = "mp4"
    media: Collection<Media>
    tracks: Collection<Track>
    clips: Collection<Clip>
    markers: Collection<Marker>
    /// The frame the preview shows at `still_at`, drawn by the engine.
    still: Text = ""
    still_at: Float = -1.0
    /// The live play running in the window, by its engine stream: the device composites the cut
    /// as it plays, and the window shows each frame by `tilo-live://{live}`.
    live: Text = ""
    /// How fast that play runs: 1 at normal speed, more to shuttle, negative backwards.
    live_rate: Float = 0.0
    /// The render this cut has running, and how far it has got.
    job: Text = ""
    job_done: Int = 0
    job_total: Int = 0
    /// The finished file an export made.
    exported: Text = ""
    /// Why the last thing the engine was asked failed.
    trouble: Text = ""
}

/// The workspace's cuts, and the one the window has open.
type Editor {
    projects: Collection<Project>
    open: Option<Project>
    /// The session the window plays through, so its sound has a device to meter.
    sessions: Collection<playback.Playback>
    playing: Option<playback.Playback>
}

root editor: Editor

default view window for Editor

default view studio for Project

row view project_row for Project

row view media_row for Media

/// What the device answered when it was asked what a file is.
type Probed {
    kind: Text = "video"
    duration: Float = 0.0
    width: Int = 0
    height: Int = 0
    fps: Float = 0.0
    has_audio: Bool = false
    codec: Text = ""
}

type ProbeAsk {
    url: Text
}

type StillAsk {
    /// The cut, as `json.encode(shot(p))` writes it.
    project: Text
    at: Float
    width: Int
    height: Int
}

type StillAnswer {
    url: Text
}

type WaveAsk {
    url: Text
    buckets: Int
}

type Peaks {
    peaks: List<Float>
}

type RenderAsk {
    /// The cut, as `json.encode(shot(p))` writes it.
    project: Text
    resolution: Text
    container: Text
    fps: Int
    name: Text
}

type JobAnswer {
    job: Text
}

/// A cut played live: the device composites it from the playhead and mixes the sound for the
/// same moment, and the window shows each frame as it arrives.
type LiveAsk {
    /// The cut, as `json.encode(shot(p))` writes it.
    project: Text
    /// Where the playhead starts, in timeline seconds.
    from: Float
    /// Timeline seconds a second; 1 is normal speed and a negative rate runs backwards.
    rate: Float
    width: Int
    height: Int
    /// Whether the device plays the cut's sound as well as its picture.
    sound: Bool
}

/// The cut as it now is, for a live play already running: an edit is seen as it is made.
type CutAsk {
    job: Text
    project: Text
}

/// A short piece of the cut's sound where the playhead was dragged.
type ScrubAsk {
    project: Text
    at: Float
    seconds: Float
}

/// The cut as the device reads it: plain values, with the names the engine knows.
type Shot {
    name: Text
    fps: Float
    width: Int
    height: Int
    mark_in: Float
    mark_out: Float
    /// One JSON text a source; each is a `ShotMedia` written out.
    media: List<Text>
    /// One JSON text a track; each is a `ShotTrack` written out.
    tracks: List<Text>
    /// One JSON text a clip; each is a `ShotClip` written out.
    clips: List<Text>
    /// One JSON text a marker; each is a `ShotMarker` written out.
    markers: List<Text>
}

type ShotMedia {
    id: Text
    name: Text
    kind: Text
    duration: Float
    width: Int
    height: Int
    fps: Float
    palette: Int
    path: Text
    has_audio: Bool
    /// A title's words; empty for every decoded source.
    content: Text
    size: Float
    color: List<Float>
    align: Text
    font: Text
    bold: Bool
    italic: Bool
    backdrop: Float
}

type ShotTrack {
    id: Text
    name: Text
    kind: Text
    index: Int
    muted: Bool
    solo: Bool
    locked: Bool
    gain_db: Float
    pan: Float
}

type ShotClip {
    id: Text
    track: Text
    media: Text
    start: Float
    in_point: Float
    out_point: Float
    opacity: Float
    volume: Float
    transition: Text
    transition_duration: Float
    fade_in: Float
    fade_out: Float
    palette: Int
    speed_rate: Float
    speed_reverse: Bool
    brightness: Float
    contrast: Float
    saturation: Float
    scale_x: Float
    scale_y: Float
    pos_x: Float
    pos_y: Float
    rotation: Float
    anchor_x: Float
    anchor_y: Float
    crop_left: Float
    crop_right: Float
    crop_top: Float
    crop_bottom: Float
    /// One JSON text an effect, in stack order.
    effects: List<Text>
    /// One JSON text a key, in clip-local seconds.
    keys: List<Text>
    key_enabled: Bool
    key_color: List<Float>
    key_tolerance: Float
    key_softness: Float
    key_spill: Float
    mask_shape: Text
    mask_x: Float
    mask_y: Float
    mask_w: Float
    mask_h: Float
    mask_rotation: Float
    mask_feather: Float
    mask_invert: Bool
    link: Text
    link_follows: Bool
    sound_split: Bool
}

type ShotEffect {
    id: Text
    form: Text
    amount: Float
}

type ShotKey {
    param: Text
    t: Float
    v: Float
    interp: Text
}

/// One source's picture at `at`, kept: a filmstrip frame.
type ThumbAsk {
    url: Text
    at: Float
    width: Int
    height: Int
}

/// The colour of the cut's frame at (x, y), in fractions of its box.
type SampleAsk {
    project: Text
    at: Float
    x: Float
    y: Float
}

type SampleAnswer {
    red: Float
    green: Float
    blue: Float
}

type ShotMarker {
    id: Text
    t: Float
    name: Text
    palette: Int
    note: Text
}

/// A project file as it is written: what a person hands to someone else.
type Saved {
    name: Text = ""
    fps: Float = 30.0
    width: Int = 1920
    height: Int = 1080
    mark_in: Float = -1.0
    mark_out: Float = -1.0
    /// One JSON text a source.
    media: List<Text>
    /// One JSON text a track.
    tracks: List<Text>
    /// One JSON text a clip.
    clips: List<Text>
    /// One JSON text a marker.
    markers: List<Text>
}

type SavedMedia {
    name: Text = ""
    form: Text = "video"
    duration: Float = 0.0
    width: Int = 0
    height: Int = 0
    fps: Float = 0.0
    palette: Int = 0
    path: Text = ""
    has_audio: Bool = false
    words: Text = ""
    size: Float = 0.12
    red: Float = 1.0
    green: Float = 1.0
    blue: Float = 1.0
    alpha: Float = 1.0
    align: Text = "centre"
    bold: Bool = true
    italic: Bool = false
    backdrop: Float = 0.0
}

type SavedTrack {
    name: Text = "Track"
    form: Text = "video"
    index: Int = 1
    muted: Bool = false
    solo: Bool = false
    locked: Bool = false
    gain_db: Float = 0.0
    pan: Float = 0.0
}

type SavedClip {
    name: Text = ""
    track: Int = 0
    media: Int = 0
    start: Float = 0.0
    in_point: Float = 0.0
    out_point: Float = 0.0
    opacity: Float = 1.0
    volume: Float = 1.0
    transition: Text = ""
    transition_duration: Float = 0.0
    fade_in: Float = 0.0
    fade_out: Float = 0.0
    speed_rate: Float = 1.0
    reverse: Bool = false
    brightness: Float = 0.5
    contrast: Float = 0.5
    saturation: Float = 0.5
    scale_x: Float = 1.0
    scale_y: Float = 1.0
    pos_x: Float = 0.0
    pos_y: Float = 0.0
    rotation: Float = 0.0
    anchor_x: Float = 0.5
    anchor_y: Float = 0.5
    crop_left: Float = 0.0
    crop_right: Float = 0.0
    crop_top: Float = 0.0
    crop_bottom: Float = 0.0
    key_enabled: Bool = false
    key_tolerance: Float = 0.22
    key_softness: Float = 0.1
    key_spill: Float = 0.5
    mask_shape: Text = ""
    mask_x: Float = 0.0
    mask_y: Float = 0.0
    mask_w: Float = 0.6
    mask_h: Float = 0.6
    mask_rotation: Float = 0.0
    mask_feather: Float = 0.12
    mask_invert: Bool = false
    link: Text = ""
    palette: Int = 0
    key_red: Float = 0.06
    key_green: Float = 0.72
    key_blue: Float = 0.2
    /// One JSON text a key of the clip.
    keys: List<Text>
    /// One JSON text an effect of the clip, in stack order.
    effects: List<Text>
}

/// Clips as a copy puts them on the person's clipboard: each with the source and track it came
/// from, so a paste into the same cut finds them again.
type Copied {
    kind: Text = ""
    clips: List<Text>
}

/// One copied clip: its look and timing, and where it came from.
type CopiedClip {
    clip: SavedClip
    media: Text = ""
    path: Text = ""
    track: Text = ""
    form: Text = ""
}

type SavedMarker {
    name: Text = ""
    t: Float = 0.0
    palette: Int = 3
    note: Text = ""
}

// ---------------------------------------------------------------- what a source and a track are
fn form_name(f: Form) -> Text {
    if f == .audio {
        "audio"
    } else if f == .image { "image" } else if f == .title { "text" } else { "video" }
}

fn form_of(name: Text) -> Form {
    if name == "audio" {
        .audio
    } else if name == "image" { .image } else if name == "text" { .title } else { .video }
}

fn form_words(f: Form) -> Text {
    if f == .audio {
        std.say("Sound")
    } else if f == .image {
        std.say("Still")
    } else if f == .title { std.say("Title") } else { std.say("Video") }
}

fn track_form(f: Form) -> Text {
    if f == .audio { "audio" } else { "video" }
}

fn align_name(a: Align) -> Text {
    if a == .left { "Left" } else if a == .right { "Right" } else { "Center" }
}

/// Where a fraction of a picture sits among the anchor's stops: 0, 1 or 2 across or down, and
/// -1 when it is none of them.
fn anchor_stop(at: Float) -> Int {
    if (at - 0.0).abs() < 0.01 {
        0
    } else if (at - 0.5).abs() < 0.01 { 1 } else if (at - 1.0).abs() < 0.01 { 2 } else { -1 }
}

/// Which stop of the anchor's grid row `row` is chosen, or 3 for none: the value the
/// inspector's three rows of segments take.
fn anchor_row(c: Clip, row: Int) -> Int {
    let column = anchor_stop(c.anchor_x)
    if anchor_stop(c.anchor_y) != row or column < 0 { 3 } else { column }
}

fn align_of(name: Text) -> Align {
    if name == "left" { .left } else if name == "right" { .right } else { .centre }
}

// ---------------------------------------------------------------- the cut as the engine reads it
fn shot(p: Project) -> Shot {
    Shot(
        name: p.name,
        fps: p.fps,
        width: p.width,
        height: p.height,
        mark_in: p.mark_in,
        mark_out: p.mark_out,
        media: p.media.map(m -> json.encode(shot_media(m))),
        tracks: p.tracks.map(t -> json.encode(shot_track(t))),
        clips: p.clips.map(c -> json.encode(shot_clip(c))),
        markers: p.markers.map(k -> json.encode(shot_marker(k))),
    )
}

fn shot_media(m: Media) -> ShotMedia {
    ShotMedia(
        id: m.id(),
        name: m.name,
        kind: form_name(m.form),
        duration: m.duration,
        width: m.width,
        height: m.height,
        fps: m.fps,
        palette: m.palette,
        path: m.path,
        has_audio: m.has_audio,
        content: if m.form == .title { m.words } else { "" },
        size: m.size,
        color: [m.red, m.green, m.blue, m.alpha],
        align: align_name(m.align),
        font: "",
        bold: m.bold,
        italic: m.italic,
        backdrop: m.backdrop,
    )
}

fn shot_track(t: Track) -> ShotTrack {
    ShotTrack(
        id: t.id(),
        name: t.name,
        kind: track_form(t.form),
        index: t.index,
        muted: t.muted,
        solo: t.solo,
        locked: t.locked,
        gain_db: t.gain_db,
        pan: t.pan,
    )
}

fn shot_clip(c: Clip) -> ShotClip {
    ShotClip(
        id: c.id(),
        track: c.track.id(),
        media: c.media.id(),
        start: c.start,
        in_point: c.in_point,
        out_point: c.out_point,
        opacity: c.opacity,
        volume: c.volume,
        transition: c.transition,
        transition_duration: c.transition_duration,
        fade_in: c.fade_in,
        fade_out: c.fade_out,
        palette: c.palette,
        speed_rate: c.speed_rate,
        speed_reverse: c.reverse,
        brightness: c.brightness,
        contrast: c.contrast,
        saturation: c.saturation,
        scale_x: c.scale_x,
        scale_y: c.scale_y,
        pos_x: c.pos_x,
        pos_y: c.pos_y,
        rotation: c.rotation,
        anchor_x: c.anchor_x,
        anchor_y: c.anchor_y,
        crop_left: c.crop_left,
        crop_right: c.crop_right,
        crop_top: c.crop_top,
        crop_bottom: c.crop_bottom,
        effects: c.effects.map(
            e -> json.encode(ShotEffect(id: e.id(), form: e.form, amount: e.amount)),
        ),
        keys: c.keys.map(
            k -> json.encode(ShotKey(param: k.param, t: k.t, v: k.v, interp: k.interp)),
        ),
        key_enabled: c.key_enabled,
        key_color: [c.key_red, c.key_green, c.key_blue],
        key_tolerance: c.key_tolerance,
        key_softness: c.key_softness,
        key_spill: c.key_spill,
        mask_shape: c.mask_shape,
        mask_x: c.mask_x,
        mask_y: c.mask_y,
        mask_w: c.mask_w,
        mask_h: c.mask_h,
        mask_rotation: c.mask_rotation,
        mask_feather: c.mask_feather,
        mask_invert: c.mask_invert,
        link: c.link,
        link_follows: c.link_follows,
        sound_split: c.sound_split,
    )
}

fn shot_marker(k: Marker) -> ShotMarker {
    ShotMarker(id: k.id(), t: k.t, name: k.name, palette: k.palette, note: k.note)
}

fn saved(p: Project) -> Saved {
    Saved(
        name: p.name,
        fps: p.fps,
        width: p.width,
        height: p.height,
        mark_in: p.mark_in,
        mark_out: p.mark_out,
        media: p.media.map(
            m -> json.encode(
                SavedMedia(
                    name: m.name,
                    form: form_name(m.form),
                    duration: m.duration,
                    width: m.width,
                    height: m.height,
                    fps: m.fps,
                    palette: m.palette,
                    path: m.path,
                    has_audio: m.has_audio,
                    words: m.words,
                    size: m.size,
                    red: m.red,
                    green: m.green,
                    blue: m.blue,
                    alpha: m.alpha,
                    align: if m.align == .left {
                        "left"
                    } else if m.align == .right { "right" } else { "centre" },
                    bold: m.bold,
                    italic: m.italic,
                    backdrop: m.backdrop,
                ),
            ),
        ),
        tracks: p.tracks.map(
            t -> json.encode(
                SavedTrack(
                    name: t.name,
                    form: track_form(t.form),
                    index: t.index,
                    muted: t.muted,
                    solo: t.solo,
                    locked: t.locked,
                    gain_db: t.gain_db,
                    pan: t.pan,
                ),
            ),
        ),
        clips: p.clips.map(c -> json.encode(saved_clip(p, c))),
        markers: p.markers.map(
            k -> json.encode(SavedMarker(name: k.name, t: k.t, palette: k.palette, note: k.note)),
        ),
    )
}

/// A clip as a saved project and a copy keep it.
fn saved_clip(p: Project, c: Clip) -> SavedClip {
    SavedClip(
        name: c.name,
        track: index_of_track(p, c.track),
        media: index_of_media(p, c.media),
        start: c.start,
        in_point: c.in_point,
        out_point: c.out_point,
        opacity: c.opacity,
        volume: c.volume,
        transition: c.transition,
        transition_duration: c.transition_duration,
        fade_in: c.fade_in,
        fade_out: c.fade_out,
        speed_rate: c.speed_rate,
        reverse: c.reverse,
        brightness: c.brightness,
        contrast: c.contrast,
        saturation: c.saturation,
        scale_x: c.scale_x,
        scale_y: c.scale_y,
        pos_x: c.pos_x,
        pos_y: c.pos_y,
        rotation: c.rotation,
        anchor_x: c.anchor_x,
        anchor_y: c.anchor_y,
        crop_left: c.crop_left,
        crop_right: c.crop_right,
        crop_top: c.crop_top,
        crop_bottom: c.crop_bottom,
        key_enabled: c.key_enabled,
        key_tolerance: c.key_tolerance,
        key_softness: c.key_softness,
        key_spill: c.key_spill,
        mask_shape: c.mask_shape,
        mask_x: c.mask_x,
        mask_y: c.mask_y,
        mask_w: c.mask_w,
        mask_h: c.mask_h,
        mask_rotation: c.mask_rotation,
        mask_feather: c.mask_feather,
        mask_invert: c.mask_invert,
        link: c.link,
        palette: c.palette,
        key_red: c.key_red,
        key_green: c.key_green,
        key_blue: c.key_blue,
        keys: c.keys.map(
            k -> json.encode(ShotKey(param: k.param, t: k.t, v: k.v, interp: k.interp)),
        ),
        effects: c.effects.map(
            e -> json.encode(ShotEffect(id: e.id(), form: e.form, amount: e.amount)),
        ),
    )
}

/// Makes the clip `c` keeps on `track`, of `media`, at `start`, with its keys and effects.
action restore_clip(
    p: Project,
    saved: Text,
    track: Track,
    media: Media,
    start: Float,
    link: Text,
) -> Option<Clip> {
    let c = match json.decode<SavedClip>(saved) {
        .ok(c) => c,
        .err(_) => {
            return .none
        }
    }
    let made = p.clips.insert(
        Clip(
            name: c.name,
            track: track,
            media: media,
            start: start,
            in_point: c.in_point,
            out_point: c.out_point,
            opacity: c.opacity,
            volume: c.volume,
            transition: c.transition,
            transition_duration: c.transition_duration,
            fade_in: c.fade_in,
            fade_out: c.fade_out,
            palette: c.palette,
            speed_rate: c.speed_rate,
            reverse: c.reverse,
            brightness: c.brightness,
            contrast: c.contrast,
            saturation: c.saturation,
            scale_x: c.scale_x,
            scale_y: c.scale_y,
            pos_x: c.pos_x,
            pos_y: c.pos_y,
            rotation: c.rotation,
            anchor_x: c.anchor_x,
            anchor_y: c.anchor_y,
            crop_left: c.crop_left,
            crop_right: c.crop_right,
            crop_top: c.crop_top,
            crop_bottom: c.crop_bottom,
            key_enabled: c.key_enabled,
            key_tolerance: c.key_tolerance,
            key_softness: c.key_softness,
            key_spill: c.key_spill,
            key_red: c.key_red,
            key_green: c.key_green,
            key_blue: c.key_blue,
            mask_shape: c.mask_shape,
            mask_x: c.mask_x,
            mask_y: c.mask_y,
            mask_w: c.mask_w,
            mask_h: c.mask_h,
            mask_rotation: c.mask_rotation,
            mask_feather: c.mask_feather,
            mask_invert: c.mask_invert,
            link: link,
        ),
    )
    for text in c.keys {
        match json.decode<ShotKey>(text) {
            .ok(key) => made.keys.insert(
                Key(param: key.param, t: key.t, v: key.v, interp: key.interp),
            ),
            .err(_) => {}
        }
    }
    for text in c.effects {
        match json.decode<ShotEffect>(text) {
            .ok(effect) => made.effects.insert(Effect(form: effect.form, amount: effect.amount)),
            .err(_) => {}
        }
    }
    .some(made)
}

fn index_of_media(p: Project, m: Media) -> Int {
    p.media.index_of(m) ?? 0
}

fn index_of_track(p: Project, t: Track) -> Int {
    p.tracks.index_of(t) ?? 0
}

/// The shortest clip a trim or a split may leave, in seconds.
// ---------------------------------------------------------------- the editing math
fn smallest() -> Float {
    0.1
}

/// How fast a clip plays; never slower than the engine accepts.
fn rate(c: Clip) -> Float {
    if c.speed_rate < 0.05 { 0.05 } else { c.speed_rate }
}

/// How much of the timeline a clip takes.
fn shown(c: Clip) -> Float {
    (c.out_point - c.in_point) / rate(c)
}

fn clip_end(c: Clip) -> Float {
    c.start + shown(c)
}

/// How long the cut runs.
fn total(p: Project) -> Float {
    let ends = p.clips.map(c -> clip_end(c)).sort_by(x -> x)
    ends.last() ?? 0.0
}

/// Where a render starts and ends: the marks when they are set, the whole cut otherwise.
fn render_from(p: Project) -> Float {
    if p.mark_in < 0.0 { 0.0 } else { p.mark_in }
}

fn render_to(p: Project) -> Float {
    if p.mark_out < 0.0 { total(p) } else { p.mark_out }
}

/// The tracks in the order the timeline draws them: video over sound, each by its index.
fn ordered_tracks(p: Project) -> List<Track> {
    [
        p.tracks.filter(t -> t.form == .video).sort_by(t -> t.index),
        p.tracks.filter(t -> t.form == .audio).sort_by(t -> t.index),
    ]
        .flat_map(x -> x)
}

/// The playhead `frames` frames from `at` on the cut's own rate, never before the start: one
/// step of the transport is one frame of the picture the cut is in.
fn stepped(p: Project, at: Float, frames: Int) -> Float {
    let fps = if p.fps > 0.0 { p.fps } else { 30.0 }
    let frame = (at.max(0.0) * fps).round() + frames
    (frame.to_float() / fps).max(0.0)
}

/// A moment on the cut, rounded to the frame it is nearest.
fn snap_time(p: Project, t: Float) -> Float {
    if p.fps <= 0.0 {
        return t.max(0.0)
    }
    (t * p.fps).round().max(0).to_float() / p.fps
}

fn overlaps(p: Project, track: Track, start: Float, end: Float, except: Text) -> Bool {
    p.clips.any(
        c -> c.track == track and c.id() != except and c.start < end and clip_end(c) > start,
    )
}

/// The first free place on `track` at or after `at` where `length` fits.
fn free_at(p: Project, track: Track, at: Float, length: Float) -> Float {
    let later = p.clips.filter(c -> c.track == track and clip_end(c) > at).sort_by(c -> c.start)
    if let c = later.first() {
        if c.start >= at + length {
            at
        } else {
            free_at(p, track, c.start + shown(c), length)
        }
    } else { at }
}

fn clip_named(p: Project, id: Text) -> Option<Clip> {
    p.clips.find(c -> c.id() == id)
}

/// Cuts a clip in two at `at`, each half keeping the source its own part shows.
action split_at(p: Project, c: Clip, at: Float) {
    let into = (at - c.start) * rate(c)
    let source = c.in_point + into
    if source - c.in_point < smallest() or c.out_point - source < smallest() {
        return
    }
    p.clips.insert(
        Clip(
            name: "{c.name} (2)",
            track: c.track,
            media: c.media,
            start: at,
            in_point: source,
            out_point: c.out_point,
            opacity: c.opacity,
            volume: c.volume,
            palette: c.palette,
            speed_rate: c.speed_rate,
            reverse: c.reverse,
            brightness: c.brightness,
            contrast: c.contrast,
            saturation: c.saturation,
            scale_x: c.scale_x,
            scale_y: c.scale_y,
            pos_x: c.pos_x,
            pos_y: c.pos_y,
            rotation: c.rotation,
            fade_in: c.fade_in,
            fade_out: c.fade_out,
            transition: c.transition,
            transition_duration: c.transition_duration,
            key_enabled: c.key_enabled,
            key_tolerance: c.key_tolerance,
            key_softness: c.key_softness,
            key_spill: c.key_spill,
            mask_shape: c.mask_shape,
            mask_x: c.mask_x,
            mask_y: c.mask_y,
            mask_w: c.mask_w,
            mask_h: c.mask_h,
            mask_rotation: c.mask_rotation,
            mask_feather: c.mask_feather,
            mask_invert: c.mask_invert,
            link: c.link,
            link_follows: c.link_follows,
            sound_split: c.sound_split,
        ),
    )
    c.out_point = source
    redraw(p)
}

/// The clips of one link group.
fn linked(p: Project, link: Text) -> List<Clip> {
    if link == "" { [] } else { p.clips.filter(c -> c.link == link) }
}

fn a_free_link(p: Project) -> Text {
    "{p.id()}-{p.clips.len()}"
}

/// Moves `c` (and its linked sound with it) to `start`, refusing an overlap.
action shifted(p: Project, c: Clip, start: Float) -> Bool {
    let to = snap_time(p, start.max(0.0))
    let delta = to - c.start
    let moving = if c.link == "" { [c] } else { linked(p, c.link) }
    for one in moving {
        if overlaps(p, one.track, one.start + delta, clip_end(one) + delta, one.id()) {
            trouble(p, "There is no room there")
            return false
        }
    }
    for one in moving {
        one.start = snap_time(p, (one.start + delta).max(0.0))
    }
    redraw(p)
    true
}

// ---------------------------------------------------------------- the engine's answers
action trouble(p: Project, why: Text) {
    p.trouble = why
}

/// Asks the device what a file is, adds it as a source and draws its swatch and waveform. What
/// the device said when it could not be added, or empty when it was: a batch names each file
/// that did not arrive and why, so a person knows what to do about it.
action absorb(p: Project, file: File) -> Text {
    let asked = engine.call("probe", json.encode(ProbeAsk(url: file.url)))
    match asked {
        .err(why) => why,
        .ok(text) => match json.decode<Probed>(text) {
            .err(why) => why,
            .ok(probed) => {
                let made = p.media.insert(
                    Media(
                        name: file.name,
                        form: form_of(probed.kind),
                        duration: if probed.kind == "image" { 5.0 } else { probed.duration },
                        width: probed.width,
                        height: probed.height,
                        fps: probed.fps,
                        path: file.url,
                        has_audio: probed.has_audio,
                        palette: p.media.len(),
                    ),
                )
                adopt_probe(p, made, text)
                draw_swatch(made, 200)
                if made.has_audio or made.form == .audio { hear(made) }
                ""
            }
        },
    }
}

/// Writes what the device read of a source onto it, as the engine said it: what it is, how long
/// it runs, its size and rate, and whether it carries sound. An action is handed the engine's
/// words rather than the decoded `Probed`, which is a value no resource holds.
action adopt_probe(p: Project, m: Media, said: Text) {
    match json.decode<Probed>(said) {
        .err(why) => mark_missing(p, m, why),
        .ok(probed) => {
            m.form = form_of(probed.kind)
            m.duration = if probed.kind == "image" { 5.0 } else { probed.duration }
            m.width = probed.width
            m.height = probed.height
            m.fps = probed.fps
            m.has_audio = probed.has_audio
            if not p.explicit_format and probed.width > 0 and probed.height > 0 {
                p.width = probed.width
                p.height = probed.height
                if probed.fps > 0.0 {
                    p.fps = probed.fps
                }
            }
        }
    }
    ""
}

/// Checks that a source's bytes are here: reads what they are again, draws its swatch and
/// waveform, and marks it not on this device when the engine cannot reach them.
action refresh_source(p: Project, m: Media) {
    match engine.call("probe", json.encode(ProbeAsk(url: m.path))) {
        .err(why) => mark_missing(p, m, why),
        .ok(text) => match json.decode<Probed>(text) {
            .err(why) => mark_missing(p, m, why),
            .ok(probed) => {
                m.missing = false
                adopt_probe(p, m, text)
                draw_swatch(m, 200)
                if m.has_audio or m.form == .audio { hear(m) }
                trouble(p, "")
                redraw(p)
            }
        },
    }
}

/// Points a source at a file the person chose: the clips are cuts of the source, so they keep
/// it, and the source reads what its new file says.
action relink(p: Project, m: Media, file: File) (icon: .link, label: "Relink the source") {
    match engine.call("probe", json.encode(ProbeAsk(url: file.url))) {
        .err(why) => mark_missing(p, m, why),
        .ok(text) => match json.decode<Probed>(text) {
            .err(why) => mark_missing(p, m, why),
            .ok(probed) => {
                m.path = file.url
                m.name = file.name
                m.missing = false
                m.still = ""
                m.peaks = []
                adopt_probe(p, m, text)
                draw_swatch(m, 200)
                if m.has_audio or m.form == .audio { hear(m) }
                trouble(p, "")
                redraw(p)
            }
        },
    }
}

/// Says the source's bytes are not on this device, and why the engine could not read them.
action mark_missing(p: Project, m: Media, why: Text) {
    m.missing = true
    trouble(p, why)
}

/// The source's picture as the bin shows it.
action draw_swatch(m: Media, width: Int) {
    if m.form == .title {
        return
    }
    let height = (width.to_float() * 9.0 / 16.0).round().max(16)
    match engine.call(
        "still",
        json.encode(
            StillAsk(project: json.encode(one_source(m)), at: 0.0, width: width, height: height),
        ),
    ) {
        .ok(text) => match json.decode<StillAnswer>(text) {
            .ok(answer) => {
                m.still = answer.url
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// A cut of one source alone, at zero: what a swatch is drawn from.
fn one_source(m: Media) -> Shot {
    Shot(
        name: m.name,
        fps: 30.0,
        width: 1280,
        height: 720,
        mark_in: -1.0,
        mark_out: -1.0,
        media: [json.encode(shot_media(m))],
        tracks: [
            json.encode(
                ShotTrack(
                    id: "swatch-track",
                    name: "Swatch",
                    kind: "video",
                    index: 1,
                    muted: false,
                    solo: false,
                    locked: false,
                    gain_db: 0.0,
                    pan: 0.0,
                ),
            ),
        ],
        clips: [
            json.encode(
                ShotClip(
                    id: "swatch-clip",
                    track: "swatch-track",
                    media: m.id(),
                    start: 0.0,
                    in_point: 0.0,
                    out_point: if m.duration > 0.0 { m.duration } else { 5.0 },
                    opacity: 1.0,
                    volume: 1.0,
                    transition: "",
                    transition_duration: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    palette: 0,
                    speed_rate: 1.0,
                    speed_reverse: false,
                    brightness: 0.5,
                    contrast: 0.5,
                    saturation: 0.5,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    pos_x: 0.0,
                    pos_y: 0.0,
                    rotation: 0.0,
                    anchor_x: 0.5,
                    anchor_y: 0.5,
                    crop_left: 0.0,
                    crop_right: 0.0,
                    crop_top: 0.0,
                    crop_bottom: 0.0,
                    effects: [],
                    keys: [],
                    key_enabled: false,
                    key_color: [0.0, 0.0, 0.0],
                    key_tolerance: 0.0,
                    key_softness: 0.0,
                    key_spill: 0.0,
                    mask_shape: "",
                    mask_x: 0.0,
                    mask_y: 0.0,
                    mask_w: 0.6,
                    mask_h: 0.6,
                    mask_rotation: 0.0,
                    mask_feather: 0.12,
                    mask_invert: false,
                    link: "",
                    link_follows: true,
                    sound_split: false,
                ),
            ),
        ],
        markers: [],
    )
}

/// The source's loudness across the file, as the waveform draws it.
action hear(m: Media) {
    match engine.call("waveform", json.encode(WaveAsk(url: m.path, buckets: 96))) {
        .ok(text) => match json.decode<Peaks>(text) {
            .ok(answer) => {
                m.peaks = answer.peaks
            }
            .err(_) => {}
        },
        .err(_) => {}
    }
}

/// The frame the preview shows at `at`, drawn by the engine.
action draw_still(p: Project, at: Float) {
    let width = 640
    let height = (width.to_float() * p.height.to_float() / p.width.to_float().max(1.0))
        .round()
        .max(16)
    match engine.call(
        "still",
        json.encode(
            StillAsk(project: json.encode(shot(p)), at: at.max(0.0), width: width, height: height),
        ),
    ) {
        .ok(text) => match json.decode<StillAnswer>(text) {
            .ok(answer) => {
                p.still = answer.url
                p.still_at = at
                p.trouble = ""
            }
            .err(why) => trouble(p, why),
        },
        .err(why) => trouble(p, why),
    }
}

fn export_name(p: Project, container: Text) -> Text {
    let suffix = if container == "webm" {
        "webm"
    } else if container == "mov" { "mov" } else { "mp4" }
    "{p.name}.{suffix}"
}

/// Starts the render a person saves.
action begin_render(p: Project, resolution: Text, container: Text, fps: Int) {
    if p.job != "" {
        return
    }
    match engine.start(
        "render",
        json.encode(
            RenderAsk(
                project: json.encode(shot(p)),
                resolution: resolution,
                container: container,
                fps: fps,
                name: export_name(p, container),
            ),
        ),
        done: j -> render_done(p, j),
        progress: j -> render_moved(p, j),
    ) {
        .ok(job) => {
            p.job = job
            p.job_done = 0
            p.job_total = 0
            p.format = container
            p.trouble = ""
        }
        .err(why) => trouble(p, why),
    }
}

/// Starts a cut: its shape and its first two tracks.
// ---------------------------------------------------------------- actions
primary action new_project(e: Editor) -> Project (icon: .plus) {
    let p = e.projects.insert(Project(name: std.say("Untitled project")))
    p.tracks.insert(Track(name: "V1", form: .video, index: 1))
    p.tracks.insert(Track(name: "A1", form: .audio, index: 1))
    e.open = .some(p)
    draw_still(p, 0.0)
    p
}

/// Opens the cut the window shows, and draws its first frame where the window has not shown
/// one yet: a cut opens on its picture, not on an empty preview.
action open_project(e: Editor, p: Project) {
    e.open = .some(p)
    if p.still == "" or p.still_at < 0.0 { draw_still(p, 0.0) }
}

/// The session this window plays through: the one it has, or one made now.
action session(e: Editor) -> playback.Playback {
    if let held = e.playing {
        return held
    }
    let made = e.sessions.insert(playback.Playback())
    e.playing = .some(made)
    made
}

action rename_project(p: Project, name: Text) {
    p.name = name
}

action forget_project(e: Editor, p: Project) (icon: .trash_2, label: "Delete project") {
    if e.open == .some(p) {
        e.open = .none
    }
    e.projects.remove(p)
}

/// Adds every file a person chose or dropped as a source, and says which files did not arrive
/// and what the device said of each.
action import_files(p: Project, files: List<File>) {
    let refused: List<Text> = []
    for file in files {
        let why = absorb(p, file)
        if why != "" {
            refused = [refused, ["{file.name}: {why}"]].flat_map(x -> x)
        }
    }
    if refused.is_empty() {
        trouble(p, "")
    } else {
        trouble(p, std.say("Not added: {refused.join("; ")}"))
    }
}

action import_one(p: Project, file: File) {
    let why = absorb(p, file)
    trouble(p, why)
}

/// Opens a video, a sound or a picture in a cut of its own: what Files and the person's own
/// system offer the editor for.
action open_media(e: Editor, file: File) -> Project (
    icon: .file_plus,
    label: "Open in the Video Editor",
    opens: [
    "video/mp4",
    "video/quicktime",
    "video/x-matroska",
    "video/webm",
    "audio/mpeg",
    "audio/wav",
    "image/png",
    "image/jpeg",
    .mp4,
    .m4v,
    .mov,
    .mkv,
    .webm,
    .mp3,
    .wav,
    .png,
    .jpg,
    .jpeg,
],
) {
    let p = e.projects.insert(Project(name: file.name))
    p.tracks.insert(Track(name: "V1", form: .video, index: 1))
    p.tracks.insert(Track(name: "A1", form: .audio, index: 1))
    let why = absorb(p, file)
    if why != "" {
        trouble(p, why)
    } else if let m = p.media.find(x -> x.path == file.url) {
        let track = if m.form == .audio {
            p.tracks.find(t -> t.form == .audio)
        } else { p.tracks.find(t -> t.form == .video) }
        if let track = track { place(p, m, track, 0.0) }
    }
    draw_still(p, 0.0)
    e.open = .some(p)
    p
}

/// Opens a project file a person chose: one undoable edit that makes the cut it holds.
action open_project_file(e: Editor, file: File) -> Project (
    icon: .file_plus,
    label: "Open a project",
    opens: [.tlvp],
) {
    let text = match file.text() {
        .ok(text) => text,
        .err(why) => {
            let p = e.projects.insert(Project(name: file.name))
            trouble(p, why)
            return p
        }
    }
    let cut = match json.decode<Saved>(text) {
        .ok(cut) => cut,
        .err(why) => {
            let p = e.projects.insert(Project(name: file.name))
            trouble(p, why)
            return p
        }
    }
    let p = e.projects.insert(
        Project(
            name: if cut.name == "" { file.name } else { cut.name },
            fps: cut.fps,
            width: cut.width,
            height: cut.height,
            mark_in: cut.mark_in,
            mark_out: cut.mark_out,
        ),
    )
    for text in cut.media {
        let m: Option<SavedMedia> = match json.decode<SavedMedia>(text) {
            .ok(m) => .some(m),
            .err(_) => .none,
        }
        if let m = m {
            p.media.insert(
                Media(
                    name: m.name,
                    form: form_of(m.form),
                    duration: m.duration,
                    width: m.width,
                    height: m.height,
                    fps: m.fps,
                    palette: m.palette,
                    path: m.path,
                    has_audio: m.has_audio,
                    words: m.words,
                    size: m.size,
                    red: m.red,
                    green: m.green,
                    blue: m.blue,
                    alpha: m.alpha,
                    align: align_of(m.align),
                    bold: m.bold,
                    italic: m.italic,
                    backdrop: m.backdrop,
                ),
            )
        }
    }
    for text in cut.tracks {
        let t: Option<SavedTrack> = match json.decode<SavedTrack>(text) {
            .ok(t) => .some(t),
            .err(_) => .none,
        }
        if let t = t {
            p.tracks.insert(
                Track(
                    name: t.name,
                    form: form_of(t.form),
                    index: t.index,
                    muted: t.muted,
                    solo: t.solo,
                    locked: t.locked,
                    gain_db: t.gain_db,
                    pan: t.pan,
                ),
            )
        }
    }
    for text in cut.clips {
        let c: Option<SavedClip> = match json.decode<SavedClip>(text) {
            .ok(c) => .some(c),
            .err(_) => .none,
        }
        if let c = c {
            let track = p.tracks.at(c.track)
            let media = p.media.at(c.media)
            if let track = track {
                if let media = media {
                    restore_clip(p, text, track, media, c.start, c.link)
                }
            }
        }
    }
    for text in cut.markers {
        match json.decode<SavedMarker>(text) {
            .ok(k) => p.markers.insert(
                Marker(name: k.name, t: k.t, palette: k.palette, note: k.note),
            ),
            .err(_) => {}
        }
    }
    draw_still(p, 0.0)
    e.open = .some(p)
    p
}

/// The cut as a file a person keeps: its sources and their places, where they stand on their own
/// device.
action save_project(p: Project) -> File (icon: .download, label: "Save the project") {
    File.of_text("{p.name}.tlvp", json.encode(saved(p)))
}

/// The cut as a film: what an export made.
action export_file(p: Project) -> File (icon: .download, label: "Save the film") {
    File.at(export_name(p, p.format), p.exported)
}

/// Places `m` on `track` at `at`, in the first place it fits.
action place(p: Project, m: Media, track: Track, at: Float) -> Clip {
    let length = if m.duration > 0.0 { m.duration } else { 5.0 }
    let start = free_at(p, track, at.max(0.0), length)
    let made = p.clips.insert(
        Clip(
            name: m.name,
            track: track,
            media: m,
            start: snap_time(p, start),
            in_point: 0.0,
            out_point: m.duration,
            palette: m.palette,
        ),
    )
    redraw(p)
    made
}

/// Places the bin's source on the first track of its form, at `at`.
action place_media(p: Project, m: Media, at: Float) -> Option<Clip> {
    let track = if m.form == .audio {
        p.tracks.find(t -> t.form == .audio)
    } else { p.tracks.find(t -> t.form == .video) }
    if let held = track {
        if held.locked {
            trouble(p, "The track is locked")
            return .none
        }
        return .some(place(p, m, held, at))
    }
    let made = add_track(p, if m.form == .audio { .audio } else { .video })
    return .some(place(p, m, made, at))
}

/// What a drag of the timeline committed: a clip moved or trimmed. A clip moved among `with`, the
/// clips chosen, moves them all.
action commit_drag(p: Project, grab: Text, clip: Text, from: Float, at: Float, with: List<Clip>) {
    let c = match clip_named(p, clip) {
        .some(c) => c,
        .none => {
            return
        }
    }
    if c.track.locked {
        trouble(p, "The track is locked")
        return
    }
    if grab == "move" {
        if with.len() > 1 and with.contains(c) {
            moved(p, with, at - from)
        } else { shifted(p, c, c.start + at - from) }
    } else if grab == "head" {
        let delta = at - from
        let source = (c.in_point + delta * rate(c)).clamp(0.0, c.out_point - smallest() * rate(c))
        let gained = (source - c.in_point) / rate(c)
        if not overlaps(p, c.track, c.start + gained, clip_end(c), c.id()) {
            c.in_point = source
            c.start = snap_time(p, c.start + gained)
        }
    } else if grab == "tail" {
        let delta = at - from
        let source = (c.out_point + delta * rate(c))
            .clamp(c.in_point + smallest() * rate(c), c.media.duration)
        let gained = (source - c.out_point) / rate(c)
        if not overlaps(p, c.track, c.start, clip_end(c) + gained, c.id()) {
            c.out_point = source
        }
    }
    redraw(p)
}

/// Moves a clip to `start`, as a drag on the timeline lands it, and every other clip among `with`,
/// the clips chosen, by as much. A clip moved alone goes onto the track `track` names when that is
/// another track of its kind, unlocked, with room for it there.
action move_to(p: Project, c: Clip, start: Float, track: Text, with: List<Clip>) {
    let alone = not (with.len() > 1 and with.contains(c))
    if let t = p.tracks.find(x -> x.id() == track) {
        if alone
            and t != c.track
            and t.form == c.track.form
            and not t.locked
            and not overlaps(p, t, start, start + shown(c), c.id()) {
            c.track = t
        }
    }
    commit_drag(p, "move", c.id(), c.start, start, with)
}

/// Trims a clip to start at `start` and last `length`, as a drag on its edge on the timeline
/// lands it: a moved start trims its head, else its tail.
action trim_to(p: Project, c: Clip, start: Float, length: Float) {
    if (start - c.start).abs() > 1e-6 {
        commit_drag(p, "head", c.id(), c.start, start, [])
    } else {
        commit_drag(p, "tail", c.id(), clip_end(c), start + length, [])
    }
}

/// Moves a marker to `at`, on the frame nearest it.
action move_marker(p: Project, k: Marker, at: Float) {
    k.t = snap_time(p, at)
}

/// The clips `cs` and every clip linked to one of them: what moves or goes with them.
fn with_links(p: Project, cs: List<Clip>) -> List<Clip> {
    p.clips.filter(c -> cs.contains(c) or c.link != "" and cs.any(x -> x.link == c.link))
}

/// Moves the clips `cs`, and the sound linked to them, by `by` seconds together, refusing an
/// overlap with a clip that stays.
action moved(p: Project, cs: List<Clip>, by: Float) -> Bool {
    let moving = with_links(p, cs)
    if moving.any(c -> c.track.locked) {
        trouble(p, "The track is locked")
        return false
    }
    let delta = by.max(0.0 - (moving.map(c -> c.start).sort_by(t -> t).first() ?? 0.0))
    for one in moving {
        let start = one.start + delta
        let end = clip_end(one) + delta
        if p.clips.any(
            c -> c.track == one.track
                and not moving.contains(c)
                and c.start < end
                and clip_end(c) > start,
        ) {
            trouble(p, "There is no room there")
            return false
        }
    }
    for one in moving {
        one.start = snap_time(p, (one.start + delta).max(0.0))
    }
    redraw(p)
    true
}

/// Cuts the clip at the playhead in two.
action split_clip(p: Project, c: Clip, at: Float) {
    if c.track.locked {
        trouble(p, "The track is locked")
        return
    }
    split_at(p, c, snap_time(p, at))
}

/// Cuts every clip the playhead stands on.
action cut_at(p: Project, at: Float) {
    for c in p.clips.filter(x -> x.start < at and clip_end(x) > at) {
        if not c.track.locked { split_at(p, c, snap_time(p, at)) }
    }
}

/// Deletes the clips, and the sound linked to them with them.
destructive action remove_clips(p: Project, cs: List<Clip>) (
    icon: .trash_2,
    label: "Delete the clips",
) {
    let going = with_links(p, cs)
    if going.any(c -> c.track.locked) {
        trouble(p, "The track is locked")
        return
    }
    for c in going {
        p.clips.remove(c)
    }
    redraw(p)
}

/// Deletes the clips and closes the gap each leaves behind it on its track.
destructive action ripple_delete(p: Project, cs: List<Clip>) (
    icon: .scissors,
    label: "Close the gap",
) {
    if cs.any(c -> c.track.locked) {
        trouble(p, "The track is locked")
        return
    }
    for c in cs.sort_by(x -> 0.0 - x.start) {
        let gap = shown(c)
        let after = p.clips
            .filter(x -> x.track == c.track and x.start >= clip_end(c))
            .sort_by(x -> x.start)
        p.clips.remove(c)
        for x in after {
            x.start = (x.start - gap).max(0.0)
        }
    }
    redraw(p)
}

/// Slips the clips along the timeline by frames.
action nudge(p: Project, cs: List<Clip>, frames: Int) {
    moved(p, cs, frames.to_float() / p.fps)
}

/// Trims the start of each clip the playhead stands in to the playhead.
action trim_start(p: Project, cs: List<Clip>, at: Float) (
    icon: .align_left,
    label: "Trim the start to the playhead",
) {
    for c in cs.filter(x -> x.start < at and clip_end(x) > at and not x.track.locked) {
        let gained = snap_time(p, at) - c.start
        c.in_point = (c.in_point + gained * rate(c)).min(c.out_point - smallest() * rate(c))
        c.start = snap_time(p, at)
    }
    redraw(p)
}

/// Trims the end of each clip the playhead stands in to the playhead.
action trim_end(p: Project, cs: List<Clip>, at: Float) (
    icon: .align_right,
    label: "Trim the end to the playhead",
) {
    for c in cs.filter(x -> x.start < at and clip_end(x) > at and not x.track.locked) {
        let lost = clip_end(c) - snap_time(p, at)
        c.out_point = (c.out_point - lost * rate(c)).max(c.in_point + smallest() * rate(c))
    }
    redraw(p)
}

/// What a copy of the clips puts on the clipboard.
fn copied(p: Project, cs: List<Clip>) -> Copied {
    Copied(
        kind: copied_kind(),
        clips: cs
            .sort_by(c -> c.start)
            .map(
                c -> json.encode(
                    CopiedClip(
                        clip: saved_clip(p, c),
                        media: c.media.id(),
                        path: c.media.path,
                        track: c.track.id(),
                        form: form_name(c.track.form),
                    ),
                ),
            ),
    )
}

/// What marks the clipboard's text as clips of this app.
fn copied_kind() -> Text {
    "so.tilo.video-editor/clips"
}

/// Puts the clips, with their looks, keys and effects, on the person's clipboard.
action copy_clips(p: Project, cs: List<Clip>) uses clipboard (icon: .copy, label: "Copy") {
    if cs.is_empty() {
        return
    }
    clipboard.copy(json.encode(copied(p, cs)))
}

/// Puts the clips on the person's clipboard and takes them out of the cut.
destructive action cut_clips(p: Project, cs: List<Clip>) uses clipboard (
    icon: .scissors,
    label: "Cut",
) {
    if cs.is_empty() {
        return
    }
    if cs.any(c -> c.track.locked) {
        trouble(p, "The track is locked")
        return
    }
    clipboard.copy(json.encode(copied(p, cs)))
    remove_clips(p, cs)
}

/// The track a pasted clip goes on: the one it came from while it is in this cut and open, else
/// the first open one of its form, else a new one.
action paste_track(p: Project, track: Text, form_named: Text) -> Track {
    if let t = p.tracks.find(t -> t.id() == track and not t.locked) {
        return t
    }
    let form = form_of(form_named)
    if let t = p.tracks.find(t -> t.form == form and not t.locked) {
        return t
    }
    add_track(p, form)
}

/// Places copied clips from `at`, the earliest there and the rest keeping their distance from
/// it; clips linked to each other stay linked, to each other alone.
action paste_clips(p: Project, at: Float) uses clipboard (icon: .clipboard_paste, label: "Paste") {
    let text = clipboard.paste() ?? ""
    let copy = match json.decode<Copied>(text) {
        .ok(copy) => copy,
        .err(_) => {
            trouble(p, "The clipboard holds no clips")
            return
        }
    }
    if copy.kind != copied_kind() {
        trouble(p, "The clipboard holds no clips")
        return
    }
    let came: List<CopiedClip> = copy.clips.flat_map(
        t -> match json.decode<CopiedClip>(t) {
            .ok(c) => [c],
            .err(_) => [],
        },
    )
    let base = came.map(c -> c.clip.start).sort_by(t -> t).first() ?? 0.0
    let fresh = "{p.clips.len()}"
    for one in came {
        let media = p.media.find(m -> m.id() == one.media)
            ?? p.media.find(m -> one.path != "" and m.path == one.path)
        if let media = media {
            let track = paste_track(p, one.track, one.form)
            let length = shown_saved(one.clip)
            let start = free_at(
                p,
                track,
                snap_time(p, (at + one.clip.start - base).max(0.0)),
                length,
            )
            let link = if one.clip.link == "" { "" } else { "{one.clip.link}~{fresh}" }
            restore_clip(p, json.encode(one.clip), track, media, start, link)
        } else {
            trouble(p, "A copied clip's source is not in this cut")
        }
    }
    redraw(p)
}

/// How long a kept clip runs on the timeline.
fn shown_saved(c: SavedClip) -> Float {
    (c.out_point - c.in_point).max(0.0) / c.speed_rate.max(0.01)
}

/// Puts a copy of the clips right after them, on the same tracks.
action duplicate_clips(p: Project, cs: List<Clip>) (icon: .copy, label: "Duplicate") {
    if cs.is_empty() {
        return
    }
    let lo = cs.map(c -> c.start).sort_by(t -> t).first() ?? 0.0
    let hi = cs.map(c -> clip_end(c)).sort_by(t -> 0.0 - t).first() ?? 0.0
    let offset = (hi - lo).max(smallest())
    let fresh = "{p.clips.len()}"
    for c in cs.sort_by(x -> x.start) {
        let link = if c.link == "" { "" } else { "{c.link}~{fresh}" }
        let start = free_at(p, c.track, snap_time(p, c.start + offset), shown(c))
        restore_clip(p, json.encode(saved_clip(p, c)), c.track, c.media, start, link)
    }
    redraw(p)
}

/// Where the playhead goes from `at` to the next marked instant: a marker, or a clip's edge.
fn next_mark(p: Project, at: Float) -> Float {
    let marks = [p.markers.map(k -> k.t), p.clips.map(c -> c.start), p.clips.map(c -> clip_end(c))]
        .flat_map(x -> x)
        .filter(t -> t > at + 0.0001)
        .sort_by(t -> t)
    marks.first() ?? at
}

/// Where the playhead goes from `at` to the marked instant before it.
fn previous_mark(p: Project, at: Float) -> Float {
    let marks = [p.markers.map(k -> k.t), p.clips.map(c -> c.start), p.clips.map(c -> clip_end(c))]
        .flat_map(x -> x)
        .filter(t -> t < at - 0.0001)
        .sort_by(t -> 0.0 - t)
    marks.first() ?? 0.0
}

/// The time a typed timecode names: seconds (`12.5`), minutes and seconds (`1:02.5`), or hours,
/// minutes, seconds and frames (`0:01:02:12`); `at` when it names none.
fn timecode(p: Project, typed: Text, at: Float) -> Float {
    let parts = typed.trim().split(":").map(x -> x.trim().to_float() ?? -1.0)
    if parts.is_empty() or parts.any(x -> x < 0.0) {
        return at
    }
    let n = parts.len()
    let a = parts.at(0) ?? 0.0
    let b = parts.at(1) ?? 0.0
    let c = parts.at(2) ?? 0.0
    let d = parts.at(3) ?? 0.0
    if n == 1 {
        a
    } else if n == 2 {
        a * 60.0 + b
    } else if n == 3 {
        a * 3600.0 + b * 60.0 + c
    } else {
        a * 3600.0 + b * 60.0 + c + d / p.fps.max(1.0)
    }
}

/// The selection after a press on the clip `owner` (empty for none): the clip alone, or with Shift
/// the selection and the clip, or with the command key the selection with the clip toggled.
fn picked(pressed: host.Press, owner: Text, selected: List<Text>) -> List<Text> {
    if pressed.command {
        if selected.contains(owner) {
            selected.filter(x -> x != owner)
        } else { [selected, [owner]].flat_map(x -> x) }
    } else if pressed.shift {
        if selected.contains(owner) { selected } else { [selected, [owner]].flat_map(x -> x) }
    } else { [owner] }
}

/// The selection after a drag of `grabbed` ended: unchanged when the drag moved nothing or moved
/// a chosen clip, else that clip alone.
fn after_drag(selected: List<Text>, grabbed: Text, moved: Bool) -> List<Text> {
    if grabbed == "" or not moved or selected.contains(grabbed) { selected } else { [grabbed] }
}

/// Sets the rate a clip plays at: 2 twice as fast and half as long, 0.5 in slow motion.
action set_speed(c: Clip, speed: Float) {
    set_look(c, .speed, speed)
}

action normal_speed(c: Clip) {
    c.speed_rate = 1.0
    c.reverse = false
    redraw_clip(c)
}

/// One look of a clip that the inspector sets with a slider: the clip's own picture and sound,
/// which the preview draws again as the person changes it.
enum Look {
    opacity
    volume
    speed
    fade_in
    fade_out
    brightness
    contrast
    saturation
    scale_x
    scale_y
    pos_x
    pos_y
    rotation
    crop_left
    crop_right
    crop_top
    crop_bottom
    key_tolerance
    key_softness
    key_spill
    mask_x
    mask_y
    mask_w
    mask_h
    mask_rotation
    mask_feather
    transition_duration
}

/// Sets one look of the clip, and draws the frame the preview shows again, so an edit is seen
/// as it is made rather than at the next move of the playhead.
action set_look(c: Clip, look: Look, value: Float) {
    match look {
        .opacity => {
            c.opacity = value.clamp(0.0, 1.0)
        }
        .volume => {
            c.volume = value.clamp(0.0, 1.0)
        }
        .speed => {
            c.speed_rate = value.clamp(0.05, 20.0)
        }
        .fade_in => {
            c.fade_in = value.max(0.0)
        }
        .fade_out => {
            c.fade_out = value.max(0.0)
        }
        .brightness => {
            c.brightness = value.clamp(0.0, 1.0)
        }
        .contrast => {
            c.contrast = value.clamp(0.0, 1.0)
        }
        .saturation => {
            c.saturation = value.clamp(0.0, 1.0)
        }
        .scale_x => {
            c.scale_x = value.clamp(0.05, 3.0)
        }
        .scale_y => {
            c.scale_y = value.clamp(0.05, 3.0)
        }
        .pos_x => {
            c.pos_x = value.clamp(-1.0, 1.0)
        }
        .pos_y => {
            c.pos_y = value.clamp(-1.0, 1.0)
        }
        .rotation => {
            c.rotation = value.clamp(-180.0, 180.0)
        }
        .crop_left => {
            c.crop_left = value.clamp(0.0, 0.95)
        }
        .crop_right => {
            c.crop_right = value.clamp(0.0, 0.95)
        }
        .crop_top => {
            c.crop_top = value.clamp(0.0, 0.95)
        }
        .crop_bottom => {
            c.crop_bottom = value.clamp(0.0, 0.95)
        }
        .key_tolerance => {
            c.key_tolerance = value.clamp(0.0, 1.0)
        }
        .key_softness => {
            c.key_softness = value.clamp(0.0, 1.0)
        }
        .key_spill => {
            c.key_spill = value.clamp(0.0, 1.0)
        }
        .mask_x => {
            c.mask_x = value.clamp(-1.0, 1.0)
        }
        .mask_y => {
            c.mask_y = value.clamp(-1.0, 1.0)
        }
        .mask_w => {
            c.mask_w = value.clamp(0.01, 2.0)
        }
        .mask_h => {
            c.mask_h = value.clamp(0.01, 2.0)
        }
        .mask_rotation => {
            c.mask_rotation = value.clamp(-180.0, 180.0)
        }
        .mask_feather => {
            c.mask_feather = value.clamp(0.0, 1.0)
        }
        .transition_duration => {
            c.transition_duration = value.clamp(0.0, 10.0)
        }
    }
    redraw_clip(c)
}

/// Moves the point a clip's scaling and turning go around: a corner, an edge or the middle,
/// which is where a picture is pinned when it is made half the frame, turned or grown.
action set_anchor(c: Clip, x: Float, y: Float) {
    c.anchor_x = x.clamp(0.0, 1.0)
    c.anchor_y = y.clamp(0.0, 1.0)
    redraw_clip(c)
}

/// Sets which shape limits where a clip shows: `off`, `rect` or `ellipse`.
action set_mask(c: Clip, shape: Text) {
    require shape == "off" or shape == "rect" or shape == "ellipse" "{shape} is not a mask shape"
    c.mask_shape = if shape == "off" { "" } else { shape }
    redraw_clip(c)
}

/// Keeps the inside of the clip's mask, or the outside of it.
action invert_mask(c: Clip, invert: Bool) {
    c.mask_invert = invert
    redraw_clip(c)
}

/// Sets the incoming transition and how long it runs.
action set_transition(c: Clip, transition: Text, seconds: Float) {
    c.transition = transition
    c.transition_duration = seconds
    redraw_clip(c)
}

/// Plays the clip's source backwards, or forwards again, and draws the frame it holds now.
action set_reverse(c: Clip, reverse: Bool) {
    c.reverse = reverse
    redraw_clip(c)
}

/// One style of a title's lines, drawn again as it changes.
action set_title_style(m: Media, bold: Bool, italic: Bool) {
    m.bold = bold
    m.italic = italic
    redraw_media(m)
}

/// One track's level, where the sound of everything on it sits in the mix.
action set_track_gain(t: Track, db: Float) {
    t.gain_db = db.clamp(-60.0, 12.0)
}

/// Where a track's sound sits across the stereo field: -1 hard left, 1 hard right.
action set_track_pan(t: Track, pan: Float) {
    t.pan = pan.clamp(-1.0, 1.0)
}

action set_track_muted(t: Track, muted: Bool) {
    t.muted = muted
}

action set_track_solo(t: Track, solo: Bool) {
    t.solo = solo
}

action set_track_locked(t: Track, locked: Bool) {
    t.locked = locked
}

/// Puts a track back at unity and the centre.
action reset_mix(t: Track) (icon: .rotate_ccw, label: "Reset the level and pan") {
    t.gain_db = 0.0
    t.pan = 0.0
}

/// One look of a title the inspector sets with a slider, drawn again as it changes.
enum TitleLook {
    size
    red
    green
    blue
    backdrop
}

action set_title_look(m: Media, look: TitleLook, value: Float) {
    match look {
        .size => {
            m.size = value.clamp(0.02, 0.5)
        }
        .red => {
            m.red = value.clamp(0.0, 1.0)
        }
        .green => {
            m.green = value.clamp(0.0, 1.0)
        }
        .blue => {
            m.blue = value.clamp(0.0, 1.0)
        }
        .backdrop => {
            m.backdrop = value.clamp(0.0, 1.0)
        }
    }
    redraw_media(m)
}

/// The cut a clip is in, as the editor holds it: the cut whose clips include this one.
fn cut_of_clip(c: Clip) -> Option<Project> {
    let e = root Editor
    e.projects.find(p -> p.clips.any(x -> x.id() == c.id()))
}

/// The cut a source is in, as the editor holds it.
fn cut_of_media(m: Media) -> Option<Project> {
    let e = root Editor
    e.projects.find(p -> p.media.any(x -> x.id() == m.id()))
}

/// Draws the frame the preview last showed again, so an edit made after it is seen at once. A
/// cut whose frame the window never showed draws nothing: there is no place to draw it at.
action redraw(p: Project) {
    if p.live != "" {
        if play_moving(p) {
            refresh_live(p)
            return
        }
        stop_cut(p)
    }
    if p.still_at >= 0.0 { draw_still(p, p.still_at) }
}

action redraw_clip(c: Clip) {
    if let p = cut_of_clip(c) { redraw(p) }
}

action redraw_media(m: Media) {
    if let p = cut_of_media(m) { redraw(p) }
}

/// What a clip's animatable field holds as its constant value.
fn param_value(c: Clip, param: Text) -> Float {
    if param == "volume" {
        c.volume
    } else if param == "brightness" {
        c.brightness
    } else if param == "contrast" {
        c.contrast
    } else if param == "saturation" {
        c.saturation
    } else if param == "scale_x" {
        c.scale_x
    } else if param == "pos_x" {
        c.pos_x
    } else if param == "pos_y" {
        c.pos_y
    } else if param == "rotation" {
        c.rotation
    } else if param == "key_tolerance" {
        c.key_tolerance
    } else if param == "key_softness" {
        c.key_softness
    } else if param == "key_spill" {
        c.key_spill
    } else if param == "mask_w" {
        c.mask_w
    } else if param == "mask_h" { c.mask_h } else { c.opacity }
}

/// Sets a key of `param` at `at` (timeline seconds), at the value the field holds now: the value
/// a curve changes from, as the inspection shows it.
action key_at(c: Clip, at: Float, param: Text, interp: Text) {
    let t = (at - c.start).max(0.0)
    let v = param_value(c, param)
    let near = c.keys.find(x -> x.param == param and (x.t - t).abs() <= 0.02)
    if let held = near {
        held.v = v
        held.interp = interp
        return
    }
    c.keys.insert(Key(param: param, t: t, v: v, interp: interp))
    redraw_clip(c)
}

/// Removes a key.
action unkey(c: Clip, k: Key) {
    c.keys.remove(k)
    redraw_clip(c)
}

/// How a key's curve leaves it.
action set_interp(k: Key, interp: Text) {
    k.interp = interp
}

fn interp_words(name: Text) -> Text {
    if name == "hold" {
        std.say("Hold")
    } else if name == "ease" { std.say("Ease") } else { std.say("Linear") }
}

/// Takes the colour of the cut's frame where the person pointed, as the key that removes it.
action sample_key(p: Project, c: Clip, at: Float, x: Float, y: Float) {
    match engine.call(
        "sample",
        json.encode(
            SampleAsk(
                project: json.encode(shot(p)),
                at: snap_time(p, at),
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
            ),
        ),
    ) {
        .ok(text) => match json.decode<SampleAnswer>(text) {
            .ok(colour) => {
                c.key_red = colour.red
                c.key_green = colour.green
                c.key_blue = colour.blue
                c.key_enabled = true
                p.trouble = ""
            }
            .err(why) => trouble(p, why),
        },
        .err(why) => trouble(p, why),
    }
}

/// Sets the colour the key removes to one of the usual screens.
action key_colour(c: Clip, red: Float, green: Float, blue: Float) {
    c.key_red = red
    c.key_green = green
    c.key_blue = blue
    c.key_enabled = true
}

/// One frame of a source at `at` seconds in it, kept as a filmstrip picture.
action thumb_frame(c: Clip, at: Float, width: Int, height: Int) -> Text {
    match engine.call(
        "thumb",
        json.encode(ThumbAsk(url: c.media.path, at: at, width: width, height: height)),
    ) {
        .ok(text) => match json.decode<StillAnswer>(text) {
            .ok(answer) => answer.url,
            .err(_) => "",
        },
        .err(_) => "",
    }
}

/// The frame range the clip's filmstrip was drawn for: a trim or a speed change draws it again.
fn strip_span(c: Clip) -> Text {
    "{c.in_point}-{c.out_point}-{c.speed_rate}"
}

/// Draws eight frames of the clip across its own part, kept until a trim asks for them again.
action filmstrip(p: Project, c: Clip) {
    let span = strip_span(c)
    if c.strips_key == span and c.strips.len() == 8 {
        return
    }
    if c.media.path == "" {
        return
    }
    let width = 160
    let height = (width.to_float() * p.height.to_float() / p.width.to_float().max(1.0))
        .round()
        .max(16)
    let inside = c.out_point - c.in_point
    c.strips = [
        thumb_frame(c, c.in_point, width, height),
        thumb_frame(c, c.in_point + inside * 0.125, width, height),
        thumb_frame(c, c.in_point + inside * 0.25, width, height),
        thumb_frame(c, c.in_point + inside * 0.375, width, height),
        thumb_frame(c, c.in_point + inside * 0.5, width, height),
        thumb_frame(c, c.in_point + inside * 0.625, width, height),
        thumb_frame(c, c.in_point + inside * 0.75, width, height),
        thumb_frame(c, c.in_point + inside * 0.875, width, height),
    ]
    c.strips_key = span
}

/// Writes what a mark says.
action note_marker(k: Marker, note: Text) {
    k.note = note
}

/// Draws the filmstrip of the first clip whose frames are missing or stale.
action draw_next_strip(p: Project) {
    for c in p.clips.sort_by(one -> one.start) {
        if c.media.form != .audio and c.media.path != "" and c.strips_key != strip_span(c) {
            filmstrip(p, c)
            return
        }
    }
}

/// The timeline shows each clip's frames as they are drawn, one clip a second.
trigger fill_strips(p: Project) every 1s: draw_next_strip(p)

/// Sets where a title's lines sit in its box.
action set_align(m: Media, align: Align) {
    m.align = align
    redraw_media(m)
}

/// What the title's lines say.
action set_words(m: Media, words: Text) {
    m.words = words
    redraw_media(m)
}

/// Adds a title to the bin and places it at `at`.
action add_title(p: Project, words: Text, at: Float) -> Media (icon: .pencil) {
    let m = p.media.insert(
        Media(
            name: if words == "" { std.say("Title") } else { words },
            form: .title,
            duration: 5.0,
            words: words,
            palette: p.media.len(),
        ),
    )
    let track = p.tracks.find(t -> t.form == .video)
    if let track = track { place(p, m, track, at) }
    m
}

/// Adds a marker at `at`.
action add_marker(p: Project, at: Float) -> Marker (icon: .flag) {
    let n = p.markers.len() + 1
    p.markers.insert(Marker(name: std.say("Marker {n}"), t: at))
}

action remove_marker(p: Project, k: Marker) {
    p.markers.remove(k)
}

action mark_in(p: Project, at: Float) (icon: .arrow_right, label: "Mark in") {
    p.mark_in = at.max(0.0)
    if p.mark_out >= 0.0 and p.mark_out <= p.mark_in {
        p.mark_out = -1.0
    }
}

action mark_out(p: Project, at: Float) (icon: .arrow_left, label: "Mark out") {
    p.mark_out = at.max(0.0)
    if p.mark_in >= 0.0 and p.mark_in >= p.mark_out {
        p.mark_in = -1.0
    }
}

action clear_marks(p: Project) {
    p.mark_in = -1.0
    p.mark_out = -1.0
}

action set_frame_rate(p: Project, fps: Float) {
    p.fps = fps.clamp(1.0, 240.0)
    p.explicit_format = true
    redraw(p)
}

action set_frame_size(p: Project, width: Int, height: Int) {
    p.width = width.clamp(16, 8192)
    p.height = height.clamp(16, 8192)
    p.explicit_format = true
    redraw(p)
}

/// Takes its shape from a source: what "Use its format" does.
action adopt_format(p: Project, m: Media) {
    if m.width > 0 and m.height > 0 {
        p.width = m.width
        p.height = m.height
        if m.fps > 0.0 {
            p.fps = m.fps
        }
        p.explicit_format = true
        redraw(p)
    }
}

action add_track(p: Project, form: Form) -> Track (icon: .plus) {
    let index = p.tracks.filter(t -> t.form == form).len() + 1
    let made = p.tracks.insert(
        Track(name: if form == .audio { "A{index}" } else { "V{index}" }, form: form, index: index),
    )
    redraw(p)
    made
}

action delete_track(p: Project, t: Track) (icon: .trash_2, label: "Delete the track") {
    for c in p.clips.filter(x -> x.track == t) {
        p.clips.remove(c)
    }
    p.tracks.remove(t)
    redraw(p)
}

/// Makes two clips one shot's picture and sound: they move and cut together.
action link_clips(p: Project, a: Clip, b: Clip) {
    let link = a_free_link(p)
    a.link = link
    b.link = link
    a.link_follows = true
    b.link_follows = true
}

action unlink_clip(c: Clip) {
    c.link_follows = false
}

/// Moves a picture clip's own sound onto an audio clip of its own, linked to it.
action split_audio(p: Project, c: Clip) {
    if c.sound_split {
        return
    }
    let track = match p.tracks.find(t -> t.form == .audio) {
        .some(t) => t,
        .none => add_track(p, .audio),
    }
    let link = if c.link == "" { a_free_link(p) } else { c.link }
    p.clips.insert(
        Clip(
            name: "{c.name} (sound)",
            track: track,
            media: c.media,
            start: c.start,
            in_point: c.in_point,
            out_point: c.out_point,
            volume: c.volume,
            speed_rate: c.speed_rate,
            reverse: c.reverse,
            link: link,
            link_follows: true,
        ),
    )
    c.link = link
    c.link_follows = true
    c.sound_split = true
}

action add_effect(c: Clip, form: Text) {
    c.effects.insert(Effect(form: form, amount: 1.0))
    redraw_clip(c)
}

action remove_effect(c: Clip, e: Effect) {
    c.effects.remove(e)
    redraw_clip(c)
}

/// How strongly one effect of the clip applies, applied to the picture at once.
action set_effect(c: Clip, e: Effect, amount: Float) {
    e.amount = amount.clamp(0.0, 1.0)
    redraw_clip(c)
}

/// Moves an effect up or down the clip's stack: the order decides what the picture goes through
/// first.
action move_effect(c: Clip, e: Effect, up: Bool) {
    let at = c.effects.index_of(e) ?? 0
    let to = if up { at - 1 } else { at + 1 }
    if to < 0 or to >= c.effects.len() {
        return
    }
    c.effects.move(e, to: to)
    redraw_clip(c)
}

/// Takes every effect off a clip.
action clear_effects(c: Clip) (icon: .x, label: "Remove every effect") {
    for e in c.effects {
        c.effects.remove(e)
    }
    redraw_clip(c)
}

/// Takes a source out of the bin, and every clip that showed it with it.
destructive action delete_media(p: Project, m: Media) (icon: .trash_2, label: "Delete the source") {
    for c in p.clips.filter(x -> x.media == m) {
        p.clips.remove(c)
    }
    p.media.remove(m)
    redraw(p)
}

/// Draws the frame the preview shows at `at`.
action preview(p: Project, at: Float) {
    draw_still(p, at)
}

/// The width a live frame is drawn at before the window shows it, and the height that gives the
/// cut's own shape.
fn live_width() -> Int {
    640
}

fn live_height(p: Project) -> Int {
    (640.0 * p.height.to_float() / p.width.to_float().max(1.0)).round().max(64)
}

/// Plays the cut from `at`: the device composites each frame as the playhead reaches it and
/// mixes the sound for the same moment, so the window shows the cut itself rather than a render
/// of it. A negative rate plays the footage and its sound backwards.
action play_cut(p: Project, at: Float, rate: Float) {
    stop_cut(p)
    // A play goes somewhere: a rate of nothing is normal speed.
    let rate = if rate.abs() < 0.01 { 1.0 } else { rate }
    let end = total(p)
    let from = if at >= end { 0.0 } else { at.max(0.0) }
    match engine.stream(
        "play",
        json.encode(
            LiveAsk(
                project: json.encode(shot(p)),
                from: from,
                rate: rate,
                width: live_width(),
                height: live_height(p),
                sound: true,
            ),
        ),
    ) {
        .ok(text) => match json.decode<JobAnswer>(text) {
            .ok(answer) => {
                p.live = answer.job
                p.live_rate = rate
                p.still_at = from
                p.trouble = ""
            }
            .err(why) => trouble(p, why),
        },
        .err(why) => trouble(p, why),
    }
}

/// Stops the live play: the device stops compositing and the sound stops with the picture.
action stop_cut(p: Project) {
    if p.live != "" {
        engine.cancel(p.live)
        p.live = ""
        p.live_rate = 0.0
    }
}

/// Stops the live play where it stands, and draws that frame: what pause does.
action pause_cut(p: Project) {
    if p.live == "" {
        return
    }
    let at = live_at(p)
    stop_cut(p)
    draw_still(p, snap_time(p, if at >= 0.0 { at } else { p.still_at }))
}

/// One press of a shuttle control: the same way doubles the speed up to sixteen times, the
/// other way starts again at normal speed that way.
action shuttle(p: Project, at: Float, way: Float) {
    let faster = if p.live_rate * way > 0.0 { p.live_rate * 2.0 } else { way }
    play_cut(p, at, faster.clamp(-16.0, 16.0))
}

/// Stops the live play and draws the frame `at` in its place: what a jump or a frame step does.
action jump_to(p: Project, at: Float) {
    stop_cut(p)
    preview(p, at)
}

/// What the transport says the playhead is doing, which is nothing while it stands.
fn motion_words(p: Project) -> Text {
    if p.live == "" {
        ""
    } else if p.live_rate < 0.0 {
        std.say("Backwards {p.live_rate.abs().round()}×")
    } else if p.live_rate > 1.0 {
        std.say("Forwards {p.live_rate.round()}×")
    } else { std.say("Playing") }
}

/// Where the live play has the playhead now, or -1 when nothing plays.
action live_at(p: Project) -> Float {
    if p.live == "" {
        return -1.0
    }
    engine.state(p.live).to_float() ?? -1.0
}

/// Scrubs to the second `at`: the live play stops, the frame under the playhead is drawn,
/// and a short piece of the sound there is played. A drag runs this at every move.
action scrub_to(p: Project, at: Float) {
    stop_cut(p)
    draw_still(p, snap_time(p, at))
    hear_scrub(p, snap_time(p, at))
}

/// Whether the live play is still running: a word of nowhere means it is just starting, and a
/// word at either end means it has stopped there.
action play_moving(p: Project) -> Bool {
    let at = live_at(p)
    if at < 0.0 {
        return true
    }
    at < total(p) and (p.live_rate >= 0.0 or at > 0.0)
}

/// Hands the device the cut as it now is: an edit made while the cut plays is seen as it is
/// made, rather than at the next play.
action refresh_live(p: Project) {
    if p.live == "" {
        return
    }
    match engine.call("cut", json.encode(CutAsk(job: p.live, project: json.encode(shot(p))))) {
        .ok(_) => {}
        .err(why) => trouble(p, why),
    }
}

/// The cut's sound where the playhead was dragged: a short snippet, not the timeline's running
/// sound.
action hear_scrub(p: Project, at: Float) {
    match engine.call(
        "scrub",
        json.encode(ScrubAsk(project: json.encode(shot(p)), at: at.max(0.0), seconds: 0.12)),
    ) {
        .ok(_) => {}
        .err(_) => {}
    }
}

/// Renders the cut to a file, refusing when a source this device cannot read would leave the
/// film missing its footage.
action export_cut(p: Project, resolution: Text, container: Text, fps: Int) {
    let missing = missing_sources(p)
    if not missing.is_empty() {
        trouble(
            p,
            std.say(
                "Cannot export: {missing.join(", ")} is not on this device, so the film would miss that footage",
            ),
        )
        return
    }
    begin_render(p, resolution, container, fps)
}

/// The sources the render's range reaches for that this device does not have, by name.
fn missing_sources(p: Project) -> List<Text> {
    let from = render_from(p)
    let to = render_to(p)
    let reached = p.clips.filter(c -> c.start + shown(c) > from and c.start < to)
    p.media.filter(m -> m.missing and reached.any(c -> c.media == m)).map(m -> m.name)
}

/// The render moved on: how far it has got, as the window shows it.
action render_moved(p: Project, j: host.Job) {
    p.job_done = j.done
    p.job_total = j.total
}

/// The render ended: the file a person saves, or why it made none.
action render_done(p: Project, j: host.Job) {
    p.job = ""
    if j.state == .done {
        match json.decode<StillAnswer>(j.detail) {
            .ok(made) => {
                p.exported = made.url
                p.trouble = ""
                draw_still(p, render_from(p))
            }
            .err(why) => trouble(p, why),
        }
    } else if j.state == .failed { trouble(p, j.error) }
}

action cancel_render(p: Project) {
    if p.job != "" {
        engine.cancel(p.job)
        p.job = ""
    }
}

/// A clock a person reads: `1:04.5`.
// ---------------------------------------------------------------- the timeline's drawing
fn clock(seconds: Float) -> Text {
    let whole = seconds.max(0.0).floor()
    let minutes = whole / 60
    let rest = whole - minutes * 60
    let tenths = ((seconds.max(0.0) - whole.to_float()) * 10.0).floor().min(9)
    "{minutes}:{padded(rest)}.{tenths}"
}

fn padded(n: Int) -> Text {
    if n < 10 { "0{n}" } else { "{n}" }
}

/// One frame of the cut, in seconds: the grid a drag on the timeline lands on.
fn frame(p: Project) -> Float {
    if p.fps > 0.0 { 1.0 / p.fps } else { 0.0 }
}

/// How long a clip's incoming transition is drawn: its own length, a second by default.
fn lead_in(c: Clip) -> Float {
    if c.transition == "" {
        0.0
    } else if c.transition_duration > 0.0 { c.transition_duration } else { 1.0 }
}

/// One track as the timeline shows it: its name and toggles, and its clips with their frames,
/// their sound and their keyframes. A press chooses a clip, with Shift adds it and with the
/// command key toggles it; a drag of a chosen clip moves every chosen clip.
template view timeline_track(
    p: Project,
    t: Track,
    chosen: List<Clip>,
    selected: State<List<Text>>,
) {
    ui.TimelineTrack(item: t.id(), label: t.name, muted: t.muted, solo: t.solo, locked: t.locked) {
        on toggled_mute(m: Bool): set_track_muted(t, m)
        on toggled_solo(m: Bool): set_track_solo(t, m)
        on toggled_lock(m: Bool): set_track_locked(t, m)
        for c in p.clips.filter(x -> x.track == t) {
            ui.TimelineSpan(
                item: c.id(),
                label: c.name,
                start: c.start,
                length: shown(c),
                tone: if c.media.form == .audio {
                    .info
                } else if c.palette % 6 == 0 {
                    .accent
                } else if c.palette % 6 == 1 {
                    .info
                } else if c.palette % 6 == 2 {
                    .success
                } else if c.palette % 6 == 3 {
                    .warning
                } else if c.palette % 6 == 4 { .danger } else { .ink },
                selected: selected.contains(c.id()),
                frames: c.strips,
                peaks: if c.media.form == .audio { c.media.peaks } else { [] },
                keys: c.keys.map(k -> k.t),
                lead_in: lead_in(c),
            ) {
                on pressed(i): set(selected, picked(i, c.id(), selected))
                on moved(start: Float, track: Text): move_to(p, c, start, track, chosen)
                    .then(set(selected, after_drag(selected, c.id(), true)))
                on trimmed(start: Float, length: Float): trim_to(p, c, start, length)
                    .then(set(selected, after_drag(selected, c.id(), true)))
            }
        }
    }
}

/// What can be done to the clips chosen on the timeline, each with its key.
view edit_menu(
    p: Project,
    chosen: List<Clip>,
    playhead: State<Float>,
    selected: State<List<Text>>,
    scroll: State<Float>,
    span: Float,
) {
    let none = chosen.is_empty()
    ui.MenuButton(std.say("Edit"), icon: .pencil) {
        ui.MenuItem(std.say("Select all clips"), icon: .check, key: "Ctrl+A") {
            on choose: set(selected, p.clips.map(c -> c.id()))
        }
        ui.MenuItem(std.say("Cut"), icon: .scissors, key: "Ctrl+X", disabled: none) {
            on choose: cut_clips(p, chosen).then(set(selected, []))
        }
        ui.MenuItem(std.say("Copy"), icon: .copy, key: "Ctrl+C", disabled: none) {
            on choose: copy_clips(p, chosen)
        }
        ui.MenuItem(std.say("Paste at the playhead"), icon: .clipboard_paste, key: "Ctrl+V") {
            on choose: paste_clips(p, playhead)
        }
        ui.MenuItem(std.say("Duplicate"), icon: .copy, key: "Ctrl+D", disabled: none) {
            on choose: duplicate_clips(p, chosen)
        }
        ui.MenuItem(
            std.say("Delete"),
            icon: .trash_2,
            key: "Delete",
            destructive: true,
            disabled: none,
        ) {
            on choose: remove_clips(p, chosen).then(set(selected, []))
        }
        ui.MenuItem(
            std.say("Delete and close the gap"),
            icon: .scissors,
            key: "Shift+Delete",
            destructive: true,
            disabled: none,
        ) {
            on choose: ripple_delete(p, chosen).then(set(selected, []))
        }
        ui.MenuItem(std.say("Trim the start to the playhead"), key: "i", disabled: none) {
            on choose: trim_start(p, chosen, playhead)
        }
        ui.MenuItem(std.say("Trim the end to the playhead"), key: "o", disabled: none) {
            on choose: trim_end(p, chosen, playhead)
        }
        ui.MenuItem(
            std.say("Nudge a frame earlier"),
            key: if none { .none } else { .some("Left") },
            disabled: none,
        ) {
            on choose: nudge(p, chosen, -1)
        }
        ui.MenuItem(
            std.say("Nudge a frame later"),
            key: if none { .none } else { .some("Right") },
            disabled: none,
        ) {
            on choose: nudge(p, chosen, 1)
        }
        ui.MenuItem(
            std.say("Nudge a second earlier"),
            key: if none { .none } else { .some("Shift+Left") },
            disabled: none,
        ) {
            on choose: nudge(p, chosen, 0 - p.fps.round())
        }
        ui.MenuItem(
            std.say("Nudge a second later"),
            key: if none { .none } else { .some("Shift+Right") },
            disabled: none,
        ) {
            on choose: nudge(p, chosen, p.fps.round())
        }
        ui.MenuItem(std.say("Previous marker"), icon: .chevron_left, key: ",") {
            on choose: set(playhead, previous_mark(p, playhead))
        }
        ui.MenuItem(std.say("Next marker"), icon: .chevron_right, key: ".") {
            on choose: set(playhead, next_mark(p, playhead))
        }
        if let c = chosen.last() {
            ui.MenuItem(std.say("Go to the clip"), icon: .locate_fixed, key: "g") {
                on choose: set(playhead, c.start).then(set(scroll, (c.start - span / 4.0).max(0.0)))
            }
        }
    }
}

/// The editor: the bin on the left, the preview over the timeline in the middle, and the
/// inspector on the right.
// ---------------------------------------------------------------- the window
view window(e: Editor) {
    state playhead: Float = 0.0
    state span: Float = 20.0
    state scroll: Float = 0.0
    state selected: List<Text> = []
    state picked_media: Text = ""
    state tab: Text = "clip"
    state resolution: Text = "1080"
    state container: Text = "mp4"
    state rate: Text = "30"
    let p = shown_project(e)
    if let p = p {
        // What the live play says of itself: the timeline second it has reached, in the word
        // the device pushes with every few frames it draws.
        let live_word = if p.live != "" { engine.state(p.live).to_float() } else { .none }
        // The playhead as the toolbar, the ruler and the clock draw it: the live play's own
        // word while the cut plays, so the head moves with the picture rather than once a second,
        // and the frame that stands otherwise.
        let head = live_word ?? if p.still_at >= 0.0 { p.still_at } else { playhead }
        // A play that reached the cut's end has stopped: the picture stands where it stopped,
        // and the transport offers the play again.
        let ended = match live_word {
            .some(at) => at >= total(p) or p.live_rate < 0.0 and at <= 0.0,
            .none => false,
        }
        let live = p.live != "" and not ended
        if live {
            every 1s: set(playhead, engine.state(p.live).to_float() ?? playhead)
        }
        let chosen = p.clips.filter(c -> selected.contains(c.id()))
        let clip = clip_named(p, selected.last() ?? "")
        let media = p.media.find(m -> m.id() == picked_media)
        let pct = percent(p)
        let title = if p.job != "" {
            std.say("Rendering the cut… {pct}%")
        } else if p.exported != "" {
            std.say("{p.name} — the film is ready")
        } else { p.name }
        ui.NavShell(content_kind: .canvas, title: title, sidebar_title: std.say("Video Editor")) {
            actions: {
                ui.Button(std.say("New project"), icon: .plus) {
                    on press: new_project(e).then(set(playhead, 0.0))
                }
                ui.Button(std.say("Open a project"), icon: .folder_open) {
                    on press: file.choose(open_project_file(e), accept: [".tlvp"])
                }
                ui.Button(std.say("Save the project"), icon: .save) {
                    on press: file.save(save_project(p), name: "{p.name}.tlvp")
                }
            }
            sidebar: {
                ui.NavList {
                    ui.NavSection(std.say("Projects")) {
                        for project in e.projects.sort_by(x -> x.name) {
                            ui.NavItem(project.id(), project.name, icon: .film, item: project) {
                                on select: open_project(e, project)
                                    .then(set(selected, []))
                                    .then(set(playhead, 0.0))
                            }
                        }
                    }
                }
            }
            content: {
                row(gap: .related, fill) {
                    // The bin: the sources of this cut.
                    stack(gap: .tight) {
                        ui.Text(std.say("Sources"), role: .heading)
                        ui.DropZone(
                            std.say("Drop video, sound or pictures"),
                            message: std.say("They become clips of this cut"),
                            icon: .file_plus,
                        ) {
                            on drop(files): import_files(p, files)
                            on choose: file.choose(
                                import_one(p),
                                accept: [
                                    "video/mp4",
                                    "video/quicktime",
                                    "video/x-matroska",
                                    "video/webm",
                                    "audio/mpeg",
                                    "audio/wav",
                                    "image/png",
                                    "image/jpeg",
                                ],
                            )
                        }
                        for m in p.media.sort_by(x -> x.name) {
                            ui.ListRow(
                                m.name,
                                item: m,
                                icon: if m.missing {
                                    .triangle_alert
                                } else if m.form == .audio {
                                    .music
                                } else if m.form == .image {
                                    .image
                                } else if m.form == .title { .pencil } else { .film },
                                meta: if m.missing {
                                    std.say("Not on this device")
                                } else if m.form == .title {
                                    std.say("Title")
                                } else { clock(m.duration) },
                                selected: m.id() == picked_media,
                            ) {
                                on activate: set(picked_media, m.id())
                            }
                        }
                    }
                    stack(gap: .related, fill) {
                        // The preview: the cut playing live as the device composites it, or the
                        // frame at the playhead while it stands.
                        ui.Section(p.name) {
                            if p.live != "" {
                                ui.Image(
                                    "tilo-live://{p.live}",
                                    alt: p.name,
                                    fit: .contain,
                                    min: 10rem,
                                    max: 26rem,
                                )
                            } else if p.still != "" {
                                ui.Image(
                                    p.still,
                                    alt: p.name,
                                    fit: .contain,
                                    min: 10rem,
                                    max: 26rem,
                                )
                            } else {
                                ui.EmptyState(
                                    std.say("Nothing to show yet"),
                                    message: std.say(
                                        "Add a source and the frame at the playhead stands here",
                                    ),
                                    icon: .film,
                                    inline: true,
                                )
                            }
                            row(gap: .tight, align: .center) {
                                ui.Button(
                                    "",
                                    icon: .chevrons_left,
                                    icon_only: true,
                                    accessible_name: std.say("Jump to the start"),
                                    key: "Home",
                                ) {
                                    on press: jump_to(p, 0.0).then(set(playhead, 0.0))
                                }
                                ui.Button(
                                    "",
                                    icon: .skip_back,
                                    icon_only: true,
                                    accessible_name: std.say("Back a second"),
                                    key: if selected.is_empty() {
                                        .some("Shift+Left")
                                    } else { .none },
                                ) {
                                    on press: jump_to(p, (playhead - 1.0).max(0.0))
                                        .then(
                                            set(playhead, (playhead - 1.0).max(0.0)),
                                        )
                                }
                                ui.Button(
                                    "",
                                    icon: .chevron_left,
                                    icon_only: true,
                                    accessible_name: std.say("Back a frame"),
                                    key: if selected.is_empty() { .some("Left") } else { .none },
                                ) {
                                    on press: jump_to(p, stepped(p, playhead, -1))
                                        .then(
                                            set(playhead, stepped(p, playhead, -1)),
                                        )
                                }
                                // Shuttle: each press the same way doubles the speed, and the
                                // other way starts again at normal speed that way.
                                ui.Button(
                                    "",
                                    icon: .rewind,
                                    icon_only: true,
                                    accessible_name: std.say(
                                        "Play backwards, faster with each press",
                                    ),
                                    key: "j",
                                ) {
                                    on press: session(e).then(shuttle(p, playhead, -1.0))
                                }
                                if live {
                                    ui.Button(
                                        "",
                                        icon: .pause,
                                        icon_only: true,
                                        accessible_name: std.say("Pause"),
                                        key: "Space",
                                    ) {
                                        on press: pause_cut(p)
                                    }
                                } else {
                                    ui.Button(
                                        "",
                                        icon: .play,
                                        icon_only: true,
                                        accessible_name: std.say("Play the cut"),
                                        key: "Space",
                                    ) {
                                        on press: session(e).then(play_cut(p, playhead, 1.0))
                                    }
                                }
                                ui.Button(
                                    "",
                                    icon: .fast_forward,
                                    icon_only: true,
                                    accessible_name: std.say("Play faster with each press"),
                                    key: "l",
                                ) {
                                    on press: session(e).then(shuttle(p, playhead, 1.0))
                                }
                                ui.Button(
                                    "",
                                    icon: .chevron_right,
                                    icon_only: true,
                                    accessible_name: std.say("Forward a frame"),
                                    key: if selected.is_empty() { .some("Right") } else { .none },
                                ) {
                                    on press: jump_to(p, stepped(p, playhead, 1))
                                        .then(
                                            set(playhead, stepped(p, playhead, 1)),
                                        )
                                }
                                ui.Button(
                                    "",
                                    icon: .skip_forward,
                                    icon_only: true,
                                    accessible_name: std.say("Forward a second"),
                                    key: if selected.is_empty() {
                                        .some("Shift+Right")
                                    } else { .none },
                                ) {
                                    on press: jump_to(p, playhead + 1.0)
                                        .then(
                                            set(playhead, playhead + 1.0),
                                        )
                                }
                                ui.Button(
                                    "",
                                    icon: .chevrons_right,
                                    icon_only: true,
                                    accessible_name: std.say("Jump to the end"),
                                    key: "End",
                                ) {
                                    on press: jump_to(p, total(p)).then(set(playhead, total(p)))
                                }
                                if live {
                                    ui.Text(motion_words(p), role: .caption, secondary: true)
                                }
                                ui.Text(clock(head), role: .caption, numeric: true)
                                ui.Text(
                                    "/ {clock(total(p))}",
                                    role: .caption,
                                    secondary: true,
                                    numeric: true,
                                )
                                ui.Button(std.say("Mark in"), icon: .arrow_right, key: "Shift+I") {
                                    on press: mark_in(p, playhead)
                                }
                                ui.Button(std.say("Mark out"), icon: .arrow_left, key: "Shift+O") {
                                    on press: mark_out(p, playhead)
                                }
                                if p.mark_in >= 0.0 or p.mark_out >= 0.0 {
                                    ui.Button(std.say("Clear"), icon: .x, key: "Shift+X") {
                                        on press: clear_marks(p)
                                    }
                                }
                                ui.Button(std.say("Frame"), icon: .image) {
                                    on press: preview(p, playhead)
                                }
                                if let s = e.playing {
                                    if let out = s.outputs.find(x -> x.device != "") {
                                        ui.LevelMeter(
                                            source: out.device,
                                            output: true,
                                            running: live,
                                            label: std.say("What the cut plays through"),
                                        )
                                    }
                                }
                            }
                            if p.job != "" {
                                ui.ProgressBar(
                                    value: .some(
                                        if p.job_total > 0 {
                                            p.job_done.to_float() / p.job_total.to_float()
                                        } else { 0.0 },
                                    ),
                                    label: std.say("Rendering the film"),
                                )
                                ui.Button(std.say("Stop the render"), icon: .x) {
                                    on press: cancel_render(p)
                                }
                            }
                            if p.trouble != "" {
                                ui.Banner(
                                    std.say("The engine could not do that"),
                                    message: p.trouble,
                                    kind: .error,
                                )
                            }
                        }
                        // The timeline: the ruler over the tracks, both drawn by the design
                        // system's drawing.
                        stack(gap: .tight) {
                            row(gap: .tight, align: .center) {
                                ui.Button(
                                    "",
                                    icon: .minus,
                                    icon_only: true,
                                    accessible_name: std.say("Zoom out"),
                                    key: "-",
                                ) {
                                    on press: set(span, (span * 2.0).clamp(1.0, 3600.0))
                                }
                                ui.Button(
                                    "",
                                    icon: .plus,
                                    icon_only: true,
                                    accessible_name: std.say("Zoom in"),
                                    key: "=",
                                ) {
                                    on press: set(span, (span / 2.0).clamp(1.0, 3600.0))
                                }
                                ui.Button(std.say("Fit"), icon: .maximize, key: "z") {
                                    on press: set(span, total(p).max(2.0)).then(set(scroll, 0.0))
                                }
                                ui.Button(
                                    "",
                                    icon: .chevron_left,
                                    icon_only: true,
                                    accessible_name: std.say("Earlier"),
                                ) {
                                    on press: set(scroll, (scroll - span / 2.0).max(0.0))
                                }
                                ui.Button(
                                    "",
                                    icon: .chevron_right,
                                    icon_only: true,
                                    accessible_name: std.say("Later"),
                                ) {
                                    on press: set(scroll, scroll + span / 2.0)
                                }
                                ui.Button(std.say("Split"), icon: .scissors, key: "s") {
                                    on press: cut_at(p, playhead).then(set(selected, []))
                                }
                                if not chosen.is_empty() {
                                    ui.Button(std.say("Delete"), icon: .trash_2) {
                                        on press: remove_clips(p, chosen).then(set(selected, []))
                                    }
                                }
                                ui.Button(std.say("Marker"), icon: .flag, key: "m") {
                                    on press: add_marker(p, playhead)
                                }
                                edit_menu(p, chosen, playhead, selected, scroll, span)
                                ui.TextInput(placeholder: std.say("Go to 1:02.5"), key: "Ctrl+G") {
                                    on submit(typed): set(playhead, timecode(p, typed, playhead))
                                }
                                ui.Button(std.say("Add a video track"), icon: .plus) {
                                    on press: add_track(p, .video)
                                }
                                ui.Button(std.say("Add a sound track"), icon: .plus) {
                                    on press: add_track(p, .audio)
                                }
                            }
                            ui.TrackTimeline(
                                label: std.say("The timeline"),
                                span: span,
                                scroll: scroll,
                                playhead: head,
                                length: total(p),
                                step: frame(p),
                            ) {
                                on scrub(t: Float): set(playhead, t).then(scrub_to(p, t))
                                on seek(t: Float): set(playhead, t).then(jump_to(p, t))
                                on zoomed(s: Float): set(span, s)
                                on scrolled(s: Float): set(scroll, s)
                                on press(t: Float): set(selected, []).then(set(playhead, t))
                                for t in ordered_tracks(p) {
                                    timeline_track(p, t, chosen, selected)
                                }
                                markers: {
                                    for k in p.markers {
                                        ui.TimelineMarker(
                                            label: k.name,
                                            at: k.t,
                                            tone: if k.palette % 6 == 0 {
                                                .accent
                                            } else if k.palette % 6 == 1 {
                                                .info
                                            } else if k.palette % 6 == 2 {
                                                .success
                                            } else if k.palette % 6 == 3 {
                                                .warning
                                            } else if k.palette % 6 == 4 {
                                                .danger
                                            } else { .ink },
                                        ) {
                                            on moved(at: Float): move_marker(p, k, at)
                                        }
                                    }
                                }
                            }
                        }
                        // The inspector: what is chosen, and what may be done to it.
                        ui.Section(std.say("Inspector")) {
                            inspector(
                                p,
                                clip,
                                media,
                                tab,
                                resolution,
                                container,
                                rate,
                                playhead,
                                live,
                            )
                        }
                    }
                    if clip == .none and media == .none {
                        stack(gap: .related, fill) {
                            ui.Section(std.say("The cut")) {
                                ui.TextInput(bind: p.name, title: true)
                                ui.NumberInput(
                                    bind: p.fps,
                                    label: std.say("Frames a second"),
                                    min: 1.0,
                                    max: 240.0,
                                    step: 1.0,
                                )
                                row(gap: .tight) {
                                    ui.NumberInput(
                                        bind: p.width,
                                        label: std.say("Width"),
                                        min: 16.0,
                                        max: 8192.0,
                                        step: 2.0,
                                    )
                                    ui.NumberInput(
                                        bind: p.height,
                                        label: std.say("Height"),
                                        min: 16.0,
                                        max: 8192.0,
                                        step: 2.0,
                                    )
                                }
                                if p.mark_in >= 0.0 or p.mark_out >= 0.0 {
                                    ui.Facts {
                                        ui.Fact(
                                            clock(render_from(p)),
                                            label: std.say("Renders from"),
                                            icon: .arrow_right,
                                        )
                                        ui.Fact(
                                            clock(render_to(p)),
                                            label: std.say("to"),
                                            icon: .arrow_left,
                                        )
                                    }
                                }
                            }
                            ui.Section(std.say("Export")) {
                                export_controls(p, resolution, container, rate)
                            }
                        }
                    }
                }
            }
        }
    } else {
        ui.EmptyState(
            std.say("No cut yet"),
            message: std.say("Start one, or open a video in the editor"),
            icon: .film,
            action: std.say("New project"),
        ) {
            on act: new_project(e)
        }
    }
}

/// What the window shows: the cut it has open, else the first there is.
fn shown_project(e: Editor) -> Option<Project> {
    if let open = e.open { .some(open) } else { e.projects.first() }
}

fn percent(p: Project) -> Int {
    if p.job_total <= 0 {
        return 0
    }
    (p.job_done * 100 / p.job_total).clamp(0, 100)
}

/// The inspector: the clip chosen, else the source, else nothing.
template view inspector(
    p: Project,
    clip: Option<Clip>,
    media: Option<Media>,
    tab: State<Text>,
    resolution: State<Text>,
    container: State<Text>,
    rate: State<Text>,
    playhead: State<Float>,
    playing: Bool,
) {
    if let c = clip {
        ui.ToggleGroup(selected: if tab == "clip" { 0 } else if tab == "look" { 1 } else { 2 }) {
            ui.Segment(std.say("Clip")) {
                on press: set(tab, "clip")
            }
            ui.Segment(std.say("Look")) {
                on press: set(tab, "look")
            }
            ui.Segment(std.say("Effects")) {
                on press: set(tab, "effects")
            }
        }
        if tab == "clip" {
            ui.TextInput(bind: c.name, placeholder: std.say("Name"))
            ui.Slider(
                bind: c.opacity,
                label: std.say("Opacity"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .opacity, v)
            }
            ui.Slider(
                bind: c.volume,
                label: std.say("Volume"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .volume, v)
            }
            ui.Slider(
                bind: c.speed_rate,
                label: std.say("Speed"),
                min: 0.05,
                max: 4.0,
                step: 0.05,
                shows_value: true,
            ) {
                on change(v): set_look(c, .speed, v)
            }
            ui.Toggle(bind: c.reverse, label: std.say("Play it backwards")) {
                on check(on): set_reverse(c, on)
            }
            ui.Slider(
                bind: c.fade_in,
                label: std.say("Fade in, seconds"),
                min: 0.0,
                max: 5.0,
                step: 0.1,
                shows_value: true,
            ) {
                on change(v): set_look(c, .fade_in, v)
            }
            ui.Slider(
                bind: c.fade_out,
                label: std.say("Fade out, seconds"),
                min: 0.0,
                max: 5.0,
                step: 0.1,
                shows_value: true,
            ) {
                on change(v): set_look(c, .fade_out, v)
            }
            row(gap: .tight, align: .center, wrap: true) {
                ui.MenuButton(std.say("Transition"), icon: .arrow_right) {
                    ui.MenuItem(std.say("Cut"), checked: c.transition == "") {
                        on choose: set_transition(c, "", 0.0)
                    }
                    ui.MenuItem(std.say("Dissolve"), checked: c.transition == "dissolve") {
                        on choose: set_transition(c, "dissolve", 0.6)
                    }
                    ui.MenuItem(std.say("Fade in"), checked: c.transition == "fade") {
                        on choose: set_transition(c, "fade", 0.6)
                    }
                    ui.MenuItem(std.say("Dip to black"), checked: c.transition == "dip") {
                        on choose: set_transition(c, "dip", 0.6)
                    }
                    ui.MenuItem(std.say("Wipe"), checked: c.transition == "wipe") {
                        on choose: set_transition(c, "wipe", 0.6)
                    }
                }
                if c.transition != "" {
                    ui.Slider(
                        bind: c.transition_duration,
                        label: std.say("Transition, seconds"),
                        min: 0.05,
                        max: 5.0,
                        step: 0.05,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .transition_duration, v)
                    }
                }
                ui.Button(std.say("Use its format"), icon: .crop) {
                    on press: adopt_format(p, c.media)
                }
                if c.link == "" {
                    ui.Button(std.say("Split the sound"), icon: .music) {
                        on press: split_audio(p, c)
                    }
                } else if c.link_follows {
                    ui.Button(std.say("Unlink"), icon: .unlink) {
                        on press: unlink_clip(c)
                    }
                }
                ui.Button(std.say("Marker here"), icon: .flag) {
                    on press: add_marker(p, playhead)
                }
            }
            if let s = editor.playing {
                if let out = s.outputs.find(x -> x.device != "") {
                    ui.LevelMeter(
                        source: out.device,
                        output: true,
                        running: playing,
                        label: std.say("Sound level"),
                    )
                }
            }
        } else if tab == "look" {
            ui.Slider(
                bind: c.brightness,
                label: std.say("Brightness"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .brightness, v)
            }
            ui.Slider(
                bind: c.contrast,
                label: std.say("Contrast"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .contrast, v)
            }
            ui.Slider(
                bind: c.saturation,
                label: std.say("Saturation"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .saturation, v)
            }
            ui.Slider(
                bind: c.scale_x,
                label: std.say("Width"),
                min: 0.1,
                max: 3.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .scale_x, v)
            }
            ui.Slider(
                bind: c.scale_y,
                label: std.say("Height"),
                min: 0.1,
                max: 3.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .scale_y, v)
            }
            ui.Slider(
                bind: c.pos_x,
                label: std.say("Across"),
                min: -1.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .pos_x, v)
            }
            ui.Slider(
                bind: c.pos_y,
                label: std.say("Up and down"),
                min: -1.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_look(c, .pos_y, v)
            }
            ui.Slider(
                bind: c.rotation,
                label: std.say("Turn, degrees"),
                min: -180.0,
                max: 180.0,
                step: 1.0,
                shows_value: true,
            ) {
                on change(v): set_look(c, .rotation, v)
            }
            // Where a clip is pinned: the corner, edge or middle that scaling and turning go
            // around. Nine places, chosen as a grid, because anything else is a picture drawn
            // off its own frame.
            ui.Section(std.say("Anchor")) {
                ui.Text(
                    std.say("Width, height and turning go around this point of the picture"),
                    role: .caption,
                    secondary: true,
                )
                ui.ToggleGroup(selected: anchor_row(c, 0)) {
                    ui.Segment(std.say("Top left")) {
                        on press: set_anchor(c, 0.0, 0.0)
                    }
                    ui.Segment(std.say("Top")) {
                        on press: set_anchor(c, 0.5, 0.0)
                    }
                    ui.Segment(std.say("Top right")) {
                        on press: set_anchor(c, 1.0, 0.0)
                    }
                }
                ui.ToggleGroup(selected: anchor_row(c, 1)) {
                    ui.Segment(std.say("Left")) {
                        on press: set_anchor(c, 0.0, 0.5)
                    }
                    ui.Segment(std.say("Centre")) {
                        on press: set_anchor(c, 0.5, 0.5)
                    }
                    ui.Segment(std.say("Right")) {
                        on press: set_anchor(c, 1.0, 0.5)
                    }
                }
                ui.ToggleGroup(selected: anchor_row(c, 2)) {
                    ui.Segment(std.say("Bottom left")) {
                        on press: set_anchor(c, 0.0, 1.0)
                    }
                    ui.Segment(std.say("Bottom")) {
                        on press: set_anchor(c, 0.5, 1.0)
                    }
                    ui.Segment(std.say("Bottom right")) {
                        on press: set_anchor(c, 1.0, 1.0)
                    }
                }
            }
            ui.Section(std.say("Crop")) {
                ui.Slider(
                    bind: c.crop_left,
                    label: std.say("From the left"),
                    min: 0.0,
                    max: 0.9,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .crop_left, v)
                }
                ui.Slider(
                    bind: c.crop_right,
                    label: std.say("From the right"),
                    min: 0.0,
                    max: 0.9,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .crop_right, v)
                }
                ui.Slider(
                    bind: c.crop_top,
                    label: std.say("From the top"),
                    min: 0.0,
                    max: 0.9,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .crop_top, v)
                }
                ui.Slider(
                    bind: c.crop_bottom,
                    label: std.say("From the bottom"),
                    min: 0.0,
                    max: 0.9,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .crop_bottom, v)
                }
            }
            ui.Toggle(bind: c.key_enabled, label: std.say("Remove a colour"))
            row(gap: .tight, align: .center, wrap: true) {
                ui.MenuButton(std.say("Screen colour"), icon: .palette) {
                    ui.MenuItem(
                        std.say("Green"),
                        checked: c.key_green > c.key_red and c.key_green > c.key_blue,
                    ) {
                        on choose: key_colour(c, 0.06, 0.72, 0.2)
                    }
                    ui.MenuItem(
                        std.say("Blue"),
                        checked: c.key_blue > c.key_green and c.key_blue > c.key_red,
                    ) {
                        on choose: key_colour(c, 0.06, 0.22, 0.72)
                    }
                    ui.MenuItem(
                        std.say("Magenta"),
                        checked: c.key_red > c.key_green and c.key_blue > c.key_green,
                    ) {
                        on choose: key_colour(c, 0.72, 0.06, 0.55)
                    }
                }
                ui.Button(std.say("Pick from the frame"), icon: .pipette) {
                    on press: set(tab, "pick")
                }
            }
            if tab == "pick" {
                ui.Text(
                    std.say("Drag across the picture where the colour is"),
                    role: .caption,
                    secondary: true,
                )
                ui.Drawing(
                    [ui.Shape.rect(x: 0.0, y: 0.0, w: 1.0, h: 1.0, fill: .some(.track))],
                    meaning: .decoration,
                    label: .some(std.say("Pick a colour from the frame")),
                    height: .some(56.0),
                    motion: .still,
                ) {
                    on drag_end(x, y): sample_key(p, c, playhead, x, y).then(set(tab, "look"))
                }
            }
            if c.key_enabled {
                ui.Slider(
                    bind: c.key_tolerance,
                    label: std.say("How much of it"),
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .key_tolerance, v)
                }
                ui.Slider(
                    bind: c.key_softness,
                    label: std.say("Soft edge"),
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .key_softness, v)
                }
                ui.Slider(
                    bind: c.key_spill,
                    label: std.say("Take the tint away"),
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    shows_value: true,
                ) {
                    on change(v): set_look(c, .key_spill, v)
                }
            }
            // A shape that limits where the clip shows, and how soft its edge is.
            ui.Section(std.say("Mask")) {
                ui.MenuButton(std.say("Shape"), icon: .layers) {
                    ui.MenuItem(std.say("Off"), checked: c.mask_shape == "") {
                        on choose: set_mask(c, "off")
                    }
                    ui.MenuItem(std.say("Rectangle"), checked: c.mask_shape == "rect") {
                        on choose: set_mask(c, "rect")
                    }
                    ui.MenuItem(std.say("Ellipse"), checked: c.mask_shape == "ellipse") {
                        on choose: set_mask(c, "ellipse")
                    }
                }
                if c.mask_shape != "" {
                    ui.Toggle(shown: c.mask_invert, label: std.say("Keep the outside")) {
                        on check(on): invert_mask(c, on)
                    }
                    ui.Slider(
                        bind: c.mask_x,
                        label: std.say("Centre across"),
                        min: -1.0,
                        max: 1.0,
                        step: 0.01,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .mask_x, v)
                    }
                    ui.Slider(
                        bind: c.mask_y,
                        label: std.say("Centre up and down"),
                        min: -1.0,
                        max: 1.0,
                        step: 0.01,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .mask_y, v)
                    }
                    ui.Slider(
                        bind: c.mask_w,
                        label: std.say("Mask width"),
                        min: 0.01,
                        max: 2.0,
                        step: 0.01,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .mask_w, v)
                    }
                    ui.Slider(
                        bind: c.mask_h,
                        label: std.say("Mask height"),
                        min: 0.01,
                        max: 2.0,
                        step: 0.01,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .mask_h, v)
                    }
                    ui.Slider(
                        bind: c.mask_rotation,
                        label: std.say("Mask turn, degrees"),
                        min: -180.0,
                        max: 180.0,
                        step: 1.0,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .mask_rotation, v)
                    }
                    ui.Slider(
                        bind: c.mask_feather,
                        label: std.say("Mask feather"),
                        min: 0.0,
                        max: 1.0,
                        step: 0.01,
                        shows_value: true,
                    ) {
                        on change(v): set_look(c, .mask_feather, v)
                    }
                }
            }
            // Where the clip's own values change.
            ui.Section(std.say("Keys")) {
                ui.MenuButton(std.say("Key a field here"), icon: .circle_dot) {
                    ui.MenuItem(std.say("Opacity")) {
                        on choose: key_at(c, playhead, "opacity", "linear")
                    }
                    ui.MenuItem(std.say("Volume")) {
                        on choose: key_at(c, playhead, "volume", "linear")
                    }
                    ui.MenuItem(std.say("Width")) {
                        on choose: key_at(c, playhead, "scale_x", "linear")
                    }
                    ui.MenuItem(std.say("Across")) {
                        on choose: key_at(c, playhead, "pos_x", "linear")
                    }
                    ui.MenuItem(std.say("Up and down")) {
                        on choose: key_at(c, playhead, "pos_y", "linear")
                    }
                    ui.MenuItem(std.say("Turn")) {
                        on choose: key_at(c, playhead, "rotation", "linear")
                    }
                }
                if c.keys.is_empty() {
                    ui.Text(
                        std.say("This clip holds one value all the way through"),
                        role: .caption,
                        secondary: true,
                    )
                }
                for k in c.keys.sort_by(x -> x.t) {
                    row(gap: .tight, align: .center) {
                        ui.Text(k.param, role: .body)
                        ui.Text(clock(k.t), role: .caption, numeric: true)
                        ui.MenuButton(interp_words(k.interp), icon: .activity) {
                            ui.MenuItem(std.say("Linear"), checked: k.interp == "linear") {
                                on choose: set_interp(k, "linear")
                            }
                            ui.MenuItem(std.say("Hold"), checked: k.interp == "hold") {
                                on choose: set_interp(k, "hold")
                            }
                            ui.MenuItem(std.say("Ease"), checked: k.interp == "ease") {
                                on choose: set_interp(k, "ease")
                            }
                        }
                        ui.Button(
                            "",
                            icon: .trash_2,
                            icon_only: true,
                            accessible_name: std.say("Remove the key"),
                        ) {
                            on press: unkey(c, k)
                        }
                    }
                }
            }
            // The clip's frames, as the engine drew them.
            ui.Section(std.say("Frames")) {
                if c.strips.is_empty() {
                    ui.Button(std.say("Draw the filmstrip"), icon: .film) {
                        on press: filmstrip(p, c)
                    }
                } else {
                    row(gap: .tight, wrap: true) {
                        for strip in c.strips {
                            if strip != "" {
                                ui.Image(
                                    strip,
                                    alt: c.name,
                                    fit: .cover,
                                    aspect: 16.0 / 9.0,
                                    min: 4rem,
                                    max: 8rem,
                                )
                            }
                        }
                    }
                }
            }
        } else {
            ui.MenuButton(std.say("Add an effect"), icon: .plus) {
                ui.MenuItem(std.say("Grayscale")) {
                    on choose: add_effect(c, "Grayscale")
                }
                ui.MenuItem(std.say("Sepia")) {
                    on choose: add_effect(c, "Sepia")
                }
                ui.MenuItem(std.say("Invert")) {
                    on choose: add_effect(c, "Invert")
                }
                ui.MenuItem(std.say("Warm")) {
                    on choose: add_effect(c, "Warm")
                }
                ui.MenuItem(std.say("Cool")) {
                    on choose: add_effect(c, "Cool")
                }
                ui.MenuItem(std.say("Vignette")) {
                    on choose: add_effect(c, "Vignette")
                }
            }
            if c.effects.is_empty() {
                ui.Text(
                    std.say("This clip has no effect of its own"),
                    role: .caption,
                    secondary: true,
                )
            }
            for effect in c.effects {
                row(gap: .tight, align: .center) {
                    ui.Text(effect.form, role: .body)
                    ui.Slider(
                        bind: effect.amount,
                        label: std.say("Amount"),
                        min: 0.0,
                        max: 1.0,
                        step: 0.01,
                        shows_value: true,
                    ) {
                        on change(v): set_effect(c, effect, v)
                    }
                    ui.Button(
                        "",
                        icon: .chevron_up,
                        icon_only: true,
                        accessible_name: std.say("Move {effect.form} earlier"),
                    ) {
                        on press: move_effect(c, effect, true)
                    }
                    ui.Button(
                        "",
                        icon: .chevron_down,
                        icon_only: true,
                        accessible_name: std.say("Move {effect.form} later"),
                    ) {
                        on press: move_effect(c, effect, false)
                    }
                    ui.Button(
                        "",
                        icon: .trash_2,
                        icon_only: true,
                        accessible_name: std.say("Remove {effect.form}"),
                    ) {
                        on press: remove_effect(c, effect)
                    }
                }
            }
            if not c.effects.is_empty() {
                ui.Button(std.say("Remove every effect"), icon: .x) {
                    on press: clear_effects(c)
                }
            }
        }
    } else if let m = media {
        ui.TextInput(bind: m.name, title: true)
        ui.Text(form_words(m.form), role: .caption, secondary: true)
        if m.form == .title {
            ui.TextInput(bind: m.words, placeholder: std.say("Words"), lines: 3) {
                on submit(words): set_words(m, words)
            }
            ui.Slider(
                bind: m.size,
                label: std.say("Size"),
                min: 0.02,
                max: 0.5,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_title_look(m, .size, v)
            }
            ui.Slider(
                bind: m.red,
                label: std.say("Red"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_title_look(m, .red, v)
            }
            ui.Slider(
                bind: m.green,
                label: std.say("Green"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_title_look(m, .green, v)
            }
            ui.Slider(
                bind: m.blue,
                label: std.say("Blue"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_title_look(m, .blue, v)
            }
            ui.Slider(
                bind: m.backdrop,
                label: std.say("Backing plate"),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(v): set_title_look(m, .backdrop, v)
            }
            ui.Toggle(bind: m.bold, label: std.say("Bold")) {
                on check(on): set_title_style(m, on, m.italic)
            }
            ui.Toggle(bind: m.italic, label: std.say("Italic")) {
                on check(style): set_title_style(m, m.bold, style)
            }
            ui.MenuButton(std.say("Lines"), icon: .align_justify) {
                ui.MenuItem(std.say("Left"), checked: m.align == .left) {
                    on choose: set_align(m, .left)
                }
                ui.MenuItem(std.say("Centre"), checked: m.align == .centre) {
                    on choose: set_align(m, .centre)
                }
                ui.MenuItem(std.say("Right"), checked: m.align == .right) {
                    on choose: set_align(m, .right)
                }
            }
        } else {
            if m.missing {
                ui.Text(
                    std.say(
                        "This device cannot read the source: it is not here, or another device holds it. Relink it to a file of its own.",
                    ),
                    role: .caption,
                    secondary: true,
                )
            }
            row(gap: .tight, align: .center, wrap: true) {
                ui.Button(std.say("Place at the playhead"), icon: .plus) {
                    on press: place_media(p, m, playhead)
                }
                ui.Button(std.say("Use its format"), icon: .crop) {
                    on press: adopt_format(p, m)
                }
                ui.Button(std.say("Relink the source"), icon: .link) {
                    on press: file.choose(
                        relink(p, m),
                        accept: [
                            "video/mp4",
                            "video/quicktime",
                            "video/x-matroska",
                            "video/webm",
                            "audio/mpeg",
                            "audio/wav",
                            "image/png",
                            "image/jpeg",
                        ],
                    )
                }
                ui.Button(std.say("Check this device"), icon: .refresh) {
                    on press: refresh_source(p, m)
                }
                ui.Button(std.say("Delete the source"), icon: .trash_2) {
                    on press: delete_media(p, m)
                }
            }
            ui.Text(clock(m.duration), role: .caption, secondary: true, numeric: true)
        }
    } else {
        ui.Text(std.say("Choose a clip or a source"), role: .caption, secondary: true)
    }
    // The mix: every track's level, pan and switches, whichever clip is chosen.
    ui.Section(std.say("Tracks")) {
        for t in ordered_tracks(p) {
            stack(gap: .tight) {
                row(gap: .tight, align: .center) {
                    ui.TextInput(bind: t.name, fill)
                    ui.Toggle(shown: t.muted, label: std.say("Mute")) {
                        on check(on): set_track_muted(t, on)
                    }
                    ui.Toggle(shown: t.solo, label: std.say("Solo")) {
                        on check(on): set_track_solo(t, on)
                    }
                    ui.Toggle(shown: t.locked, label: std.say("Lock")) {
                        on check(on): set_track_locked(t, on)
                    }
                    ui.Button(
                        "",
                        icon: .trash_2,
                        icon_only: true,
                        accessible_name: std.say("Delete the track {t.name}"),
                    ) {
                        on press: delete_track(p, t)
                    }
                }
                ui.Slider(
                    bind: t.gain_db,
                    label: std.say("Level, dB"),
                    min: -60.0,
                    max: 12.0,
                    step: 0.5,
                    shows_value: true,
                ) {
                    on change(v): set_track_gain(t, v)
                }
                ui.Slider(
                    bind: t.pan,
                    label: std.say("Pan"),
                    min: -1.0,
                    max: 1.0,
                    step: 0.05,
                    shows_value: true,
                ) {
                    on change(v): set_track_pan(t, v)
                }
                row(gap: .tight) {
                    ui.Button(std.say("Reset the level and pan"), icon: .rotate_ccw) {
                        on press: reset_mix(t)
                    }
                }
            }
        }
    }
    // The cut's marks, and what each says.
    if not p.markers.is_empty() {
        ui.Section(std.say("Markers")) {
            for k in p.markers.sort_by(x -> x.t) {
                row(gap: .tight, align: .center) {
                    ui.Text(k.name, role: .body)
                    ui.Text(clock(k.t), role: .caption, numeric: true)
                    ui.Button(
                        "",
                        icon: .trash_2,
                        icon_only: true,
                        accessible_name: std.say("Remove the mark"),
                    ) {
                        on press: remove_marker(p, k)
                    }
                }
                ui.TextInput(bind: k.note, placeholder: std.say("What it marks"))
            }
        }
    }
}

/// The export controls: the shape, the container and the rate, then the render.
template view export_controls(
    p: Project,
    resolution: State<Text>,
    container: State<Text>,
    rate: State<Text>,
) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.Dropdown(options: ["720", "1080", "2160"], bind: resolution, caption: std.say("Size"))
        ui.Dropdown(options: ["mp4", "webm", "mov"], bind: container, caption: std.say("Format"))
        ui.Dropdown(options: ["24", "30", "60"], bind: rate, caption: std.say("Frames"))
    }
    let missing = missing_sources(p)
    if not missing.is_empty() {
        ui.Banner(
            std.say("This film needs sources this device does not have"),
            message: std.say(
                "{missing.join(", ")}: relink it in the bin, or export on the device that has it",
            ),
            kind: .warning,
        )
    }
    row(gap: .tight, align: .center, wrap: true) {
        if p.job != "" {
            ui.Button(std.say("Stop the render"), icon: .x) {
                on press: cancel_render(p)
            }
        } else {
            ui.Button(std.say("Render the film"), icon: .play) {
                on press: export_cut(p, resolution, container, rate.to_int() ?? 30)
            }
        }
        if p.exported != "" {
            ui.Button(std.say("Save the film"), icon: .download) {
                on press: file.save(export_file(p), name: export_name(p, p.format))
            }
        }
    }
    if p.exported != "" {
        ui.Text(export_name(p, p.format), role: .caption)
    }
}

/// A cut opened on its own: what it is, and the way into the editor.
view studio(p: Project) {
    let e = root Editor
    let clips = p.clips.len()
    std.page(p) {
        header: {
            ui.Button(std.say("Open in the editor"), icon: .film) {
                on press: open_project(e, p)
            }
        }
        content: {
            ui.Text(p.name, role: .title)
            ui.Facts {
                ui.Fact(clock(total(p)), icon: .clock)
                ui.Fact(std.say("{p.width} × {p.height}"), icon: .image)
                ui.Fact(std.say("{clips} clips"), icon: .film)
            }
            ui.EmptyState(
                std.say("This cut opens in the Video Editor"),
                message: std.say(
                    "Its preview, its timeline and its inspector are in the app's window",
                ),
                icon: .film,
            )
        }
    }
}

view project_row(p: Project) {
    ui.ListRow(p.name, item: p, icon: .film, meta: clock(total(p)))
}

view media_row(m: Media) {
    ui.ListRow(
        m.name,
        item: m,
        icon: if m.form == .audio {
            .music
        } else if m.form == .image { .image } else if m.form == .title { .pencil } else { .film },
        meta: if m.form == .title { std.say("Title") } else { clock(m.duration) },
    )
}

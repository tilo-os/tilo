//! The Video Editor's engine, as a package reaches it: `engine.call` and `engine.start` answered
//! for `so.tilo.video-editor` by the editing and picture engines on this device.
//!
//! The app runs sandboxed with no decoder and no renderer, so it asks by name. A request carries
//! the cut as JSON the app wrote; the answer is JSON the app reads back into its own records. A
//! still, a waveform and a probe answer now; a render starts as a job whose progress is asked by
//! its id and which can be stopped.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

use serde_json::json;
use tilo_content::ContentStore;
use tilo_resource::{Keyed, Ordered};
use tilolang_tilo::engine::{self, JobOp, JobSink, Needs, Provider, Running, State};
use video_edit::model;
use video_edit::param::{ClipAnimation, Interp, Param};
use video_picture::export::{Container, ExportSettings, FrameRate, Resolution};

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.video-editor";

/// A render is a job: it runs in the app's own process, whose content store keeps the finished
/// file as it ends.
const JOBS: &[JobOp] = &[JobOp::new("render", Needs::APP_STATE)];

/// Video Editor's engine, keeping what it makes in `store`.
struct Engine {
    store: ContentStore,
}

/// Register Video Editor's engine over the app's content store `store`.
pub fn register(store: &ContentStore) {
    engine::register(
        APP,
        Arc::new(Engine {
            store: store.clone(),
        }),
    );
}

impl Provider for Engine {
    fn jobs(&self) -> &'static [JobOp] {
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        let store = &self.store;
        match op {
            "probe" => probe(store, input),
            "still" => still(store, input),
            "thumb" => thumb(store, input),
            "waveform" => waveform(store, input),
            "sample" => sample(store, input),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        match op {
            "render" => render(&self.store, job, input, moved),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn cancel(&self, job: &str) -> bool {
        let Some((render, _)) = running().get(job) else {
            return false;
        };
        render.store(true, Ordering::Relaxed);
        running().end(job, State::Cancelled, json!({ "url": "", "error": "" }));
        true
    }
}

/// The renders running now, each by what stops it.
fn running() -> &'static Running<AtomicBool> {
    static RUNNING: OnceLock<Running<AtomicBool>> = OnceLock::new();
    RUNNING.get_or_init(Running::default)
}

/// The media type of a render written to `path`.
fn media_of(path: &Path) -> &'static str {
    match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        _ => "video/mp4",
    }
}

/// The still the app asked for at `at`, kept as a picture in the content store.
fn still(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        project: String,
        at: f64,
        width: u32,
        height: u32,
    }
    let ask: Ask = reads(input)?;
    let project = reads::<Shot>(&ask.project)?.into_project(Some(store))?;
    let settings = video_picture::render::AgentFrameSettings {
        at: ask.at,
        width: ask.width.clamp(16, 4096),
        height: ask.height.clamp(16, 4096),
    };
    let png = video_picture::render::render_still(&project, &settings)?;
    let kept = crate::files::keep(store, "frame.png", "image/png", &png)?;
    Ok(serde_json::json!({ "url": format!("tilo-blob://{}", kept.hash) }).to_string())
}

/// One source's picture at `at`, kept as a picture in the content store: a bin swatch or a
/// filmstrip frame.
fn thumb(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        at: f64,
        width: u32,
        height: u32,
    }
    let ask: Ask = reads(input)?;
    let mut project = model::Project::new("thumb");
    project.set_frame_size(ask.width.clamp(16, 4096), ask.height.clamp(16, 4096));
    let path = media_path(store, &ask.url)?;
    let probed = video_edit::probe::probe_file(&path).map_err(|e| format!("{e:?}"))?;
    let media = model::MediaItem {
        id: uuid::Uuid::from_u128(1),
        name: "still".to_owned(),
        kind: probed.kind,
        duration: probed.duration,
        width: probed.width,
        height: probed.height,
        fps: probed.fps,
        has_audio: probed.has_audio,
        path: path.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let track = model::Track {
        id: uuid::Uuid::from_u128(2),
        kind: model::TrackKind::Video,
        index: 1,
        ..Default::default()
    };
    let end = if probed.kind == model::MediaKind::Image {
        video_edit::probe::DEFAULT_IMAGE_DURATION
    } else {
        probed.duration
    };
    let clip = model::Clip {
        id: uuid::Uuid::from_u128(3),
        track: track.id,
        media: media.id,
        start: 0.0,
        in_point: 0.0,
        out_point: end,
        ..Default::default()
    };
    project.media.push(media);
    project.tracks.push(track);
    project.clips.push(clip);
    let settings = video_picture::render::AgentFrameSettings {
        at: ask.at.max(0.0),
        width: ask.width.clamp(16, 4096),
        height: ask.height.clamp(16, 4096),
    };
    let png = video_picture::render::render_still(&project, &settings)?;
    let kept = crate::files::keep(store, "thumb.png", "image/png", &png)?;
    Ok(serde_json::json!({ "url": format!("tilo-blob://{}", kept.hash) }).to_string())
}

/// What the file at `url` really is.
fn probe(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
    }
    let ask: Ask = reads(input)?;
    let path = media_path(store, &ask.url)?;
    let probed = video_edit::probe::probe_file(&path).map_err(|e| format!("{e:?}"))?;
    Ok(serde_json::json!({
        "kind": probed.kind.as_str(),
        "duration": probed.duration,
        "width": probed.width,
        "height": probed.height,
        "fps": probed.fps,
        "has_audio": probed.has_audio,
        "codec": probed.codec,
    })
    .to_string())
}

/// The source's peaks, as fractions of full scale.
fn waveform(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        buckets: Option<usize>,
    }
    let ask: Ask = reads(input)?;
    let path = media_path(store, &ask.url)?;
    let buckets = ask.buckets.unwrap_or(512).clamp(16, 4096);
    let peaks = video_edit::waveform::peaks_from_file(&path, buckets)
        .ok_or_else(|| "no samples could be read from that source".to_owned())?;
    Ok(serde_json::json!({ "peaks": peaks }).to_string())
}

/// The colour of the cut's frame at (x, y), a fraction of its box each.
fn sample(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        project: String,
        at: f64,
        x: f64,
        y: f64,
    }
    let ask: Ask = reads(input)?;
    let project = reads::<Shot>(&ask.project)?.into_project(Some(store))?;
    let (width, height) = (480_u32, 270_u32);
    let mut compositor = video_picture::gpu::HeadlessCompositor::new()
        .map_err(|error| format!("this device cannot draw the frame: {error}"))?;
    let mut sources =
        video_picture::gpu::MediaFrameSource::new(&project, video_picture::gpu::Fetch::Exact)
            .with_timeout(std::time::Duration::from_secs(2))
            .strict();
    let rgba = compositor.frame(&project, ask.at, width, height, &mut sources);
    let x = axis_index(ask.x, width);
    let y = axis_index(ask.y, height);
    let at = y
        .saturating_mul(usize::try_from(width).unwrap_or(usize::MAX))
        .saturating_add(x)
        .saturating_mul(4);
    let channel = |i: usize| f64::from(*rgba.get(at.saturating_add(i)).unwrap_or(&0)) / 255.0;
    Ok(
        serde_json::json!({ "red": channel(0), "green": channel(1), "blue": channel(2) })
            .to_string(),
    )
}

/// The index along an axis `extent` cells long that `fraction` of it names, rounded and kept on
/// the axis.
fn axis_index(fraction: f64, extent: u32) -> usize {
    let last = extent.saturating_sub(1);
    let at = ui_core::num::usize_of_f64((fraction.clamp(0.0, 1.0) * f64::from(last)).round());
    at.min(usize::try_from(last).unwrap_or(usize::MAX))
}

/// Start a cut's render as the job `job`: its frames done of how many are told as it goes, and
/// the finished file is kept in `store` as it ends.
fn render(store: &ContentStore, job: &str, input: &str, moved: JobSink) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Ask {
        project: String,
        resolution: Option<String>,
        container: Option<String>,
        fps: Option<i64>,
        /// The container the finished file takes: `mp4`, `webm` or `mov`.
        name: Option<String>,
    }
    let ask: Ask = reads(input)?;
    let project = reads::<Shot>(&ask.project)?.into_project(Some(store))?;
    let resolution = match ask.resolution.as_deref() {
        Some("720") => Resolution::P720,
        Some("2160" | "4k") => Resolution::P2160,
        _ => Resolution::P1080,
    };
    let container = match ask.container.as_deref() {
        Some("webm") => Container::WebM,
        Some("mov") => Container::Mov,
        _ => Container::Mp4,
    };
    let frame_rate = match ask.fps {
        Some(24) => FrameRate::F24,
        Some(60) => FrameRate::F60,
        _ => FrameRate::F30,
    };
    let settings = ExportSettings {
        resolution,
        container,
        frame_rate,
    };
    let extension = match container {
        Container::WebM => "webm",
        Container::Mov => "mov",
        Container::Mp4 => "mp4",
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = ask.name.unwrap_or_else(|| format!("cut-{id}.{extension}"));
    let path = scratch_dir().join(format!("render-{id}.{extension}"));
    let cancel = Arc::new(AtomicBool::new(false));
    moved.moved(
        State::Running,
        json!({ "done": 0, "total": 0, "url": "", "error": "" }),
    );
    running().keep(job, Arc::clone(&cancel), moved.clone());
    let (named, store) = (job.to_owned(), store.clone());
    let spawned = std::thread::Builder::new()
        .name("video-editor-render".to_owned())
        .spawn(move || {
            let total = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let counted = Arc::clone(&total);
            let outcome = video_picture::export::render_to(
                &project,
                settings,
                &path,
                cancel,
                move |done, of| {
                    counted.store(of, Ordering::Relaxed);
                    moved.moved(
                        State::Running,
                        json!({ "done": done, "total": of, "url": "", "error": "" }),
                    );
                },
            );
            let total = total.load(Ordering::Relaxed);
            let kept = outcome.and_then(|_| {
                let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                crate::files::keep(&store, &name, media_of(&path), &bytes)
            });
            let _ = std::fs::remove_file(&path);
            match kept {
                Ok(kept) => running().end(
                    &named,
                    State::Done,
                    json!({
                        "done": total,
                        "total": total,
                        "url": format!("tilo-blob://{}", kept.hash),
                        "error": "",
                    }),
                ),
                Err(error) => running().end(
                    &named,
                    State::Failed,
                    json!({ "done": 0, "total": total, "url": "", "error": error }),
                ),
            }
        });
    if spawned.is_err() {
        running().forget(job);
        return Err("could not start the render".to_owned());
    }
    Ok(())
}

/// Where a render writes its file before the answer keeps it: one directory per process.
fn scratch_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-video-engine-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
    .clone()
}

fn reads<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the request does not read: {error}"))
}

/// A source's bytes as a file this device can decode: what a `tilo-blob://` address names,
/// laid out once, or a path already here.
fn media_path(store: &ContentStore, url: &str) -> Result<PathBuf, String> {
    let hash = url.strip_prefix("tilo-blob://");
    let Some(hash) = hash else {
        let path = Path::new(url);
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        return Err(format!("`{url}` is not a file on this device"));
    };
    let dir = scratch_dir();
    let file = dir.join(format!("source-{hash}"));
    if file.exists() {
        return Ok(file);
    }
    let bytes = crate::files::read(store, hash)
        .ok_or_else(|| "that source's bytes are not on this device yet".to_owned())?;
    std::fs::write(&file, bytes)
        .map_err(|error| format!("the source cannot be laid out: {error}"))?;
    Ok(file)
}

/// The cut as the app writes it: plain records, so the app and the engine agree on one JSON and
/// neither depends on the other's field spells beyond these names.
#[derive(Deserialize)]
struct Shot {
    #[serde(default)]
    name: String,
    #[serde(default = "fps_default")]
    fps: f64,
    #[serde(default = "width_default")]
    width: u32,
    #[serde(default = "height_default")]
    height: u32,
    #[serde(default = "nothing")]
    mark_in: f64,
    #[serde(default = "nothing")]
    mark_out: f64,
    #[serde(default)]
    media: Vec<String>,
    #[serde(default)]
    tracks: Vec<String>,
    #[serde(default)]
    clips: Vec<String>,
    #[serde(default)]
    markers: Vec<String>,
}

fn fps_default() -> f64 {
    30.0
}

/// A mark that is not set.
fn nothing() -> f64 {
    -1.0
}

fn width_default() -> u32 {
    1920
}

fn height_default() -> u32 {
    1080
}

#[derive(Deserialize)]
struct ShotMedia {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default = "kind_video")]
    kind: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default = "fps_default")]
    fps: f64,
    #[serde(default)]
    palette: u8,
    #[serde(default)]
    path: String,
    #[serde(default)]
    has_audio: bool,
    #[serde(default)]
    content: String,
    #[serde(default = "text_size")]
    size: f64,
    #[serde(default = "text_color")]
    color: [f64; 4],
    #[serde(default)]
    align: String,
    #[serde(default)]
    font: String,
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    backdrop: f64,
}

fn kind_video() -> String {
    "video".to_owned()
}

fn text_size() -> f64 {
    0.12
}

fn text_color() -> [f64; 4] {
    [1.0, 1.0, 1.0, 1.0]
}

#[derive(Deserialize)]
struct ShotTrack {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default = "kind_video")]
    kind: String,
    #[serde(default = "one")]
    index: u32,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    solo: bool,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    gain_db: f64,
    #[serde(default)]
    pan: f64,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct ShotClip {
    id: String,
    track: String,
    media: String,
    /// The switches a clip's own motion and sound carry.
    #[serde(flatten)]
    playback: ClipPlayback,
    /// The switches a clip's key and mask carry.
    #[serde(flatten)]
    key: ClipKey,
    #[serde(default)]
    start: f64,
    #[serde(default)]
    in_point: f64,
    #[serde(default)]
    out_point: f64,
    #[serde(default = "one_f")]
    opacity: f64,
    #[serde(default = "one_f")]
    volume: f64,
    #[serde(default)]
    transition: Option<String>,
    #[serde(default)]
    transition_duration: f64,
    #[serde(default)]
    fade_in: f64,
    #[serde(default)]
    fade_out: f64,
    #[serde(default)]
    palette: u8,
    #[serde(default)]
    speed_rate: Option<f64>,
    #[serde(default = "half")]
    brightness: f64,
    #[serde(default = "half")]
    contrast: f64,
    #[serde(default = "half")]
    saturation: f64,
    #[serde(default)]
    scale_x: Option<f64>,
    #[serde(default)]
    scale_y: Option<f64>,
    #[serde(default)]
    pos_x: Option<f64>,
    #[serde(default)]
    pos_y: Option<f64>,
    #[serde(default)]
    rotation: Option<f64>,
    #[serde(default)]
    crop_left: Option<f64>,
    #[serde(default)]
    crop_right: Option<f64>,
    #[serde(default)]
    crop_top: Option<f64>,
    #[serde(default)]
    crop_bottom: Option<f64>,
    #[serde(default)]
    effects: Vec<String>,
    /// One JSON text a key: its param, clip-local seconds, value and interp.
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default = "key_color")]
    key_color: [f64; 3],
    #[serde(default = "key_tolerance")]
    key_tolerance: f64,
    #[serde(default = "key_softness")]
    key_softness: f64,
    #[serde(default = "key_spill")]
    key_spill: f64,
    #[serde(default)]
    mask_shape: Option<String>,
    #[serde(default)]
    mask_x: Option<f64>,
    #[serde(default)]
    mask_y: Option<f64>,
    #[serde(default)]
    mask_w: Option<f64>,
    #[serde(default)]
    mask_h: Option<f64>,
    #[serde(default)]
    mask_rotation: Option<f64>,
    #[serde(default)]
    mask_feather: Option<f64>,
    #[serde(default)]
    link: String,
    #[serde(default = "yes")]
    link_follows: bool,
}

/// The switches a clip's own motion and sound carry, beside its numbers.
#[derive(Deserialize)]
struct ClipPlayback {
    #[serde(default)]
    speed_reverse: bool,
    #[serde(default)]
    sound_split: bool,
}

/// The switches a clip's key and mask carry, beside their numbers.
#[derive(Deserialize)]
struct ClipKey {
    #[serde(default)]
    key_enabled: bool,
    #[serde(default)]
    mask_invert: bool,
}

fn one_f() -> f64 {
    1.0
}

fn half() -> f64 {
    0.5
}

fn yes() -> bool {
    true
}

fn key_color() -> [f64; 3] {
    [0.06, 0.72, 0.20]
}

fn key_tolerance() -> f64 {
    0.22
}

fn key_softness() -> f64 {
    0.10
}

fn key_spill() -> f64 {
    0.5
}

#[derive(Deserialize)]
struct ShotKey {
    param: String,
    #[serde(default)]
    t: f64,
    #[serde(default)]
    v: f64,
    #[serde(default)]
    interp: Option<String>,
}

#[derive(Deserialize)]
struct ShotEffect {
    id: String,
    form: String,
    #[serde(default = "one_f")]
    amount: f64,
}

#[derive(Deserialize)]
struct ShotMarker {
    id: String,
    #[serde(default)]
    t: f64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    palette: u8,
    #[serde(default)]
    note: String,
}

/// A text id as the model's stable id: the same text is the same id on every device, so a cut
/// renders where it was made and nowhere else.
fn id_of(text: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, text.as_bytes())
}

fn kind_of(name: &str) -> model::MediaKind {
    match name {
        "audio" => model::MediaKind::Audio,
        "image" => model::MediaKind::Image,
        "text" | "title" => model::MediaKind::Text,
        _ => model::MediaKind::Video,
    }
}

fn transition_of(name: Option<&str>) -> model::Transition {
    match name {
        Some("dissolve") => model::Transition::CrossDissolve,
        Some("fade") => model::Transition::FadeIn,
        Some("dip") => model::Transition::DipToBlack,
        Some("wipe") => model::Transition::Wipe,
        _ => model::Transition::None,
    }
}

fn align_of(name: Option<&str>) -> model::TextAlign {
    match name {
        Some("Left") => model::TextAlign::Left,
        Some("Right") => model::TextAlign::Right,
        _ => model::TextAlign::Center,
    }
}

fn effect_of(name: &str) -> Option<model::EffectKind> {
    Some(match name {
        "Grayscale" | "grayscale" => model::EffectKind::Grayscale,
        "Sepia" | "sepia" => model::EffectKind::Sepia,
        "Invert" | "invert" => model::EffectKind::Invert,
        "Warm" | "warm" => model::EffectKind::Warm,
        "Cool" | "cool" => model::EffectKind::Cool,
        "Vignette" | "vignette" => model::EffectKind::Vignette,
        _ => return None,
    })
}

impl Shot {
    /// The model this cut is, with every source laid out where this device can decode it.
    fn into_project(self, store: Option<&ContentStore>) -> Result<model::Project, String> {
        let mut project = model::Project::new(&self.name);
        project.set_frame_size(self.width, self.height);
        project.fps = self.fps.clamp(model::MIN_FPS, model::MAX_FPS);
        project.mark_in = (self.mark_in >= 0.0).then_some(self.mark_in);
        project.mark_out = (self.mark_out >= 0.0).then_some(self.mark_out);
        for media in self.media {
            let media: ShotMedia = reads(&media)?;
            project.media.push(media_item(media, store));
        }
        for track in self.tracks {
            let track: ShotTrack = reads(&track)?;
            project.tracks.push(track_of(track));
        }
        for clip in self.clips {
            let clip: ShotClip = reads(&clip)?;
            project.clips.push(clip_of(clip)?);
        }
        for marker in self.markers {
            let marker: ShotMarker = reads(&marker)?;
            project.markers.push(marker_of(marker));
        }
        Ok(project)
    }
}

/// The media `media` names, laid out where this device can decode it: the content store's bytes
/// laid out, or a path that is already here. An address this device cannot read stays as the app
/// wrote it, which the picture draws from the source's own fill.
fn media_item(media: ShotMedia, store: Option<&ContentStore>) -> model::MediaItem {
    let path = store
        .and_then(|store| media_path(store, &media.path).ok())
        .map_or_else(
            || media.path.clone(),
            |at| at.to_string_lossy().into_owned(),
        );
    model::MediaItem {
        id: id_of(&media.id),
        name: media.name,
        kind: kind_of(&media.kind),
        duration: media.duration,
        width: media.width,
        height: media.height,
        fps: media.fps,
        palette: media.palette,
        path,
        text: (media.kind == "text").then(|| model::TextSource {
            content: media.content,
            size: media.size,
            color: media.color,
            align: align_of(Some(&media.align)),
            font: media.font,
            bold: media.bold,
            italic: media.italic,
            backdrop: media.backdrop,
        }),
        has_audio: media.has_audio,
    }
}

/// The track `track` names, as the model holds it.
fn track_of(track: ShotTrack) -> model::Track {
    model::Track {
        id: id_of(&track.id),
        name: track.name,
        kind: if track.kind == "audio" {
            model::TrackKind::Audio
        } else {
            model::TrackKind::Video
        },
        index: track.index,
        muted: track.muted,
        solo: track.solo,
        locked: track.locked,
        gain_db: track.gain_db,
        pan: track.pan,
    }
}

/// The clip `clip` names, its keys and effects read, as the model holds it.
fn clip_of(clip: ShotClip) -> Result<model::Clip, String> {
    let mut anim = ClipAnimation::default();
    for text in &clip.keys {
        let key: ShotKey = reads(text)?;
        if let Some(param) = Param::from_name(&key.param) {
            anim.ensure(param).set_key(
                key.t,
                key.v,
                Interp::from_name(key.interp.as_deref().unwrap_or_default()),
            );
        }
    }
    let mut effects = Ordered::new();
    for effect in clip.effects {
        let effect: ShotEffect = reads(&effect)?;
        if let Some(kind) = effect_of(&effect.form) {
            effects.push(model::ClipEffect {
                id: id_of(&effect.id),
                kind,
                amount: effect.amount,
            });
        }
    }
    Ok(model::Clip {
        id: id_of(&clip.id),
        track: id_of(&clip.track),
        media: id_of(&clip.media),
        start: clip.start,
        in_point: clip.in_point,
        out_point: clip.out_point,
        opacity: clip.opacity,
        volume: clip.volume,
        transition: transition_of(clip.transition.as_deref()),
        palette: clip.palette,
        filters: model::Filters {
            brightness: clip.brightness,
            contrast: clip.contrast,
            saturation: clip.saturation,
        },
        curves: video_edit::curve::Curve::default(),
        fade_in: clip.fade_in,
        fade_out: clip.fade_out,
        transition_duration: clip.transition_duration,
        effects,
        transform: model::Transform {
            scale_x: clip.scale_x.unwrap_or(1.0),
            scale_y: clip.scale_y.unwrap_or(1.0),
            pos_x: clip.pos_x.unwrap_or(0.0),
            pos_y: clip.pos_y.unwrap_or(0.0),
            rotation: clip.rotation.unwrap_or(0.0),
            anchor_x: 0.5,
            anchor_y: 0.5,
            crop_left: clip.crop_left.unwrap_or(0.0),
            crop_right: clip.crop_right.unwrap_or(0.0),
            crop_top: clip.crop_top.unwrap_or(0.0),
            crop_bottom: clip.crop_bottom.unwrap_or(0.0),
        },
        speed: model::Speed {
            rate: clip
                .speed_rate
                .unwrap_or(1.0)
                .clamp(model::MIN_SPEED, model::MAX_SPEED),
            reverse: clip.playback.speed_reverse,
        },
        key: model::ChromaKey {
            enabled: clip.key.key_enabled,
            color: clip.key_color,
            tolerance: clip.key_tolerance,
            softness: clip.key_softness,
            spill: clip.key_spill,
        },
        mask: model::Mask {
            shape: match clip.mask_shape.as_deref() {
                Some("rect") => model::MaskShape::Rect,
                Some("ellipse") => model::MaskShape::Ellipse,
                _ => model::MaskShape::None,
            },
            x: clip.mask_x.unwrap_or(0.0),
            y: clip.mask_y.unwrap_or(0.0),
            w: clip.mask_w.unwrap_or(0.6),
            h: clip.mask_h.unwrap_or(0.6),
            rotation: clip.mask_rotation.unwrap_or(0.0),
            feather: clip.mask_feather.unwrap_or(0.12),
            invert: clip.key.mask_invert,
        },
        anim,
        markers: Keyed::new(),
        link: (!clip.link.is_empty()).then(|| id_of(&clip.link)),
        link_follows: clip.link_follows,
        sound_split: clip.playback.sound_split,
    })
}

/// The marker `marker` names, as the model holds it.
fn marker_of(marker: ShotMarker) -> model::Marker {
    model::Marker {
        id: id_of(&marker.id),
        t: marker.t,
        name: marker.name,
        palette: marker.palette,
        note: marker.note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_id_is_one_id_on_every_device() {
        assert_eq!(id_of("abc"), id_of("abc"));
        assert_ne!(id_of("abc"), id_of("abd"));
    }

    #[test]
    fn a_cut_itself_reads_into_the_model() {
        let shot: Shot = serde_json::from_str(r#"{
                "name": "Cut",
                "fps": 30.0,
                "width": 1920,
                "height": 1080,
                "media": ["{\"id\":\"m1\",\"name\":\"a.mp4\",\"kind\":\"video\",\"duration\":10.0,\"width\":1920,\"height\":1080,\"path\":\"/media/a.mp4\"}"],
                "tracks": ["{\"id\":\"t1\",\"name\":\"V1\",\"kind\":\"video\"}"],
                "clips": ["{\"id\":\"c1\",\"track\":\"t1\",\"media\":\"m1\",\"start\":0.0,\"out_point\":4.0,\"transition\":\"dissolve\",\"keys\":[\"{\\\"param\\\":\\\"opacity\\\",\\\"t\\\":0.0,\\\"v\\\":0.2}\",\"{\\\"param\\\":\\\"opacity\\\",\\\"t\\\":2.0,\\\"v\\\":1.0}\"]}"],
                "markers": ["{\"id\":\"k1\",\"t\":1.0,\"name\":\"Beat\"}"]
            }"#,
        )
        .unwrap();
        let project = shot.into_project(None).unwrap();
        assert_eq!(project.clips.len(), 1);
        assert_eq!(
            project.clips[0].transition,
            model::Transition::CrossDissolve
        );
        assert_eq!(project.tracks[0].kind, model::TrackKind::Video);
        assert_eq!(project.markers[0].name, "Beat");
        assert_eq!(project.media[0].path, "/media/a.mp4");
        assert!(project.clips[0].anim.is_animated(Param::Opacity));
        // Halfway between 0.2 and 1.0: interpolation rounds the last bits, so compare near it.
        let opacity = project.clips[0].anim.sample(Param::Opacity, 1.0);
        assert!(
            opacity.is_some_and(|value| (value - 0.6).abs() < 1e-6),
            "{opacity:?}"
        );
    }
}

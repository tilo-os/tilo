//! The Video Editor's engine, as a package reaches it: `engine.call` and `engine.start` answered
//! for `so.tilo.video-editor` by the editing and picture engines on this device.
//!
//! The app runs sandboxed with no decoder and no renderer, so it asks by name. A request carries
//! the cut as JSON the app wrote; the answer is JSON the app reads back into its own records. A
//! still, a waveform and a probe answer now; a render starts as a job whose progress is asked by
//! its id and which can be stopped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use serde_json::json;
use tilo_content::ContentStore;
use tilo_resource::{Keyed, Ordered};
use tilolang_tilo::engine::{self, JobOp, JobSink, Needs, Provider, Running, State};
use tilolang_tilo::runtime::host_work::{Frame as LiveFrame, Push};
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
            "mix" => mix(store, input),
            "scrub" => scrub(store, input),
            "cut" => recut(store, input),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        match op {
            "render" => render(&self.store, job, input, moved),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn stream(&self, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
        match op {
            "play" => play(&self.store, input, push),
            other => Err(format!("`{other}` is no stream this device runs for {APP}")),
        }
    }

    fn cancel(&self, job: &str) -> bool {
        let Some((render, _)) = running().get(job) else {
            return stop_play(job);
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

/// Where the cut's mix goes over full scale, and by how much: the whole cut measured block by
/// block, as the device plays it.
///
/// A mix is a sum, so the report is about the sum, not one track: two tracks at two-thirds each
/// are over without either being over alone. `tracks` names the ones that were over on their
/// own, which is what a person looks at first.
fn mix(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        project: String,
    }
    let ask: Ask = reads(input)?;
    let cut = reads::<Shot>(&ask.project)?.into_project(Some(store))?;
    let (over_db, overs, hot) = measure_mix(&cut, mix_sources());
    Ok(serde_json::json!({
        "over_db": over_db,
        "overs": overs,
        "tracks": hot,
    })
    .to_string())
}

/// The sources the mix check reads through, held for the process: a check runs again after every
/// edit, and a source's samples are the same on each until the file itself changes.
fn mix_sources() -> &'static Mutex<video_edit::audio::SourcePcm> {
    /// How much decoded sound the check keeps: a cut's sources, bounded so a long film cannot
    /// pin a device's memory.
    const KEPT_BYTES: usize = 512 * 1024 * 1024;
    static SOURCES: OnceLock<Mutex<video_edit::audio::SourcePcm>> = OnceLock::new();
    SOURCES.get_or_init(|| Mutex::new(video_edit::audio::SourcePcm::new(KEPT_BYTES)))
}

/// The cut's mix measured to its end, answering how far over full scale it went, where, and
/// which tracks were over on their own.
fn measure_mix(
    cut: &model::Project,
    sources: &Mutex<video_edit::audio::SourcePcm>,
) -> (f64, Vec<f64>, Vec<String>) {
    // A cut with no sound in it cannot go over: nothing is decoded to find out.
    if !video_edit::audio::has_audio(cut) {
        return (0.0, Vec::new(), Vec::new());
    }
    let mut cache = sources
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    measure_blocks(cut, &mut cache)
}

/// The cut's mix measured block by block to its end, over `cache`: how far over full scale it
/// went, where, and which tracks were over on their own.
fn measure_blocks(
    cut: &model::Project,
    cache: &mut video_edit::audio::SourcePcm,
) -> (f64, Vec<f64>, Vec<String>) {
    let mut source = video_edit::audio::CachedSource::new(cut, cache);
    let meters = video_edit::audio::MixMeters::new();
    let block = video_edit::audio::MONITOR_BLOCK;
    let step = ui_core::num::f64_of_usize(block) / f64::from(video_edit::audio::SAMPLE_RATE);
    let end = cut.duration().max(0.0);
    let mut index = 0u64;
    loop {
        // Each block's start is counted, not accumulated: a mix's length never drifts.
        let at = ui_core::num::f64_of_u64(index) * step;
        if at >= end {
            break;
        }
        let (_, report) = video_edit::audio::mix_block_metered(cut, at, block, &mut source);
        meters.record(at, &report);
        index = index.saturating_add(1);
    }
    let snapshot = meters.snapshot();
    let hot = snapshot
        .clipped_tracks()
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    (snapshot.worst_over_db(), snapshot.overs, hot)
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

/// One live play running now: the cut it shows, and what stops it.
struct Play {
    cancel: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    /// The cut a live play is showing now, replaced when the app hands over an edited one.
    cut: Mutex<Option<Arc<model::Project>>>,
    error: Mutex<Option<String>>,
}

/// The live plays running now, by their job.
fn plays() -> &'static Mutex<BTreeMap<String, Arc<Play>>> {
    static PLAYS: OnceLock<Mutex<BTreeMap<String, Arc<Play>>>> = OnceLock::new();
    PLAYS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Stop the live play `job`: false when none runs by that name.
fn stop_play(job: &str) -> bool {
    let Ok(plays) = plays().lock() else {
        return false;
    };
    let Some(play) = plays.get(job) else {
        return false;
    };
    play.cancel.store(true, Ordering::Relaxed);
    true
}

/// The width a live frame is composited at before the window shows it: enough for the preview,
/// small enough to keep at the rate a transport plays.
const LIVE_WIDTH: u32 = 640;

/// The most a transport shuttles at, forward or backward.
const MAX_RATE: f64 = 16.0;

/// How often a running play says where it has got, in frames: often enough for a playhead to
/// move smoothly, rarely enough that a window is not drawn again for every frame.
const SAY_EVERY_FRAMES: u64 = 4;

/// How one play runs: where it starts, how fast, the frame it draws and whether its sound is
/// played.
struct Playing {
    from: f64,
    rate: f64,
    width: u32,
    height: u32,
    sound: bool,
}

/// The cut played live: each frame composited as the playhead reaches it, with the sound mixed
/// for the same moment, both from the cut as the app wrote it.
///
/// The frames are the window's, pushed as they are drawn and shown by `tilo-live://<job>`; the
/// sound is the device's, mixed ahead of the playhead, so an edit is heard almost at once.
fn play(store: &ContentStore, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        /// The cut, as `json.encode(shot(p))` writes it.
        project: String,
        /// Where the playhead starts, in timeline seconds.
        #[serde(default)]
        from: f64,
        /// Timeline seconds a second of wall time: 1 is normal speed, negative is backwards.
        #[serde(default = "one_f")]
        rate: f64,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        /// Whether the device plays the cut's sound as a preview does.
        #[serde(default = "yes")]
        sound: bool,
    }
    let ask: Ask = reads(input)?;
    let project = reads::<Shot>(&ask.project)?.into_project(Some(store))?;
    let rate = if ask.rate.abs() < 1e-9 {
        1.0
    } else {
        ask.rate.clamp(-MAX_RATE, MAX_RATE)
    };
    let width = ask.width.unwrap_or(LIVE_WIDTH).clamp(64, 1920);
    let height = ask.height.map_or_else(
        || {
            let by_shape = u64::from(width)
                .saturating_mul(u64::from(project.height.max(1)))
                .checked_div(u64::from(project.width.max(1)).max(1))
                .unwrap_or_default();
            u32::try_from(by_shape).unwrap_or(u32::MAX).clamp(64, 1920)
        },
        |height| height.clamp(64, 1920),
    );
    let playing = Playing {
        from: ask.from.max(0.0),
        rate,
        width,
        height,
        sound: ask.sound,
    };
    let id = next_job_id();
    let state = Arc::new(Play {
        cancel: AtomicBool::new(false),
        done: AtomicU64::new(0),
        total: AtomicU64::new(0),
        cut: Mutex::new(Some(Arc::new(project))),
        error: Mutex::new(None),
    });
    plays()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&state));
    let (slot, slot_of) = (id.clone(), id.clone());
    let worker = Arc::clone(&state);
    let spawned = std::thread::Builder::new()
        .name("video-editor-play".to_owned())
        .spawn(move || {
            let outcome = run_play(&playing, &worker, &push, &slot);
            if let Err(error) = outcome
                && let Ok(mut slot) = worker.error.lock()
            {
                *slot = Some(error);
            }
            if let Ok(mut plays) = plays().lock() {
                plays.remove(&slot_of);
            }
        });
    if spawned.is_err() {
        return Err("could not start playing the cut".to_owned());
    }
    Ok(serde_json::json!({ "job": id }).to_string())
}

/// One play's loop: composite, hand over, mix, pace, and stop where the cut ends. Answers the
/// timeline second it reached.
///
/// The cut is read from the job each frame, so an edit made while it plays is shown and heard
/// from the next frame on.
fn run_play(
    playing: &Playing,
    worker: &Play,
    push: &Arc<dyn Push>,
    slot: &str,
) -> Result<f64, String> {
    let Playing {
        from,
        rate,
        width,
        height,
        sound,
    } = *playing;
    let mut cut = taken(worker)?;
    let mut mix = video_edit::audio::mix_signature(&cut);
    let mut compositor = video_picture::gpu::HeadlessCompositor::new()
        .map_err(|error| format!("this device cannot draw the cut as it plays: {error}"))?;
    let mut media = video_picture::gpu::media_signature(&cut);
    let mut sources =
        video_picture::gpu::MediaFrameSource::new(&cut, video_picture::gpu::Fetch::Responsive)
            .with_timeout(std::time::Duration::from_millis(500));
    // What every block the device mixes measured, kept beside the monitor: the play says
    // where the mix it has heard went over, as well as where the playhead is.
    let meters = Arc::new(video_edit::audio::MixMeters::new());
    let mut monitor = sound.then(|| {
        video_edit::audio::AudioMonitor::new(
            &cut,
            video_edit::audio::open_output(),
            Box::new(video_edit::audio::DecodedPcm::new(&cut)),
            Arc::clone(&meters),
        )
    });
    let began = std::time::Instant::now();
    let mut frames: u64 = 0;
    let mut generation: u64 = 0;
    let mut reached = from;
    loop {
        if worker.cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(edited) = swapped(worker) {
            let its_media = video_picture::gpu::media_signature(&edited);
            // An edit that changes the mix makes what was measured describe a mix that no
            // longer exists; a rename or a grade does not.
            if video_edit::audio::mix_signature(&edited) != mix {
                mix = video_edit::audio::mix_signature(&edited);
                meters.clear();
            }
            if its_media != media {
                media = its_media;
                sources = video_picture::gpu::MediaFrameSource::new(
                    &edited,
                    video_picture::gpu::Fetch::Responsive,
                )
                .with_timeout(std::time::Duration::from_millis(500));
            }
            if let Some(monitor) = monitor.as_mut() {
                monitor.set_project(&edited);
            }
            cut = edited;
        }
        let period = 1.0 / cut.fps.clamp(1.0, 60.0);
        let end = cut.duration().max(0.0);
        let at = from + began.elapsed().as_secs_f64() * rate;
        let ended = if rate < 0.0 { at <= 0.0 } else { at >= end };
        let now = at.clamp(0.0, end);
        reached = now;
        let rgba = compositor.frame(&cut, now, width, height, &mut sources);
        generation = generation.wrapping_add(1);
        push.frame(LiveFrame {
            slot: slot.to_owned(),
            generation,
            width,
            height,
            pixels: Arc::new(rgba),
        });
        if let Some(monitor) = monitor.as_mut() {
            // The device's monitor keeps its own clock from the samples it played, so it is
            // started once and let run, and brought back to the playhead only when the two
            // drift apart or the picture is shuttled. Backwards there is nothing to play: the
            // mix is cut forward only.
            if rate < 0.0 {
                if monitor.is_playing() {
                    monitor.pause();
                }
            } else if !monitor.is_playing() {
                monitor.play(now);
            } else if (monitor.position() - now).abs() > 0.35 {
                monitor.seek(now);
            }
        }
        if frames.is_multiple_of(SAY_EVERY_FRAMES) {
            worker.done.store(frames, Ordering::Relaxed);
            push.state(slot, &play_word(now, &meters));
        }
        frames = frames.saturating_add(1);
        if ended {
            break;
        }
        frames_due(began, period, frames);
    }
    if let Some(monitor) = monitor.as_mut() {
        monitor.pause();
    }
    // Where the play left the playhead, so a window that reads the stream's word after it ends
    // stands where the picture did.
    push.state(slot, &play_word(reached, &meters));
    worker.done.store(frames, Ordering::Relaxed);
    worker.total.store(frames, Ordering::Relaxed);
    Ok(reached)
}

/// What a live play says of itself: the timeline second its playhead has reached, and what the
/// mix it has heard so far measured — how far over full scale, where, and which tracks were over
/// on their own.
fn play_word(at: f64, meters: &video_edit::audio::MixMeters) -> String {
    let snapshot = meters.snapshot();
    let hot: Vec<String> = snapshot
        .clipped_tracks()
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    serde_json::json!({
        "at": at,
        "over_db": snapshot.worst_over_db(),
        "overs": snapshot.overs,
        "tracks": hot,
    })
    .to_string()
}

/// Waits until frame `frames` is due, so a play runs at the cut's rate rather than as fast as the
/// device can draw.
fn frames_due(began: std::time::Instant, period: f64, frames: u64) {
    let wait = std::time::Duration::from_secs_f64(period * ui_core::num::f64_of_u64(frames));
    let due = began.checked_add(wait).unwrap_or(began);
    std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
}

/// The cut a live play starts from, which the app handed over with the play.
fn taken(worker: &Play) -> Result<Arc<model::Project>, String> {
    worker
        .cut
        .lock()
        .map_err(|_| "the play's cut is unreadable".to_owned())?
        .take()
        .ok_or_else(|| "the play has no cut".to_owned())
}

/// The cut the app handed over since the last frame, when it edited one as it played.
fn swapped(worker: &Play) -> Option<Arc<model::Project>> {
    worker.cut.lock().ok().and_then(|mut cut| cut.take())
}

/// The cut a live play shows, replaced by an edit: what makes an edit seen as it is made rather
/// than at the next play.
fn recut(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        /// The live play to change.
        job: String,
        /// The cut, as `json.encode(shot(p))` writes it.
        project: String,
    }
    let ask: Ask = reads(input)?;
    let cut = Arc::new(reads::<Shot>(&ask.project)?.into_project(Some(store))?);
    let state = plays()
        .lock()
        .map_err(|_| "the play table is unreadable".to_owned())?
        .get(&ask.job)
        .cloned()
        .ok_or_else(|| format!("`{}` is no live play on this device", ask.job))?;
    *state
        .cut
        .lock()
        .map_err(|_| "the play's cut is unreadable".to_owned())? = Some(cut);
    Ok(String::new())
}

/// The device's one scrubbing output: a sink kept between drags, and the thread that pushes each
/// drag's short piece of the mix to it.
///
/// The sink blocks while its queue is full, which is what paces a mix; that wait is the scrub
/// thread's, never the action's, so a drag is never held up by the sound it makes.
struct Scrubbing {
    asked: std::sync::mpsc::Sender<ScrubAsk>,
}

/// One drag's piece of the mix.
struct ScrubAsk {
    project: Arc<model::Project>,
    at: f64,
    seconds: f64,
}

/// The scrubbing output, made the first time a playhead is dragged.
fn scrubber() -> &'static Mutex<Option<Scrubbing>> {
    static SCRUB: OnceLock<Mutex<Option<Scrubbing>>> = OnceLock::new();
    SCRUB.get_or_init(|| Mutex::new(None))
}

/// A short piece of the cut's sound at `at`: what dragging the playhead sounds like.
fn scrub(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        /// The cut, as `json.encode(shot(p))` writes it.
        project: String,
        at: f64,
        /// How much of the mix to play, in seconds.
        #[serde(default = "snippet")]
        seconds: f64,
    }
    let ask: Ask = reads(input)?;
    let project = Arc::new(reads::<Shot>(&ask.project)?.into_project(Some(store))?);
    let asked = ScrubAsk {
        project,
        at: ask.at.max(0.0),
        seconds: ask.seconds,
    };
    // The output is taken out of the lock before the ask is queued: the send is the thread's
    // work, not the scrubber's.
    let sender = {
        let mut held = scrubber()
            .lock()
            .map_err(|_| "the scrubbing output is unreadable".to_owned())?;
        if held.is_none() {
            *held = Some(open_scrubbing()?);
        }
        held.as_ref().map(|scrubbing| scrubbing.asked.clone())
    };
    let Some(sender) = sender else {
        return Ok(String::new());
    };
    sender
        .send(asked)
        .map_err(|_| "the scrubbing output has gone".to_owned())?;
    Ok(String::new())
}

/// The scrubbing thread: the device's output opened once, and the newest piece of the mix pushed
/// as soon as the last one has played, so a fast drag stays close behind the playhead rather than
/// queueing up behind it.
fn open_scrubbing() -> Result<Scrubbing, String> {
    let (asked, heard) = std::sync::mpsc::channel::<ScrubAsk>();
    std::thread::Builder::new()
        .name("video-editor-scrub".to_owned())
        .spawn(move || {
            let mut sink = video_edit::audio::open_output();
            let mut pcm: Option<(u64, video_edit::audio::DecodedPcm)> = None;
            let Ok(mut ask) = heard.recv() else {
                return;
            };
            loop {
                while let Ok(newer) = heard.try_recv() {
                    ask = newer;
                }
                let mix = video_edit::audio::mix_signature(&ask.project);
                if pcm.as_ref().map(|(signature, _)| *signature) != Some(mix) {
                    pcm = Some((mix, video_edit::audio::DecodedPcm::new(&ask.project)));
                }
                if let Some((_, source)) = pcm.as_mut() {
                    let frames = ui_core::num::usize_of_f64(
                        (ask.seconds.clamp(0.02, 0.4) * f64::from(video_edit::audio::SAMPLE_RATE))
                            .round(),
                    )
                    .max(1);
                    let (block, _report) =
                        video_edit::audio::mix_block_metered(&ask.project, ask.at, frames, source);
                    if !sink.push(&block) {
                        break;
                    }
                }
                let Ok(next) = heard.recv() else {
                    break;
                };
                ask = next;
            }
        })
        .map_err(|error| format!("this device cannot play the scrub: {error}"))?;
    Ok(Scrubbing { asked })
}

/// How much of the mix one move of a dragged playhead plays.
fn snippet() -> f64 {
    0.12
}

fn next_job_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("job-{}", NEXT.fetch_add(1, Ordering::Relaxed))
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
    /// The point of the picture scaling and turning go around, a fraction of it.
    #[serde(default)]
    anchor_x: Option<f64>,
    #[serde(default)]
    anchor_y: Option<f64>,
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
    /// One channel's tone curve: its points' x and y, each 0..1, in ascending x.
    #[serde(default)]
    curve_master: Vec<f64>,
    #[serde(default)]
    curve_red: Vec<f64>,
    #[serde(default)]
    curve_green: Vec<f64>,
    #[serde(default)]
    curve_blue: Vec<f64>,
    /// How much of the curve is mixed in: 0 bypasses it, 1 is the curve as drawn.
    #[serde(default = "one_f")]
    curve_mix: f64,
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
    let curves = curves_of(&clip);
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
        curves,
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
            anchor_x: clip.anchor_x.unwrap_or(0.5).clamp(0.0, 1.0),
            anchor_y: clip.anchor_y.unwrap_or(0.5).clamp(0.0, 1.0),
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

/// A clip's tone curves, from the flat point lists the app keeps: both ends of each channel, at
/// x 0 and x 1, so the ends of the curve are defined whatever points the app sends.
fn curves_of(clip: &ShotClip) -> video_edit::curve::Curve {
    let spline = |points: &[f64]| -> video_edit::curve::Spline {
        let mut drawn = video_edit::curve::Spline::identity();
        for pair in points.chunks_exact(2) {
            if let [x, y] = pair {
                drawn.set_point(*x, *y);
            }
        }
        drawn.normalised()
    };
    video_edit::curve::Curve {
        master: spline(&clip.curve_master),
        red: spline(&clip.curve_red),
        green: spline(&clip.curve_green),
        blue: spline(&clip.curve_blue),
        mix: clip.curve_mix.clamp(0.0, 1.0),
    }
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
    fn a_clip_written_before_anchors_turns_around_its_middle() {
        let shot: Shot = serde_json::from_str(
            r#"{
                "clips": ["{\"id\":\"c1\",\"track\":\"t1\",\"media\":\"m1\"}"]
            }"#,
        )
        .expect("a cut without an anchor still reads");
        let project = shot.into_project(None).expect("it is a cut");
        assert!((project.clips[0].transform.anchor_x - 0.5).abs() < 1e-9);
        assert!((project.clips[0].transform.anchor_y - 0.5).abs() < 1e-9);
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
                "clips": ["{\"id\":\"c1\",\"track\":\"t1\",\"media\":\"m1\",\"start\":0.0,\"out_point\":4.0,\"transition\":\"dissolve\",\"anchor_x\":0.0,\"anchor_y\":1.0,\"keys\":[\"{\\\"param\\\":\\\"opacity\\\",\\\"t\\\":0.0,\\\"v\\\":0.2}\",\"{\\\"param\\\":\\\"opacity\\\",\\\"t\\\":2.0,\\\"v\\\":1.0}\"]}"],
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
        assert_eq!(
            (
                project.clips[0].transform.anchor_x,
                project.clips[0].transform.anchor_y
            ),
            (0.0, 1.0),
            "a clip's anchor is where its own cut puts it"
        );
        // Halfway between 0.2 and 1.0: interpolation rounds the last bits, so compare near it.
        let opacity = project.clips[0].anim.sample(Param::Opacity, 1.0);
        assert!(
            opacity.is_some_and(|value| (value - 0.6).abs() < 1e-6),
            "{opacity:?}"
        );
    }
    /// What a live play handed the window: the frames it drew, by number, and what it said of
    /// itself.
    #[derive(Default)]
    struct Recording {
        frames: Mutex<Vec<u64>>,
        states: Mutex<Vec<String>>,
    }

    impl Push for Recording {
        fn frame(&self, frame: LiveFrame) {
            let want = usize::try_from(frame.width)
                .unwrap_or(0)
                .saturating_mul(usize::try_from(frame.height).unwrap_or(0))
                .saturating_mul(4);
            assert_eq!(frame.pixels.len(), want, "a frame is its whole picture");
            self.frames
                .lock()
                .expect("the recorder")
                .push(frame.generation);
        }

        fn state(&self, _slot: &str, state: &str) {
            self.states
                .lock()
                .expect("the recorder")
                .push(state.to_owned());
        }
    }

    /// A cut of one palette source, `seconds` long: a picture no decoder is needed for.
    fn cut(seconds: f64) -> model::Project {
        let mut project = model::Project::new("live");
        project.set_frame_size(160, 90);
        project.fps = 10.0;
        let media = model::MediaItem {
            id: uuid::Uuid::from_u128(1),
            name: "palette".to_owned(),
            kind: model::MediaKind::Video,
            duration: seconds,
            width: 160,
            height: 90,
            fps: 10.0,
            palette: 1,
            path: video_edit::media::workspace_path("live.mp4"),
            ..Default::default()
        };
        let track = model::Track {
            id: uuid::Uuid::from_u128(2),
            kind: model::TrackKind::Video,
            index: 1,
            ..Default::default()
        };
        let clip = model::Clip {
            id: uuid::Uuid::from_u128(3),
            track: track.id,
            media: media.id,
            start: 0.0,
            in_point: 0.0,
            out_point: seconds,
            ..Default::default()
        };
        project.media.push(media);
        project.tracks.push(track);
        project.clips.push(clip);
        project
    }

    fn playing(cut: model::Project) -> Play {
        Play {
            cancel: AtomicBool::new(false),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cut: Mutex::new(Some(Arc::new(cut))),
            error: Mutex::new(None),
        }
    }

    #[test]
    fn a_live_play_draws_the_cut_as_it_goes_and_says_where_it_has_got() {
        let job = playing(cut(1.5));
        let heard = Arc::new(Recording::default());
        let push: Arc<dyn Push> = Arc::clone(&heard) as Arc<dyn Push>;
        let playing = Playing {
            from: 0.0,
            rate: 1.0,
            width: 64,
            height: 36,
            sound: false,
        };
        let ended_at =
            run_play(&playing, &job, &push, "live-test").expect("a cut of a palette source plays");
        assert!(ended_at >= 1.49, "it played to the cut's end: {ended_at}");
        let frames = heard.frames.lock().expect("the recorder").clone();
        assert!(
            frames.len() >= 3,
            "the window was handed frames: {frames:?}"
        );
        assert!(
            frames.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "each frame is numbered after the one before: {frames:?}"
        );
        let states = heard.states.lock().expect("the recorder").clone();
        let at: Vec<f64> = states
            .iter()
            .map(|state| {
                state
                    .parse::<f64>()
                    .unwrap_or_else(|_| panic!("a play says where it is, not {state:?}"))
            })
            .collect();
        assert!(
            at.windows(2).all(|pair| pair[1] >= pair[0]),
            "it never goes back: {at:?}"
        );
        assert!(
            at.last().is_some_and(|last| (last - ended_at).abs() < 0.01),
            "its last word is where it ended: {at:?} at {ended_at}"
        );
    }

    #[test]
    fn an_edit_handed_over_as_the_cut_plays_is_taken_once() {
        let job = playing(cut(0.3));
        let edited = Arc::new(cut(0.2));
        *job.cut.lock().expect("the job's cut") = Some(Arc::clone(&edited));
        let taken = swapped(&job).expect("the edit reaches the play");
        assert!(
            (taken.clips[0].end() - 0.2).abs() < 1e-9,
            "the play draws the cut as it now is"
        );
        assert!(swapped(&job).is_none(), "and keeps it until the next edit");
    }
}

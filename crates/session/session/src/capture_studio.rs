//! The capture studio the session shows: it captures what the bar asks of
//! it, from the compositor's own frames.
//!
//! The studio app draws and asks ([`tilo_capture_studio::Ask`]); the session
//! answers each pass, because only it has the compositor. A screenshot is
//! read back from the frames the compositor renders — the same path a share
//! stream and the portal's screenshots take, so what is captured is what the
//! desktop shows, private windows blanked — written to the person's
//! Screenshots folder and, when asked, put on the clipboard. A recording
//! feeds the same read-back into the recorder ([`Recorder`]) at the replay
//! buffer's rate, which writes the file and keeps the last half minute
//! encoded.
//!
//! What came of each capture goes back to the studio as its scene
//! ([`tilo_capture_studio::Scene`]).

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "frames are bounded by the displays' pixels, far from the integers' limits"
)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_capture_studio::{
    Ask, Frame, Kept, REPLAY_FPS, Recorder, Recording, Scene, Settings, Studio, Take,
};
use tilo_compositor::capture::Target;
use tilo_compositor::{Compositor, Screenshot, Selection};
use tilo_surface_host::Doors;
use ui::vello::kurbo::Rect;
use ui::widget::{CaptureDestination, CapturePreview, CaptureTarget, ScreenshotKept, SourceMode};

use crate::Options;
use crate::panels::Held;
use crate::sheets::Sheet;

/// One frame every this long: the replay buffer's rate, so a recording and
/// a clip of what just happened share one encode.
const FRAME_INTERVAL: Duration = Duration::from_millis(1_000 / 30);

/// The columns and rows of a receipt's thumbnail.
const PREVIEW: (u8, u8) = (12, 8);

/// The capture studio as the session runs it, and what it is doing.
pub(crate) struct CaptureStudio {
    seat: Studio,
    /// The folder screenshots and recordings are written to.
    directory: PathBuf,
    /// The last screenshot, for its receipt and for opening it again.
    last: Option<Kept>,
    /// What feeds the recorder now: a recording, or the replay buffer.
    feeding: Option<Feeding>,
    /// The recording that runs, as the studio shows it.
    recording: Option<Recording>,
    /// Seconds of the last half minute ready to clip.
    replay: Option<u8>,
    /// How many clips the replay buffer gave.
    replay_saves: u64,
}

/// Frames going into a recorder.
struct Feeding {
    take: Take,
    recorder: Recorder,
    /// The file written: the recording's, or `None` while only the replay
    /// buffer is fed.
    path: Option<PathBuf>,
    last: Option<Instant>,
    paused: bool,
}

/// Mount the capture studio over the desktop, and hand back the session's end
/// of it.
pub(crate) fn mount(
    compositor: &mut Compositor,
    doors: &mut Doors,
    options: &Options,
) -> Option<CaptureStudio> {
    let who = (options.device.as_str(), options.person.as_str());
    let seat = Studio::new();
    match Sheet::mount(
        tilo_capture_studio::definition(),
        seat,
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let sheet = sheet.always_serving();
            let seat = sheet.seat().clone();
            let sheet = Rc::new(RefCell::new(sheet));
            compositor.add_host(Box::new(Held(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(CaptureStudio {
                seat,
                directory: options.pictures.clone().unwrap_or_else(screenshots),
                last: None,
                feeding: None,
                recording: None,
                replay: None,
                replay_saves: 0,
            })
        }
        Err(error) => {
            tracing::warn!(%error, "capture studio not mounted");
            None
        }
    }
}

/// The person's Screenshots folder: the one they kept the desktop's in.
fn screenshots() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join("Pictures").join("Screenshots")
}

impl CaptureStudio {
    /// The studio sheet's presentation: where the sheet sits.
    pub(crate) fn presentation(&self) -> &tilo_sheets::Presentation {
        tilo_sheets::Seat::presentation(&self.seat)
    }

    /// Answer what the studio asked since the last pass, feed the recorder
    /// and hand back what came of it.
    pub(crate) fn serve(&mut self, compositor: &mut Compositor) {
        for ask in self.seat.link().take_asks() {
            self.answer(ask, compositor);
        }
        self.feed(compositor);
        self.publish(compositor);
    }

    fn answer(&mut self, ask: Ask, compositor: &mut Compositor) {
        match ask {
            Ask::Screenshot(take) => match self.screenshot(&take, compositor) {
                Ok(kept) => self.last = Some(kept),
                Err(error) => tracing::warn!(%error, "a screenshot was not taken"),
            },
            Ask::StartRecording(take) => self.start(take, compositor),
            Ask::PauseRecording => {
                if let Some(feeding) = &mut self.feeding {
                    feeding.paused = true;
                    feeding.recorder.pause();
                }
                if let Some(recording) = &mut self.recording {
                    recording.paused = true;
                }
            }
            Ask::ResumeRecording => {
                if let Some(feeding) = &mut self.feeding {
                    feeding.paused = false;
                    feeding.recorder.resume();
                }
                if let Some(recording) = &mut self.recording {
                    recording.paused = false;
                }
            }
            Ask::StopRecording => self.stop(),
            Ask::StartReplay(take) => self.start_replay(take, compositor),
            Ask::SaveReplay => self.save_replay(),
            Ask::StopReplay => {
                if self
                    .feeding
                    .as_ref()
                    .is_some_and(|feeding| feeding.path.is_none())
                {
                    self.feeding = None;
                }
                self.replay = None;
            }
            Ask::OpenLast => self.open(false),
            Ask::RevealLast => self.open(true),
            Ask::CopyLast => {
                if let Some(kept) = &self.last
                    && let Err(error) = copy(kept, compositor)
                {
                    tracing::warn!(%error, "a screenshot was not copied again");
                }
            }
        }
    }

    fn start(&mut self, take: Take, compositor: &mut Compositor) {
        let path = unique(&self.directory, "recording", "mp4");
        let frame = match grab(compositor, &take) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "a recording was not started");
                return;
            }
        };
        let recorder = match Recorder::new(Settings::for_size(frame.width, frame.height)) {
            Ok(recorder) => recorder,
            Err(error) => {
                tracing::warn!(%error, "no encoder takes this screen's size");
                return;
            }
        };
        let mut recorder = recorder;
        if let Err(error) = recorder.start(path.clone()) {
            tracing::warn!(%error, "a recording file was not opened");
            return;
        }
        if let Err(error) = recorder.frame(&frame) {
            tracing::warn!(%error, "the first frame of a recording was not written");
            let _ = recorder.stop();
            return;
        }
        self.feeding = Some(Feeding {
            take,
            recorder,
            path: Some(path.clone()),
            last: Some(Instant::now()),
            paused: false,
        });
        self.recording = Some(Recording {
            path,
            paused: false,
        });
    }

    fn stop(&mut self) {
        let Some(feeding) = self.feeding.take() else {
            return;
        };
        if feeding.path.is_none() {
            // Only the replay buffer was fed.
            self.feeding = Some(feeding);
            return;
        }
        let mut recorder = feeding.recorder;
        match recorder.stop() {
            Ok(Some(path)) => tracing::info!(path = %path.display(), "a recording was finished"),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "a recording was not finished"),
        }
        self.recording = None;
    }

    fn start_replay(&mut self, take: Take, compositor: &mut Compositor) {
        if self.feeding.is_some() {
            // One encode feeds a recording and the replay buffer together.
            return;
        }
        let frame = match grab(compositor, &take) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "the replay buffer was not armed");
                return;
            }
        };
        let mut recorder = match Recorder::new(Settings::for_size(frame.width, frame.height)) {
            Ok(recorder) => recorder,
            Err(error) => {
                tracing::warn!(%error, "no encoder takes this screen's size");
                return;
            }
        };
        if let Err(error) = recorder.frame(&frame) {
            tracing::warn!(%error, "the replay buffer took no frame");
            return;
        }
        self.feeding = Some(Feeding {
            take,
            recorder,
            path: None,
            last: Some(Instant::now()),
            paused: false,
        });
        self.replay = Some(0);
    }

    fn save_replay(&mut self) {
        let Some(feeding) = self.feeding.as_mut() else {
            return;
        };
        let path = unique(&self.directory, "replay", "mp4");
        match feeding.recorder.save_replay(path) {
            Ok(frames) => {
                tracing::info!(frames, "a clip of what just happened was saved");
                self.replay_saves = self.replay_saves.wrapping_add(1);
            }
            Err(error) => tracing::warn!(%error, "a clip of what just happened was not saved"),
        }
    }

    /// Take one frame into the recorder when it is due.
    fn feed(&mut self, compositor: &mut Compositor) {
        let Some(feeding) = self.feeding.as_mut() else {
            return;
        };
        if feeding.paused {
            return;
        }
        let now = Instant::now();
        if feeding
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < FRAME_INTERVAL)
        {
            return;
        }
        feeding.last = Some(now);
        let frame = match grab(compositor, &feeding.take) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "a frame of the recording was not read");
                return;
            }
        };
        if let Err(error) = feeding.recorder.frame(&frame) {
            tracing::warn!(%error, "a frame of the recording was not encoded");
            if feeding.path.take().is_some() {
                let _ = feeding.recorder.stop();
                self.recording = None;
            }
            self.feeding = None;
            self.replay = None;
            return;
        }
        if feeding.path.is_none() {
            let seconds = feeding.recorder.buffered() / usize::try_from(REPLAY_FPS).unwrap_or(30);
            self.replay = Some(u8::try_from(seconds.min(30)).unwrap_or(30));
        }
    }

    /// Take a screenshot of `take` and keep it: a file, and the clipboard
    /// when asked.
    fn screenshot(&self, take: &Take, compositor: &mut Compositor) -> Result<Kept, String> {
        let frame = grab(compositor, take)?;
        std::fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let path = unique(&self.directory, "screenshot", "png");
        let picture = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
            .ok_or("a frame of the wrong size")?;
        picture
            .save_with_format(&path, image::ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        let kept = Kept {
            kept: ScreenshotKept::SAVED,
            path,
            width: frame.width,
            height: frame.height,
            preview: preview(&frame),
        };
        if take.destination != CaptureDestination::File {
            copy(&kept, compositor)?;
            return Ok(Kept {
                kept: ScreenshotKept::SAVED_AND_COPIED,
                ..kept
            });
        }
        Ok(kept)
    }

    /// Open the last screenshot in the person's image app, or show it in
    /// Files.
    fn open(&self, reveal: bool) {
        let Some(kept) = &self.last else {
            return;
        };
        let path = if reveal {
            kept.path.parent().unwrap_or_else(|| Path::new("."))
        } else {
            kept.path.as_path()
        };
        if let Err(error) = std::process::Command::new("xdg-open").arg(path).spawn() {
            tracing::warn!(%error, path = %path.display(), "a screenshot was not opened");
        }
    }

    /// Hand the studio what it shows now.
    fn publish(&self, compositor: &Compositor) {
        let scene = Scene {
            windows: windows(compositor),
            system_audio: false,
            microphone: false,
            kept: self.last.clone(),
            recording: self.recording.clone(),
            replay: self.replay,
            replay_saves: self.replay_saves,
            ..Scene::default()
        };
        self.seat.link().publish(scene);
    }
}

/// Put `kept`'s file on the clipboard as an image.
fn copy(kept: &Kept, compositor: &mut Compositor) -> Result<(), String> {
    let png = std::fs::read(&kept.path).map_err(|error| error.to_string())?;
    compositor
        .set_selection(Selection::Clipboard, vec![(String::from("image/png"), png)])
        .map_err(|error| error.to_string())
}

/// The windows a capture may take, as the studio lists them.
fn windows(compositor: &Compositor) -> Vec<CaptureTarget> {
    let focused = compositor.focused_shareable_window();
    compositor
        .shareable_windows()
        .into_iter()
        .filter_map(|(id, app_id, title)| {
            let (x, y, width, height) = compositor.shareable_window_rect(&id)?;
            let rect = Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x.saturating_add(width)),
                f64::from(y.saturating_add(height)),
            );
            Some(CaptureTarget {
                focused: focused.as_deref() == Some(id.as_str()),
                id,
                title,
                app_id: app_id.unwrap_or_default(),
                visible_rect: rect,
                virtual_rect: rect,
                complete_rect: None,
                on_active_strip: true,
            })
        })
        .collect()
}

/// A frame of what `take` names, from the compositor's own rendering.
fn grab(compositor: &mut Compositor, take: &Take) -> Result<Frame, String> {
    match take.source {
        SourceMode::Display => {
            let picture = crate::capture::picture(compositor)?;
            let (width, height) = picture.dimensions();
            Frame::new(width, height, picture.into_raw())
        }
        SourceMode::Window | SourceMode::Strip => {
            let targets = if take.targets.is_empty() {
                match compositor.focused_shareable_window() {
                    Some(focused) => vec![focused],
                    None => return Err("no window has focus to capture".to_owned()),
                }
            } else {
                take.targets.clone()
            };
            let shots: Vec<Screenshot> = targets
                .iter()
                .filter_map(|id| {
                    compositor
                        .read_back(&Target::window(id.clone()))
                        .map_err(
                            |error| tracing::warn!(window = %id, %error, "a window was not read"),
                        )
                        .ok()
                })
                .collect();
            compose(shots)
        }
        SourceMode::Region if take.regions.is_empty() => {
            Err("no selection was made to capture".to_owned())
        }
        SourceMode::Region => regions(compositor, &take.regions),
    }
}

/// The selections taken from the output the studio is over, next to each
/// other as they were made.
fn regions(compositor: &mut Compositor, regions: &[Rect]) -> Result<Frame, String> {
    let output = compositor
        .outputs()
        .into_iter()
        .next()
        .ok_or("no display is on")?;
    let (_, _, width, _) = output.rect.ok_or("no display is on")?;
    let whole = compositor
        .read_back(&Target::output(output.name.clone()))
        .map_err(|error| error.to_string())?;
    // What is captured is in the studio's own pixels: the frame is in the
    // output's.
    let scale = f64::from(whole.width) / f64::from(width.max(1));
    let mut shots = Vec::new();
    for region in regions {
        let shot = compositor
            .read_back_region(&output.name, physical(*region, scale))
            .map_err(|error| error.to_string())?;
        shots.push(shot);
    }
    compose(shots)
}

/// `region` in an output's physical pixels: its corner and its size.
fn physical(region: Rect, scale: f64) -> (i32, i32, i32, i32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a screen coordinate in pixels is far from i32's limits"
    )]
    let round = |value: f64| -> i32 { (value * scale).round() as i32 };
    (
        round(region.x0),
        round(region.y0),
        round(region.width()).max(1),
        round(region.height()).max(1),
    )
}

/// `shots` side by side, as one frame.
fn compose(mut shots: Vec<Screenshot>) -> Result<Frame, String> {
    if shots.is_empty() {
        return Err("nothing could be captured".to_owned());
    }
    let width = shots.iter().map(|shot| shot.width).sum::<u32>();
    let height = shots.iter().map(|shot| shot.height).max().unwrap_or(0);
    if width == 0 || height == 0 {
        return Err("nothing could be captured".to_owned());
    }
    if shots.len() == 1 {
        let shot = shots.swap_remove(0);
        return Frame::new(shot.width, shot.height, shot.rgba);
    }
    let mut pixels =
        vec![0_u8; usize::try_from(width).unwrap_or(0) * usize::try_from(height).unwrap_or(0) * 4];
    let mut at = 0;
    for shot in shots {
        for row in 0..shot.height {
            let from = usize::try_from((row * shot.width) * 4).unwrap_or(0);
            let to = usize::try_from(((row * width) + at) * 4).unwrap_or(0);
            let bytes = usize::try_from(shot.width * 4).unwrap_or(0);
            let Some(source) = shot.rgba.get(from..from.saturating_add(bytes)) else {
                continue;
            };
            let Some(target) = pixels.get_mut(to..to.saturating_add(bytes)) else {
                continue;
            };
            target.copy_from_slice(source);
        }
        at = at.saturating_add(shot.width);
    }
    Frame::new(width, height, pixels)
}

/// The thumbnail a receipt shows of `frame`.
fn preview(frame: &Frame) -> CapturePreview {
    let (columns, rows) = PREVIEW;
    let mut pixels = Vec::with_capacity(usize::from(columns) * usize::from(rows));
    for row in 0..u32::from(rows) {
        for column in 0..u32::from(columns) {
            let x = (column * frame.width / u32::from(columns)).min(frame.width.saturating_sub(1));
            let y = (row * frame.height / u32::from(rows)).min(frame.height.saturating_sub(1));
            let at = usize::try_from(((y * frame.width) + x) * 4).unwrap_or(0);
            let pixel = frame
                .rgba
                .get(at..at.saturating_add(4))
                .unwrap_or(&[0, 0, 0, 255]);
            pixels.push([pixel[0], pixel[1], pixel[2], pixel[3]]);
        }
    }
    CapturePreview {
        columns,
        rows,
        pixels,
    }
}

/// A fresh file name under `directory`.
fn unique(directory: &Path, kind: &str, extension: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    directory.join(format!("{kind}-{stamp}.{extension}"))
}

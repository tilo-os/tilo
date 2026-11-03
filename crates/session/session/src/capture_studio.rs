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

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_capture_studio::audio::{Availability, Track as AudioTrack};
use tilo_capture_studio::{
    Ask, Frame, Kept, REPLAY_FPS, Recorder, Recording, Refusal, Scene, Selection, Sent, Settings,
    Studio, Take,
};
use tilo_compositor::capture::Target;
use tilo_compositor::{Compositor, Screenshot};
use tilo_portal::CaptureError;
use tilo_surface_host::Doors;
use ui::vello::kurbo::Rect;
use ui::widget::{
    AudioCaptureMode, CaptureDestination, CapturePreview, CaptureTarget, ScreenshotKept,
    ScreenshotSendDevice, SourceMode,
};

use crate::Options;
use crate::capture_send::Sends;
use crate::nearby_transfer::NearbyTransfer;
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
    /// The session's transfer engine, when this session runs on a device
    /// fabric: who a screenshot may be sent to.
    nearby: Option<Arc<NearbyTransfer>>,
    /// The screenshots on their way to those devices.
    sends: Sends,
    /// What became of sending the last screenshot, while its receipt shows
    /// that.
    sent: Option<Sent>,
    /// An app's screenshot of what only the person can name, while it waits
    /// for them: the studio is up choosing exactly that.
    asking: Option<Asked>,
    /// What feeds the recorder now: a recording, or the replay buffer.
    feeding: Option<Feeding>,
    /// What the machine can record, probed once the bar first comes up: a
    /// session that never records opens no probe.
    audio: Cell<Option<Availability>>,
    /// The recording that runs, as the studio shows it.
    recording: Option<Recording>,
    /// Seconds of the last half minute ready to clip.
    replay: Option<u8>,
    /// How many clips the replay buffer gave.
    replay_saves: u64,
}

/// A screenshot an app asked the person for.
struct Asked {
    /// The request it belongs to, so a request the app closed takes only its
    /// own selection down.
    id: u64,
    /// What the person is asked to select.
    selection: Selection,
    /// Where the picture is written: the portal's own folder, never the
    /// person's.
    directory: PathBuf,
    reply: tokio::sync::oneshot::Sender<Result<PathBuf, CaptureError>>,
}

/// Frames going into a recorder.
struct Feeding {
    take: Take,
    recorder: Recorder,
    /// The sound going into it, in the recorder's track order.
    audio: Vec<AudioTrack>,
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
    nearby: Option<&Arc<NearbyTransfer>>,
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
                sends: Sends::new(
                    nearby.cloned(),
                    tilo_paths::workspace_root(),
                    options.wake.handle(),
                ),
                nearby: nearby.cloned(),
                sent: None,
                asking: None,
                feeding: None,
                audio: Cell::new(None),
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

    /// Open the studio on what an app asked for, as the person's own shortcut
    /// opens it: the bar comes up choosing that kind of capture.
    pub(crate) fn open_asked(&self, mode: ui::floating_surface::CaptureStudioProtocolMode) {
        use ui::floating_surface::CaptureStudioProtocolMode as Mode;
        use ui::widget::CaptureMode;
        let mode = match mode {
            Mode::Screenshot => CaptureMode::Screenshot,
            Mode::Recording => CaptureMode::Recording,
            Mode::PictureInPicture => CaptureMode::Pip,
        };
        self.seat.link().open_on(mode);
        self.presentation().open(false);
    }

    /// Answer what the studio asked since the last pass, feed the recorder
    /// and hand back what came of it.
    pub(crate) fn serve(&mut self, compositor: &mut Compositor) {
        if let Some(sent) = self.sends.take() {
            self.sent = Some(sent);
        }
        for ask in self.seat.link().take_asks() {
            self.answer(ask, compositor);
        }
        // Asked after what the person took, so a screenshot that settled an
        // app's request is an answer rather than a selection left closed.
        self.cancel_asking();
        self.feed(compositor);
        self.publish(compositor);
    }

    /// Ask the person for what an app's screenshot of `target` needs: the
    /// window they pick or the area they draw. The app is answered once
    /// they took it, or that it was cancelled when they close the bar.
    pub(crate) fn ask_selection(
        &mut self,
        target: tilo_portal::CaptureTarget,
        id: u64,
        reply: tokio::sync::oneshot::Sender<Result<PathBuf, CaptureError>>,
        directory: PathBuf,
    ) {
        let Some(selection) = selection_of(target) else {
            let _ = reply.send(Err(CaptureError::Failed(format!(
                "a {target:?} is not the person's to take"
            ))));
            return;
        };
        if self.asking.is_some() {
            // One at a time: the person is already choosing for another app,
            // and their choice would answer neither well.
            let _ = reply.send(Err(CaptureError::Failed(
                "another screenshot selection is already open".to_owned(),
            )));
            return;
        }
        self.asking = Some(Asked {
            id,
            selection,
            directory,
            reply,
        });
        self.presentation().open(false);
    }

    /// Request `id` is not waited for any more: close what is up for it.
    pub(crate) fn withdraw(&mut self, id: u64) {
        if self.asking.as_ref().is_none_or(|asked| asked.id != id) {
            return;
        }
        self.asking = None;
        self.presentation().close();
    }

    /// The person closed the selector without taking anything: the app is
    /// told it was cancelled rather than left waiting.
    fn cancel_asking(&mut self) {
        if self.asking.is_none() || self.presentation().is_drawn() {
            return;
        }
        let Some(asked) = self.asking.take() else {
            return;
        };
        let _ = asked.reply.send(Err(CaptureError::Cancelled));
    }

    fn answer(&mut self, ask: Ask, compositor: &mut Compositor) {
        match ask {
            Ask::Screenshot(take) => self.screenshot_asked(&take, compositor),
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
            Ask::SendLastTo(id) => self.send_last_to(&id),
            Ask::SendLastToWorkspace => self.send_last_to_workspace(),
        }
    }

    /// Offer the last screenshot to one of the person's other devices, by the
    /// id the studio listed: the screenshot the studio wrote, never what a
    /// private window showed.
    fn send_last_to(&mut self, id: &str) {
        // A new send: what was said of the last one is not about this one.
        self.sent = None;
        let Some(kept) = &self.last else {
            self.sent = Some(Sent::refused(id.to_owned(), Refusal::NotKept));
            return;
        };
        self.sent = self.sends.send(id, kept.path.clone());
    }

    /// Copy the last screenshot into this workspace's files, under
    /// `Screenshots`, where the person's collaborators see it: the screenshot
    /// the studio wrote, never what a private window showed.
    fn send_last_to_workspace(&mut self) {
        // A new send: what was said of the last one is not about this one.
        self.sent = None;
        let Some(kept) = &self.last else {
            self.sent = Some(Sent::not_saved(Refusal::NotKept));
            return;
        };
        self.sent = self.sends.send_to_workspace(kept.path.clone());
    }

    fn start(&mut self, take: Take, compositor: &mut Compositor) {
        let audio = match AudioTrack::start(take.audio) {
            Ok(audio) => audio,
            Err(error) => {
                tracing::warn!(%error, "a recording's sound was not started");
                return;
            }
        };
        let frame = match grab(compositor, &take, self.presentation().place()) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "a recording was not started");
                return;
            }
        };
        let mut recorder = match Recorder::with_audio(
            Settings::for_size(frame.width, frame.height),
            audio.iter().map(|track| track.name().to_owned()).collect(),
        ) {
            Ok(recorder) => recorder,
            Err(error) => {
                tracing::warn!(%error, "no encoder takes this screen's size");
                return;
            }
        };
        let path = crate::capture::unique(&self.directory, "recording", recorder.extension());
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
            audio,
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
        let Some(mut feeding) = self.feeding.take() else {
            return;
        };
        if feeding.path.is_none() {
            // Only the replay buffer was fed.
            self.feeding = Some(feeding);
            return;
        }
        // What was heard up to the stop is in the file too.
        feeding.drain_audio(true);
        // The recorders end with the file: Drop kills each process group.
        feeding.audio.clear();
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
        let frame = match grab(compositor, &take, self.presentation().place()) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "the replay buffer was not armed");
                return;
            }
        };
        let audio = match AudioTrack::start(take.audio) {
            Ok(audio) => audio,
            Err(error) => {
                tracing::warn!(%error, "the replay buffer's sound was not started");
                return;
            }
        };
        let mut recorder = match Recorder::with_audio(
            Settings::for_size(frame.width, frame.height),
            audio.iter().map(|track| track.name().to_owned()).collect(),
        ) {
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
            audio,
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
        let path = crate::capture::unique(&self.directory, "replay", "mp4");
        match feeding.recorder.save_replay(path) {
            Ok(frames) => {
                tracing::info!(frames, "a clip of what just happened was saved");
                self.replay_saves = self.replay_saves.wrapping_add(1);
            }
            Err(error) => tracing::warn!(%error, "a clip of what just happened was not saved"),
        }
    }

    /// When the recorder next takes a frame: only a buffer that runs and is
    /// not paused feeds itself.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let feeding = self.feeding.as_ref().filter(|feeding| !feeding.paused)?;
        let now = Instant::now();
        Some(
            feeding
                .last
                .and_then(|last| last.checked_add(FRAME_INTERVAL))
                .unwrap_or(now),
        )
    }

    /// Take one frame into the recorder when it is due.
    fn feed(&mut self, compositor: &mut Compositor) {
        let place = self.presentation().place();
        let Some(feeding) = self.feeding.as_mut() else {
            return;
        };
        if feeding.paused {
            // The pause is not recorded; its sound is not either.
            feeding.drain_audio(false);
            return;
        }
        let now = Instant::now();
        if feeding
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < FRAME_INTERVAL)
        {
            feeding.drain_audio(true);
            return;
        }
        feeding.last = Some(now);
        let frame = match grab(compositor, &feeding.take, place) {
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
        feeding.drain_audio(true);
        if feeding.path.is_none() {
            let seconds = feeding.recorder.buffered() / usize::try_from(REPLAY_FPS).unwrap_or(30);
            self.replay = Some(u8::try_from(seconds.min(30)).unwrap_or(30));
        }
    }

    /// A screenshot the person took: what an app asked for, when it asked
    /// for exactly that, or the studio's own.
    fn screenshot_asked(&mut self, take: &Take, compositor: &mut Compositor) {
        let asking = self
            .asking
            .as_ref()
            .is_some_and(|asked| asked.selection.source() == take.source);
        if asking {
            let Some(asked) = self.asking.take() else {
                return;
            };
            let kept = keep(
                &asked.directory,
                compositor,
                take,
                self.presentation().place(),
            );
            let _ = asked.reply.send(kept);
            return;
        }
        match self.screenshot(take, compositor) {
            Ok(kept) => {
                self.last = Some(kept);
                self.sent = None;
            }
            Err(error) => tracing::warn!(%error, "a screenshot was not taken"),
        }
    }

    /// Take a screenshot of `take` and keep it: a file, and the clipboard
    /// when asked.
    fn screenshot(&self, take: &Take, compositor: &mut Compositor) -> Result<Kept, String> {
        let frame = grab(compositor, take, self.presentation().place())?;
        let picture = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
            .ok_or("a frame of the wrong size")?;
        let path = crate::capture::keep(&self.directory, &picture)?;
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

    /// What this machine can record, probed when the bar is first up: the
    /// person's toggles offer only sounds that would start.
    fn audio_availability(&self) -> Availability {
        if let Some(audio) = self.audio.get() {
            return audio;
        }
        if !self.presentation().is_drawn() {
            return Availability::default();
        }
        let audio = tilo_capture_studio::audio::probe();
        self.audio.set(Some(audio));
        audio
    }

    /// The sound the replay buffer takes: the one its feed was started with.
    fn replay_audio(&self) -> AudioCaptureMode {
        self.feeding
            .as_ref()
            .filter(|feeding| feeding.path.is_none())
            .map_or(AudioCaptureMode::None, |feeding| feeding.take.audio)
    }

    /// Hand the studio what it shows now.
    fn publish(&self, compositor: &Compositor) {
        let devices: Vec<ScreenshotSendDevice> = self
            .nearby
            .as_ref()
            .map_or_else(Vec::new, |nearby| nearby.devices());
        let audio = self.audio_availability();
        let scene = Scene {
            windows: windows(compositor, self.presentation().place()),
            asking: self.asking.as_ref().map(|asked| asked.selection),
            system_audio: audio.system,
            microphone: audio.microphone,
            kept: self.last.clone(),
            devices,
            sent: self.sent.clone(),
            recording: self.recording.clone(),
            replay: self.replay,
            replay_audio: self.replay_audio(),
            replay_saves: self.replay_saves,
            ..Scene::default()
        };
        self.seat.link().publish(scene);
    }
}

impl Feeding {
    /// Take the sound captured since the last pass into the file, in the
    /// recorder's track order: with `keep` false — a pause — it is discarded,
    /// so the recording resumes at the current sound. A source that ended is
    /// dropped with a warning rather than written as a silent gap, and its
    /// track keeps its place.
    fn drain_audio(&mut self, keep: bool) {
        let mut ended = Vec::new();
        for (index, track) in self.audio.iter_mut().enumerate() {
            match track.drain(keep) {
                Ok(pcm) if keep && !pcm.is_empty() => {
                    if let Err(error) = self.recorder.audio(track.mux(), &pcm) {
                        tracing::warn!(
                            %error,
                            source = track.name(),
                            "a recording's sound was not written"
                        );
                        ended.push(index);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        source = track.name(),
                        "a recording's sound stopped"
                    );
                    ended.push(index);
                }
            }
        }
        for index in ended.into_iter().rev() {
            self.audio.remove(index);
        }
    }
}

/// Put `kept`'s file on the clipboard as an image.
fn copy(kept: &Kept, compositor: &mut Compositor) -> Result<(), String> {
    let png = std::fs::read(&kept.path).map_err(|error| error.to_string())?;
    compositor
        .set_selection(
            tilo_compositor::Selection::Clipboard,
            vec![(String::from("image/png"), png)],
        )
        .map_err(|error| error.to_string())
}

/// The windows a capture may take, as the studio lists them: where they
/// stand on the display the studio covers, which is the surface it draws on
/// and the one a press lands on.
fn windows(compositor: &Compositor, place: Option<Place>) -> Vec<CaptureTarget> {
    let focused = compositor.focused_shareable_window();
    let (left, top) = place.map_or((0, 0), |(x, y, _, _)| (x, y));
    compositor
        .shareable_windows()
        .into_iter()
        .filter_map(|(id, app_id, title)| {
            let (x, y, width, height) = compositor.shareable_window_rect(&id)?;
            let (x, y) = (x.saturating_sub(left), y.saturating_sub(top));
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

/// A display's rectangle, as the sheet it carries is placed: its corner in
/// the layout.
type Place = (i32, i32, u32, u32);

/// `take`'s picture, written as a PNG under `directory`; its path. This is
/// what an app is handed: the file stays out of the person's own folders.
fn keep(
    directory: &Path,
    compositor: &mut Compositor,
    take: &Take,
    place: Option<Place>,
) -> Result<PathBuf, CaptureError> {
    let frame = grab(compositor, take, place).map_err(CaptureError::Failed)?;
    let picture = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .ok_or_else(|| CaptureError::Failed("a frame of the wrong size".to_owned()))?;
    crate::capture::keep(directory, &picture).map_err(CaptureError::Failed)
}

/// What the studio selects for the portal's `target`.
fn selection_of(target: tilo_portal::CaptureTarget) -> Option<Selection> {
    match target {
        tilo_portal::CaptureTarget::Window => Some(Selection::Window),
        tilo_portal::CaptureTarget::Area => Some(Selection::Area),
        tilo_portal::CaptureTarget::Screen | tilo_portal::CaptureTarget::ActiveWindow => None,
    }
}

/// A frame of what `take` names, from the compositor's own rendering.
fn grab(compositor: &mut Compositor, take: &Take, place: Option<Place>) -> Result<Frame, String> {
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
        SourceMode::Region => regions(compositor, &take.regions, place),
    }
}

/// The selections taken from the display the studio is over, next to each
/// other as they were made.
fn regions(
    compositor: &mut Compositor,
    regions: &[Rect],
    place: Option<Place>,
) -> Result<Frame, String> {
    // A selection is drawn where the studio is, and its coordinates start at
    // that display's corner: what is captured is that display, never another
    // one that happens to be listed first.
    let outputs = compositor.outputs();
    let covered = place
        .and_then(|(x, y, w, h)| {
            let (w, h) = (i32::try_from(w).ok()?, i32::try_from(h).ok()?);
            Some((x, y, w, h))
        })
        .and_then(|covered| outputs.iter().find(|output| output.rect == Some(covered)))
        .or_else(|| outputs.first())
        .ok_or("no display is on")?;
    let (_, _, width, _) = covered.rect.ok_or("no display is on")?;
    let whole = compositor
        .read_back(&Target::output(covered.name.clone()))
        .map_err(|error| error.to_string())?;
    // What is captured is in the studio's own pixels: the frame is in the
    // output's.
    let scale = f64::from(whole.width) / f64::from(width.max(1));
    let mut shots = Vec::new();
    for region in regions {
        let shot = compositor
            .read_back_region(&covered.name, physical(*region, scale))
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

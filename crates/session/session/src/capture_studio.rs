//! The capture studio the session shows: it captures what the bar asks of
//! it, from the compositor's own frames.
//!
//! The studio app draws and asks ([`tilo_capture_studio::Ask`]); the session
//! answers each pass, because only it has the compositor. A screenshot is
//! read back from the frames the compositor renders — the same path a share
//! stream and the portal's screenshots take, so what is captured is what the
//! desktop shows, private windows blanked — written to the person's
//! Screenshots folder and, when asked, put on the clipboard. A recording
//! feeds the same read-back into the recorder at the replay buffer's rate,
//! which writes the file and keeps the last half minute encoded.
//!
//! What came of each capture goes back to the studio as its scene
//! ([`tilo_capture_studio::Scene`]).
//!
//! Only the read-back is the session's: the encoder, the muxer and every
//! file a capture makes run on [`CaptureIo`]'s worker, which the loop hands
//! frames to without waiting for it ([`crate::capture_io`]).

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "frames are bounded by the displays' pixels, far from the integers' limits"
)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use tilo_capture_studio::audio::{Availability, Track as AudioTrack};
use tilo_capture_studio::{
    Ask, Failed, Failure, Finished, Frame, Kept, REPLAY_FPS, Recording, Refusal, Scene, Selection,
    Sent, Studio, Take, WriteCause,
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
use crate::capture_io::{CaptureIo, NO_WORKER, Note, Sound, WriteFailed, Written};
use crate::capture_send::Sends;
use crate::nearby_transfer::NearbyTransfer;
use crate::platform::surfaces::Held;
use crate::sheets::Sheet;

/// One frame every this long: the replay buffer's rate, so a recording and a clip of what just
/// happened share one encode.
const FRAME_INTERVAL: Duration = Duration::from_millis(1_000 / 30);

/// Frames dropped in a row before the person is told again that the encoder cannot keep up.
const FALLING_BEHIND: u64 = 30;

/// The columns and rows of a receipt's thumbnail.
const PREVIEW: (u8, u8) = (12, 8);

/// How wide the shot's picture is on the floating layer's card, in pixels: the card heads itself
/// with it at nine sixteenths of its width, so this stays crisp on a high-density display.
const CARD_PICTURE: u32 = 640;

/// How long the last screenshot stays on the floating layer: as long as the studio's own receipt
/// stays up on its bar.
const SHOT_SHOWN: Duration = Duration::from_millis(5_500);

/// How long a capture that could not be saved stays on the floating layer: longer than a receipt,
/// because it asks the person to read why and choose what to do with the picture instead.
const FAILED_SHOWN: Duration = Duration::from_millis(12_000);

/// The capture studio as the session runs it, and what it is doing.
pub(crate) struct CaptureStudio {
    seat: Studio,
    /// The folder screenshots and recordings are written to.
    directory: PathBuf,
    /// The one worker that encodes a recording and writes every capture's file.
    io: Rc<CaptureIo>,
    /// What became of the recorder's work, as the loop takes it up.
    notes: Receiver<Note>,
    /// The screenshots being written, in the order they were asked for.
    shots: VecDeque<Shot>,
    /// The screenshots being read back for the clipboard.
    copies: VecDeque<Receiver<Result<Vec<u8>, String>>>,
    /// The clips being cut from the replay buffer.
    clips: VecDeque<Receiver<Result<usize, String>>>,
    /// The last screenshot, for its receipt and for opening it again.
    last: Option<Kept>,
    /// The session's transfer engine, when this session runs on a device fabric: who a screenshot
    /// may be sent to.
    nearby: Option<Arc<NearbyTransfer>>,
    /// The screenshots on their way to those devices.
    sends: Sends,
    /// What became of sending the last screenshot, while its receipt shows that.
    sent: Option<Sent>,
    /// Why the last capture the person took did not come out, while the bar says so.
    failed: Option<Failed>,
    /// How many of the person's captures did not come out: what tells two alike failures apart, so
    /// taking the same capture again says it again.
    failures: u64,
    /// The run whose unread frames were already said: a capture that reads no frame says it once,
    /// not on every frame.
    unread_said: Option<u64>,
    /// An app's screenshot of what only the person can name, while it waits for them: the studio is
    /// up choosing exactly that.
    asking: Option<Asked>,
    /// What feeds the recorder now: a recording, or the replay buffer.
    feeding: Option<Feeding>,
    /// The number of the recorder that runs, a recording or the replay buffer: what the worker's
    /// notes about one are about.
    run: u64,
    /// What the machine can record, probed once the bar first comes up: a session that never
    /// records opens no probe.
    audio: Cell<Option<Availability>>,
    /// The recording that runs, as the studio shows it.
    recording: Option<Recording>,
    /// The recording that ended, while its receipt says what it made of it.
    finished: Option<Finished>,
    /// Seconds of the last half minute ready to clip.
    replay: Option<u8>,
    /// How many clips the replay buffer gave.
    replay_saves: u64,
    /// Where what the studio is doing shows while its own bar is down: the apps' live activities,
    /// which the bar's pill and the floating layer draw.
    board: Arc<runtime_collab::LiveActivities>,
    /// The serial of the last control the person pressed on that activity that the studio took up.
    pressed: u64,
    /// What the studio shows there now, so a pass that changes nothing says nothing.
    activity: Option<serde_json::Value>,
    /// The screenshot the floating layer shows, while it shows it, and the number of the shot, so
    /// the person's putting one away does not put away the next.
    shown_shot: Option<ShownShot>,
    /// How many screenshots were shown there.
    shots_shown: u64,
    /// The shot from the desk that could not be saved, while its card says so and offers to copy
    /// what it caught.
    failed_shot: Option<FailedShot>,
}

/// A screenshot being written off the loop.
struct Shot {
    /// Where its file was to go: what a failure names as the folder it could not be saved in.
    folder: PathBuf,
    /// The picture itself, when it may be copied instead: a shot taken from the desk, whose card
    /// offers that once its file could not be written.
    picture: Option<Arc<image::RgbaImage>>,
    /// What its receipt shows, when the person took it.
    receipt: Option<Receipt>,
    /// The app waiting for the file, when it asked for a picture of what only the person can name.
    reply: Option<tokio::sync::oneshot::Sender<Result<PathBuf, CaptureError>>>,
    /// What the worker wrote, taken up on a later pass.
    answer: Receiver<Result<Written, WriteFailed>>,
}

/// What the receipt of a screenshot shows once its file is written.
struct Receipt {
    width: u32,
    height: u32,
    /// The thumbnail it shows.
    preview: CapturePreview,
    /// Whether the file goes on the clipboard too.
    copy: bool,
    /// Whether the person took it from outside the studio, whose bar is down: the shot then shows
    /// on the floating layer, where the card is the only thing saying it was taken.
    floating: bool,
    /// The picture the floating layer heads the shot's card with, when it shows there: a `data:`
    /// picture.
    thumb: String,
}

/// The screenshot the floating layer is showing: what it made, the picture its card is headed with,
/// and when it was taken.
struct ShownShot {
    path: PathBuf,
    thumb: String,
    number: u64,
    at: Instant,
}

/// A screenshot the person took from the desk that could not be saved: the card says it in their
/// words, with what they can do instead, while the picture is still here to do it with.
struct FailedShot {
    /// What the person tried to keep.
    picture: Arc<image::RgbaImage>,
    /// The card's picture, a `data:` URL.
    thumb: String,
    /// Why it was not saved, in their words.
    words: String,
    /// How many shots have shown, so putting this one away does not put away the next.
    number: u64,
    /// When it was taken: the card goes away once it has been up long enough.
    at: Instant,
}

/// A screenshot an app asked the person for.
struct Asked {
    /// The request it belongs to, so a request the app closed takes only its own selection down.
    id: u64,
    /// What the person is asked to select.
    selection: Selection,
    /// Where the picture is written: the portal's own folder, never the person's.
    directory: PathBuf,
    reply: tokio::sync::oneshot::Sender<Result<PathBuf, CaptureError>>,
}

/// Frames going into a recorder.
struct Feeding {
    take: Take,
    /// The sound going into it, in the recorder's track order.
    audio: Vec<AudioTrack>,
    /// The file written: the recording's, or `None` while only the replay buffer is fed.
    path: Option<PathBuf>,
    last: Option<Instant>,
    /// How long it has recorded, the paused time left out: what the floating layer counts while the
    /// studio's bar is down.
    elapsed: Duration,
    paused: bool,
    /// A source of its sound stopped: the bar says the recording carries no more of it, once.
    sound_lost: bool,
}

/// Mount the capture studio over the desktop, and hand back the session's end of it.
pub(crate) fn mount(
    compositor: &mut Compositor,
    doors: &mut Doors,
    options: &Options,
    nearby: Option<&Arc<NearbyTransfer>>,
    io: Rc<CaptureIo>,
    notes: Receiver<Note>,
    board: &Arc<runtime_collab::LiveActivities>,
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
            compositor.add_host(Box::new(Held::new(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(CaptureStudio {
                seat,
                directory: options.pictures.clone().unwrap_or_else(screenshots),
                io,
                notes,
                shots: VecDeque::new(),
                copies: VecDeque::new(),
                clips: VecDeque::new(),
                last: None,
                sends: Sends::new(
                    nearby.cloned(),
                    tilo_paths::workspace_root(),
                    options.wake.handle(),
                ),
                nearby: nearby.cloned(),
                sent: None,
                failed: None,
                failures: 0,
                unread_said: None,
                asking: None,
                feeding: None,
                run: 0,
                audio: Cell::new(None),
                recording: None,
                finished: None,
                replay: None,
                replay_saves: 0,
                board: Arc::clone(board),
                pressed: 0,
                activity: None,
                shown_shot: None,
                shots_shown: 0,
                failed_shot: None,
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

    /// Open the studio on what an app asked for, as the person's own shortcut opens it: the bar
    /// comes up choosing that kind of capture.
    pub(crate) fn open_asked(&self, mode: ui::floating_surface::CaptureStudioProtocolMode) {
        self.seat.link().open_on(mode.into());
        self.presentation().open(false);
    }

    /// Answer what the studio asked since the last pass, feed the recorder and hand back what came
    /// of it.
    pub(crate) fn serve(&mut self, compositor: &mut Compositor) {
        if let Some(sent) = self.sends.take() {
            self.sent = Some(sent);
        }
        // What the worker made of the last pass is said before this one asks for more.
        self.take_notes();
        self.take_shots(compositor);
        self.take_copies(compositor);
        self.take_clips();
        for ask in self.seat.link().take_asks() {
            self.answer(ask, compositor);
        }
        // Asked after what the person took, so a screenshot that settled an app's request is an
        // answer rather than a selection left closed.
        self.cancel_asking();
        self.feed(compositor);
        self.tell_sound_lost();
        self.publish(compositor);
        // What the person pressed on the floating layer's card is the same ask as the button it
        // stands for.
        self.take_presses(compositor);
        self.publish_activity();
    }

    /// Take up what became of the recorder's work since the last pass.
    fn take_notes(&mut self) {
        while let Ok(note) = self.notes.try_recv() {
            match note {
                // What became of a recorder that has since been replaced says nothing about the one
                // that runs now.
                Note::Opened { run, .. }
                | Note::Frame { run, .. }
                | Note::SoundLost { run, .. }
                | Note::Stopped { run, .. }
                    if run != self.run => {}
                Note::Opened { result: Ok(()), .. } => {}
                Note::Opened {
                    result: Err(error), ..
                } => {
                    tracing::warn!(%error, "a capture was not started");
                    let keeping = self.replay_buffer_runs();
                    self.failed(if keeping {
                        Failure::ReplayNotStarted(error)
                    } else {
                        Failure::RecordingNotStarted(error)
                    });
                    self.end_feeding();
                }
                Note::Frame { error, .. } => {
                    tracing::warn!(%error, "a frame of the recording was not encoded");
                    let keeping = self.replay_buffer_runs();
                    self.failed(if keeping {
                        Failure::ReplayStopped(error)
                    } else {
                        Failure::RecordingStopped(error)
                    });
                    // Whatever was written stays in a file that is finished rather than left
                    // behind.
                    self.end_feeding();
                    self.io.stop();
                }
                Note::SoundLost { track, .. } => {
                    if let Some(feeding) = &mut self.feeding {
                        feeding.audio.retain(|audio| audio.mux() != track);
                        feeding.sound_lost = true;
                    }
                }
                Note::Stopped { ended, dropped, .. } => match ended {
                    Ok(Some(path)) => {
                        tracing::info!(path = %path.display(), dropped, "a recording was finished");
                        // The receipt says what the file is missing.
                        self.finished = Some(Finished {
                            path: Some(path),
                            skipped: dropped,
                            failure: None,
                        });
                    }
                    Ok(None) => {}
                    Err(unfinished) => {
                        // What was written is kept rather than left behind,
                        // and the receipt says why it is not whole.
                        tracing::warn!(
                            %unfinished.error,
                            path = ?unfinished.path,
                            dropped,
                            "a recording was not finished"
                        );
                        self.finished = Some(Finished {
                            path: unfinished.path,
                            skipped: dropped,
                            failure: Some(unfinished.error),
                        });
                    }
                },
            }
        }
    }

    /// Take up the screenshots the worker wrote since the last pass: the person's receipt, and the
    /// app that asked for what only they could name.
    fn take_shots(&mut self, compositor: &mut Compositor) {
        loop {
            let Some(shot) = self.shots.front() else {
                return;
            };
            let answer = match shot.answer.try_recv() {
                Ok(answer) => answer,
                // Not written yet: it and every shot after it wait for their turn on the one
                // worker.
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err(WriteFailed {
                    folder: shot.folder.clone(),
                    cause: WriteCause::Other,
                    reason: NO_WORKER.to_owned(),
                }),
            };
            let Some(shot) = self.shots.pop_front() else {
                return;
            };
            match answer {
                Ok(written) => {
                    if let Some(reply) = shot.reply {
                        let _ = reply.send(Ok(written.path));
                        continue;
                    }
                    let Some(receipt) = shot.receipt else {
                        continue;
                    };
                    if receipt.copy {
                        if let Some(png) = written.png {
                            if let Err(error) = clipboard(png, compositor) {
                                tracing::warn!(%error, "a screenshot was not copied");
                                self.failed(Failure::NotCopied(error));
                            }
                        } else {
                            tracing::warn!("a screenshot was written without its picture");
                            self.failed(Failure::NotCopied("its picture was not kept".to_owned()));
                        }
                    }
                    let path = written.path;
                    self.last = Some(Kept {
                        kept: if receipt.copy {
                            ScreenshotKept::SAVED_AND_COPIED
                        } else {
                            ScreenshotKept::SAVED
                        },
                        path: path.clone(),
                        width: receipt.width,
                        height: receipt.height,
                        preview: receipt.preview,
                    });
                    // A shot that came out supersedes the one whose card says it could not be
                    // saved.
                    self.failed_shot = None;
                    // The person took it from outside the studio: its card says so, with what it
                    // caught.
                    if receipt.floating {
                        self.shots_shown = self.shots_shown.wrapping_add(1);
                        self.shown_shot = Some(ShownShot {
                            path,
                            thumb: receipt.thumb,
                            number: self.shots_shown,
                            at: Instant::now(),
                        });
                    }
                    self.sent = None;
                }
                Err(error) => {
                    tracing::warn!(%error, "a screenshot was not written");
                    match shot.reply {
                        // An app asked for this one: it is answered, not the person, who never saw
                        // the capture.
                        Some(reply) => {
                            let _ = reply.send(Err(CaptureError::Failed(error.to_string())));
                        }
                        None => self.shot_not_saved(shot, &error),
                    }
                }
            }
        }
    }

    /// Put the screenshots read back for the clipboard on it.
    fn take_copies(&mut self, compositor: &mut Compositor) {
        while let Some(answer) = next_answer(&mut self.copies) {
            match answer {
                Ok(png) => {
                    if let Err(error) = clipboard(png, compositor) {
                        tracing::warn!(%error, "a screenshot was not copied again");
                        self.failed(Failure::NotCopied(error));
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "a screenshot was not read again");
                    self.failed(Failure::NotRead(error));
                }
            }
        }
    }

    /// Say what became of the clips the replay buffer gave.
    fn take_clips(&mut self) {
        while let Some(answer) = next_answer(&mut self.clips) {
            match answer {
                Ok(frames) => {
                    tracing::info!(frames, "a clip of what just happened was saved");
                    self.replay_saves = self.replay_saves.wrapping_add(1);
                }
                Err(error) => {
                    tracing::warn!(%error, "a clip of what just happened was not saved");
                    self.failed(Failure::ClipNotSaved(error));
                }
            }
        }
    }

    /// The recording or the replay buffer ended without a file of its own.
    fn end_feeding(&mut self) {
        self.feeding = None;
        self.recording = None;
        self.replay = None;
    }

    /// Whether what runs feeds the replay buffer rather than a recording.
    fn replay_buffer_runs(&self) -> bool {
        self.feeding
            .as_ref()
            .is_some_and(|feeding| feeding.path.is_none())
    }

    /// Say on the bar what went wrong with the capture the person took: the reason stays on the
    /// thing they touched, and a capture taken again says itself again.
    fn failed(&mut self, failure: Failure) {
        self.failures = self.failures.wrapping_add(1);
        self.failed = Some(Failed {
            number: self.failures,
            failure,
        });
    }

    /// Say that a screenshot the person took was not saved: the bar says it in their words where
    /// they touched it, and a shot taken from the desk — whose bar is down — is said on the
    /// floating layer's card, which offers to copy the picture instead.
    fn shot_not_saved(&mut self, shot: Shot, why: &WriteFailed) {
        self.failed(Failure::NotWritten {
            folder: why.folder.clone(),
            cause: why.cause,
        });
        let (Some(receipt), Some(picture)) = (shot.receipt, shot.picture) else {
            return;
        };
        if !receipt.floating {
            return;
        }
        self.shots_shown = self.shots_shown.wrapping_add(1);
        self.shown_shot = None;
        self.failed_shot = Some(FailedShot {
            picture,
            thumb: receipt.thumb,
            words: failed_words(why),
            number: self.shots_shown,
            at: Instant::now(),
        });
    }

    /// Say that what runs carries no more of a sound that stopped, once.
    fn tell_sound_lost(&mut self) {
        let lost = self
            .feeding
            .as_mut()
            .is_some_and(|feeding| std::mem::take(&mut feeding.sound_lost));
        if lost {
            self.failed(Failure::SoundLost);
        }
    }

    /// Ask the person for what an app's screenshot of `target` needs: the window they pick or the
    /// area they draw. The app is answered once they took it, or that it was cancelled when they
    /// close the bar.
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

    /// The person closed the selector without taking anything: the app is told it was cancelled
    /// rather than left waiting.
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
            Ask::StartRecording(take) => self.start(take),
            Ask::PauseRecording => self.paused(true),
            Ask::ResumeRecording => self.paused(false),
            Ask::StopRecording => self.stop(),
            Ask::StartReplay(take) => self.start_replay(take),
            Ask::SaveReplay => self.save_replay(),
            Ask::StopReplay => {
                if self
                    .feeding
                    .as_ref()
                    .is_some_and(|feeding| feeding.path.is_none())
                {
                    self.feeding = None;
                    self.io.close();
                }
                self.replay = None;
            }
            Ask::Open(path) => self.open(&path, false),
            Ask::Reveal(path) => self.open(&path, true),
            Ask::CopyLast => {
                if let Some(kept) = &self.last {
                    let read = self.io.read(kept.path.clone());
                    self.copies.push_back(read);
                }
            }
            Ask::SendLastTo(id) => self.send_last_to(&id),
            Ask::SendLastToWorkspace => self.send_last_to_workspace(),
        }
    }

    /// Hold the recorder where it is, or let it write frames again: no frame is read back while it
    /// is held, and the sound of that time is dropped rather than written as a gap.
    fn paused(&mut self, paused: bool) {
        if self.feeding.is_some() {
            self.io.paused(paused);
        }
        if let Some(feeding) = &mut self.feeding {
            feeding.paused = paused;
            // The pause is not counted: the frame after it starts the count again rather than
            // adding the time the recorder was held.
            if paused {
                feeding.last = None;
            }
        }
        if let Some(recording) = &mut self.recording {
            recording.paused = paused;
        }
    }

    /// Offer the last screenshot to one of the person's other devices, by the id the studio listed:
    /// the screenshot the studio wrote, never what a private window showed.
    fn send_last_to(&mut self, id: &str) {
        // A new send: what was said of the last one is not about this one.
        self.sent = None;
        let Some(kept) = &self.last else {
            self.sent = Some(Sent::refused(id.to_owned(), Refusal::NotKept));
            return;
        };
        self.sent = self.sends.send(id, kept.path.clone());
    }

    /// Copy the last screenshot into this workspace's files, under `Screenshots` , where the
    /// person's collaborators see it: the screenshot the studio wrote, never what a private window
    /// showed.
    fn send_last_to_workspace(&mut self) {
        // A new send: what was said of the last one is not about this one.
        self.sent = None;
        let Some(kept) = &self.last else {
            self.sent = Some(Sent::not_saved(Refusal::NotKept));
            return;
        };
        self.sent = self.sends.send_to_workspace(kept.path.clone());
    }

    fn start(&mut self, take: Take) {
        let audio = match AudioTrack::start(take.audio) {
            Ok(audio) => audio,
            Err(error) => {
                tracing::warn!(%error, "a recording's sound was not started");
                self.failed(Failure::RecordingNotStarted(error));
                return;
            }
        };
        // The first frame and the encoder are the worker's: building an encoder stalls one pass of
        // the loop, and this one has a desk to keep drawing.
        let path = crate::capture::unique(
            &self.directory,
            "Recording",
            tilo_capture_studio::extension(!audio.is_empty()),
        );
        // The file is opened by the frame the worker encodes: a folder that cannot be written is
        // said rather than shown as a recording.
        self.run = self.run.wrapping_add(1);
        let run = self.run;
        let names = audio.iter().map(|track| track.name().to_owned()).collect();
        self.io.open(run, Some(path.clone()), names);
        self.unread_said = None;
        self.feeding = Some(Feeding {
            take,
            audio,
            path: Some(path.clone()),
            last: None,
            elapsed: Duration::ZERO,
            paused: false,
            sound_lost: false,
        });
        self.recording = Some(Recording {
            path,
            paused: false,
            skipped: 0,
        });
        self.finished = None;
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
        // What was heard up to the stop is in the file too, and the recorders end with the file:
        // Drop kills each process group.
        let sound = feeding.drain_audio(true);
        self.io.sound(sound);
        feeding.audio.clear();
        self.io.stop();
        self.recording = None;
    }

    fn start_replay(&mut self, take: Take) {
        if self.feeding.is_some() {
            // One encode feeds a recording and the replay buffer together.
            return;
        }
        let audio = match AudioTrack::start(take.audio) {
            Ok(audio) => audio,
            Err(error) => {
                tracing::warn!(%error, "the replay buffer's sound was not started");
                self.failed(Failure::ReplayNotStarted(error));
                return;
            }
        };
        self.run = self.run.wrapping_add(1);
        let run = self.run;
        let names = audio.iter().map(|track| track.name().to_owned()).collect();
        self.io.open(run, None, names);
        self.unread_said = None;
        self.feeding = Some(Feeding {
            take,
            audio,
            path: None,
            last: None,
            elapsed: Duration::ZERO,
            paused: false,
            sound_lost: false,
        });
        self.finished = None;
        self.replay = Some(0);
    }

    fn save_replay(&mut self) {
        if self.feeding.is_none() {
            return;
        }
        let path = crate::capture::unique(&self.directory, "Replay", "mp4");
        let clip = self.io.save_replay(path);
        self.clips.push_back(clip);
    }

    /// When the recorder next takes a frame: only a buffer that runs and is not paused feeds
    /// itself.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let feeding = self
            .feeding
            .as_ref()
            .filter(|feeding| !feeding.paused)
            .map(|feeding| {
                feeding
                    .last
                    .and_then(|last| last.checked_add(FRAME_INTERVAL))
                    .unwrap_or_else(Instant::now)
            });
        // A shot's card goes away once it has been up long enough: the pass that takes it down is
        // due then, rather than waiting for the person to touch something.
        let shot = self
            .shown_shot
            .as_ref()
            .and_then(|shot| shot.at.checked_add(SHOT_SHOWN));
        // So does a card that says a shot could not be saved, which the person may never touch.
        let failed = self
            .failed_shot
            .as_ref()
            .and_then(|shot| shot.at.checked_add(FAILED_SHOWN));
        feeding.into_iter().chain(shot).chain(failed).min()
    }

    /// Take one frame into the recorder when it is due: read back here, and handed to the worker,
    /// which drops it rather than waiting when the encoder is behind.
    fn feed(&mut self, compositor: &mut Compositor) {
        let place = self.presentation().place();
        let io = Rc::clone(&self.io);
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
            return;
        }
        let frame = match grab(compositor, &feeding.take, place) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "a frame of the recording was not read");
                if self.unread_said != Some(self.run) {
                    self.unread_said = Some(self.run);
                    let keeping = feeding.path.is_none();
                    self.failed(if keeping {
                        Failure::ReplayStopped(error)
                    } else {
                        Failure::RecordingStopped(error)
                    });
                }
                return;
            }
        };
        // What the floating layer counts while the studio's bar is down: the time the recorder was
        // fed, never the pauses.
        if let Some(last) = feeding.last {
            feeding.elapsed += now.saturating_duration_since(last);
        }
        feeding.last = Some(now);
        let sound = feeding.drain_audio(true);
        if !io.feed(frame, sound) {
            // The desk keeps drawing; the recording says what it lost.
            let dropped = io.dropped();
            if dropped == 1 || dropped.is_multiple_of(FALLING_BEHIND) {
                tracing::warn!(
                    dropped,
                    "the encoder fell behind: a frame of the recording was dropped"
                );
            }
            return;
        }
        if feeding.path.is_none() {
            let seconds = io.buffered() / usize::try_from(REPLAY_FPS).unwrap_or(30);
            self.replay = Some(u8::try_from(seconds.min(30)).unwrap_or(30));
        }
    }

    /// A screenshot the person took: what an app asked for, when it asked for exactly that, or the
    /// studio's own.
    fn screenshot_asked(&mut self, take: &Take, compositor: &mut Compositor) {
        let asking = self
            .asking
            .as_ref()
            .is_some_and(|asked| asked.selection.source() == take.source);
        if asking {
            let Some(asked) = self.asking.take() else {
                return;
            };
            let picture = match picture(compositor, take, self.presentation().place()) {
                Ok(picture) => picture,
                Err(error) => {
                    let _ = asked.reply.send(Err(CaptureError::Failed(error)));
                    return;
                }
            };
            let answer = self
                .io
                .write(Arc::new(picture), asked.directory.clone(), false);
            self.shots.push_back(Shot {
                folder: asked.directory,
                picture: None,
                receipt: None,
                reply: Some(asked.reply),
                answer,
            });
            return;
        }
        self.screenshot(take, compositor);
    }

    /// Take a screenshot of `take` and keep it: a file, and the clipboard when asked, both written
    /// off the loop, so the receipt shows once its file is there.
    fn screenshot(&mut self, take: &Take, compositor: &mut Compositor) {
        let frame = match grab(compositor, take, self.presentation().place()) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "a screenshot was not taken");
                self.failed(Failure::NotTaken(error));
                return;
            }
        };
        let Some(picture) =
            image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
        else {
            tracing::warn!("a screenshot was not taken: a frame of the wrong size");
            self.failed(Failure::NotTaken("the frame was the wrong size".to_owned()));
            return;
        };
        let receipt = Receipt {
            width: frame.width,
            height: frame.height,
            preview: preview(&frame),
            copy: take.destination != CaptureDestination::File,
            // A shot the person took from outside the studio has no bar to show its receipt: its
            // card shows it instead.
            floating: !self.presentation().is_drawn(),
            thumb: if self.presentation().is_drawn() {
                String::new()
            } else {
                ui::held_images::hero(&picture, CARD_PICTURE)
            },
        };
        // A shot taken from the desk keeps its picture until its card is gone: a file that could
        // not be written can still be copied.
        let picture = Arc::new(picture);
        let keep = receipt.floating.then(|| Arc::clone(&picture));
        let answer = self.io.write(picture, self.directory.clone(), receipt.copy);
        self.shots.push_back(Shot {
            folder: self.directory.clone(),
            picture: keep,
            receipt: Some(receipt),
            reply: None,
            answer,
        });
    }

    /// Open the capture at `path` in the person's app for it, or show it in Files.
    fn open(&mut self, path: &Path, reveal: bool) {
        let path = if reveal {
            path.parent().unwrap_or_else(|| Path::new("."))
        } else {
            path
        };
        if let Err(error) = std::process::Command::new("xdg-open").arg(path).spawn() {
            tracing::warn!(%error, path = %path.display(), "a capture was not opened");
            self.failed(Failure::NotOpened {
                path: path.to_path_buf(),
                error: error.to_string(),
            });
        }
    }

    /// What this machine can record, probed when the bar is first up: the person's toggles offer
    /// only sounds that would start.
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
            failed: self.failed.clone(),
            // Frames the encoder was too far behind to take so far: the studio says them while the
            // recording runs and on its receipt.
            recording: self.recording.as_ref().map(|recording| Recording {
                skipped: self.io.dropped(),
                ..recording.clone()
            }),
            finished: self.finished.clone(),
            replay: self.replay,
            replay_audio: self.replay_audio(),
            replay_saves: self.replay_saves,
            ..Scene::default()
        };
        self.seat.link().publish(scene);
    }

    /// What the person pressed on the floating layer's card, as the ask the button it stands for
    /// would be.
    fn take_presses(&mut self, compositor: &mut Compositor) {
        let Some((serial, controls)) = self
            .board
            .pressed_controls(tilo_capture_studio::APP_ID, self.pressed)
        else {
            return;
        };
        self.pressed = serial;
        for control in controls {
            let Some(command) = control.get("command").and_then(serde_json::Value::as_str) else {
                continue;
            };
            self.pressed_command(command, compositor);
        }
    }

    /// One control of the card, by the id its app named it with.
    fn pressed_command(&mut self, command: &str, compositor: &mut Compositor) {
        match command {
            // The last shot's card: it acts on the shot it shows.
            "copy-shot" => self.answer(Ask::CopyLast, compositor),
            "open-shot" => {
                let path = self.shown_shot.as_ref().map(|shot| shot.path.clone());
                if let Some(path) = path {
                    self.open(&path, false);
                }
            }
            "reveal-shot" => {
                let path = self.shown_shot.as_ref().map(|shot| shot.path.clone());
                if let Some(path) = path {
                    self.open(&path, true);
                }
            }
            "dismiss-shot" => self.shown_shot = None,
            // The card of a shot that could not be saved: its picture is copied instead, without a
            // file of its own.
            "copy-failed-shot" => {
                if let Some(failed) = self.failed_shot.as_ref() {
                    let encode = self.io.encode(Arc::clone(&failed.picture));
                    self.copies.push_back(encode);
                }
            }
            "dismiss-failed-shot" => self.failed_shot = None,
            // A recording's card, and the buffer's.
            "stop" => {
                if self
                    .feeding
                    .as_ref()
                    .is_some_and(|feeding| feeding.path.is_some())
                {
                    self.answer(Ask::StopRecording, compositor);
                } else {
                    self.answer(Ask::StopReplay, compositor);
                }
            }
            "pause" => self.answer(Ask::PauseRecording, compositor),
            "resume" => self.answer(Ask::ResumeRecording, compositor),
            "save-replay" => self.answer(Ask::SaveReplay, compositor),
            other => tracing::debug!(command = other, "a capture card control nothing answers"),
        }
    }

    /// Show on the floating layer what the studio is doing, while its own bar is down: the
    /// recording or the replay buffer that runs, and the shot that was just taken. The bar's pill
    /// and the floating layer draw it, so a recording the person walked away from is still a
    /// control away.
    fn publish_activity(&mut self) {
        let activity = self.activity_now();
        if activity == self.activity {
            return;
        }
        self.activity.clone_from(&activity);
        self.board.show(tilo_capture_studio::APP_ID, activity);
    }

    /// What the floating layer shows now, if anything.
    fn activity_now(&mut self) -> Option<serde_json::Value> {
        // With the bar up the studio says it on its own status bar; the card would only say the
        // same thing twice.
        let summary = if self.presentation().is_drawn() {
            self.shown_shot = None;
            self.failed_shot = None;
            None
        } else if self
            .feeding
            .as_ref()
            .is_some_and(|feeding| feeding.path.is_some())
        {
            Some(self.recording_summary())
        } else if self
            .feeding
            .as_ref()
            .is_some_and(|feeding| feeding.path.is_none())
        {
            Some(self.replay_summary())
        } else {
            let expired = self
                .shown_shot
                .as_ref()
                .is_some_and(|shot| shot.at.elapsed() >= SHOT_SHOWN);
            if expired {
                self.shown_shot = None;
            }
            if self
                .failed_shot
                .as_ref()
                .is_some_and(|shot| shot.at.elapsed() >= FAILED_SHOWN)
            {
                self.failed_shot = None;
            }
            // What could not be saved is said before what was: a card asking the person to choose
            // something comes before one that only shows a shot.
            self.failed_shot_summary().or_else(|| self.shot_summary())
        };
        let summary = summary?;
        let published = ui::live_activity::Published {
            summary: Some(summary),
            overlay: true,
            announce: None,
        };
        serde_json::to_value(published)
            .inspect_err(|error| tracing::warn!(%error, "a capture activity does not write"))
            .ok()
    }

    /// The recording that runs, as the card says it: how long it has run and the controls that end
    /// it or hold it.
    fn recording_summary(&self) -> ui::live_activity::Summary {
        use ui::live_activity::{Control, ControlKind, Kind, Summary};
        let elapsed = self
            .feeding
            .as_ref()
            .map_or(0, |feeding| feeding.elapsed.as_secs());
        let paused = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.paused);
        Summary::new(
            Kind::Stopwatch,
            crate::i18n::t("capture-recording-title"),
            clock(elapsed),
        )
        .with_id(format!("recording-{}", self.run))
        .with_icon("circle-dot")
        .with_subtitle(self.target_line())
        .with_tone(if paused {
            ui::live_activity::Tone::Alert
        } else {
            ui::live_activity::Tone::Accent
        })
        .with_control(
            Control::new(
                "stop",
                crate::i18n::t("capture-recording-stop"),
                ControlKind::Destructive,
            )
            .with_icon("square"),
        )
        .with_control(if paused {
            Control::new(
                "resume",
                crate::i18n::t("capture-recording-resume"),
                ControlKind::Secondary,
            )
            .with_icon("play")
        } else {
            Control::new(
                "pause",
                crate::i18n::t("capture-recording-pause"),
                ControlKind::Secondary,
            )
            .with_icon("pause")
        })
        .with_control(Control::new(
            "stop",
            crate::i18n::t("capture-recording-stop"),
            ControlKind::Close,
        ))
    }

    /// The replay buffer, as the card says it: how much of the last half minute it keeps, and the
    /// controls that save it or let it go.
    fn replay_summary(&self) -> ui::live_activity::Summary {
        use ui::live_activity::{Control, ControlKind, Kind, Summary};
        let kept = self.replay.unwrap_or(0);
        Summary::new(
            Kind::Stopwatch,
            crate::i18n::t("capture-replay-title"),
            clock(u64::from(kept)),
        )
        .with_id(format!("replay-{}", self.run))
        .with_icon("history")
        .with_subtitle(self.target_line())
        .with_tone(ui::live_activity::Tone::Neutral)
        .with_control(
            Control::new(
                "save-replay",
                crate::i18n::t("capture-replay-save"),
                ControlKind::Primary,
            )
            .with_icon("download"),
        )
        .with_control(Control::new(
            "stop",
            crate::i18n::t("capture-replay-stop"),
            ControlKind::Close,
        ))
    }

    /// A shot from the desk that could not be saved, as its card says it: why in the person's
    /// words, and the one action that keeps what they caught.
    fn failed_shot_summary(&self) -> Option<ui::live_activity::Summary> {
        use ui::live_activity::{Control, ControlKind, Kind, Summary};
        let failed = self.failed_shot.as_ref()?;
        Some(
            Summary::new(Kind::Other, crate::i18n::t("capture-failed-not-saved"), "")
                .with_id(format!("failed-shot-{}", failed.number))
                .with_icon("camera")
                .with_subtitle(failed.words.clone())
                .with_tone(ui::live_activity::Tone::Alert)
                .with_hero_artwork(failed.thumb.clone())
                .with_control(
                    Control::new(
                        "copy-failed-shot",
                        crate::i18n::t("capture-failed-copy-instead"),
                        ControlKind::Primary,
                    )
                    .with_icon("copy"),
                )
                .with_control(Control::new(
                    "dismiss-failed-shot",
                    crate::i18n::t("capture-failed-dismiss"),
                    ControlKind::Close,
                )),
        )
    }

    /// The shot just taken, as the card says it, while it stays up.
    fn shot_summary(&self) -> Option<ui::live_activity::Summary> {
        use ui::live_activity::{Control, ControlKind, Kind, Summary};
        let shot = self.shown_shot.as_ref()?;
        let name = shot
            .path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        Some(
            Summary::new(Kind::Other, crate::i18n::t("capture-shot-title"), "")
                .with_id(format!("shot-{}", shot.number))
                .with_icon("camera")
                .with_subtitle(name)
                .with_hero_artwork(shot.thumb.clone())
                .with_control(
                    Control::new(
                        "copy-shot",
                        crate::i18n::t("capture-shot-copy"),
                        ControlKind::Primary,
                    )
                    .with_icon("copy"),
                )
                .with_control(
                    Control::new(
                        "open-shot",
                        crate::i18n::t("capture-shot-open"),
                        ControlKind::Secondary,
                    )
                    .with_icon("image"),
                )
                .with_control(
                    Control::new(
                        "reveal-shot",
                        crate::i18n::t("capture-shot-reveal"),
                        ControlKind::Secondary,
                    )
                    .with_icon("folder"),
                )
                .with_control(Control::new(
                    "dismiss-shot",
                    crate::i18n::t("capture-shot-dismiss"),
                    ControlKind::Close,
                )),
        )
    }

    /// What the recording or the buffer takes, in the person's words: the desktop, a window, or an
    /// area of it.
    fn target_line(&self) -> String {
        let Some(feeding) = self.feeding.as_ref() else {
            return String::new();
        };
        crate::i18n::t(match feeding.take.source {
            SourceMode::Display => "capture-target-display",
            SourceMode::Window | SourceMode::Strip => "capture-target-window",
            SourceMode::Region => "capture-target-region",
        })
    }
}

/// Why a screenshot's file was not written, in the person's words: the card says it under the title
/// that says what happened.
fn failed_words(why: &WriteFailed) -> String {
    let key = format!("capture-failed-not-saved-{}", why.cause.words());
    crate::i18n::t_args(
        &key,
        &[(
            "folder",
            tilo_capture_studio::folder_words(&why.folder).into(),
        )],
    )
}

/// Minutes and seconds, as a card counts them.
fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Feeding {
    /// Take the sound captured since the last pass into what goes to the recorder, in the
    /// recorder's track order: with `keep` false — a pause — it is discarded, so the recording
    /// resumes at the current sound. A source that ended is dropped with a warning rather than
    /// written as a silent gap.
    fn drain_audio(&mut self, keep: bool) -> Sound {
        let mut sound = Vec::new();
        let mut ended = Vec::new();
        for (index, track) in self.audio.iter_mut().enumerate() {
            match track.drain(keep) {
                Ok(pcm) if keep && !pcm.is_empty() => sound.push((track.mux(), pcm)),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        source = track.name(),
                        "a recording's sound stopped"
                    );
                    self.sound_lost = true;
                    ended.push(index);
                }
            }
        }
        for index in ended.into_iter().rev() {
            self.audio.remove(index);
        }
        sound
    }
}

/// The answer of the oldest job `queue` holds, taken off it; `None` while none has landed, so a job
/// whose answer is not there holds up the ones behind it.
fn next_answer<T>(queue: &mut VecDeque<Receiver<Result<T, String>>>) -> Option<Result<T, String>> {
    let front = queue.front()?;
    let answer = match front.try_recv() {
        Ok(answer) => answer,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Err(NO_WORKER.to_owned()),
    };
    queue.pop_front();
    Some(answer)
}

/// Put `png` on the clipboard as an image.
fn clipboard(png: Vec<u8>, compositor: &mut Compositor) -> Result<(), String> {
    compositor
        .set_selection(
            tilo_compositor::Selection::Clipboard,
            vec![(String::from("image/png"), png)],
        )
        .map_err(|error| error.to_string())
}

/// The windows a capture may take, as the studio lists them: where they stand on the display the
/// studio covers, which is the surface it draws on and the one a press lands on.
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

/// A display's rectangle, as the sheet it carries is placed: its corner in the layout.
type Place = (i32, i32, u32, u32);

/// `take`'s picture, for the worker that writes it: what an app is handed,
/// in a file that stays out of the person's own folders.
fn picture(
    compositor: &mut Compositor,
    take: &Take,
    place: Option<Place>,
) -> Result<image::RgbaImage, String> {
    let frame = grab(compositor, take, place)?;
    image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .ok_or_else(|| "a frame of the wrong size".to_owned())
}

/// What the studio selects for the portal's `target` .
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

/// The selections taken from the display the studio is over, next to each other as they were made.
fn regions(
    compositor: &mut Compositor,
    regions: &[Rect],
    place: Option<Place>,
) -> Result<Frame, String> {
    // A selection is drawn where the studio is, and its coordinates start at that display's corner:
    // what is captured is that display, never another one that happens to be listed first.
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
    // What is captured is in the studio's own pixels: the frame is in the output's.
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

/// The thumbnail a receipt shows of `frame` .
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Sender, channel};

    use tilo_capture_studio::Unfinished;
    use tilo_compositor::{Compositor, OutputConfig, StackPolicy};

    use super::*;

    /// The studio over a headless compositor, keeping its captures in `pictures` , and where what
    /// became of the recorder is said: the worker's own notes go nowhere, so a test says what a
    /// failing encoder says.
    fn studio(pictures: &Path) -> (CaptureStudio, Compositor, Sender<Note>) {
        let options = Options {
            presentation: ui::headless::fixture_runner_inputs,
            pictures: Some(pictures.to_path_buf()),
            ..Options::default()
        };
        let mut compositor =
            Compositor::headless(Box::new(StackPolicy)).expect("an in-process compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 1280, 800))
            .expect("a display");
        let mut doors = Doors::default();
        let (io, worker_notes) = CaptureIo::new(options.wake.handle());
        let (said, notes) = channel();
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let studio = mount(
            &mut compositor,
            &mut doors,
            &options,
            None,
            io,
            notes,
            &board,
        )
        .expect("the studio mounts");
        drop(worker_notes);
        (studio, compositor, said)
    }

    /// A recording the person started and stopped, and the file it asked for.
    fn stopped_recording(studio: &mut CaptureStudio) -> (u64, PathBuf) {
        studio.start(Take::default());
        let recording = studio.recording.clone().expect("the recording runs");
        let run = studio.run;
        studio.stop();
        (run, recording.path)
    }

    /// A recording whose file the encoder could not finish is said on the studio it was started
    /// from, with the file it kept or that it kept none: the person sees what became of it rather
    /// than nothing.
    #[test]
    fn a_recording_the_encoder_could_not_finish_says_so_and_what_it_kept() {
        let pictures = tempfile::tempdir().expect("a folder");
        let (mut studio, mut compositor, said) = studio(pictures.path());

        // What the encoder left: half a file, and why it stopped there.
        let (run, path) = stopped_recording(&mut studio);
        std::fs::write(&path, b"half a recording").expect("what was written");
        said.send(Note::Stopped {
            run,
            ended: Err(Unfinished {
                path: Some(path.clone()),
                error: "the muxer failed: Internal data stream error".to_owned(),
            }),
            dropped: 4,
        })
        .expect("the worker says what became of the recording");
        studio.serve(&mut compositor);
        assert_eq!(
            studio.seat.link().scene().finished,
            Some(Finished {
                path: Some(path),
                skipped: 4,
                failure: Some("the muxer failed: Internal data stream error".to_owned()),
            }),
            "the studio is handed the file it kept and what went wrong"
        );
        assert!(
            studio.seat.link().scene().recording.is_none(),
            "nothing is recording any more"
        );

        // A recording that wrote nothing: the same receipt, without a file.
        let (run, path) = stopped_recording(&mut studio);
        said.send(Note::Stopped {
            run,
            ended: Err(Unfinished {
                path: None,
                error: "no frame was written to the file".to_owned(),
            }),
            dropped: 0,
        })
        .expect("the worker says what became of the recording");
        studio.serve(&mut compositor);
        assert_eq!(
            studio.seat.link().scene().finished,
            Some(Finished {
                path: None,
                skipped: 0,
                failure: Some("no frame was written to the file".to_owned()),
            }),
            "a failure that kept no file says so"
        );
        assert!(!path.exists(), "nothing was written there");
    }

    /// What the floating layer shows for the studio, now.
    fn shown(studio: &CaptureStudio) -> ui::live_activity::Published {
        let activity = studio
            .board
            .activity(tilo_capture_studio::APP_ID)
            .expect("the studio says something on the floating layer");
        serde_json::from_value(activity).expect("a live activity")
    }

    /// A shot taken while the bar was down is said on the floating layer with what it caught, and
    /// the card's own controls act on it: the person sees the shot at once and can put it away from
    /// there.
    #[test]
    fn a_shot_the_bar_was_down_for_shows_a_card_with_its_picture() {
        let pictures = tempfile::tempdir().expect("a folder");
        let (mut studio, mut compositor, _said) = studio(pictures.path());
        let path = pictures.path().join("Screenshot 2026-07-30 12.00.05.png");
        let thumb = "data:image/jpeg;base64,AA==".to_owned();
        studio.last = Some(Kept {
            kept: ScreenshotKept::SAVED,
            path: path.clone(),
            width: 1280,
            height: 800,
            preview: CapturePreview {
                columns: 2,
                rows: 1,
                pixels: vec![[0, 0, 0, 255]; 2],
            },
        });
        studio.shots_shown = 1;
        studio.shown_shot = Some(ShownShot {
            path,
            thumb: thumb.clone(),
            number: 1,
            at: Instant::now(),
        });
        studio.serve(&mut compositor);

        let published = shown(&studio);
        assert!(published.overlay, "the card is kept over the windows");
        let summary = published.summary.expect("the shot");
        assert_eq!(summary.kind, ui::live_activity::Kind::Other);
        assert_eq!(summary.title, "Screenshot");
        assert_eq!(
            summary.subtitle, "Screenshot 2026-07-30 12.00.05.png",
            "the card names the file, which is what search finds it by"
        );
        assert_eq!(summary.artwork, thumb, "the card shows what was caught");
        let controls: Vec<&str> = summary
            .controls
            .iter()
            .map(|control| control.id.as_str())
            .collect();
        assert_eq!(
            controls,
            ["copy-shot", "open-shot", "reveal-shot", "dismiss-shot"]
        );

        // The person puts it away on the card.
        studio.board.press(
            tilo_capture_studio::APP_ID,
            serde_json::json!({"command": "dismiss-shot"}),
        );
        studio.serve(&mut compositor);
        assert!(
            studio.shown_shot.is_none(),
            "the card's own control acted on the shot"
        );
        assert!(
            studio.board.activity(tilo_capture_studio::APP_ID).is_none(),
            "and nothing is shown any more"
        );
    }

    /// The shot's card goes away on its own: the session comes back for it when it has been up long
    /// enough, rather than leaving it over the desk until the person touches something.
    #[test]
    fn the_shots_card_goes_away_on_its_own() {
        let pictures = tempfile::tempdir().expect("a folder");
        let (mut studio, mut compositor, _said) = studio(pictures.path());
        let path = pictures.path().join("Screenshot 2026-07-30 12.00.05.png");
        studio.shots_shown = 1;
        studio.shown_shot = Some(ShownShot {
            path,
            thumb: String::new(),
            number: 1,
            at: Instant::now(),
        });
        studio.serve(&mut compositor);
        assert!(
            studio.board.activity(tilo_capture_studio::APP_ID).is_some(),
            "the card is up with the shot"
        );
        let due = studio.next_deadline().expect("the card is due to go");
        let now = Instant::now();
        assert!(
            due <= now + SHOT_SHOWN && due + Duration::from_millis(100) >= now + SHOT_SHOWN,
            "the pass that takes it down is due when it has been up long enough"
        );

        // That pass: the card is gone, and nothing is left over the desk.
        studio.shown_shot = Some(ShownShot {
            path: pictures.path().join("gone.png"),
            thumb: String::new(),
            number: 1,
            at: Instant::now()
                .checked_sub(SHOT_SHOWN)
                .expect("the card has been up long enough"),
        });
        studio.serve(&mut compositor);
        assert!(
            studio.board.activity(tilo_capture_studio::APP_ID).is_none(),
            "the card goes away once it has been up long enough"
        );
        assert!(studio.next_deadline().is_none(), "and nothing else is due");
    }

    /// A recording that runs while the bar is down is a card that counts the time and stops where
    /// the person presses it.
    #[test]
    fn a_recording_the_bar_was_down_for_counts_its_time_and_stops_on_its_card() {
        let pictures = tempfile::tempdir().expect("a folder");
        let (mut studio, mut compositor, _said) = studio(pictures.path());
        studio.start(Take {
            source: SourceMode::Display,
            ..Take::default()
        });
        studio.serve(&mut compositor);

        let published = shown(&studio);
        let summary = published.summary.expect("the recording");
        assert_eq!(summary.kind, ui::live_activity::Kind::Stopwatch);
        assert_eq!(summary.title, "Screen recording");
        assert_eq!(summary.value, "0:00", "the card says how long it runs");
        assert_eq!(summary.subtitle, "The whole desktop");
        let controls: Vec<&str> = summary
            .controls
            .iter()
            .map(|control| control.id.as_str())
            .collect();
        assert_eq!(
            controls,
            ["stop", "pause", "stop"],
            "the card stops it and pauses it where the bar would"
        );

        studio.board.press(
            tilo_capture_studio::APP_ID,
            serde_json::json!({"command": "stop"}),
        );
        studio.serve(&mut compositor);
        assert!(
            studio.recording.is_none(),
            "the card's Stop ends the recording"
        );
        assert!(
            studio.board.activity(tilo_capture_studio::APP_ID).is_none(),
            "and the card goes with it"
        );
    }

    /// A shot from the desk whose file could not be written is said in the person's words, never
    /// the system's, and the card offers to copy what it caught while the picture is still here.
    #[test]
    fn a_shot_from_the_desk_that_could_not_be_saved_offers_to_copy_it() {
        let pictures = tempfile::tempdir().expect("a folder");
        let (mut studio, mut compositor, _said) = studio(pictures.path());
        let folder = pictures.path().join("Screenshots");
        // Nothing writes it: the worker is gone, which is a folder that took no file as far as the
        // person reads it.
        let (writer, answer) = std::sync::mpsc::channel();
        drop(writer);
        studio.shots.push_back(Shot {
            folder: folder.clone(),
            picture: Some(Arc::new(image::RgbaImage::new(4, 2))),
            receipt: Some(Receipt {
                width: 4,
                height: 2,
                preview: CapturePreview {
                    columns: 1,
                    rows: 1,
                    pixels: vec![[0, 0, 0, 255]],
                },
                copy: false,
                floating: true,
                thumb: String::from("data:image/jpeg;base64,AA=="),
            }),
            reply: None,
            answer,
        });
        studio.serve(&mut compositor);

        // The bar says it in the person's words.
        let failed = studio
            .seat
            .link()
            .scene()
            .failed
            .expect("the failure is said");
        assert!(matches!(
            failed.failure,
            Failure::NotWritten {
                cause: WriteCause::Other,
                ..
            }
        ));
        let words = tilo_capture_studio::failure_words(&failed.failure);
        assert!(
            words.starts_with("Couldn't save the screenshot") && !words.contains("os error"),
            "the bar says what it means for the person, not what the system said: {words}"
        );

        // Its card says the same, heads itself with the picture, and offers the one thing that
        // keeps it.
        let summary = shown(&studio).summary.expect("the failure's card");
        assert_eq!(summary.title, "Couldn't save the screenshot");
        assert_eq!(summary.art, ui::live_activity::Art::Hero);
        assert_eq!(summary.artwork, "data:image/jpeg;base64,AA==");
        assert!(
            summary.subtitle.contains(&folder.display().to_string()),
            "the card names the folder: {}",
            summary.subtitle
        );
        let controls: Vec<&str> = summary
            .controls
            .iter()
            .map(|control| control.id.as_str())
            .collect();
        assert_eq!(controls, ["copy-failed-shot", "dismiss-failed-shot"]);

        // Copying it instead puts the picture on its way to the clipboard without a file of its
        // own.
        studio.board.press(
            tilo_capture_studio::APP_ID,
            serde_json::json!({"command": "copy-failed-shot"}),
        );
        studio.take_presses(&mut compositor);
        assert_eq!(
            studio.copies.len(),
            1,
            "the picture is encoded to be copied"
        );

        // The card goes away by itself once it has been up long enough, and the session comes back
        // for it rather than leaving it over the desk.
        let due = studio.next_deadline().expect("the card is due to go");
        let now = Instant::now();
        assert!(
            due <= now + FAILED_SHOWN && due + Duration::from_millis(100) >= now + FAILED_SHOWN,
            "the pass that takes it down is due when it has been up long enough"
        );
        studio.board.press(
            tilo_capture_studio::APP_ID,
            serde_json::json!({"command": "dismiss-failed-shot"}),
        );
        studio.serve(&mut compositor);
        assert!(studio.failed_shot.is_none(), "the person put the card away");
        assert!(
            studio.board.activity(tilo_capture_studio::APP_ID).is_none(),
            "and nothing is shown any more"
        );
    }
}

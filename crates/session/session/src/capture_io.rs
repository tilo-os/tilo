//! Capture work that waits on an encoder or the disk, run off the session's
//! loop.
//!
//! A screenshot encodes a PNG of the whole desk and writes it into a folder
//! that may be a network mount; a recording encodes every frame it keeps,
//! muxes it into a file and, when it ends, drains the encoder and writes the
//! file's trailer. On the loop's thread any of these is a frozen desktop:
//! while a file is written nothing draws.
//!
//! [`CaptureIo`] runs that work on one `tilo-capture-io` thread, in the order
//! it was submitted, so a command that reads what an earlier one wrote finds
//! it there and the frames of a recording keep their order. The loop only
//! hands work over: frames go through a depth bound ([`DEPTH`]), and a frame
//! that arrives while the encoder already holds that many is dropped and
//! counted instead of waited on, so a slow disk shortens a recording rather
//! than stalling the desk.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_capture_studio::{Frame, Recorder, Settings, Unfinished, WriteCause};
use tilo_service::Wake;

/// Frames the encoder may hold before the loop keeps one back. One frame is being encoded and one
/// waits behind it; a third lets a writer that lags a little catch up without a visible gap in the
/// recording.
pub(crate) const DEPTH: usize = 3;

/// One track's sound, as the recorder numbers it, and the samples taken since the last drain:
/// interleaved stereo S16LE at 48 kHz.
pub(crate) type Sound = Vec<(usize, Vec<u8>)>;

/// What became of the recorder's work, taken up on the loop's next pass.
/// One that carries a `run` is about the recording started as number `run` : a note about one that
/// ended says nothing about the one running now.
pub(crate) enum Note {
    /// The recording or the replay buffer took its first frame, or why not.
    Opened {
        run: u64,
        result: Result<(), String>,
    },
    /// A frame was not encoded, and why: the recorder can take no more.
    Frame { run: u64, error: String },
    /// A track's sound ended or could not be written, by its recorder number.
    SoundLost { run: u64, track: usize },
    /// The recording's file was finished, or why it was not and what it left behind, and how many
    /// frames the encoder was too far behind to take.
    Stopped {
        run: u64,
        /// The file it finished, or why it could not be finished and the file it left. `Ok(None)` :
        /// only the replay buffer ran.
        ended: Result<Option<PathBuf>, Unfinished>,
        dropped: u64,
    },
}

/// Why a screenshot's file was not written: the folder it was to go in, the reason a person reads,
/// and what the system said for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WriteFailed {
    /// The folder the file was to be written in.
    pub folder: PathBuf,
    /// What stopped it, as the person reads it.
    pub cause: WriteCause,
    /// What the system said, in its own words: for the log and an app's own error, never for the
    /// person.
    pub reason: String,
}

impl WriteFailed {
    /// A write that failed for `error` in `folder` .
    fn new(folder: &Path, cause: WriteCause, reason: impl Into<String>) -> Self {
        Self {
            folder: folder.to_path_buf(),
            cause,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for WriteFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.folder.display(), self.reason)
    }
}

/// A picture the worker wrote.
pub(crate) struct Written {
    /// The file it is at.
    pub path: PathBuf,
    /// Its bytes, when the clipboard takes the same file.
    pub png: Option<Vec<u8>>,
}

/// One piece of capture work.
enum Job {
    /// Start run number `run` , which every note about it carries: its file goes to `file` when
    /// there is one — a recording, or only the replay buffer — and its sound is `audio` , the
    /// recorder's track names.
    /// The encoder is not built here: it waits for the first frame, which arrives on the
    /// recording's own cadence, so starting a recording stalls neither the loop that asked for it
    /// nor the work queued before it.
    Open {
        run: u64,
        file: Option<PathBuf>,
        audio: Vec<String>,
    },
    /// One frame of what is being recorded, and the sound that came with it.
    Feed {
        frame: Option<Frame>,
        sound: Sound,
    },
    Pause,
    Resume,
    /// Finish the file and end the recorder.
    Stop {
        dropped: u64,
    },
    /// End the recorder without a file: the replay buffer goes with it.
    Close,
    /// Write the replay buffer to `path` as its own clip.
    SaveReplay {
        path: PathBuf,
        done: Sender<Result<usize, String>>,
    },
    /// Write `picture` as a PNG under `directory` .
    Write {
        picture: Arc<image::RgbaImage>,
        directory: PathBuf,
        copy: bool,
        done: Sender<Result<Written, WriteFailed>>,
    },
    /// Encode `picture` as a PNG, for a clipboard that takes it instead of a file.
    Encode {
        picture: Arc<image::RgbaImage>,
        done: Sender<Result<Vec<u8>, String>>,
    },
    /// The bytes of the file at `path` .
    Read {
        path: PathBuf,
        done: Sender<Result<Vec<u8>, String>>,
    },
}

/// The session's one capture worker: the encoder, the muxer and every file a capture makes.
pub(crate) struct CaptureIo {
    /// The worker's queue, made the first time work is handed over: a session that never captures
    /// opens no thread.
    jobs: RefCell<Option<Sender<Job>>>,
    /// Whether the worker was asked to start, so one that could not is not asked again for every
    /// job.
    started: Cell<bool>,
    /// Where what became of the recorder is said.
    notes: Sender<Note>,
    /// Frames handed over that the encoder has not taken yet.
    depth: Arc<AtomicUsize>,
    /// Frames the running recorder's replay buffer holds.
    buffered: Arc<AtomicUsize>,
    /// Frames this recording dropped because the encoder was behind.
    dropped: Cell<u64>,
    /// The number of the recorder the worker was last told to start: a stop is about that one, so a
    /// recording started after it is not confused with the file it left.
    run: Cell<u64>,
    wake: Wake,
}

impl CaptureIo {
    /// The session's capture worker, and where what it makes of the recorder's work is said.
    pub(crate) fn new(wake: Wake) -> (std::rc::Rc<Self>, Receiver<Note>) {
        let (notes, said) = channel();
        (
            std::rc::Rc::new(Self {
                jobs: RefCell::new(None),
                started: Cell::new(false),
                notes,
                depth: Arc::new(AtomicUsize::new(0)),
                buffered: Arc::new(AtomicUsize::new(0)),
                dropped: Cell::new(0),
                run: Cell::new(0),
                wake,
            }),
            said,
        )
    }

    /// Start the recording numbered `run` , writing its file to `file` when there is one, with its
    /// sound named by `audio` : a recording, or only the replay buffer. The encoder is built from
    /// the first frame the worker takes, and whether it started is said as [`Note::Opened`].
    pub(crate) fn open(&self, run: u64, file: Option<PathBuf>, audio: Vec<String>) {
        self.dropped.set(0);
        self.run.set(run);
        if !self.submit(Job::Open { run, file, audio }) {
            let _ = self.notes.send(Note::Opened {
                run,
                result: Err(NO_WORKER.to_owned()),
            });
        }
    }

    /// Hand one frame and the sound that came with it to the encoder. Returns whether it took it: a
    /// frame handed over while the encoder already holds [`DEPTH`] is dropped with its sound, so
    /// what the recording keeps stays whole, and counted for [`Self::dropped`].
    pub(crate) fn feed(&self, frame: Frame, sound: Sound) -> bool {
        let held = self.depth.load(Ordering::Acquire);
        if held >= DEPTH {
            self.dropped.set(self.dropped.get().saturating_add(1));
            return false;
        }
        self.depth.store(held.saturating_add(1), Ordering::Release);
        if !self.submit(Job::Feed {
            frame: Some(frame),
            sound,
        }) {
            self.depth.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    /// Hand the encoder the sound of a pass that took no frame: what was heard just before a stop.
    pub(crate) fn sound(&self, sound: Sound) {
        if sound.is_empty() {
            return;
        }
        self.submit(Job::Feed { frame: None, sound });
    }

    /// Stop writing the file into it, keeping the recorder open, or write frames into it again.
    pub(crate) fn paused(&self, paused: bool) {
        self.submit(if paused { Job::Pause } else { Job::Resume });
    }

    /// Finish the file and end the recorder: [`Note::Stopped`] says where it is, and how many
    /// frames were dropped on the way.
    pub(crate) fn stop(&self) {
        let dropped = self.dropped.get();
        if !self.submit(Job::Stop { dropped }) {
            let _ = self.notes.send(Note::Stopped {
                run: self.run.get(),
                ended: Err(Unfinished {
                    path: None,
                    error: NO_WORKER.to_owned(),
                }),
                dropped,
            });
        }
    }

    /// End the recorder without finishing a file: the replay buffer goes with it.
    pub(crate) fn close(&self) {
        self.submit(Job::Close);
    }

    /// Write what the replay buffer holds to `path` as its own clip.
    pub(crate) fn save_replay(&self, path: PathBuf) -> Receiver<Result<usize, String>> {
        let (done, answer) = channel();
        if !self.submit(Job::SaveReplay {
            path,
            done: done.clone(),
        }) {
            let _ = done.send(Err(NO_WORKER.to_owned()));
        }
        answer
    }

    /// Write `picture` as a PNG under `directory` , named fresh; `copy` also hands the file's bytes
    /// back for the clipboard.
    pub(crate) fn write(
        &self,
        picture: Arc<image::RgbaImage>,
        directory: PathBuf,
        copy: bool,
    ) -> Receiver<Result<Written, WriteFailed>> {
        let (done, answer) = channel();
        let failed = WriteFailed::new(&directory, WriteCause::Other, NO_WORKER);
        if !self.submit(Job::Write {
            picture,
            directory,
            copy,
            done: done.clone(),
        }) {
            let _ = done.send(Err(failed));
        }
        answer
    }

    /// Encode `picture` as a PNG for the clipboard, without a file of its own: a shot that could
    /// not be saved can still be copied.
    pub(crate) fn encode(
        &self,
        picture: Arc<image::RgbaImage>,
    ) -> Receiver<Result<Vec<u8>, String>> {
        let (done, answer) = channel();
        if !self.submit(Job::Encode {
            picture,
            done: done.clone(),
        }) {
            let _ = done.send(Err(NO_WORKER.to_owned()));
        }
        answer
    }

    /// The bytes of the file at `path` , for the clipboard.
    pub(crate) fn read(&self, path: PathBuf) -> Receiver<Result<Vec<u8>, String>> {
        let (done, answer) = channel();
        if !self.submit(Job::Read {
            path,
            done: done.clone(),
        }) {
            let _ = done.send(Err(NO_WORKER.to_owned()));
        }
        answer
    }

    /// How many frames the running recorder's replay buffer holds.
    pub(crate) fn buffered(&self) -> usize {
        self.buffered.load(Ordering::Acquire)
    }

    /// Frames dropped since this recording began because the encoder was behind.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.get()
    }

    fn submit(&self, job: Job) -> bool {
        self.worker().is_some_and(|jobs| jobs.send(job).is_ok())
    }

    /// The worker's queue, started the first time it is asked for; `None` when no thread can run
    /// it.
    fn worker(&self) -> Option<Sender<Job>> {
        let mut jobs = self.jobs.borrow_mut();
        if jobs.is_none() && !self.started.get() {
            self.started.set(true);
            let (submitted, waiting) = channel();
            let notes = self.notes.clone();
            let depth = Arc::clone(&self.depth);
            let buffered = Arc::clone(&self.buffered);
            let wake = Arc::clone(&self.wake);
            let spawned = std::thread::Builder::new()
                .name("tilo-capture-io".into())
                .spawn(move || {
                    Worker {
                        jobs: waiting,
                        notes,
                        depth,
                        buffered,
                        wake,
                        recorder: None,
                        opening: None,
                        run: 0,
                    }
                    .run();
                });
            match spawned {
                Ok(_) => *jobs = Some(submitted),
                Err(error) => tracing::warn!(%error, NO_WORKER),
            }
        }
        jobs.clone()
    }
}

/// What a capture is refused with when no thread runs it.
pub(crate) const NO_WORKER: &str = "no thread encodes or writes a capture";

/// The worker: one recorder for as long as it runs, and every file a capture makes, in the order
/// the loop handed them over.
struct Worker {
    /// The loop's queue.
    jobs: Receiver<Job>,
    /// Where what became of the recorder is said.
    notes: Sender<Note>,
    /// Frames handed over that the encoder has not taken yet.
    depth: Arc<AtomicUsize>,
    /// Frames the running recorder's replay buffer holds.
    buffered: Arc<AtomicUsize>,
    /// Asks the loop for a pass, so what the worker says is taken up.
    wake: Wake,
    /// The recorder that runs now, if one does.
    recorder: Option<Box<Recorder>>,
    /// A recording whose encoder waits for its first frame.
    opening: Option<Opening>,
    /// The number of that recorder, which the notes about it carry.
    run: u64,
}

/// A recording that was started but whose encoder is not built yet: it takes its size from the
/// first frame, so nothing expensive runs before there is one.
struct Opening {
    run: u64,
    file: Option<PathBuf>,
    audio: Vec<String>,
    /// Whether a pause came in before the encoder was built.
    paused: bool,
}

impl Worker {
    /// Take up the loop's work until it hands no more over.
    fn run(mut self) {
        while let Ok(job) = self.jobs.recv() {
            self.take(job);
        }
    }

    /// Ask the loop for a pass, so what the worker made of its work is taken up on the next one.
    fn wake(&self) {
        (self.wake)();
    }

    /// One piece of work, in the order it was handed over.
    fn take(&mut self, job: Job) {
        match job {
            Job::Open { run, file, audio } => self.open(run, file, audio),
            Job::Feed { frame, sound } => self.feed(frame, sound),
            Job::Pause => {
                if let Some(opening) = &mut self.opening {
                    opening.paused = true;
                }
                if let Some(open) = self.recorder.as_mut() {
                    open.pause();
                }
            }
            Job::Resume => {
                if let Some(opening) = &mut self.opening {
                    opening.paused = false;
                }
                if let Some(open) = self.recorder.as_mut() {
                    open.resume();
                }
            }
            Job::Stop { dropped } => self.stop(dropped),
            Job::Close => {
                self.opening = None;
                self.recorder = None;
                self.buffered.store(0, Ordering::Release);
            }
            Job::SaveReplay { path, done } => {
                // The media stack says what went wrong; the caller names the file it was writing,
                // which the stack no longer does.
                let answer = match self.recorder.as_mut() {
                    Some(open) => open
                        .save_replay(path.clone())
                        .map_err(|error| format!("{}: {error}", path.display())),
                    None => Err("no replay buffer is kept".to_owned()),
                };
                let _ = done.send(answer);
                self.wake();
            }
            Job::Write {
                picture,
                directory,
                copy,
                done,
            } => {
                let _ = done.send(write_png(&picture, &directory, copy));
                self.wake();
            }
            Job::Encode { picture, done } => {
                let _ = done.send(encode_png(&picture));
                self.wake();
            }
            Job::Read { path, done } => {
                let _ = done.send(read(&path));
                self.wake();
            }
        }
    }

    /// Start run `run` : its file goes to `file` when there is one, and its sound is `audio` .
    /// Nothing is built until its first frame is here, so the folder it writes into is checked then
    /// too.
    fn open(&mut self, run: u64, file: Option<PathBuf>, audio: Vec<String>) {
        // The frames the recorder before this one left in flight are counted out as they are taken;
        // what it held is not this recording's to show.
        self.buffered.store(0, Ordering::Release);
        self.opening = Some(Opening {
            run,
            file,
            audio,
            paused: false,
        });
    }

    /// Build the encoder of the opening recording at the size of its first frame, then take that
    /// frame into it.
    fn begin(&mut self, opening: Opening, first: &Frame) {
        let Opening {
            run,
            file,
            audio,
            paused,
        } = opening;
        let result = Recorder::with_audio(Settings::for_size(first.width, first.height), audio)
            .and_then(|mut recorder| {
                if let Some(path) = &file {
                    in_folder(path)?;
                    recorder.start(path.clone())?;
                }
                if paused {
                    recorder.pause();
                }
                recorder.frame(first)?;
                Ok(recorder)
            });
        let result = result.map(|recorder| {
            self.run = run;
            self.buffered.store(recorder.buffered(), Ordering::Release);
            self.recorder = Some(Box::new(recorder));
        });
        let _ = self.notes.send(Note::Opened { run, result });
        self.wake();
    }

    /// One frame of what is being recorded, and the sound that came with it.
    fn feed(&mut self, frame: Option<Frame>, sound: Sound) {
        if frame.is_some() {
            self.depth.fetch_sub(1, Ordering::AcqRel);
        }
        let mut taken = false;
        if let Some(opening) = self.opening.take() {
            match &frame {
                Some(first) => {
                    self.begin(opening, first);
                    taken = true;
                }
                // Nothing has run yet, so there is nowhere for sound to go.
                None => self.opening = Some(opening),
            }
        }
        let Some(open) = self.recorder.as_mut() else {
            return;
        };
        let mut lost = false;
        if let Some(frame) = frame.filter(|_| !taken)
            && let Err(error) = open.frame(&frame)
        {
            let _ = self.notes.send(Note::Frame {
                run: self.run,
                error,
            });
            lost = true;
        }
        for (track, pcm) in sound {
            if let Err(error) = open.audio(track, &pcm) {
                tracing::warn!(%error, track, "a recording's sound was not written");
                let _ = self.notes.send(Note::SoundLost {
                    run: self.run,
                    track,
                });
                lost = true;
            }
        }
        self.buffered.store(open.buffered(), Ordering::Release);
        if lost {
            self.wake();
        }
    }

    /// Finish the file of the recorder that runs, and end it.
    fn stop(&mut self, dropped: u64) {
        // A recording whose encoder was never built wrote nothing: its first frame never came.
        self.opening = None;
        let ended = match self.recorder.take() {
            Some(mut open) => open.stop(),
            None => Ok(None),
        };
        self.buffered.store(0, Ordering::Release);
        let _ = self.notes.send(Note::Stopped {
            run: self.run,
            ended,
            dropped,
        });
        self.wake();
    }
}

/// Make the folder `path` goes in, so a capture is not lost to a folder that was never made.
fn in_folder(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(directory) => std::fs::create_dir_all(directory).map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

/// Write `picture` as a PNG under `directory` , named fresh, and read it back when the clipboard
/// takes it too.
/// The file is written beside its name and moved onto it once it is whole: a screenshot folder
/// watched by anything — a person's file manager, a test — never shows a picture that is still
/// being written.
fn write_png(
    picture: &image::RgbaImage,
    directory: &Path,
    copy: bool,
) -> Result<Written, WriteFailed> {
    std::fs::create_dir_all(directory).map_err(|error| refused(directory, &error))?;
    let path = crate::capture::unique(directory, "Screenshot", "png");
    let writing = path.with_extension("part");
    picture
        .save_with_format(&writing, image::ImageFormat::Png)
        .map_err(|error| {
            // An encoder's own refusal of the picture is not about the folder;
            // one that failed writing the file is.
            match &error {
                image::ImageError::IoError(why) => refused(directory, why),
                _ => WriteFailed::new(directory, WriteCause::Other, error.to_string()),
            }
        })?;
    std::fs::rename(&writing, &path).map_err(|error| {
        let _ = std::fs::remove_file(&writing);
        refused(directory, &error)
    })?;
    let png = if copy {
        Some(read(&path).map_err(|why| WriteFailed::new(directory, WriteCause::Other, why))?)
    } else {
        None
    };
    Ok(Written { path, png })
}

/// A write refused by `error` in `folder` .
fn refused(folder: &Path, error: &std::io::Error) -> WriteFailed {
    WriteFailed::new(folder, WriteCause::of(error), error.to_string())
}

/// `picture` as the bytes of a PNG.
fn encode_png(picture: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(png)
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    use tilo_test_env::SpawnUnderEnvLock as _;

    use super::*;

    /// A frame of `width` x `height` one colour.
    fn frame(width: u32, height: u32) -> Frame {
        let pixels = usize::try_from(width)
            .expect("a small width")
            .checked_mul(usize::try_from(height).expect("a small height"))
            .and_then(|pixels| pixels.checked_mul(4))
            .expect("a small frame");
        Frame::new(width, height, vec![0x40; pixels]).expect("a frame of its own size")
    }

    /// A named pipe nothing could read without a writer: reading it waits.
    fn pipe(directory: &Path) -> PathBuf {
        let path = directory.join("blocked");
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status_locked()
            .expect("a pipe is made");
        assert!(made.success(), "mkfifo {path:?}");
        path
    }

    /// The worker, inside a read of a pipe nothing could finish without a writer: every job handed
    /// over after it waits for that writer, and the answer of the blocked read comes back with it.
    fn hold_the_worker(
        folder: &Path,
        io: &CaptureIo,
    ) -> (Receiver<Result<Vec<u8>, String>>, std::fs::File) {
        // The worker runs, and takes what it was handed in order: what was handed over before the
        // pipe is taken before it, and the pipe's read is where it waits.
        let ahead = io.read(folder.join("nothing.txt"));
        assert!(
            ahead.recv_timeout(Duration::from_secs(10)).is_ok(),
            "the worker reads what it is handed"
        );

        let blocked = pipe(folder);
        let waiting = io.read(blocked.clone());
        // Opening the pipe for writing waits for the reader: once it opens,
        // the worker is inside the read and holds every job behind it. The
        // test thread waits for that with a bound, so a worker that never
        // gets there fails the test rather than hanging it.
        let (opened, ready) = channel();
        std::thread::spawn(move || {
            let writer = std::fs::OpenOptions::new().write(true).open(&blocked);
            let _ = opened.send(writer);
        });
        let writer = ready
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker is inside the read")
            .expect("the worker reads the pipe");
        (waiting, writer)
    }

    /// A writer that waits holds neither a frame nor the loop that hands it over: frames are
    /// dropped, and counted, rather than waited on.
    #[test]
    fn a_slow_writer_never_delays_a_frame() {
        let folder = tempfile::tempdir().expect("a folder");
        let (io, notes) = CaptureIo::new(Arc::new(|| {}));
        let (waiting, mut writer) = hold_the_worker(folder.path(), &io);

        let started = Instant::now();
        for taken in 0..DEPTH {
            assert!(
                io.feed(frame(1280, 800), Vec::new()),
                "the encoder takes the {taken}th frame it is handed"
            );
        }
        assert!(
            !io.feed(frame(1280, 800), Vec::new()),
            "a frame handed over while the encoder holds {DEPTH} is dropped"
        );
        assert_eq!(io.dropped(), 1, "the dropped frame is counted");
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "handing the frames over took {:?}: the loop waited for the writer",
            started.elapsed()
        );

        // What the worker waited for finishes, and what it was handed after it follows: the
        // hand-over never lost one to the drop.
        writer.write_all(b"a picture").expect("the pipe is written");
        drop(writer);
        assert_eq!(
            waiting.recv_timeout(Duration::from_secs(10)),
            Ok(Ok(b"a picture".to_vec())),
            "the read finishes once its writer does"
        );
        assert!(
            notes.try_recv().is_err(),
            "nothing of the recorder was said: no recording ran"
        );

        // The next recording counts its own frames: what this one dropped is its own.
        io.open(2, None, Vec::new());
        assert_eq!(io.dropped(), 0, "a new recording counts from zero");
    }

    /// Starting a recording hands the work over: the encoder is built on the worker, from the first
    /// frame, so the loop that started it never waits for it.
    #[test]
    fn starting_a_recording_builds_its_encoder_off_the_loop() {
        let folder = tempfile::tempdir().expect("a folder");
        let (io, notes) = CaptureIo::new(Arc::new(|| {}));
        let (waiting, mut writer) = hold_the_worker(folder.path(), &io);
        let path = folder.path().join("recording.mp4");

        let started = Instant::now();
        io.open(1, Some(path.clone()), Vec::new());
        assert!(
            io.feed(frame(1280, 800), Vec::new()),
            "the first frame is taken"
        );
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "starting took {:?}: the loop waited for the encoder",
            started.elapsed()
        );
        assert!(
            notes.try_recv().is_err(),
            "the encoder is built on the worker, not on the loop that started it"
        );

        // The worker gets to it: the encoder is built there, at the first frame's size, and the
        // recording's file is finished there.
        writer.write_all(b"a picture").expect("the pipe is written");
        drop(writer);
        assert_eq!(
            waiting.recv_timeout(Duration::from_secs(10)),
            Ok(Ok(b"a picture".to_vec())),
            "the read finishes once its writer does"
        );
        let opened = notes
            .recv_timeout(Duration::from_secs(10))
            .expect("the start is said");
        let Note::Opened { run, result } = opened else {
            panic!("the worker says what became of the recording before anything else");
        };
        assert_eq!(run, 1, "the note is about the recording that was started");
        if let Err(error) = result {
            // No encoder on this machine takes the frame: that is said, and there is no file to
            // check.
            assert!(!error.is_empty(), "a failure says why");
            return;
        }

        // A second frame lands in the file, and stopping finishes it.
        assert!(io.feed(frame(1280, 800), Vec::new()));
        assert!(io.feed(frame(1280, 800), Vec::new()));
        io.stop();
        let stopped = notes
            .recv_timeout(Duration::from_secs(20))
            .expect("the stop is said");
        let Note::Stopped {
            run,
            ended,
            dropped,
        } = stopped
        else {
            panic!("the drained recording is said when it stops");
        };
        assert_eq!(run, 1);
        assert_eq!(dropped, 0, "nothing was dropped");
        assert_eq!(
            ended,
            Ok(Some(path.clone())),
            "the file was finished where it was started"
        );
        let bytes = std::fs::read(&path).expect("the recording is on disk");
        assert!(!bytes.is_empty(), "the file is written");
    }

    /// A recording the encoder could not finish says what went wrong, and whether a file was left:
    /// a file it never wrote is not one to keep.
    #[test]
    fn a_recording_that_could_not_be_finished_says_so_and_what_it_left() {
        let folder = tempfile::tempdir().expect("a folder");
        let (io, notes) = CaptureIo::new(Arc::new(|| {}));
        let path = folder.path().join("recording.mp4");
        io.open(1, Some(path.clone()), Vec::new());
        // Held before its first frame and stopped: the encoder is built for a file that never takes
        // a frame, so finishing it fails.
        io.paused(true);
        assert!(
            io.feed(frame(1280, 800), Vec::new()),
            "the first frame goes"
        );
        io.stop();
        let mut heard = Vec::new();
        loop {
            let note = notes
                .recv_timeout(Duration::from_secs(20))
                .expect("the worker says what became of the recording");
            let stopped = matches!(note, Note::Stopped { .. });
            heard.push(note);
            if stopped {
                break;
            }
        }
        if heard
            .iter()
            .any(|note| matches!(note, Note::Opened { result: Err(_), .. }))
        {
            // No encoder on this machine takes the frame: the start said why,
            // and no file was opened to fail on.
            return;
        }
        let Some(Note::Stopped {
            run,
            ended,
            dropped,
        }) = heard.last()
        else {
            panic!("the stop is said");
        };
        assert_eq!(
            *run, 1,
            "the note is about the recording the person stopped"
        );
        assert_eq!(*dropped, 0, "nothing was dropped");
        let Err(Unfinished { path: left, error }) = ended else {
            panic!("a file that never took a frame is not finished: {ended:?}");
        };
        assert!(!error.is_empty(), "a failure says why");
        assert_eq!(*left, None, "the encoder wrote no file to keep");
        assert!(
            !path.exists(),
            "the file a finished stop would have written is not there"
        );
    }
}

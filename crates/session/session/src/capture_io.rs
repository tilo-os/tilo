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

use tilo_capture_studio::{Frame, Recorder};
use tilo_service::Wake;

/// Frames the encoder may hold before the loop keeps one back. One frame is
/// being encoded and one waits behind it; a third lets a writer that lags a
/// little catch up without a visible gap in the recording.
pub(crate) const DEPTH: usize = 3;

/// One track's sound, as the recorder numbers it, and the samples taken
/// since the last drain: interleaved stereo S16LE at 48 kHz.
pub(crate) type Sound = Vec<(usize, Vec<u8>)>;

/// What became of the recorder's work, taken up on the loop's next pass.
///
/// One that carries a `run` is about the recording started as number `run`:
/// a note about one that ended says nothing about the one running now.
pub(crate) enum Note {
    /// The recording or the replay buffer took its first frame, or why not.
    Opened {
        run: u64,
        result: Result<(), String>,
    },
    /// A frame was not encoded, and why: the recorder can take no more.
    Frame { run: u64, error: String },
    /// A track's sound ended or could not be written, by its recorder
    /// number.
    SoundLost { run: u64, track: usize },
    /// The recording's file was finished, or why it was not, and how many
    /// frames the encoder was too far behind to take.
    Stopped {
        result: Result<Option<PathBuf>, String>,
        dropped: u64,
    },
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
    /// Take `first` into `recorder`, writing its file to `file` when there is
    /// one: a recording, or only the replay buffer. It is run number `run`,
    /// which every note about it carries.
    Open {
        run: u64,
        recorder: Box<Recorder>,
        file: Option<PathBuf>,
        first: Frame,
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
    /// Write `picture` as a PNG under `directory`.
    Write {
        picture: image::RgbaImage,
        directory: PathBuf,
        copy: bool,
        done: Sender<Result<Written, String>>,
    },
    /// The bytes of the file at `path`.
    Read {
        path: PathBuf,
        done: Sender<Result<Vec<u8>, String>>,
    },
}

/// The session's one capture worker: the encoder, the muxer and every file a
/// capture makes.
pub(crate) struct CaptureIo {
    /// The worker's queue, made the first time work is handed over: a
    /// session that never captures opens no thread.
    jobs: RefCell<Option<Sender<Job>>>,
    /// Whether the worker was asked to start, so one that could not is not
    /// asked again for every job.
    started: Cell<bool>,
    /// Where what became of the recorder is said.
    notes: Sender<Note>,
    /// Frames handed over that the encoder has not taken yet.
    depth: Arc<AtomicUsize>,
    /// Frames the running recorder's replay buffer holds.
    buffered: Arc<AtomicUsize>,
    /// Frames this recording dropped because the encoder was behind.
    dropped: Cell<u64>,
    wake: Wake,
}

impl CaptureIo {
    /// The session's capture worker, and where what it makes of the
    /// recorder's work is said.
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
                wake,
            }),
            said,
        )
    }

    /// Start the recording numbered `run` in `recorder`, writing its file to
    /// `file` when there is one, from `first`: a recording, or only the
    /// replay buffer. Whether it started is said as [`Note::Opened`].
    pub(crate) fn open(&self, run: u64, recorder: Recorder, file: Option<PathBuf>, first: Frame) {
        self.dropped.set(0);
        if !self.submit(Job::Open {
            run,
            recorder: Box::new(recorder),
            file,
            first,
        }) {
            let _ = self.notes.send(Note::Opened {
                run,
                result: Err(NO_WORKER.to_owned()),
            });
        }
    }

    /// Hand one frame and the sound that came with it to the encoder.
    /// Returns whether it took it: a frame handed over while the encoder
    /// already holds [`DEPTH`] is dropped with its sound, so what the
    /// recording keeps stays whole, and counted for [`Self::dropped`].
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

    /// Hand the encoder the sound of a pass that took no frame: what was
    /// heard just before a stop.
    pub(crate) fn sound(&self, sound: Sound) {
        if sound.is_empty() {
            return;
        }
        self.submit(Job::Feed { frame: None, sound });
    }

    /// Stop writing the file into it, keeping the recorder open, or write
    /// frames into it again.
    pub(crate) fn paused(&self, paused: bool) {
        self.submit(if paused { Job::Pause } else { Job::Resume });
    }

    /// Finish the file and end the recorder: [`Note::Stopped`] says where it
    /// is, and how many frames were dropped on the way.
    pub(crate) fn stop(&self) {
        let dropped = self.dropped.get();
        if !self.submit(Job::Stop { dropped }) {
            let _ = self.notes.send(Note::Stopped {
                result: Err(NO_WORKER.to_owned()),
                dropped,
            });
        }
    }

    /// End the recorder without finishing a file: the replay buffer goes
    /// with it.
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

    /// Write `picture` as a PNG under `directory`, named fresh; `copy` also
    /// hands the file's bytes back for the clipboard.
    pub(crate) fn write(
        &self,
        picture: image::RgbaImage,
        directory: PathBuf,
        copy: bool,
    ) -> Receiver<Result<Written, String>> {
        let (done, answer) = channel();
        if !self.submit(Job::Write {
            picture,
            directory,
            copy,
            done: done.clone(),
        }) {
            let _ = done.send(Err(NO_WORKER.to_owned()));
        }
        answer
    }

    /// The bytes of the file at `path`, for the clipboard.
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

    /// Frames dropped since this recording began because the encoder was
    /// behind.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.get()
    }

    fn submit(&self, job: Job) -> bool {
        self.worker().is_some_and(|jobs| jobs.send(job).is_ok())
    }

    /// The worker's queue, started the first time it is asked for; `None`
    /// when no thread can run it.
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

/// The worker: one recorder for as long as it runs, and every file a capture
/// makes, in the order the loop handed them over.
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
    /// The number of that recorder, which the notes about it carry.
    run: u64,
}

impl Worker {
    /// Take up the loop's work until it hands no more over.
    fn run(mut self) {
        while let Ok(job) = self.jobs.recv() {
            self.take(job);
        }
    }

    /// Ask the loop for a pass, so what the worker made of its work is taken
    /// up on the next one.
    fn wake(&self) {
        (self.wake)();
    }

    /// One piece of work, in the order it was handed over.
    fn take(&mut self, job: Job) {
        match job {
            Job::Open {
                run,
                recorder,
                file,
                first,
            } => self.open(run, recorder, file.as_deref(), &first),
            Job::Feed { frame, sound } => self.feed(frame.as_ref(), sound),
            Job::Pause => {
                if let Some(open) = self.recorder.as_mut() {
                    open.pause();
                }
            }
            Job::Resume => {
                if let Some(open) = self.recorder.as_mut() {
                    open.resume();
                }
            }
            Job::Stop { dropped } => self.stop(dropped),
            Job::Close => {
                self.recorder = None;
                self.buffered.store(0, Ordering::Release);
            }
            Job::SaveReplay { path, done } => {
                let answer = match self.recorder.as_mut() {
                    Some(open) => open.save_replay(path),
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
            Job::Read { path, done } => {
                let _ = done.send(read(&path));
                self.wake();
            }
        }
    }

    /// Start `recorder` as run `run`, writing its file to `file` when there is
    /// one, from `first`.
    fn open(&mut self, run: u64, mut recorder: Box<Recorder>, file: Option<&Path>, first: &Frame) {
        let result = match file {
            Some(path) => in_folder(path).and_then(|()| recorder.start(path.to_path_buf())),
            None => Ok(()),
        }
        .and_then(|()| recorder.frame(first));
        // The frames the recorder before this one left in flight are counted
        // out as they are taken; what it held is not this recording's to show.
        self.buffered.store(0, Ordering::Release);
        if result.is_ok() {
            self.run = run;
            self.buffered.store(recorder.buffered(), Ordering::Release);
            self.recorder = Some(recorder);
        }
        let _ = self.notes.send(Note::Opened { run, result });
        self.wake();
    }

    /// One frame of what is being recorded, and the sound that came with it.
    fn feed(&mut self, frame: Option<&Frame>, sound: Sound) {
        let Some(open) = self.recorder.as_mut() else {
            if frame.is_some() {
                self.depth.fetch_sub(1, Ordering::AcqRel);
            }
            return;
        };
        if frame.is_some() {
            self.depth.fetch_sub(1, Ordering::AcqRel);
        }
        let mut lost = false;
        if let Some(frame) = frame
            && let Err(error) = open.frame(frame)
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
        let result = match self.recorder.take() {
            Some(mut open) => open.stop(),
            None => Ok(None),
        };
        self.buffered.store(0, Ordering::Release);
        let _ = self.notes.send(Note::Stopped { result, dropped });
        self.wake();
    }
}

/// Make the folder `path` goes in, so a capture is not lost to a folder that
/// was never made.
fn in_folder(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(directory) => std::fs::create_dir_all(directory).map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

/// Write `picture` as a PNG under `directory`, named fresh, and read it back
/// when the clipboard takes it too.
///
/// The file is written beside its name and moved onto it once it is whole: a
/// screenshot folder watched by anything — a person's file manager, a test —
/// never shows a picture that is still being written.
fn write_png(picture: &image::RgbaImage, directory: &Path, copy: bool) -> Result<Written, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = crate::capture::unique(directory, "screenshot", "png");
    let writing = path.with_extension("part");
    picture
        .save_with_format(&writing, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    std::fs::rename(&writing, &path).map_err(|error| {
        let _ = std::fs::remove_file(&writing);
        format!("{}: {error}", path.display())
    })?;
    let png = if copy { Some(read(&path)?) } else { None };
    Ok(Written { path, png })
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

    /// A writer that waits holds neither a frame nor the loop that hands it
    /// over: frames are dropped, and counted, rather than waited on.
    #[test]
    fn a_slow_writer_never_delays_a_frame() {
        let folder = tempfile::tempdir().expect("a folder");
        let (io, notes) = CaptureIo::new(Arc::new(|| {}));
        // The worker runs, and takes what it was handed in order: the frame
        // handed over before the pipe is written before it, and the pipe's
        // read is where it waits.
        let ahead = io.read(folder.path().join("nothing.txt"));
        assert!(
            ahead.recv_timeout(Duration::from_secs(10)).is_ok(),
            "the worker reads what it is handed"
        );

        let blocked = pipe(folder.path());
        let waiting = io.read(blocked.clone());
        // Opening the pipe for writing waits for the reader: once it opens,
        // the worker is inside the read and holds every frame behind it. The
        // test thread waits for that with a bound, so a worker that never
        // gets there fails the test rather than hanging it.
        let (opened, ready) = channel();
        std::thread::spawn(move || {
            let writer = std::fs::OpenOptions::new().write(true).open(&blocked);
            let _ = opened.send(writer);
        });
        let mut writer = ready
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker is inside the read")
            .expect("the worker reads the pipe");

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

        // What the worker waited for finishes, and what it was handed after
        // it follows: the hand-over never lost one to the drop.
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
    }
}

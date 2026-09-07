//! Bounded Camera file/blob work. The UI only admits a job; `CameraApp::sync`
//! owns the resulting document mutation and operation settlement.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use qos_actions::InvocationId;

use crate::frame::FrameBuf;
use crate::model::{Capture, ScannedCode};
use crate::model::{Facing, PhotoFormat};

/// A bounded thumbnail request. `media_id` is a verified qdoc identity; local
/// paths are admitted only from Camera's own indexed library projection.
pub enum ThumbnailSource {
    Path(PathBuf),
    Media(String),
}

pub struct ThumbnailJob {
    pub generation: u64,
    pub id: String,
    pub source: ThumbnailSource,
    pub width: u32,
    pub height: u32,
}

pub struct ThumbnailResult {
    pub generation: u64,
    pub id: String,
    pub result: Option<(Vec<u8>, u32, u32)>,
}

/// A filesystem-index refresh. The only path comes from Camera's configured
/// capture directory; worker results are projections, never document input.
pub struct LibraryJob {
    pub generation: u64,
    pub dir: PathBuf,
    pub favorites: HashSet<String>,
}

pub struct LibraryResult {
    pub generation: u64,
    pub result: Result<Vec<Capture>, String>,
}

/// Decode an existing local or qdoc-backed image for the offline scanner.
/// The decode and detector both run away from the UI thread.
pub struct ScanJob {
    pub generation: u64,
    pub id: String,
    pub source: ScanSource,
    pub record_history: bool,
}

pub enum ScanSource {
    Path(PathBuf),
    Media(String),
    Frame(FrameBuf),
}

pub struct ScanResult {
    pub generation: u64,
    pub id: String,
    pub record_history: bool,
    pub result: Result<Option<ScannedCode>, String>,
}

pub struct PhotoJob {
    pub invocation: Option<InvocationId>,
    pub generation: u64,
    pub dir: PathBuf,
    pub frame: FrameBuf,
    pub format: PhotoFormat,
    pub facing: Facing,
}

pub struct PhotoResult {
    pub invocation: Option<InvocationId>,
    pub generation: u64,
    pub result: Result<PhotoSaved, String>,
}

pub struct PhotoSaved {
    pub name: String,
    pub local_path: PathBuf,
    pub media_id: String,
    pub w: u32,
    pub h: u32,
    pub facing: Facing,
}

pub struct VideoJob {
    pub invocation: Option<InvocationId>,
    pub generation: u64,
    /// `Some` writes a local library copy after verification. `None` is the
    /// hermetic sample path: it still produces/validates qdoc media bytes but
    /// intentionally has no filesystem projection.
    pub dir: Option<PathBuf>,
    pub facing: Facing,
}

pub struct VideoResult {
    pub invocation: Option<InvocationId>,
    pub generation: u64,
    pub result: Result<VideoSaved, String>,
}
pub struct VideoSaved {
    pub name: String,
    pub local_path: Option<PathBuf>,
    pub media_id: String,
    pub w: u32,
    pub h: u32,
    pub dur_ms: i64,
    pub facing: Facing,
}

/// One worker-owned recording session. The UI never JPEG-encodes a frame: it
/// merely replaces `latest` and wakes this state machine. The single-slot
/// wake channel coalesces bursts without growing a queue.
struct RecordingControl {
    start: bool,
    finish: Option<VideoJob>,
    latest: Option<FrameBuf>,
}

impl RecordingControl {
    fn replace_latest(&mut self, frame: FrameBuf) {
        self.latest = Some(frame);
    }

    fn take_pending(&mut self) -> (bool, Option<FrameBuf>, Option<VideoJob>) {
        (
            std::mem::take(&mut self.start),
            self.latest.take(),
            self.finish.take(),
        )
    }
}

pub struct CameraWorker {
    tx: Option<SyncSender<WorkerJob>>,
    rx: Receiver<PhotoResult>,
    video_rx: Receiver<VideoResult>,
    recording: Arc<Mutex<RecordingControl>>,
    thumb_rx: Receiver<ThumbnailResult>,
    library_rx: Receiver<LibraryResult>,
    scan_rx: Receiver<ScanResult>,
}

enum WorkerJob {
    Photo(PhotoJob),
    RecordingWake,
    Thumbnail(ThumbnailJob),
    Library(LibraryJob),
    Scan(ScanJob),
}

struct WorkerLoop {
    recording: Arc<Mutex<RecordingControl>>,
    recorder: Option<crate::video::Recorder>,
    photo_results: SyncSender<PhotoResult>,
    video_results: SyncSender<VideoResult>,
    thumbnail_results: SyncSender<ThumbnailResult>,
    library_results: SyncSender<LibraryResult>,
    scan_results: SyncSender<ScanResult>,
}

impl WorkerLoop {
    fn process_recording_control(&mut self) -> bool {
        let (start, frame, finish) = match self.recording.lock() {
            Ok(mut control) => control.take_pending(),
            Err(_) => return false,
        };
        if start {
            self.recorder = Some(crate::video::Recorder::new(crate::video::FPS));
        }
        if let (Some(recorder), Some(frame)) = (&mut self.recorder, frame) {
            // This is deliberately the only Recorder::push call: RGBA
            // conversion and JPEG encode never touch UI.
            recorder.push(&frame);
        }
        let Some(job) = finish else {
            return true;
        };
        let (w, h, dur_ms) = self.recorder.as_ref().map_or((0, 0, 0), |recorder| {
            let (w, h) = recorder.dimensions();
            (w, h, recorder.duration_ms())
        });
        let result = self
            .recorder
            .take()
            .and_then(crate::video::Recorder::finish)
            .ok_or_else(|| "nothing recorded".to_string())
            .and_then(|bytes| {
                crate::media::store(&bytes)
                    .ok_or_else(|| "couldn't verify portable clip bytes".to_string())
                    .and_then(|media_id| {
                        let name = match job.dir.as_ref() {
                            Some(dir) => crate::video::write_clip(
                                dir,
                                &bytes,
                                &crate::library::timestamp_name(),
                            )
                            .map_err(|error| error.to_string())?,
                            None => format!("VID_{}.avi", crate::library::timestamp_name()),
                        };
                        Ok(VideoSaved {
                            local_path: job.dir.as_ref().map(|dir| dir.join(&name)),
                            name,
                            media_id,
                            w,
                            h,
                            dur_ms,
                            facing: job.facing,
                        })
                    })
            });
        self.video_results
            .send(VideoResult {
                invocation: job.invocation,
                generation: job.generation,
                result,
            })
            .is_ok()
    }

    fn process_job(&self, job: WorkerJob) -> bool {
        match job {
            WorkerJob::Photo(job) => {
                let result = crate::library::write_photo(&job.dir, &job.frame, job.format)
                    .map_err(|error| error.to_string())
                    .and_then(|name| {
                        let local_path = job.dir.join(&name);
                        let saved_path = local_path.clone();
                        crate::media::store_file(&local_path)
                            .map(|media_id| PhotoSaved {
                                name,
                                local_path: saved_path,
                                media_id,
                                w: job.frame.w,
                                h: job.frame.h,
                                facing: job.facing,
                            })
                            .ok_or_else(|| {
                                let _ = std::fs::remove_file(&local_path);
                                "couldn't verify portable media bytes".into()
                            })
                    });
                self.photo_results
                    .send(PhotoResult {
                        invocation: job.invocation,
                        generation: job.generation,
                        result,
                    })
                    .is_ok()
            }
            WorkerJob::RecordingWake => true,
            WorkerJob::Thumbnail(job) => {
                let result = match job.source {
                    ThumbnailSource::Path(path) => {
                        crate::library::load_thumb(&path, job.width, job.height)
                    }
                    ThumbnailSource::Media(id) => crate::media::read(&id).and_then(|bytes| {
                        crate::library::load_thumb_bytes(&bytes, job.width, job.height)
                    }),
                };
                self.thumbnail_results
                    .send(ThumbnailResult {
                        generation: job.generation,
                        id: job.id,
                        result,
                    })
                    .is_ok()
            }
            WorkerJob::Library(job) => self
                .library_results
                .send(LibraryResult {
                    generation: job.generation,
                    result: Ok(crate::library::scan(&job.dir, &job.favorites)),
                })
                .is_ok(),
            WorkerJob::Scan(job) => {
                let luma = match job.source {
                    ScanSource::Path(path) => crate::library::load_luma(&path, 1024),
                    ScanSource::Media(id) => crate::media::read(&id)
                        .and_then(|bytes| crate::library::load_luma_bytes(&bytes, 1024)),
                    ScanSource::Frame(frame) => Some((
                        crate::frame::luma(&frame),
                        frame.w as usize,
                        frame.h as usize,
                    )),
                };
                let result = luma
                    .and_then(|(luma, w, h)| crate::scan::decode_and_classify(&luma, w, h, |p| p));
                self.scan_results
                    .send(ScanResult {
                        generation: job.generation,
                        id: job.id,
                        record_history: job.record_history,
                        result: Ok(result),
                    })
                    .is_ok()
            }
        }
    }
}

impl CameraWorker {
    pub fn new() -> Self {
        // Every ingress and result queue is bounded. One sleeping event loop
        // replaces the former eager per-feature worker set.
        let (tx, jobs) = std::sync::mpsc::sync_channel::<WorkerJob>(1);
        let (photo_results, rx) = std::sync::mpsc::sync_channel::<PhotoResult>(1);
        let (video_results, video_rx) = std::sync::mpsc::sync_channel::<VideoResult>(1);
        let (thumbnail_results, thumb_rx) = std::sync::mpsc::sync_channel::<ThumbnailResult>(1);
        let (library_results, library_rx) = std::sync::mpsc::sync_channel::<LibraryResult>(1);
        let (scan_results, scan_rx) = std::sync::mpsc::sync_channel::<ScanResult>(1);
        let recording = Arc::new(Mutex::new(RecordingControl {
            start: false,
            finish: None,
            latest: None,
        }));
        let worker_recording = Arc::clone(&recording);
        let started = std::thread::Builder::new()
            .name("camera-bounded-worker".into())
            .spawn(move || {
                let mut worker = WorkerLoop {
                    recording: worker_recording,
                    recorder: None,
                    photo_results,
                    video_results,
                    thumbnail_results,
                    library_results,
                    scan_results,
                };
                // Inspect recording state before blocking and after all jobs.
                // A full unified queue can coalesce a recording wake, so this
                // makes the slot observable even without a queued wake event.
                while worker.process_recording_control() {
                    let Ok(job) = jobs.recv() else {
                        break;
                    };
                    if !worker.process_job(job) || !worker.process_recording_control() {
                        break;
                    }
                }
            })
            .is_ok();
        Self {
            tx: started.then_some(tx),
            rx,
            video_rx,
            recording,
            thumb_rx,
            library_rx,
            scan_rx,
        }
    }

    /// # Errors
    ///
    /// Returns an explanatory error when the worker failed to start, stopped,
    /// or its single admitted job is still running.
    pub fn submit(&self, job: PhotoJob) -> Result<(), String> {
        match self
            .tx
            .as_ref()
            .ok_or_else(|| "Camera background worker couldn't start".to_string())?
            .try_send(WorkerJob::Photo(job))
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("Camera is still saving the previous photo".into()),
            Err(TrySendError::Disconnected(_)) => {
                Err("Camera background worker disconnected".into())
            }
        }
    }

    pub fn try_recv(&self) -> Option<PhotoResult> {
        self.rx.try_recv().ok()
    }

    /// # Errors
    ///
    fn wake_recording(&self) -> Result<(), String> {
        match self
            .tx
            .as_ref()
            .ok_or_else(|| "Camera video worker couldn't start".to_string())?
            .try_send(WorkerJob::RecordingWake)
        {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err("Camera video worker disconnected".into()),
        }
    }

    /// Begin a worker-owned recording. The caller may immediately submit
    /// frames; they coalesce in a single latest-frame slot.
    ///
    /// # Errors
    ///
    /// Returns an error if the recorder worker cannot be woken or a prior
    /// finalization is still admitted.
    pub fn start_recording(&self) -> Result<(), String> {
        let mut control = self
            .recording
            .lock()
            .map_err(|_| "Camera video worker state is unavailable".to_string())?;
        if control.start || control.finish.is_some() {
            return Err("Camera is still finalizing the previous video".into());
        }
        control.start = true;
        drop(control);
        self.wake_recording()
    }

    /// Replace the admitted recorder frame and coalesce wake-ups. This does
    /// no pixel conversion, allocation beyond the caller's bounded frame
    /// clone, or blocking send on the UI path.
    ///
    /// # Errors
    ///
    /// Returns an error if the recorder worker state or wake channel ended.
    pub fn submit_recording_frame(&self, frame: FrameBuf) -> Result<(), String> {
        let mut control = self
            .recording
            .lock()
            .map_err(|_| "Camera video worker state is unavailable".to_string())?;
        control.replace_latest(frame);
        drop(control);
        self.wake_recording()
    }

    /// Request worker-side finish. AVI muxing, qdoc verification, and local
    /// projection all remain on the worker; sync owns canonical settlement.
    ///
    /// # Errors
    ///
    /// Returns an error if a finalization is already admitted or the worker
    /// state/wake channel is unavailable.
    pub fn finish_recording(&self, job: VideoJob) -> Result<(), String> {
        let mut control = self
            .recording
            .lock()
            .map_err(|_| "Camera video worker state is unavailable".to_string())?;
        if control.finish.is_some() {
            return Err("Camera is still finalizing the previous video".into());
        }
        control.finish = Some(job);
        drop(control);
        self.wake_recording()
    }

    pub fn try_recv_video(&self) -> Option<VideoResult> {
        self.video_rx.try_recv().ok()
    }

    /// # Errors
    ///
    /// Returns a visible error if the bounded queue is full or its worker ended.
    pub fn submit_thumbnail(&self, job: ThumbnailJob) -> Result<(), String> {
        match self
            .tx
            .as_ref()
            .ok_or_else(|| "Camera thumbnail worker couldn't start".to_string())?
            .try_send(WorkerJob::Thumbnail(job))
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("Thumbnail queue is busy".into()),
            Err(TrySendError::Disconnected(_)) => {
                Err("Camera thumbnail worker disconnected".into())
            }
        }
    }

    pub fn try_recv_thumbnail(&self) -> Option<ThumbnailResult> {
        self.thumb_rx.try_recv().ok()
    }

    /// # Errors
    ///
    /// Returns a visible error if the bounded queue is full or its worker ended.
    pub fn submit_library(&self, job: LibraryJob) -> Result<(), String> {
        match self
            .tx
            .as_ref()
            .ok_or_else(|| "Camera library worker couldn't start".to_string())?
            .try_send(WorkerJob::Library(job))
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("Camera library is already refreshing".into()),
            Err(TrySendError::Disconnected(_)) => Err("Camera library worker disconnected".into()),
        }
    }

    pub fn try_recv_library(&self) -> Option<LibraryResult> {
        self.library_rx.try_recv().ok()
    }

    /// # Errors
    ///
    /// Returns a visible error if the bounded queue is full or its worker ended.
    pub fn submit_scan(&self, job: ScanJob) -> Result<(), String> {
        match self
            .tx
            .as_ref()
            .ok_or_else(|| "Camera code scanner couldn't start".to_string())?
            .try_send(WorkerJob::Scan(job))
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("A code scan is already running".into()),
            Err(TrySendError::Disconnected(_)) => Err("Camera code scanner disconnected".into()),
        }
    }

    pub fn try_recv_scan(&self) -> Option<ScanResult> {
        self.scan_rx.try_recv().ok()
    }
}

impl Default for CameraWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_ingress_keeps_only_the_newest_frame() {
        let mut control = RecordingControl {
            start: false,
            finish: None,
            latest: None,
        };
        control.replace_latest(crate::frame::viewfinder(1, 32, 24));
        control.replace_latest(crate::frame::viewfinder(2, 48, 36));

        let newest = control.latest.as_ref().expect("latest frame");
        assert_eq!((newest.w, newest.h), (48, 36));
        assert_eq!(control.latest.iter().count(), 1, "one-slot backpressure");
    }

    #[test]
    fn recorder_control_orders_start_newest_frame_then_finish() {
        let mut control = RecordingControl {
            start: true,
            finish: Some(VideoJob {
                invocation: None,
                generation: 7,
                dir: None,
                facing: Facing::Back,
            }),
            latest: None,
        };
        control.replace_latest(crate::frame::viewfinder(1, 32, 24));
        control.replace_latest(crate::frame::viewfinder(2, 48, 36));

        let (start, frame, finish) = control.take_pending();
        assert!(start, "worker creates Recorder before consuming frame");
        assert_eq!(
            frame.map(|frame| (frame.w, frame.h)),
            Some((48, 36)),
            "finish consumes the coalesced newest frame"
        );
        assert_eq!(finish.map(|job| job.generation), Some(7));
        assert!(control.latest.is_none() && control.finish.is_none() && !control.start);
    }
}

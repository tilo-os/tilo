//! Bounded Camera file/blob work. The UI only admits a job; `CameraApp::sync`
//! owns the resulting document mutation and operation settlement.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use tilo_content::ContentStore;
use ui::tilo_actions::InvocationId;

use crate::frame::FrameBuf;
use crate::model::ScannedCode;
use crate::model::{Facing, PhotoFormat};

/// Decode an existing local or content-store image for the offline scanner.
/// The decode and detector both run away from the UI thread.
pub struct ScanJob {
    pub generation: u64,
    pub id: String,
    pub source: ScanSource,
    pub record_history: bool,
}

pub enum ScanSource {
    /// The bytes of an image the person chose.
    Bytes(Vec<u8>),
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
    /// `None` keeps the photo only in the content store: a host whose files
    /// have no native path (a browser page) has no directory to project to.
    pub dir: Option<PathBuf>,
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
    pub local_path: Option<PathBuf>,
    pub media_id: String,
    pub w: u32,
    pub h: u32,
    pub facing: Facing,
}

pub struct VideoJob {
    pub invocation: Option<InvocationId>,
    pub generation: u64,
    /// `Some` writes an optional local projection after verification. `None` is the
    /// hermetic sample path: it still produces/validates stored media bytes but
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
    scan_rx: Receiver<ScanResult>,
    /// Where there are no threads (a browser page) the loop runs each job
    /// as it is admitted, on the caller's thread, and its jobs' queue is
    /// drained there.
    inline: Option<Mutex<(WorkerLoop, Receiver<WorkerJob>)>>,
}

/// Results a thread-free worker may hold before the UI drains them; each
/// admitted job settles at once, so this only bounds a UI that stops
/// draining.
const INLINE_RESULTS: usize = 8;

enum WorkerJob {
    Photo(PhotoJob),
    RecordingWake,
    Scan(ScanJob),
}

struct WorkerLoop {
    recording: Arc<Mutex<RecordingControl>>,
    recorder: Option<crate::video::Recorder>,
    photo_results: SyncSender<PhotoResult>,
    video_results: SyncSender<VideoResult>,
    scan_results: SyncSender<ScanResult>,
    content: ContentStore,
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
                crate::media::store(&self.content, &bytes)
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
                let saved =
                    |name: String, local_path: Option<PathBuf>, media_id: String| PhotoSaved {
                        name,
                        local_path,
                        media_id,
                        w: job.frame.w,
                        h: job.frame.h,
                        facing: job.facing,
                    };
                let result = match &job.dir {
                    Some(dir) => crate::library::write_photo(dir, &job.frame, job.format)
                        .map_err(|error| error.to_string())
                        .and_then(|name| {
                            let local_path = dir.join(&name);
                            crate::media::store_file(&self.content, &local_path)
                                .map(|media_id| saved(name, Some(local_path.clone()), media_id))
                                .ok_or_else(|| {
                                    crate::files::remove(&local_path);
                                    "couldn't verify portable media bytes".into()
                                })
                        }),
                    None => crate::library::encode_photo(&job.frame, job.format)
                        .map_err(|error| error.to_string())
                        .and_then(|(name, bytes)| {
                            crate::media::store(&self.content, &bytes)
                                .map(|media_id| saved(name, None, media_id))
                                .ok_or_else(|| "couldn't verify portable media bytes".into())
                        }),
                };
                self.photo_results
                    .send(PhotoResult {
                        invocation: job.invocation,
                        generation: job.generation,
                        result,
                    })
                    .is_ok()
            }
            WorkerJob::RecordingWake => true,
            WorkerJob::Scan(job) => {
                let luma = match job.source {
                    ScanSource::Bytes(bytes) => crate::library::load_luma_bytes(&bytes, 1024),
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
    pub fn new(content: ContentStore) -> Self {
        // Every ingress and result queue is bounded. One sleeping event loop
        // replaces the former eager per-feature worker set.
        let threads = ui::rt::available(ui::rt::Feature::Threads);
        let results = if threads { 1 } else { INLINE_RESULTS };
        let (tx, jobs) = std::sync::mpsc::sync_channel::<WorkerJob>(1);
        let (photo_results, rx) = std::sync::mpsc::sync_channel::<PhotoResult>(results);
        let (video_results, video_rx) = std::sync::mpsc::sync_channel::<VideoResult>(results);
        let (scan_results, scan_rx) = std::sync::mpsc::sync_channel::<ScanResult>(results);
        let recording = Arc::new(Mutex::new(RecordingControl {
            start: false,
            finish: None,
            latest: None,
        }));
        let worker = WorkerLoop {
            recording: Arc::clone(&recording),
            recorder: None,
            photo_results,
            video_results,
            scan_results,
            content,
        };
        if !threads {
            return Self {
                tx: Some(tx),
                rx,
                video_rx,
                recording,
                scan_rx,
                inline: Some(Mutex::new((worker, jobs))),
            };
        }
        let started = ui::rt::spawn_thread("camera-bounded-worker", move || {
            let mut worker = worker;
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
            scan_rx,
            inline: None,
        }
    }

    /// A thread-free worker runs what was just admitted.
    fn run_inline(&self) {
        let Some(inline) = &self.inline else {
            return;
        };
        let Ok(mut guard) = inline.lock() else {
            return;
        };
        let (worker, jobs) = &mut *guard;
        while let Ok(job) = jobs.try_recv() {
            worker.process_job(job);
        }
        worker.process_recording_control();
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
            Ok(()) => {
                self.run_inline();
                Ok(())
            }
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
            Ok(()) | Err(TrySendError::Full(_)) => {
                self.run_inline();
                Ok(())
            }
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

    /// Request worker-side finish. AVI muxing, content-store verification, and local
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
    pub fn submit_scan(&self, job: ScanJob) -> Result<(), String> {
        match self
            .tx
            .as_ref()
            .ok_or_else(|| "Camera code scanner couldn't start".to_string())?
            .try_send(WorkerJob::Scan(job))
        {
            Ok(()) => {
                self.run_inline();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("A code scan is already running".into()),
            Err(TrySendError::Disconnected(_)) => Err("Camera code scanner disconnected".into()),
        }
    }

    pub fn try_recv_scan(&self) -> Option<ScanResult> {
        self.scan_rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_control_coalesces_the_newest_frame_and_orders_start_then_finish() {
        // Phase `recorder_ingress_keeps_only_the_newest_frame`.
        {
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
        // Phase `recorder_control_orders_start_newest_frame_then_finish`.
        {
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
}

//! The host's render service: an agent's video renders, made outside the
//! record's app worker.
//!
//! A worker holds its record with no GPU, no media stack and no paths, and
//! answers within a minute; rendering a timeline needs all three and can
//! take longer. So the operations a record publishes tagged
//! [`HOST_RENDER_TAG`] (the video editor's `export-video-content`,
//! `export-video-status`, `cancel-video-export` and `view-frame-content`)
//! are answered here instead:
//!
//! 1. the record is read from the copy the host holds of it (the replica's
//!    mirror), never from the worker, and checked with the editor's own
//!    pure loader: at most [`video_editor::export::AGENT_MAX_SECONDS`], and
//!    only sources that are room content the room holds;
//! 2. the document and those sources are laid out in a private job
//!    directory, within [`SOURCE_BUDGET`];
//! 3. `tilo-video-render` ([`video_editor::render_helper`]) runs on it,
//!    confined, and writes into a file it is handed open, with no name;
//! 4. what it wrote, at most [`MAX_OUTPUT`], becomes room content, and the
//!    agent is answered with its address.
//!
//! One render runs at a time on a host, each for at most [`WALL_LIMIT`];
//! renders are kept by record, document heads and settings, so asking for
//! the same one again answers it rather than rendering twice. A host
//! without the helper offers none of these operations, and one named
//! anyway is unavailable here.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_actions::{ActionOutput, HOST_RENDER_TAG};
use tilo_content::ContentStore;
use video_editor::export::{
    AgentExportSettings, AgentFrameSettings, AgentRendered, RenderState, agent_done_answer,
    agent_failed_answer, agent_rendering_answer, agent_wait_from_args,
};
use video_editor::model::Project;
use video_editor::render_helper::client::{self, JobDir};
use video_editor::render_helper::proto::{Reply, Request};

use crate::worker::ConfinedReplica;

/// The most bytes of sources one render reads.
pub const SOURCE_BUDGET: u64 = 512 * 1024 * 1024;

/// The most bytes a render's file may weigh to be kept as content.
pub const MAX_OUTPUT: u64 = 60 * 1024 * 1024;

/// The most bytes of a still.
const MAX_STILL: u64 = 16 * 1024 * 1024;

/// The longest one render runs before it is stopped.
pub const WALL_LIMIT: Duration = Duration::from_secs(180);

/// How long a still may take, waiting for the render slot included.
const STILL_LIMIT: Duration = Duration::from_secs(60);

/// How many renders are remembered.
const KEPT_JOBS: usize = 8;

/// The operations the service answers.
const EXPORT: &str = "export-video-content";
const STATUS: &str = "export-video-status";
const CANCEL: &str = "cancel-video-export";
const FRAME: &str = "view-frame-content";

/// Whether `descriptor` is one the render service answers.
#[must_use]
pub fn answers(descriptor: &ActionDescriptor) -> bool {
    descriptor
        .contract
        .tags
        .iter()
        .any(|tag| tag == HOST_RENDER_TAG)
}

/// Whether this host can render: whether it has the helper.
#[must_use]
pub fn available() -> bool {
    RenderService::shared().helper.is_some()
}

/// `descriptors` without the renders, on a host that cannot make them.
#[must_use]
pub fn offered(mut descriptors: Vec<ActionDescriptor>) -> Vec<ActionDescriptor> {
    if !available() {
        descriptors.retain(|descriptor| !answers(descriptor));
    }
    descriptors
}

/// One render, running or finished.
struct Job {
    id: String,
    settings: AgentExportSettings,
    state: RenderState,
    outcome: Mutex<Option<Result<Made, String>>>,
    settled: Condvar,
}

/// A finished render: where the room keeps it, and what it holds.
#[derive(Clone)]
struct Made {
    address: String,
    rendered: AgentRendered,
    bytes: u64,
}

/// The host's render service.
pub struct RenderService {
    helper: Option<std::path::PathBuf>,
    jobs: Mutex<Vec<Arc<Job>>>,
    /// Held by the one render running.
    slot: Mutex<()>,
}

impl RenderService {
    /// The service of this process, with the helper it found at start.
    #[must_use]
    pub fn shared() -> &'static Self {
        static SERVICE: OnceLock<RenderService> = OnceLock::new();
        SERVICE.get_or_init(|| Self {
            helper: client::locate(),
            jobs: Mutex::new(Vec::new()),
            slot: Mutex::new(()),
        })
    }

    /// Answer the render operation at `address` of `record`, whose sources
    /// are in `content`, as the agent invoking it with `arguments`.
    ///
    /// # Errors
    ///
    /// The render is unavailable on this host, or refused (bad arguments, a
    /// timeline an agent may not render, a source the room lacks), or it
    /// failed.
    pub fn invoke(
        &self,
        record: &ConfinedReplica,
        content: &ContentStore,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, DoorError> {
        let Some(helper) = self.helper.clone() else {
            return Err(unavailable(
                "rendering video is unavailable on this host: it has no render service \
                 (tilo-video-render)",
            ));
        };
        let refused = |reason: String| DoorError::Unavailable {
            code: "render_refused".into(),
            reason,
        };
        match address.action.as_str() {
            EXPORT => {
                let snapshot = Snapshot::of(record).map_err(refused)?;
                video_editor::export::agent_renderable(&snapshot.project).map_err(refused)?;
                let settings = AgentExportSettings::from_args(arguments, snapshot.project.aspect())
                    .map_err(DoorError::InvalidArguments)?;
                let wait = agent_wait_from_args(arguments).map_err(DoorError::InvalidArguments)?;
                snapshot.check_sources(content).map_err(refused)?;
                let job = self.start(helper, snapshot, settings, content);
                Ok(Self::wait(&job, wait))
            }
            STATUS => {
                let wait = agent_wait_from_args(arguments).map_err(DoorError::InvalidArguments)?;
                let job = self.find(arguments)?;
                Ok(Self::wait(&job, wait))
            }
            CANCEL => {
                let job = self.find(arguments)?;
                job.state.cancel.store(true, Ordering::Relaxed);
                Ok(value(agent_failed_answer(
                    &job.id,
                    video_editor::export::CANCELLED,
                )))
            }
            FRAME => {
                let snapshot = Snapshot::of(record).map_err(refused)?;
                video_editor::export::agent_renderable(&snapshot.project).map_err(refused)?;
                let frame = AgentFrameSettings::from_args(
                    arguments,
                    snapshot.project.aspect(),
                    snapshot.project.duration(),
                )
                .map_err(DoorError::InvalidArguments)?;
                snapshot.check_sources(content).map_err(refused)?;
                self.still(&helper, &snapshot, frame, content)
                    .map_err(DoorError::Failed)
            }
            _ => Err(DoorError::Unknown(address.clone())),
        }
    }

    /// The job `arguments` name.
    fn find(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Arc<Job>, DoorError> {
        let id = arguments
            .get("job")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| DoorError::InvalidArguments("name the render: `job`".into()))?;
        lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .ok_or_else(|| DoorError::InvalidArguments(format!("there is no render {id}")))
    }

    /// The render of `snapshot` at `settings`: the one made or running, or
    /// a new one on a thread of its own.
    fn start(
        &self,
        helper: std::path::PathBuf,
        snapshot: Snapshot,
        settings: AgentExportSettings,
        content: &ContentStore,
    ) -> Arc<Job> {
        let id = snapshot.job_id(&settings);
        let mut jobs = lock(&self.jobs);
        if let Some(job) = jobs
            .iter()
            .find(|job| job.id == id && !matches!(*lock(&job.outcome), Some(Err(_))))
        {
            return Arc::clone(job);
        }
        jobs.retain(|job| job.id != id);
        while jobs.len() >= KEPT_JOBS {
            let Some(oldest) = jobs.iter().position(|job| lock(&job.outcome).is_some()) else {
                break;
            };
            jobs.remove(oldest);
        }
        let job = Arc::new(Job {
            id,
            settings,
            state: RenderState::default(),
            outcome: Mutex::new(None),
            settled: Condvar::new(),
        });
        jobs.push(Arc::clone(&job));
        drop(jobs);
        let (worker, content) = (Arc::clone(&job), content.clone());
        let started = std::thread::Builder::new()
            .name("agent-video-render".into())
            .spawn(move || {
                let outcome = Self::shared().video(&helper, &snapshot, &worker, &content);
                if let Err(error) = &outcome {
                    tracing::warn!(job = %worker.id, %error, "render: the video was not made");
                }
                *lock(&worker.outcome) = Some(outcome);
                worker.settled.notify_all();
            });
        if let Err(error) = started {
            *lock(&job.outcome) = Some(Err(format!("the render could not start: {error}")));
        }
        job
    }

    /// Render the video of `job`, holding the slot.
    fn video(
        &self,
        helper: &std::path::Path,
        snapshot: &Snapshot,
        job: &Job,
        content: &ContentStore,
    ) -> Result<Made, String> {
        let _slot = lock(&self.slot);
        if job.state.cancel.load(Ordering::Relaxed) {
            return Err(video_editor::export::CANCELLED.to_owned());
        }
        let dir = snapshot.lay_out(content)?;
        let finished = client::render(
            helper,
            &dir,
            &Request::Video {
                settings: job.settings,
            },
            after(WALL_LIMIT),
            &mut |done, total| {
                job.state.done.store(done, Ordering::Relaxed);
                job.state.total.store(total, Ordering::Relaxed);
                !job.state.cancel.load(Ordering::Relaxed)
            },
        )?;
        let Reply::Video(rendered) = finished.reply else {
            return Err("the render helper answered with something else".into());
        };
        let (address, bytes) = keep(finished.output, MAX_OUTPUT, "video/mp4", content)?;
        Ok(Made {
            address,
            rendered,
            bytes,
        })
    }

    /// Draw one still and keep it.
    fn still(
        &self,
        helper: &std::path::Path,
        snapshot: &Snapshot,
        frame: AgentFrameSettings,
        content: &ContentStore,
    ) -> Result<ActionOutput, String> {
        let until = after(STILL_LIMIT);
        let _slot = loop {
            if let Ok(slot) = self.slot.try_lock() {
                break slot;
            }
            if Instant::now() >= until {
                return Err("another render holds this host's renderer; try again".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let dir = snapshot.lay_out(content)?;
        let finished = client::render(
            helper,
            &dir,
            &Request::Frame { frame },
            until,
            &mut |_, _| true,
        )?;
        let (address, _) = keep(finished.output, MAX_STILL, "image/png", content)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({
                "content": address,
                "at": frame.at,
                "width": frame.width,
                "height": frame.height,
            })),
            resources: vec![address],
        })
    }

    /// Wait at most `wait` for `job`, then answer where it is.
    fn wait(job: &Job, wait: Duration) -> ActionOutput {
        let until = after(wait);
        let mut outcome = lock(&job.outcome);
        while outcome.is_none() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            outcome = job
                .settled
                .wait_timeout(outcome, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        match outcome.as_ref() {
            Some(Ok(made)) => ActionOutput {
                value: Some(agent_done_answer(
                    &made.address,
                    made.rendered,
                    job.settings,
                    made.bytes,
                )),
                resources: vec![made.address.clone()],
            },
            Some(Err(error)) => value(agent_failed_answer(&job.id, error)),
            None => value(agent_rendering_answer(
                &job.id,
                job.state.done.load(Ordering::Relaxed),
                job.state.total.load(Ordering::Relaxed),
            )),
        }
    }
}

/// A record as the host holds it, read once for one render.
struct Snapshot {
    record: String,
    heads: Vec<String>,
    bytes: Vec<u8>,
    project: Project,
}

impl Snapshot {
    fn of(record: &ConfinedReplica) -> Result<Self, String> {
        let (bytes, heads, project) = record
            .read(|doc| {
                let mut doc = doc.clone();
                let mut heads: Vec<String> =
                    doc.get_heads().iter().map(ToString::to_string).collect();
                heads.sort();
                let project = video_editor::doc::VideoDoc::load_project(&doc);
                (doc.save(), heads, project)
            })
            .ok_or("the record is not held")?;
        Ok(Self {
            record: record.record().to_owned(),
            heads,
            bytes,
            project,
        })
    }

    /// Refuse a source the room does not hold: a render would draw a
    /// stand-in for it.
    fn check_sources(&self, content: &ContentStore) -> Result<(), String> {
        for hash in video_editor::export::placed_content(&self.project) {
            if !content.has(&hash) {
                return Err(format!(
                    "the room does not hold tilo.content:{hash}, which the timeline places"
                ));
            }
        }
        Ok(())
    }

    /// The render's identity: the record at these heads, at `settings`.
    fn job_id(&self, settings: &AgentExportSettings) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.record.as_bytes());
        for head in &self.heads {
            hasher.update(head.as_bytes());
        }
        hasher
            .update(format!("{}x{}@{}", settings.width, settings.height, settings.fps).as_bytes());
        let hash = hasher.finalize().to_hex();
        format!("render-{}", hash.get(..16).unwrap_or_default())
    }

    /// The job directory: the document and every source it places.
    fn lay_out(&self, content: &ContentStore) -> Result<JobDir, String> {
        let mut dir = JobDir::create(&std::env::temp_dir(), SOURCE_BUDGET)?;
        dir.put_snapshot(&self.bytes)?;
        for hash in video_editor::export::placed_content(&self.project) {
            let mut reader = content
                .open(&hash)
                .ok_or_else(|| format!("the room no longer holds tilo.content:{hash}"))?;
            dir.put_source(&hash, &mut reader)?;
        }
        Ok(dir)
    }
}

/// Keep what a render wrote as room content, at most `limit` bytes.
fn keep(
    output: std::fs::File,
    limit: u64,
    mime: &str,
    content: &ContentStore,
) -> Result<(String, u64), String> {
    let len = output
        .metadata()
        .map_err(|error| format!("the render could not be read: {error}"))?
        .len();
    if len == 0 {
        return Err("the render wrote nothing".into());
    }
    if len > limit {
        return Err(format!(
            "the render is {} MiB, more than the {} MiB a render may keep: export a shorter \
             range or a smaller size",
            len >> 20,
            limit >> 20
        ));
    }
    let mut reader = std::io::Read::take(output, limit);
    let stored = content
        .put_reader(&mut reader, mime)
        .map_err(|error| format!("the render could not be kept: {error}"))?;
    let address = tilo_actions::content_resource(&stored.hash)
        .ok_or("the room kept the render under no address")?;
    Ok((address, stored.len))
}

fn value(value: serde_json::Value) -> ActionOutput {
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

fn unavailable(reason: &str) -> DoorError {
    DoorError::Unavailable {
        code: "host_render".into(),
        reason: reason.into(),
    }
}

/// The instant `wait` from now; now, past the clock's range.
fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

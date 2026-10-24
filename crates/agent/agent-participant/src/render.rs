//! The host's render service: an agent's video renders, made outside the
//! record's app worker.
//!
//! A worker holds its record with no GPU, no media stack and no paths, and
//! answers within a minute; rendering a timeline needs all three and can
//! take longer. So the operations a record publishes tagged
//! [`HOST_RENDER_TAG`] (the video editor's `render`,
//! `export-video-content`, `export-video-status`, `cancel-video-export` and
//! `view-frame-content`) are answered here instead:
//!
//! 1. the record is read from the copy the host holds of it (the confined
//!    replica's mirror), never from the worker, and checked: at most
//!    [`AGENT_MAX_SECONDS`](video_editor::agent_render::AGENT_MAX_SECONDS),
//!    and only sources that are content the room holds;
//! 2. the document and those sources are laid out in a private job
//!    directory, within [`SOURCE_BUDGET`];
//! 3. `tilo-video-render` ([`tilo_video_render`]) runs on it,
//!    confined, and writes into a file it is handed open;
//! 4. what it wrote, at most [`MAX_OUTPUT`], becomes room content, and the
//!    agent is answered with its address.
//!
//! One render runs at a time on a host, each for at most [`WALL_LIMIT`];
//! renders are kept by record, document heads and settings, so asking for
//! the same one again answers it rather than rendering twice. A host without
//! the helper (`$TILO_VIDEO_RENDER`, or beside its executable) offers none
//! of these operations ([`offered`]), and the record's worker refuses one
//! named anyway.

use std::io::Read as _;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_actions::{ActionOutput, HOST_RENDER_TAG};
use tilo_content::ContentStore;
use tilo_lock::MutexRecover as _;
use tilo_video_render::client::{self, JobDir};
use tilo_video_render::proto::{Reply, Request};
use ui::tilo_resource::Resource as _;
use video_editor::Project;
use video_editor::agent_render::{
    self, AgentExportSettings, AgentFrameSettings, AgentRendered, done_answer, failed_answer,
    rendering_answer,
};

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

/// The app whose records publish the operations answered here.
const VIDEO_EDITOR: &str = "so.tilo.video-editor";

/// The operations the service answers.
const RENDER: &str = "render-timeline";
const EXPORT: &str = "export-video-content";
const STATUS: &str = "export-video-status";
const CANCEL: &str = "cancel-video-export";
const FRAME: &str = "view-frame-content";

/// Whether `descriptor` is one a host's render service answers.
#[must_use]
pub fn host_rendered(descriptor: &ActionDescriptor) -> bool {
    descriptor
        .contract
        .tags
        .iter()
        .any(|tag| tag == HOST_RENDER_TAG)
}

/// Whether this host can render: whether it found the helper.
#[must_use]
pub fn available() -> bool {
    RenderService::shared().helper.is_some()
}

/// `descriptors` as an agent on this host is offered them: without the
/// renders when the host cannot make them.
#[must_use]
pub fn offered(descriptors: Vec<ActionDescriptor>) -> Vec<ActionDescriptor> {
    offered_where(descriptors, available())
}

/// [`offered`] on a host that `can_render` or not.
fn offered_where(
    mut descriptors: Vec<ActionDescriptor>,
    can_render: bool,
) -> Vec<ActionDescriptor> {
    if !can_render {
        descriptors.retain(|descriptor| !host_rendered(descriptor));
    }
    descriptors
}

/// Answer the operation at `address` of an agent's `record` here when it is
/// one of the renders and this host can make them; `None` sends it on to
/// the record's worker (which refuses a render it cannot make).
pub(crate) fn route(
    record: &ConfinedReplica,
    content: Option<&ContentStore>,
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<Result<ActionOutput, DoorError>> {
    let ours = record.app_id() == VIDEO_EDITOR
        && [RENDER, EXPORT, STATUS, CANCEL, FRAME].contains(&address.action.as_str());
    if !ours || !record.is_agents() || !available() {
        return None;
    }
    let Some(content) = content else {
        return Some(Err(refused(
            "this record keeps no room content to render into".into(),
        )));
    };
    Some(RenderService::shared().invoke(record, content, address, arguments))
}

/// One render, running or finished.
struct Job {
    id: String,
    settings: AgentExportSettings,
    /// What the timeline rendered holds ([`video_editor::facts`]) and the
    /// version of the record it was rendered from: an export's answer
    /// carries both, so a later check compares the cut with what this render
    /// made, and a `facts` read at that version states an export's again.
    facts: serde_json::Value,
    version: String,
    /// Frames written, of how many.
    progress: Mutex<(u64, u64)>,
    cancelled: std::sync::atomic::AtomicBool,
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
    /// The service of this process, with the helper it found at first use.
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
            return Err(DoorError::Unavailable {
                code: "host_render".into(),
                reason: "rendering video is unavailable on this host: it has no render \
                         service (tilo-video-render)"
                    .into(),
            });
        };
        let wait = || agent_render::wait_from_args(arguments).map_err(DoorError::InvalidArguments);
        match address.action.as_str() {
            EXPORT | RENDER => {
                let mut snapshot = Snapshot::of(record).map_err(refused)?;
                let aspect = snapshot.project.aspect();
                let settings = if address.action == RENDER {
                    let start = number(arguments, "start")?;
                    let end = number(arguments, "end")?;
                    snapshot.project =
                        agent_render::ranged(&snapshot.project, start, end).map_err(refused)?;
                    snapshot.range = (start, end);
                    AgentExportSettings::from_render_args(arguments, aspect)
                } else {
                    AgentExportSettings::from_args(arguments, aspect)
                }
                .map_err(DoorError::InvalidArguments)?;
                let wait = wait()?;
                agent_render::renderable(&snapshot.project).map_err(refused)?;
                snapshot.check_sources(content).map_err(refused)?;
                let job = self.start(helper, snapshot, settings, content);
                Ok(Self::wait(&job, wait))
            }
            STATUS => {
                let wait = wait()?;
                let job = self.find(arguments)?;
                Ok(Self::wait(&job, wait))
            }
            CANCEL => {
                let job = self.find(arguments)?;
                job.cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(value(failed_answer(&job.id, client::STOPPED)))
            }
            FRAME => {
                let snapshot = Snapshot::of(record).map_err(refused)?;
                let frame = AgentFrameSettings::from_args(
                    arguments,
                    snapshot.project.aspect(),
                    snapshot.project.duration(),
                )
                .map_err(DoorError::InvalidArguments)?;
                agent_render::renderable(&snapshot.project).map_err(refused)?;
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
        self.jobs
            .lock_recover()
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .ok_or_else(|| DoorError::InvalidArguments(format!("there is no render {id}")))
    }

    /// The render of `snapshot` at `settings`: the one made or running, or a
    /// new one on a thread of its own.
    fn start(
        &self,
        helper: std::path::PathBuf,
        snapshot: Snapshot,
        settings: AgentExportSettings,
        content: &ContentStore,
    ) -> Arc<Job> {
        let id = snapshot.job_id(&settings);
        let mut jobs = self.jobs.lock_recover();
        if let Some(job) = jobs
            .iter()
            .find(|job| job.id == id && !matches!(*job.outcome.lock_recover(), Some(Err(_))))
        {
            return Arc::clone(job);
        }
        jobs.retain(|job| job.id != id);
        while jobs.len() >= KEPT_JOBS {
            let Some(oldest) = jobs
                .iter()
                .position(|job| job.outcome.lock_recover().is_some())
            else {
                break;
            };
            jobs.remove(oldest);
        }
        let job = Arc::new(Job {
            id,
            settings,
            facts: video_editor::facts::of(&snapshot.project),
            version: snapshot.heads.join("+"),
            progress: Mutex::new((0, 0)),
            cancelled: std::sync::atomic::AtomicBool::new(false),
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
                *worker.outcome.lock_recover() = Some(outcome);
                worker.settled.notify_all();
            });
        if let Err(error) = started {
            *job.outcome.lock_recover() = Some(Err(format!("the render could not start: {error}")));
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
        let _slot = self.slot.lock_recover();
        if job.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(client::STOPPED.to_owned());
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
                *job.progress.lock_recover() = (done, total);
                !job.cancelled.load(std::sync::atomic::Ordering::Relaxed)
            },
        )?;
        let Reply::Video(rendered) = finished.reply else {
            return Err("the render helper answered with something else".into());
        };
        let (address, bytes) = keep(finished.output, MAX_OUTPUT, content)?;
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
        let (address, bytes) = keep(finished.output, MAX_STILL, content)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({
                "content": address,
                "media_type": "image/png",
                "at": frame.at,
                "width": frame.width,
                "height": frame.height,
                "bytes": bytes,
            })),
            resources: vec![address],
        })
    }

    /// Wait at most `wait` for `job`, then answer where it is.
    fn wait(job: &Job, wait: Duration) -> ActionOutput {
        let until = after(wait);
        let mut outcome = job.outcome.lock_recover();
        while outcome.is_none() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            outcome = job
                .settled
                .wait_timeout(outcome, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        match outcome.as_ref() {
            Some(Ok(made)) => {
                let mut answer =
                    done_answer(&made.address, made.rendered, job.settings, made.bytes);
                if let Some(answer) = answer.as_object_mut() {
                    answer.insert("version".into(), job.version.clone().into());
                    answer.insert(crate::facts::FACTS.into(), job.facts.clone());
                }
                ActionOutput {
                    value: Some(answer),
                    resources: vec![made.address.clone()],
                }
            }
            Some(Err(error)) => value(failed_answer(&job.id, error)),
            None => {
                let (done, total) = *job.progress.lock_recover();
                value(rendering_answer(&job.id, done, total))
            }
        }
    }
}

/// A record as the host holds it, read once for one render.
struct Snapshot {
    record: String,
    heads: Vec<String>,
    bytes: Vec<u8>,
    project: Project,
    /// The range the editor's `render` named, part of the render's identity.
    range: (Option<f64>, Option<f64>),
}

impl Snapshot {
    fn of(record: &ConfinedReplica) -> Result<Self, String> {
        let (bytes, heads, project) = record
            .read(|doc| {
                let mut doc = doc.clone();
                let mut heads: Vec<String> =
                    doc.get_heads().iter().map(ToString::to_string).collect();
                heads.sort();
                (doc.save(), heads, Project::read(&doc))
            })
            .ok_or("the record is not held")?;
        let project = project.map_err(|error| format!("the timeline does not read: {error}"))?;
        Ok(Self {
            record: record.record_id().as_str().to_owned(),
            heads,
            bytes,
            project,
            range: (None, None),
        })
    }

    /// The render's identity: the record at these heads, rendered so.
    fn job_id(&self, settings: &AgentExportSettings) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo-agent-render/v1\0");
        hasher.update(self.record.as_bytes());
        for head in &self.heads {
            hasher.update(b"\0");
            hasher.update(head.as_bytes());
        }
        let settings = serde_json::json!({
            "settings": settings,
            "range": [self.range.0, self.range.1],
        });
        hasher.update(settings.to_string().as_bytes());
        let hex = hasher.finalize().to_hex();
        format!("render-{}", hex.get(..16).unwrap_or_default())
    }

    /// Refuse a source the room does not hold.
    fn check_sources(&self, content: &ContentStore) -> Result<(), String> {
        match agent_render::placed_content(&self.project)
            .into_iter()
            .find(|hash| !content.has(hash))
        {
            Some(hash) => Err(format!(
                "the room does not hold tilo.content:{hash}, which the timeline places: \
                 import it again"
            )),
            None => Ok(()),
        }
    }

    /// The job directory the helper renders: the document and its sources.
    fn lay_out(&self, content: &ContentStore) -> Result<JobDir, String> {
        let mut dir = JobDir::create(&std::env::temp_dir(), SOURCE_BUDGET)?;
        dir.put_snapshot(&self.bytes)?;
        for hash in agent_render::placed_content(&self.project) {
            let mut reader = content
                .open(&hash)
                .ok_or_else(|| format!("the room no longer holds tilo.content:{hash}"))?;
            dir.put_source(&hash, &mut reader)?;
        }
        Ok(dir)
    }
}

/// Keep what the helper wrote, at most `limit` bytes, as room content:
/// its address and size.
fn keep(
    output: std::fs::File,
    limit: u64,
    content: &ContentStore,
) -> Result<(String, u64), String> {
    let mut bytes = Vec::new();
    output
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("the render could not be read: {error}"))?;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > limit {
        return Err(format!(
            "the render is larger than an agent's render may be kept ({} at most): render a \
             shorter range or a smaller size",
            ui::format::size(limit)
        ));
    }
    if size == 0 {
        return Err("the render helper wrote nothing".into());
    }
    let hash = content
        .put(&bytes)
        .map_err(|error| format!("the render could not be kept: {error}"))?;
    let address = tilo_actions::content_resource(&hash)
        .ok_or_else(|| format!("{hash:?} is not a content hash"))?;
    Ok((address, size))
}

/// A seconds argument, absent or a finite number.
fn number(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<f64>, DoorError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|seconds| seconds.is_finite())
            .map(Some)
            .ok_or_else(|| DoorError::InvalidArguments(format!("`{name}` is seconds"))),
    }
}

/// `wait` from now; a wait past what the clock can name ends now.
fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

fn value(answer: serde_json::Value) -> ActionOutput {
    ActionOutput {
        value: Some(answer),
        resources: Vec::new(),
    }
}

fn refused(reason: String) -> DoorError {
    DoorError::Unavailable {
        code: "render_refused".into(),
        reason,
    }
}

#[cfg(test)]
mod tests;

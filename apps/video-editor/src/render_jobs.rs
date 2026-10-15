//! The timeline renders an agent asks for (`export-video-content`,
//! `export-video-status`, `cancel-video-export`, `view-frame-content`),
//! answered by the editor itself.
//!
//! An agent's host answers these in its render service, never in the
//! confined worker that holds the record (`tilo.host-render`). A desktop
//! build with the media stack answers the same actions in process, so a
//! script or a person's own automation gets the same answers: the render
//! goes to a temporary file and into the window's content store. A build
//! without the media stack says where these renders are made instead.

use crate::actions::VideoEditorEffect;
use crate::model::Project;

/// What a build that cannot render answers.
#[cfg(not(all(feature = "gst", feature = "decode")))]
pub const UNAVAILABLE: &str = "rendering the timeline for a caller runs in a host's render \
    service (tilo-video-render), which this editor does not have: render it in the editor \
    with media support, or on a host that has the service";

/// Answer one of the render effects for `project`.
///
/// # Errors
///
/// Why the render cannot be made: bad arguments, a timeline an agent may
/// not render, or no media stack in this build.
pub(crate) fn answer(
    effect: &VideoEditorEffect,
    project: &Project,
    cx: &mut ui::framework::EffectCx<crate::resource::VideoProject, crate::VideoEditorApp>,
) -> ui::framework::EffectAnswer {
    #[cfg(all(feature = "gst", feature = "decode"))]
    {
        local::answer(effect, project, cx)
    }
    #[cfg(not(all(feature = "gst", feature = "decode")))]
    {
        let _ = (effect, project, cx);
        Err(UNAVAILABLE.to_owned())
    }
}

#[cfg(all(feature = "gst", feature = "decode"))]
mod local {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ui::framework::{EffectAnswer, Effected};
    use ui::tilo_resource::Effect as _;

    use super::VideoEditorEffect;
    use crate::export::{
        AgentExportSettings, AgentFrameSettings, AgentRendered, RenderState, agent_done_answer,
        agent_failed_answer, agent_rendering_answer, agent_wait_from_args,
    };
    use crate::model::Project;

    type Cx = ui::framework::EffectCx<crate::resource::VideoProject, crate::VideoEditorApp>;

    /// One render, running or finished, kept so asking again is cheap.
    struct Job {
        id: String,
        settings: AgentExportSettings,
        state: RenderState,
        output: std::path::PathBuf,
        outcome: Mutex<Option<Result<AgentRendered, String>>>,
    }

    /// Renders this process made, newest last; the oldest finished ones go.
    static JOBS: Mutex<Vec<Arc<Job>>> = Mutex::new(Vec::new());
    const KEPT_JOBS: usize = 8;

    /// Where a job has got to when its caller stops waiting.
    enum Seen {
        Done(AgentRendered),
        Running(u64, u64),
        Failed(String),
    }

    pub(super) fn answer(
        effect: &VideoEditorEffect,
        project: &Project,
        cx: &mut Cx,
    ) -> EffectAnswer {
        let args = effect.arguments();
        match effect {
            VideoEditorEffect::ExportVideoContent { .. } => {
                crate::export::agent_renderable(project)?;
                let settings = AgentExportSettings::from_args(&args, project.aspect())?;
                let wait = agent_wait_from_args(&args)?;
                let job = start(project, settings)?;
                Ok(wait_then_answer(job, wait, cx))
            }
            VideoEditorEffect::ExportVideoStatus { job, .. } => {
                let wait = agent_wait_from_args(&args)?;
                let job = find(job).ok_or_else(|| format!("there is no render {job}"))?;
                Ok(wait_then_answer(job, wait, cx))
            }
            VideoEditorEffect::CancelVideoExport { job } => {
                let found = find(job).ok_or_else(|| format!("there is no render {job}"))?;
                found.state.cancel.store(true, Ordering::Relaxed);
                Ok(Effected::value(agent_failed_answer(
                    job,
                    crate::export::CANCELLED,
                )))
            }
            VideoEditorEffect::ViewFrameContent { .. } => {
                let frame =
                    AgentFrameSettings::from_args(&args, project.aspect(), project.duration())?;
                let project = project.clone();
                Ok(cx.thread(
                    move |_| crate::export::render_frame_png(&project, &frame),
                    move |_, png, _| {
                        let png = png?;
                        let hash = keep(&png)?;
                        Ok(frame_answer(&hash, &frame))
                    },
                ))
            }
            _ => Err("not a render".into()),
        }
    }

    fn frame_answer(hash: &str, frame: &AgentFrameSettings) -> Effected {
        let address = crate::media::content_address(hash);
        Effected::Done(ui::tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "content": address,
                "at": frame.at,
                "width": frame.width,
                "height": frame.height,
            })),
            resources: vec![address],
        })
    }

    fn keep(bytes: &[u8]) -> Result<String, String> {
        let content = ui::tilo_resource::content::current()
            .ok_or("this window keeps no content to render into")?;
        content
            .put(bytes)
            .map_err(|error| format!("the render could not be kept: {error}"))
    }

    fn find(id: &str) -> Option<Arc<Job>> {
        JOBS.lock().ok()?.iter().find(|job| job.id == id).cloned()
    }

    /// The job rendering `project` at `settings`: the one already made or
    /// running, else a new one on a thread of its own.
    fn start(project: &Project, settings: AgentExportSettings) -> Result<Arc<Job>, String> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{project:?}").hash(&mut hasher);
        settings.hash(&mut hasher);
        let id = format!("render-{:016x}", hasher.finish());
        let mut jobs = JOBS
            .lock()
            .map_err(|_| "the render list is unusable".to_owned())?;
        if let Some(job) = jobs.iter().find(|job| {
            job.id == id
                && job
                    .outcome
                    .lock()
                    .is_ok_and(|outcome| !matches!(*outcome, Some(Err(_))))
        }) {
            return Ok(job.clone());
        }
        let output = tilo_paths::temp_dir()
            .ok_or("this device has no temporary directory to render into")?
            .join(format!("tilo-{id}.mp4"));
        let job = Arc::new(Job {
            id,
            settings,
            state: RenderState::default(),
            output,
            outcome: Mutex::new(None),
        });
        jobs.retain(|old| old.id != job.id);
        while jobs.len() >= KEPT_JOBS {
            let Some(oldest) = jobs
                .iter()
                .position(|old| old.outcome.lock().is_ok_and(|o| o.is_some()))
            else {
                break;
            };
            let old = jobs.remove(oldest);
            crate::files::remove(&old.output);
        }
        jobs.push(job.clone());
        drop(jobs);
        let (worker, project) = (job.clone(), project.clone());
        ui::rt::spawn_thread("video-editor-agent-render", move || {
            let result = crate::export::run_to_sink(
                &project,
                &crate::export::Encoding::Agent(worker.settings),
                &crate::export::Sink::Path(&worker.output),
                &worker.state,
            );
            if let Ok(mut outcome) = worker.outcome.lock() {
                *outcome = Some(result);
            }
        })
        .map_err(|error| format!("the render could not start: {error}"))?;
        Ok(job)
    }

    /// Wait off the window's thread for `job`, at most `wait`, then answer
    /// what it came to.
    fn wait_then_answer(job: Arc<Job>, wait: Duration, cx: &mut Cx) -> Effected {
        let waited = job.clone();
        cx.thread(
            move |progress| {
                let until = std::time::Instant::now() + wait;
                loop {
                    if let Some(seen) = settled(&waited) {
                        return seen;
                    }
                    if std::time::Instant::now() >= until || progress.is_cancelled() {
                        return Seen::Running(
                            waited.state.done.load(Ordering::Relaxed),
                            waited.state.total.load(Ordering::Relaxed),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            },
            move |_, seen, _| match seen {
                Seen::Running(done, total) => Ok(Effected::value(agent_rendering_answer(
                    &job.id, done, total,
                ))),
                Seen::Failed(error) => Ok(Effected::value(agent_failed_answer(&job.id, &error))),
                Seen::Done(rendered) => {
                    let bytes = std::fs::read(&job.output)
                        .map_err(|error| format!("the render could not be read: {error}"))?;
                    let hash = keep(&bytes)?;
                    let address = crate::media::content_address(&hash);
                    Ok(Effected::Done(ui::tilo_actions::ActionOutput {
                        value: Some(agent_done_answer(
                            &address,
                            rendered,
                            job.settings,
                            bytes.len() as u64,
                        )),
                        resources: vec![address],
                    }))
                }
            },
        )
    }

    fn settled(job: &Job) -> Option<Seen> {
        match job.outcome.lock().ok()?.as_ref()? {
            Ok(rendered) => Some(Seen::Done(*rendered)),
            Err(error) => Some(Seen::Failed(error.clone())),
        }
    }
}

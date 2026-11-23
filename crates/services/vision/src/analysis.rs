//! The library's analysis, as one job over every picture the system index
//! holds: each result is written back into that index as an entry beside the
//! app's own, so search finds a picture by the words in it and by what it
//! shows, in every app.
//!
//! The queue, the counts and the failures are the job's own: a picture is
//! taken one at a time, a pause stops between pictures, a cancellation stops
//! the run, and a retry takes only what failed. What runs the model is
//! [`VisionModel`], so a test's fixture model drives the whole path and the
//! device's own model answers in production.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use crate::model::{ModelError, VisionModel};
use crate::{AnalysisFailure, AnalysisFinding, AnalysisReading};

/// Reads a picture's bytes from this device's store, by the picture's
/// address and the workspace that keeps it.
pub type PictureRead = dyn Fn(&str, &str) -> Option<Vec<u8>> + Send + Sync;
/// [`PictureRead`], shared with the run's thread.
pub type PictureBytes = Arc<PictureRead>;

/// Enough failures to explain a run without letting a corrupt library grow
/// an unbounded log. The total failure count is kept apart, so cutting the
/// detail never makes the summary dishonest.
pub const FAILURE_DETAIL_LIMIT: usize = 32;

/// One picture the job analyzes: where search keeps it, and where its bytes
/// are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    /// The resource id a search hit opens.
    pub resource: String,
    /// The app that keeps it.
    pub app: String,
    /// The workspace it is in.
    pub workspace: String,
    /// The record the app publishes it from.
    pub record: String,
    /// The picture's bytes: a content reference in this device's store.
    pub image: String,
}

/// Every picture the system index holds whose bytes this device can read.
pub trait ImageSource: Send + Sync {
    /// The pictures to analyze.
    ///
    /// # Errors
    /// Why the index cannot be read now.
    fn images(&self) -> Result<Vec<ImageRef>, String>;
}

/// What one analyzed picture is found by: the words a search looks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub description: String,
    pub ocr_text: String,
    pub objects: Vec<String>,
    pub pets: Vec<String>,
    pub model: String,
    pub revision: String,
}

/// Where one analyzed picture's words go: the system index, beside the entry
/// the app that keeps it published.
pub trait AnalysisSink: Send + Sync {
    /// Publish what `found` says of `image`.
    ///
    /// # Errors
    /// Why the index refused it.
    fn enriched(&self, image: &ImageRef, found: &Found) -> Result<(), String>;
}

/// Where a run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Running,
    Paused,
    Complete,
    Cancelled,
}

impl Phase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Default)]
struct State {
    phase: Phase,
    total: usize,
    completed: usize,
    succeeded: usize,
    failed: usize,
    current: String,
    model: String,
    revision: String,
    targets: VecDeque<String>,
    by_id: BTreeMap<String, ImageRef>,
    failures: VecDeque<AnalysisFailure>,
    /// A run was asked to stop before its next picture.
    stop: bool,
}

impl State {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a library's picture count is far inside f64's exact range"
    )]
    fn reading(&self) -> AnalysisReading {
        AnalysisReading {
            phase: self.phase.as_str().to_owned(),
            total: self.total,
            completed: self.completed,
            succeeded: self.succeeded,
            failed: self.failed,
            remaining: self.targets.len(),
            current: self.current.clone(),
            model: self.model.clone(),
            revision: self.revision.clone(),
            progress: if self.total == 0 {
                if self.phase == Phase::Complete {
                    1.0
                } else {
                    0.0
                }
            } else {
                self.completed.min(self.total) as f64 / self.total as f64
            },
            failures: self.failures.iter().cloned().collect(),
        }
    }

    fn finish_if_done(&mut self) {
        if self.phase == Phase::Running && self.targets.is_empty() {
            self.phase = Phase::Complete;
            self.current.clear();
        }
    }
}

/// The library analysis: its reading, and the run behind it.
pub struct AnalysisJob {
    state: Arc<Mutex<State>>,
    source: Arc<dyn ImageSource>,
    sink: Arc<dyn AnalysisSink>,
    model: Arc<dyn VisionModel>,
    /// The bytes of the picture `image` names, from this device's content
    /// store.
    bytes: PictureBytes,
    thread: Option<std::thread::JoinHandle<()>>,
    wake: tilo_service::Wake,
    published: String,
}

impl AnalysisJob {
    /// A job over `source`, writing what `model` finds through `sink`, reading
    /// pictures through `bytes`.
    #[must_use]
    pub fn new(
        source: Arc<dyn ImageSource>,
        sink: Arc<dyn AnalysisSink>,
        model: Arc<dyn VisionModel>,
        bytes: PictureBytes,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            source,
            sink,
            model,
            bytes,
            thread: None,
            wake,
            published: String::new(),
        }
    }

    /// The run's reading.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a library's picture count is far inside f64's exact range"
    )]
    pub fn reading(&self) -> AnalysisReading {
        self.state.lock_recover().reading()
    }

    /// Whether the reading moved since the last call.
    pub fn poll(&mut self) -> bool {
        if self
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
        let reading = self.reading();
        let published = serde_json::to_string(&reading).unwrap_or_default();
        if published == self.published {
            return false;
        }
        self.published = published;
        true
    }

    /// Wait until the run stops of itself, for a caller that reads the
    /// reading once it is final: a headless run, a test.
    pub fn settle(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.poll();
    }

    /// Start over every picture the index holds. `false` for a run already in
    /// progress.
    ///
    /// # Errors
    /// Why the index cannot be read.
    pub fn start(&mut self) -> Result<bool, String> {
        let images = self.source.images()?;
        let mut state = self.state.lock_recover();
        if state.phase == Phase::Running {
            return Ok(false);
        }
        Self::begin(&mut state, images, &self.model);
        drop(state);
        self.spawn();
        Ok(true)
    }

    /// Retry the pictures that failed. `false` when none did or a run is in
    /// progress.
    pub fn retry(&mut self) -> bool {
        let mut state = self.state.lock_recover();
        if state.phase == Phase::Running || state.failures.is_empty() {
            return false;
        }
        let failed: Vec<ImageRef> = state
            .failures
            .iter()
            .filter_map(|failure| state.by_id.get(&failure.resource).cloned())
            .collect();
        Self::begin(&mut state, failed, &self.model);
        drop(state);
        self.spawn();
        true
    }

    /// Pause after the picture in progress. `false` when it is not running.
    pub fn pause(&self) -> bool {
        let mut state = self.state.lock_recover();
        if state.phase != Phase::Running {
            return false;
        }
        state.phase = Phase::Paused;
        true
    }

    /// Resume a paused run. `false` when it is not paused.
    pub fn resume(&mut self) -> bool {
        let mut state = self.state.lock_recover();
        if state.phase != Phase::Paused {
            return false;
        }
        state.stop = false;
        state.phase = Phase::Running;
        drop(state);
        // A thread still finishing the picture it was on takes the run up
        // again itself; only a run whose thread stopped needs another.
        if self
            .thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
        {
            self.spawn();
        }
        true
    }

    /// Stop the run before its next picture. `false` when nothing runs.
    pub fn cancel(&self) -> bool {
        let mut state = self.state.lock_recover();
        if state.phase != Phase::Running {
            return false;
        }
        state.stop = true;
        state.phase = Phase::Cancelled;
        true
    }

    fn begin(state: &mut State, images: Vec<ImageRef>, model: &Arc<dyn VisionModel>) {
        state.phase = Phase::Running;
        state.total = images.len();
        state.completed = 0;
        state.succeeded = 0;
        state.failed = 0;
        state.current.clear();
        state.failures.clear();
        state.by_id = images
            .iter()
            .map(|image| (image.resource.clone(), image.clone()))
            .collect();
        state.targets = images.into_iter().map(|image| image.resource).collect();
        state.stop = false;
        let reading = model.reading();
        state.model = reading.model;
        state.revision = reading.revision;
    }

    fn spawn(&mut self) {
        let state = Arc::clone(&self.state);
        let source_model = Arc::clone(&self.model);
        let sink = Arc::clone(&self.sink);
        let bytes = Arc::clone(&self.bytes);
        let wake = Arc::clone(&self.wake);
        let spawned = std::thread::Builder::new()
            .name("tilo-vision-analysis".into())
            .spawn(move || {
                run(
                    &state,
                    source_model.as_ref(),
                    sink.as_ref(),
                    bytes.as_ref(),
                    &wake,
                );
            });
        match spawned {
            Ok(thread) => self.thread = Some(thread),
            Err(error) => {
                tracing::warn!(%error, "the image analysis did not start");
                let mut state = self.state.lock_recover();
                state.phase = Phase::Idle;
            }
        }
    }
}

/// The run: one picture at a time, until stopped or done.
fn run(
    state: &Arc<Mutex<State>>,
    model: &dyn VisionModel,
    sink: &dyn AnalysisSink,
    bytes: &PictureRead,
    wake: &tilo_service::Wake,
) {
    loop {
        let next = {
            let mut state = state.lock_recover();
            if state.stop || state.phase != Phase::Running {
                break;
            }
            match state.targets.pop_front() {
                None => {
                    state.finish_if_done();
                    break;
                }
                Some(resource) => {
                    state.current.clone_from(&resource);
                    state.by_id.get(&resource).cloned()
                }
            }
        };
        let Some(image) = next else {
            let mut state = state.lock_recover();
            state.completed = state.completed.saturating_add(1);
            state.failed = state.failed.saturating_add(1);
            state.failures.push_back(AnalysisFailure {
                resource: String::new(),
                reason: "the picture left the index before it was analyzed".to_owned(),
            });
            state.current.clear();
            drop(state);
            continue;
        };
        let found = bytes(&image.image, &image.workspace).map_or_else(
            || {
                Err(ModelError::Failed(
                    "the picture's bytes are not on this device".to_owned(),
                ))
            },
            |bytes| {
                model.analyze(&bytes).map(|analysis| Found {
                    description: analysis.description,
                    // The model's own words about the picture are enough to
                    // find it by; the local text reader only adds to them
                    // where one runs here.
                    ocr_text: analysis.ocr_text,
                    objects: analysis.objects,
                    pets: analysis.pets,
                    model: analysis.model,
                    revision: analysis.revision,
                })
            },
        );
        let mut state = state.lock_recover();
        state.current.clear();
        match found {
            Ok(found) => match sink.enriched(&image, &found) {
                Ok(()) => {
                    state.completed = state.completed.saturating_add(1);
                    state.succeeded = state.succeeded.saturating_add(1);
                }
                Err(reason) => {
                    state.completed = state.completed.saturating_add(1);
                    state.failed = state.failed.saturating_add(1);
                    record_failure(&mut state, &image.resource, reason);
                }
            },
            Err(ModelError::Refused(reason)) => {
                // A model's own refusal is the run's: asking again cannot
                // change the answer, and every picture would fail alike.
                state.completed = state.completed.saturating_add(1);
                state.failed = state.failed.saturating_add(1);
                record_failure(&mut state, &image.resource, reason);
                state.phase = Phase::Cancelled;
                state.stop = true;
                break;
            }
            Err(error) => {
                state.completed = state.completed.saturating_add(1);
                state.failed = state.failed.saturating_add(1);
                record_failure(&mut state, &image.resource, error.to_string());
            }
        }
        state.finish_if_done();
        drop(state);
        wake();
    }
    let mut state = state.lock_recover();
    if state.phase == Phase::Running {
        state.finish_if_done();
    }
    drop(state);
    wake();
}

fn record_failure(state: &mut State, resource: &str, reason: String) {
    if state.failures.len() == FAILURE_DETAIL_LIMIT {
        state.failures.pop_front();
    }
    state.failures.push_back(AnalysisFailure {
        resource: resource.to_owned(),
        reason,
    });
}

/// What one analyzed picture is found by, as a sink publishes it.
#[must_use]
pub fn finding(image: &ImageRef, found: &Found) -> AnalysisFinding {
    use tilo_resource::search_index::IndexWeight;
    let mut fields = Vec::new();
    if !found.ocr_text.is_empty() {
        fields.push((found.ocr_text.clone(), IndexWeight::Body));
    }
    fields.extend(
        found
            .objects
            .iter()
            .chain(&found.pets)
            .map(|label| (label.clone(), IndexWeight::Metadata)),
    );
    AnalysisFinding {
        resource: image.resource.clone(),
        key: image.resource.clone(),
        title: if found.description.is_empty() {
            image.resource.clone()
        } else {
            found.description.clone()
        },
        fields,
        model: found.model.clone(),
        revision: found.revision.clone(),
    }
}

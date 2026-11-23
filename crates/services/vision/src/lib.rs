//! The device's image model: the one place a picture is bounded and encoded
//! for a model, and the model is called, published as actions, with the
//! library analysis job that writes what it finds into the system index.
//!
//! A shell, an agent, a script and another app reach the model the same way:
//! [`IMAGE_MODEL_RESOURCE`] offers `describe`, `ocr`, `tags` and `faces`, each
//! taking a picture's address in the content store and answering with what the
//! model found. The model is the AI settings hub's choice
//! ([`model::LocalVision`]), reached through the model gateway, which holds
//! the provider's key: the key is used, never read.
//!
//! [`ANALYSIS_RESOURCE`] is the whole-library run: starting it analyzes every
//! picture the system index holds and publishes each result back into that
//! index ([`analysis::AnalysisSink`]), so search finds a picture by the words
//! in it and by what it shows, in every app. Its reading is the run's own
//! progress, and `pause`, `resume`, `retry` and `cancel` act on that run.
//!
//! A call acts as the member it is made for: only the person at this device
//! may ask it to read their pictures, and a refusal stays a refusal.

pub mod analysis;
pub mod engine;
pub mod index;
pub mod model;

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionInvoker, ActionOutput, ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

pub use analysis::{AnalysisJob, AnalysisSink, Found, ImageRef, ImageSource, Phase, finding};
pub use model::{FaceBatch, FaceCluster, LocalVision, ModelError, VisionModel};

/// The platform package the model service answers as, as a view reads it
/// (`use so.tilo.vision as vision`).
pub const APP: &str = "so.tilo.vision";

/// The resource the model's actions are published on.
pub const IMAGE_MODEL_RESOURCE: &str = "vision:vision.image-model";
/// Its kind.
pub const IMAGE_MODEL_KIND: &str = "vision.image-model";

/// The resource the library analysis run is published on.
pub const ANALYSIS_RESOURCE: &str = "vision:vision.analysis";
/// Its kind.
pub const ANALYSIS_KIND: &str = "vision.analysis";

/// What wakes the session's loop when a run's reading moves.
pub type Wake = tilo_service::Wake;

mod i18n {
    tilo_resource::declared_catalogue!(CATALOGUE);

    pub(crate) fn name(kind: &str, source: &str) -> String {
        tilo_resource::label::localized(kind, source, Some(&CATALOGUE))
    }
}

/// Which model answers now, as the AI settings hub chose it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct ImageModelReading {
    /// Whether a model answers here now.
    pub available: bool,
    /// The provider it came from, by id.
    pub provider: String,
    /// The model, by id.
    pub model: String,
    /// The model's pinned revision, kept with every result.
    pub revision: String,
    /// Whether a face model answers too.
    pub faces: bool,
    /// Why no model answers, in words for a person; empty when one does.
    pub detail: String,
}

/// Where the library analysis stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct AnalysisReading {
    /// `idle`, `running`, `paused`, `complete` or `cancelled`.
    pub phase: String,
    pub total: usize,
    pub completed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub remaining: usize,
    /// The picture being analyzed now, by resource id; empty between them.
    pub current: String,
    pub model: String,
    pub revision: String,
    /// Done over total, from 0 to 1.
    pub progress: f64,
    /// Up to [`analysis::FAILURE_DETAIL_LIMIT`] failures, newest last.
    pub failures: Vec<AnalysisFailure>,
}

/// One picture the analysis could not do, and why.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct AnalysisFailure {
    /// The picture, by resource id.
    pub resource: String,
    pub reason: String,
}

/// What one analyzed picture is found by, as its app's record publishes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnalysisFinding {
    /// The resource a hit opens.
    pub resource: String,
    /// The entry's own key within the record.
    pub key: String,
    /// The row's title: the description, else the resource id.
    pub title: String,
    /// The words the picture is found by: its text, objects and pets, each
    /// with the weight a search gives it.
    pub fields: Vec<(String, tilo_resource::search_index::IndexWeight)>,
    pub model: String,
    pub revision: String,
}

/// The model's resources, as declared types.
#[derive(tilo_resource::Row)]
#[row(kind = "vision.image-model", label = "Image model")]
pub struct ImageModelResource;

#[derive(tilo_resource::Row)]
#[row(kind = "vision.analysis", label = "Photo analysis")]
pub struct AnalysisResource;

/// What the image model offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.vision.image-model")]
pub enum ImageModelEffect {
    /// Describe a picture
    ///
    /// What it shows, in a sentence, with the model and revision that said
    /// it. The picture is sent to the provider the AI settings hub chose,
    /// which keeps the key this device uses.
    #[effect(kind = "outside")]
    #[effect(returns)]
    Describe {
        #[param("The picture's address in the content store")]
        image: String,
    },
    /// Read the text in a picture
    ///
    /// The words it holds, read on this device. Needs no language model, so it
    /// answers on a device with none.
    #[effect(returns, kind = "read")]
    Ocr {
        #[param("The picture's address in the content store")]
        image: String,
        #[param("The text's language, such as `eng` or `deu`; English when left out")]
        language: Option<String>,
    },
    /// Name what a picture shows
    ///
    /// The objects and pets in it, from the same model `describe` asks.
    #[effect(kind = "outside")]
    #[effect(returns)]
    Tags {
        #[param("The picture's address in the content store")]
        image: String,
    },
    /// Find pictures of the same face
    ///
    /// The groups of pictures a face model found the same face in. The model
    /// never receives a name, a person or a photograph's own id: each picture
    /// is named by its address here, and a cluster names the pictures it
    /// joined.
    #[effect(kind = "outside")]
    #[effect(returns)]
    Faces {
        #[param("The pictures' addresses in the content store, at most twelve")]
        images: Vec<String>,
    },
}

/// What the library analysis offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.vision.analysis")]
pub enum AnalysisEffect {
    /// Analyze the library's pictures
    ///
    /// Analyze every picture the system index holds and add what each shows to
    /// its entry in search. Pixels stay on this device; only the bounded
    /// result is shared.
    #[effect(kind = "outside")]
    #[effect(job, cancellable)]
    Start,
    /// Pause the analysis
    ///
    /// Stop after the picture in progress; `resume` continues it.
    #[effect(kind = "outside")]
    #[effect(returns)]
    Pause,
    /// Resume the analysis
    ///
    /// Continue a paused run without widening its queue.
    #[effect(kind = "outside")]
    #[effect(job, cancellable)]
    Resume,
    /// Retry the pictures that failed
    ///
    /// Analyze only the pictures whose last analysis failed, without starting
    /// the whole library again.
    #[effect(kind = "outside")]
    #[effect(job, cancellable)]
    Retry,
    /// Stop the analysis
    ///
    /// Stop before the next picture. What was analyzed stays analyzed.
    #[effect(kind = "outside")]
    #[effect(returns)]
    Cancel,
}

/// One resource's surface, as a package of another language reads it.
#[must_use]
pub fn surfaces() -> Vec<tilo_actions::surface::Surface> {
    vec![
        tilo_resource::surface::reading_surface::<ImageModelReading, ImageModelEffect>(
            IMAGE_MODEL_KIND,
            "Image model",
        ),
        tilo_resource::surface::reading_surface::<AnalysisReading, AnalysisEffect>(
            ANALYSIS_KIND,
            "Photo analysis",
        ),
    ]
}

/// The device's image model service.
pub struct VisionService {
    model: Arc<dyn VisionModel>,
    content: Arc<Mutex<Option<tilo_content::ContentStore>>>,
    job: AnalysisJob,
    /// The person at this device, whose pictures it reads.
    person: Option<String>,
}

impl VisionService {
    /// A service answering with `model` over the pictures `source` names,
    /// writing what it finds through `sink`, waking the session's loop with
    /// `wake`.
    pub fn new(
        model: Arc<dyn VisionModel>,
        source: Arc<dyn ImageSource>,
        sink: Arc<dyn AnalysisSink>,
        wake: Wake,
    ) -> Self {
        let content: Arc<Mutex<Option<tilo_content::ContentStore>>> = Arc::default();
        let bytes = {
            let content = Arc::clone(&content);
            Arc::new(move |reference: &str, workspace: &str| {
                read_picture(&content, reference, workspace)
            }) as Arc<dyn Fn(&str, &str) -> Option<Vec<u8>> + Send + Sync>
        };
        Self {
            job: AnalysisJob::new(source, sink, Arc::clone(&model), bytes, wake),
            model,
            content,
            person: None,
        }
    }

    /// Read the picture `address` names from this device's store, in any
    /// workspace this session shows: a view names a picture without saying
    /// which workspace keeps it.
    fn picture(&self, address: &str) -> Result<Vec<u8>, ApiError> {
        read_picture(&self.content, address, "").ok_or_else(|| ApiError::Unavailable {
            code: "image_not_here".to_owned(),
            reason: "the picture's bytes are not on this device yet; retry after it syncs"
                .to_owned(),
        })
    }

    /// `error` as the API refuses it: a model's own refusal stays one.
    fn refusal(error: ModelError) -> ApiError {
        match error {
            ModelError::Unavailable(reason) => ApiError::Unavailable {
                code: "vision_unavailable".to_owned(),
                reason,
            },
            ModelError::Refused(reason) => ApiError::Refused {
                code: "model_refused".to_owned(),
                reason,
            },
            ModelError::Failed(reason) => ApiError::Failed(reason),
        }
    }

    /// Whether `invoker` is the person at this device: the model reads their
    /// pictures, and nobody else's.
    fn permitted(&self, invoker: &ActionInvoker) -> bool {
        let Some(person) = &self.person else {
            return true;
        };
        matches!(
            invoker.acting.member_id(),
            Some(tilo_actions::MemberId::Person(id)) if id.as_str() == person
        )
    }

    fn image_model(
        &self,
        published: &[ResourceSpec],
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let effect =
            decode::<ImageModelEffect>(published, IMAGE_MODEL_RESOURCE, action, arguments)?;
        let value = match effect {
            ImageModelEffect::Describe { image } => {
                let picture = self.picture(&image)?;
                let analysis = self.model.analyze(&picture).map_err(Self::refusal)?;
                serde_json::json!({
                    "description": analysis.description,
                    "model": analysis.model,
                    "revision": analysis.revision,
                })
            }
            ImageModelEffect::Ocr { image, language } => {
                let picture = self.picture(&image)?;
                let text = self
                    .model
                    .ocr(&picture, language.as_deref().unwrap_or("eng"))
                    .map_err(Self::refusal)?;
                serde_json::json!({ "text": text })
            }
            ImageModelEffect::Tags { image } => {
                let picture = self.picture(&image)?;
                let analysis = self.model.analyze(&picture).map_err(Self::refusal)?;
                serde_json::json!({
                    "objects": analysis.objects,
                    "pets": analysis.pets,
                    "model": analysis.model,
                    "revision": analysis.revision,
                })
            }
            ImageModelEffect::Faces { images } => {
                if images.is_empty() || images.len() > crate::engine::MAX_FACE_BATCH {
                    return Err(ApiError::InvalidArguments(format!(
                        "a face batch holds one to {} pictures",
                        crate::engine::MAX_FACE_BATCH
                    )));
                }
                let mut pictures = Vec::with_capacity(images.len());
                for address in images {
                    pictures.push((address.clone(), self.picture(&address)?));
                }
                let batch = self.model.faces(&pictures).map_err(Self::refusal)?;
                serde_json::to_value(&batch).unwrap_or(Value::Null)
            }
        };
        Ok(Started::Done(ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        }))
    }

    fn analysis(
        &mut self,
        published: &[ResourceSpec],
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let effect = decode::<AnalysisEffect>(published, ANALYSIS_RESOURCE, action, arguments)?;
        let started = match &effect {
            AnalysisEffect::Start => self.job.start().map_err(ApiError::Failed)?,
            AnalysisEffect::Pause => self.job.pause(),
            AnalysisEffect::Resume => self.job.resume(),
            AnalysisEffect::Retry => self.job.retry(),
            AnalysisEffect::Cancel => self.job.cancel(),
        };
        if !started && !matches!(&effect, AnalysisEffect::Start) {
            return Err(ApiError::Unavailable {
                code: "analysis_not_running".to_owned(),
                reason: "no analysis run is in the state that action needs".to_owned(),
            });
        }
        Ok(match &effect {
            AnalysisEffect::Pause | AnalysisEffect::Cancel => {
                Started::Done(ActionOutput::default())
            }
            _ => Started::Accepted,
        })
    }
}

impl Service for VisionService {
    fn own_person(&mut self, person: &str, _earlier: &[String]) {
        self.person = Some(person.to_owned());
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            offer::<ImageModelEffect>(tilo_resource::typed_resource::<ImageModelResource>(
                IMAGE_MODEL_RESOURCE,
                i18n::name(IMAGE_MODEL_KIND, "Image model"),
            )),
            offer::<AnalysisEffect>(tilo_resource::typed_resource::<AnalysisResource>(
                ANALYSIS_RESOURCE,
                i18n::name(ANALYSIS_KIND, "Photo analysis"),
            )),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            IMAGE_MODEL_RESOURCE => Some(reading(&self.model.reading())),
            ANALYSIS_RESOURCE => Some(reading(&self.job.reading())),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let invoker = ActionInvoker::person(self.person.clone().unwrap_or_default());
        self.invoke_as(&invoker, resource, action, arguments)
    }

    fn invoke_as(
        &mut self,
        invoker: &ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.permitted(invoker) {
            return Err(ApiError::Refused {
                code: "permission_denied".to_owned(),
                reason: "Only the person at this device may ask it to read pictures".to_owned(),
            });
        }
        let published = self.resources();
        match resource {
            IMAGE_MODEL_RESOURCE => self.image_model(&published, action, arguments),
            ANALYSIS_RESOURCE => self.analysis(&published, action, arguments),
            _ => Err(ApiError::Unknown(ActionAddress::new(resource, action))),
        }
    }

    fn poll(&mut self) -> bool {
        self.job.poll()
    }

    fn settle(&mut self) {
        self.job.settle();
    }

    fn lend_content(&mut self, store: &mut dyn FnMut(&str) -> tilo_content::ContentStore) {
        *self.content.lock_recover() = Some(store(APP));
    }
}

/// The bytes `address` names, read through the store this session lent the
/// service and then through the session's own media store: a picture of any
/// app in a workspace this session shows, on any device that keeps it.
fn read_picture(
    content: &Mutex<Option<tilo_content::ContentStore>>,
    address: &str,
    workspace: &str,
) -> Option<Vec<u8>> {
    let reference = address
        .strip_prefix("tilo-blob://")
        .or_else(|| address.strip_prefix("tilo.content:"))
        .unwrap_or(address);
    let read = content
        .lock_recover()
        .as_ref()
        .and_then(|store| store.read(reference));
    if let Some(bytes) = read {
        return Some(bytes);
    }
    session_media(workspace)?.read(reference)
}

/// This session's media store, which reads a picture of any app in any
/// workspace it shows. `None` for a host that draws no session media.
#[cfg(unix)]
fn session_media(workspace: &str) -> Option<tilo_content::ContentStore> {
    tilo_content_device::session(workspace)
}

#[cfg(not(unix))]
fn session_media(_workspace: &str) -> Option<tilo_content::ContentStore> {
    None
}

/// The picture `address` names in `store`, for a test's fixture.
#[cfg(test)]
fn put_image(store: &tilo_content::ContentStore, bytes: &[u8]) -> String {
    store.put(bytes).expect("the picture is kept")
}

#[cfg(test)]
mod tests;

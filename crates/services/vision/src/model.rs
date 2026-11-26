//! What the device's image model is, and how an image reaches it.
//!
//! A [`VisionModel`] answers about one picture: what it shows, the text in
//! it, the objects and pets in it, and which pictures show the same face.
//! [`LocalVision`] is the one this device runs: the AI settings hub's chosen
//! provider and model, reached through the model gateway's connection, which
//! holds the provider's key and never hands it over. Models are discovered
//! from [`agent_provider_config::ProviderConfig`] as the settings hub writes
//! it, so choosing a model in Settings is what this service answers with; a
//! device with no model refuses in words rather than answering empty.

#[cfg(target_os = "linux")]
use std::os::fd::AsFd as _;

use agent_provider_config::ProviderConfig;
use serde::{Deserialize, Serialize};

use crate::ImageModelReading;

/// A connection to the model gateway: a Unix socket's descriptor, or a pipe's handle on Windows,
/// where no gateway runs yet.
#[cfg(unix)]
pub type Gateway = std::os::fd::OwnedFd;
/// A connection to the model gateway: a Unix socket's descriptor, or a pipe's handle on Windows,
/// where no gateway runs yet.
#[cfg(windows)]
pub type Gateway = std::os::windows::io::OwnedHandle;

/// Why the model did not answer. A refusal is the model's own word (its
/// provider said no, or a policy did), and stays one to the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    /// No model is chosen or ready on this device.
    Unavailable(String),
    /// The model or its provider refused the request.
    Refused(String),
    /// It ran and failed.
    Failed(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) | Self::Refused(reason) | Self::Failed(reason) => {
                formatter.write_str(reason)
            }
        }
    }
}

/// Everything one model call answers about a picture: what it shows, the
/// words in it, and the objects and pets it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Analysis {
    pub description: String,
    pub ocr_text: String,
    pub objects: Vec<String>,
    pub pets: Vec<String>,
    pub model: String,
    pub revision: String,
}

/// One group of pictures the model found the same face in. Ids are the
/// caller's own, so the model never names a photograph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FaceBatch {
    pub model: String,
    pub revision: String,
    pub clusters: Vec<FaceCluster>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FaceCluster {
    pub id: String,
    pub images: Vec<String>,
}

/// The device's image model, as actions ask it. Every method takes the
/// decoded bytes of the pictures it reads, never a path.
pub trait VisionModel: Send + Sync {
    /// Which model answers now, and why none does.
    fn reading(&self) -> ImageModelReading;

    /// Everything the model answers about the picture in one call: what it
    /// shows, the words in it, the objects and pets.
    ///
    /// # Errors
    /// [`ModelError`] when no model answers.
    fn analyze(&self, image: &[u8]) -> Result<Analysis, ModelError>;

    /// The text in the picture, read on this device. Separate from
    /// [`Self::analyze`] because it needs no model: a device with no language
    /// model still reads text.
    ///
    /// # Errors
    /// [`ModelError`] when no engine reads it.
    fn ocr(&self, image: &[u8], language: &str) -> Result<String, ModelError>;

    /// Which of `images` show the same face, each named by the caller's own
    /// id.
    ///
    /// # Errors
    /// [`ModelError`] when no face model answers.
    fn faces(&self, images: &[(String, Vec<u8>)]) -> Result<FaceBatch, ModelError>;
}

/// This device's model: the AI settings hub's local vision and face models,
/// reached through the model gateway connection the service opens itself.
pub struct LocalVision {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "only Linux runs a model gateway to reach")
    )]
    gateway: Option<Gateway>,
}

impl LocalVision {
    /// The model this device runs, with the model gateway connection handed
    /// in (`None` when no gateway runs here: OCR still answers). Tests and a
    /// host that already holds a connection use this; the service opens its
    /// own with [`Self::local`].
    #[must_use]
    pub fn new(gateway: Option<Gateway>) -> Self {
        Self { gateway }
    }

    /// This device's model, holding its own connection to the model gateway:
    /// the service's capability, opened for `so.tilo.vision` the way the
    /// launcher opens one for an app it starts, so a service needs no
    /// descriptor from the process around it.
    #[must_use]
    pub fn local() -> Self {
        Self::new(Self::own_gateway())
    }

    /// The service's own connection to the model gateway.
    #[cfg(target_os = "linux")]
    fn own_gateway() -> Option<Gateway> {
        let name = launcher::services::MODEL_GATEWAY_SERVICE;
        let requirement = launcher::manifest::ServiceReq::parse(
            name,
            &format!("optional@{}", runtime_interface::RUNTIME_INTERFACE_VERSION),
        )
        .ok()?;
        let wanted = std::collections::BTreeMap::from([(name.to_owned(), requirement)]);
        let context = launcher::services::ServiceLaunchContext {
            app_id: crate::APP,
            workspace_id: None,
        };
        match launcher::services::resolve_service_binds_for_launch(&wanted, false, context) {
            Ok(resolved) => resolved.into_iter().next().map(|service| service.host),
            Err(error) => {
                tracing::debug!(%error, "no model gateway for image analysis");
                None
            }
        }
    }

    /// The model gateway's connection, when this device has one.
    #[cfg(not(target_os = "linux"))]
    fn own_gateway() -> Option<Gateway> {
        None
    }

    /// The model gateway's connection, when this device has one.
    #[cfg(target_os = "linux")]
    fn connected(&self) -> Result<&Gateway, ModelError> {
        self.gateway.as_ref().ok_or_else(|| {
            ModelError::Unavailable(
                "No model gateway runs for this device, so no model answers".to_owned(),
            )
        })
    }

    /// The text in the picture, read on this device with no model.
    fn read_text(image: &[u8], language: &str) -> Result<String, ModelError> {
        if language.is_empty()
            || !language
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        {
            return Err(ModelError::Failed(
                "A language is named by its letters only, such as `eng`".to_owned(),
            ));
        }
        let decoded = Self::decode(image)?;
        let document = tilo_ocr::recognize(&decoded, language)
            .map_err(|error| ModelError::Failed(error.user_message().to_owned()))?;
        Ok(crate::engine::bounded_text(
            &document.plain_text(),
            crate::engine::MAX_OCR_CHARS,
        ))
    }

    /// The picture's bytes decoded as the toolkit's pixels, which every model
    /// takes.
    fn decode(image: &[u8]) -> Result<tilo_image_codec::DecodedImage, ModelError> {
        tilo_image_codec::decode_now(image, Some(crate::engine::MAX_INPUT_EDGE))
            .map_err(|error| ModelError::Failed(format!("the picture cannot be read: {error}")))
    }
}

/// The local vision capability the settings hub chose, if any.
fn vision_capability() -> Option<crate::engine::LocalVisionCapability> {
    crate::engine::discover_local_capability(&ProviderConfig::load())
}

/// The local face capability the settings hub chose, if any.
fn face_capability() -> Option<crate::engine::LocalFaceCapability> {
    crate::engine::discover_local_face_capability(&ProviderConfig::load())
}

/// The reading of the chosen model: which provider, model and revision
/// answer, or why none does. It never names a key or an endpoint.
#[must_use]
pub fn reading() -> ImageModelReading {
    let config = ProviderConfig::load();
    let vision = vision_capability();
    let face = face_capability();
    let model = vision
        .as_ref()
        .map(|capability| capability.model_label.clone());
    let provider = vision
        .as_ref()
        .map(|capability| capability.profile_id.clone());
    let revision = vision
        .as_ref()
        .map(|capability| capability.revision.clone());
    let detail = if vision.is_none() {
        match config.active_profile() {
            None => "No model provider is set up. Choose one in Settings, under Agent models.",
            Some(_) => {
                "No local image model is ready. Install one in Settings, under Agent models."
            }
        }
        .to_owned()
    } else {
        String::new()
    };
    ImageModelReading {
        available: vision.is_some(),
        provider: provider.unwrap_or_default(),
        model: model.unwrap_or_default(),
        revision: revision.unwrap_or_default(),
        faces: face.is_some(),
        detail,
    }
}

#[cfg(target_os = "linux")]
fn gateway_error(error: &tilo_model_client::ClientError) -> ModelError {
    use agent_provider_config::protocol::ErrorKind;
    let kind = error.model_error().kind;
    match kind {
        ErrorKind::Unavailable => ModelError::Unavailable(error.to_string()),
        ErrorKind::PermissionDenied | ErrorKind::Authentication => {
            ModelError::Refused(error.to_string())
        }
        _ => ModelError::Failed(error.to_string()),
    }
}

/// Elsewhere no model gateway runs for the device, so no model answers.
#[cfg(not(target_os = "linux"))]
impl VisionModel for LocalVision {
    fn reading(&self) -> ImageModelReading {
        reading()
    }

    fn analyze(&self, _image: &[u8]) -> Result<Analysis, ModelError> {
        Err(ModelError::Unavailable(
            "No image model runs on this device".to_owned(),
        ))
    }

    fn ocr(&self, image: &[u8], language: &str) -> Result<String, ModelError> {
        Self::read_text(image, language)
    }

    fn faces(&self, _images: &[(String, Vec<u8>)]) -> Result<FaceBatch, ModelError> {
        Err(ModelError::Unavailable(
            "No face model runs on this device".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
impl VisionModel for LocalVision {
    fn reading(&self) -> ImageModelReading {
        reading()
    }

    fn analyze(&self, image: &[u8]) -> Result<Analysis, ModelError> {
        let capability = vision_capability().ok_or_else(|| {
            ModelError::Unavailable(
                "No local image model is ready. Install one in Settings, under Agent models."
                    .to_owned(),
            )
        })?;
        let decoded = Self::decode(image)?;
        let prepared = crate::engine::prepare(&decoded, image)
            .map_err(|error| ModelError::Failed(error.user_message().to_owned()))?;
        let analysis = crate::engine::analyze_on(self.connected()?.as_fd(), &capability, prepared)
            .map_err(|error| match error {
                crate::engine::Error::Gateway(error) => gateway_error(&error),
                error => ModelError::Failed(error.user_message().to_owned()),
            })?;
        Ok(Analysis {
            description: analysis.description,
            ocr_text: analysis.ocr,
            objects: analysis.labels,
            pets: analysis.pets,
            model: capability.model_id,
            revision: capability.revision,
        })
    }

    fn ocr(&self, image: &[u8], language: &str) -> Result<String, ModelError> {
        Self::read_text(image, language)
    }

    fn faces(&self, images: &[(String, Vec<u8>)]) -> Result<FaceBatch, ModelError> {
        let capability = face_capability().ok_or_else(|| {
            ModelError::Unavailable(
                "No local face model is ready. Install one in Settings, under Agent models."
                    .to_owned(),
            )
        })?;
        let prepared = images
            .iter()
            .map(|(id, bytes)| Ok((id.clone(), Self::decode(bytes)?, bytes.clone())))
            .collect::<Result<Vec<_>, ModelError>>()?;
        let response =
            crate::engine::cluster_faces_on(self.connected()?.as_fd(), &capability, prepared)
                .map_err(|error| match error {
                    crate::engine::Error::Gateway(error) => gateway_error(&error),
                    error => ModelError::Failed(error.user_message().to_owned()),
                })?;
        Ok(FaceBatch {
            model: capability.model_id,
            revision: capability.revision,
            clusters: response
                .clusters
                .into_iter()
                .map(|cluster| FaceCluster {
                    id: cluster.id,
                    images: cluster.photo_ids,
                })
                .collect(),
        })
    }
}

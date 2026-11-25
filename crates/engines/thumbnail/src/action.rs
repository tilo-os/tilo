//! The thumbnail batch as a device action: `make-thumbnails` of the content a
//! caller names, answered by the engine itself.
//!
//! The action is a content transform any device that serves this engine
//! performs: a batch of pictures in, a PNG of each out, byte for byte the same
//! wherever it ran. A device's dispatch answers it like any app action
//! ([`Providers`]): the engine's handler is the local run, and a device that
//! took an offloaded invocation of it runs exactly this.
//!
//! [`Providers`]: tilo_actions::remote::ActionProvider

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, ResourceSpec};

/// The provider a call to the action names.
pub const PROVIDER: &str = "tilo.thumbnail";
/// The app whose action it is, and whose grants decide who may run it.
pub const APP: &str = "so.tilo.photos";
/// The resource the action is offered on.
pub const RESOURCE: &str = "photos.library";
/// The action.
pub const ACTION: &str = "make-thumbnails";

/// The photos library as a declared type: what the engine offers its action on.
#[derive(tilo_resource::Row)]
#[row(kind = "photos.library", label = "Photos")]
pub struct Library;

/// The engine's action, as any app declares one: a content transform whose output depends on
/// its inputs alone, which the system may dispatch to the person's best-placed device.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.thumbnail", app = "so.tilo.photos")]
pub enum ThumbnailAction {
    /// Make thumbnails
    ///
    /// A small PNG of each picture, in order, the same bytes whichever device makes them.
    #[effect(
        content,
        idempotent,
        kind = "read",
        offload(cpu_ms = 120000, memory_mb = 1024, output_mb = 16)
    )]
    MakeThumbnails {
        #[param(content, "The pictures")]
        photos: Vec<tilo_resource::Bytes>,
    },
}

/// The workspace's content as the engine reads and keeps it: the pictures it
/// makes thumbnails of and the thumbnails it answers with, in the app's scope
/// in the workspace.
pub trait Content: Send + Sync {
    /// The bytes of `digest`, when this device keeps them.
    fn read(&self, workspace: &str, app: &str, digest: [u8; 32]) -> Option<Vec<u8>>;

    /// Keep `bytes` as the app's content in the workspace.
    ///
    /// # Errors
    ///
    /// The content could not be kept.
    fn keep(&self, workspace: &str, app: &str, bytes: &[u8]) -> Result<[u8; 32], String>;
}

/// The engine's `make-thumbnails`, over `content`.
pub struct Thumbnails {
    content: Arc<dyn Content>,
}

impl Thumbnails {
    /// The action over `content`.
    #[must_use]
    pub fn new(content: Arc<dyn Content>) -> Self {
        Self { content }
    }

    /// The action's specification: one content transform over a list of
    /// pictures, answered with one content address per picture that made a
    /// thumbnail.
    #[must_use]
    pub fn resource() -> ResourceSpec {
        let mut spec = tilo_resource::typed_resource::<Library>(RESOURCE, "Photos");
        spec.app_id = Some(APP.to_owned());
        tilo_resource::effect::offer::<ThumbnailAction>(spec)
    }

    /// The pictures `argument` names, by digest, or why it names none.
    fn photos(argument: &InvokeArgument) -> Result<Vec<[u8; 32]>, ActionCallError> {
        let InvokeArgument::Json(arguments) = argument else {
            return Err(ActionCallError::InvalidRequest {
                reason: "make-thumbnails reads its arguments as JSON".into(),
            });
        };
        let photos = arguments
            .get("photos")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: "make-thumbnails names the pictures to thumbnail".into(),
            })?;
        let mut digests = Vec::with_capacity(photos.len());
        for photo in photos {
            let Some(reference) = photo
                .as_str()
                .and_then(tilo_actions::named_reference)
                .filter(|reference| reference.len() == 64)
            else {
                return Err(ActionCallError::InvalidRequest {
                    reason: "a picture is not a content address".into(),
                });
            };
            let mut digest = [0; 32];
            hex::decode_to_slice(reference, &mut digest).map_err(|_| {
                ActionCallError::InvalidRequest {
                    reason: "a picture is not a content address".into(),
                }
            })?;
            digests.push(digest);
        }
        Ok(digests)
    }

    /// One batch of thumbnails over `digests`, answered as the action's output.
    fn thumbnails(
        &self,
        workspace: &str,
        digests: &[[u8; 32]],
    ) -> Result<ActionOutput, ActionCallError> {
        // A picture named twice is one picture: the batch answers one thumbnail
        // per distinct picture, as the same bytes.
        let mut distinct = std::collections::BTreeSet::new();
        let mut inputs = Vec::with_capacity(digests.len());
        let mut failed = Vec::new();
        for digest in digests {
            if !distinct.insert(*digest) {
                continue;
            }
            match self.content.read(workspace, APP, *digest) {
                Some(bytes) => inputs.push((hex::encode(digest), bytes)),
                None => failed.push(serde_json::json!({
                    "photo": hex::encode(digest),
                    "reason": "is not on this device",
                })),
            }
        }
        let distinct: Vec<Vec<u8>> = inputs.iter().map(|(_, bytes)| bytes.clone()).collect();
        let mut resources = Vec::with_capacity(inputs.len());
        for ((photo, _), outcome) in inputs.iter().zip(crate::batch::thumbnail_batch(&distinct)) {
            match outcome {
                Ok(thumbnail) => {
                    let hash = self
                        .content
                        .keep(workspace, APP, &thumbnail)
                        .map_err(|reason| ActionCallError::Unavailable {
                            reason: format!("a thumbnail was not kept: {reason}"),
                        })?;
                    resources.push(
                        tilo_actions::content_resource(&hex::encode(hash)).ok_or_else(|| {
                            ActionCallError::Unavailable {
                                reason: "the content provider returned an invalid address".into(),
                            }
                        })?,
                    );
                }
                Err(_) => {
                    failed.push(serde_json::json!({
                        "photo": photo,
                        "reason": "is not an image",
                    }));
                }
            }
        }
        Ok(ActionOutput {
            value: (!failed.is_empty()).then(|| serde_json::json!({ "failed": failed })),
            resources,
        })
    }
}

impl ActionProvider for Thumbnails {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        vec![Self::resource()]
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let resources = self.published();
        Pending::ready(Ok(CataloguePage::published(PROVIDER, &resources, &query)))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let id = request.invocation;
        let Some(workspace) = caller.workspace().map(str::to_owned) else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        if request.target.address != ActionAddress::new(RESOURCE, ACTION) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let answer = Self::photos(&request.argument)
            .and_then(|digests| self.thumbnails(&workspace, &digests));
        let update = match answer {
            Ok(output) => tilo_actions::InvocationUpdate::Succeeded {
                invocation: id,
                output,
            },
            Err(error) => {
                return Pending::ready(Ok(InvocationState::new(
                    id,
                    tilo_actions::InvocationUpdate::Failed {
                        invocation: id,
                        error: tilo_actions::ActionFailure {
                            code: error.code().into(),
                            message: error.to_string(),
                            details: None,
                            retryable: false,
                        },
                    },
                )
                .receipt()));
            }
        };
        Pending::ready(Ok(InvocationState::new(id, update).receipt()))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "make-thumbnails answers at once".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use tilo_actions::remote::PendingPoll;

    use super::*;

    /// The workspace's content, in memory.
    #[derive(Default)]
    struct Held(Mutex<BTreeMap<[u8; 32], Vec<u8>>>);

    impl Content for Held {
        fn read(&self, _workspace: &str, _app: &str, digest: [u8; 32]) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(&digest).cloned()
        }

        fn keep(&self, _workspace: &str, _app: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
            let digest = *blake3::hash(bytes).as_bytes();
            self.0.lock().unwrap().insert(digest, bytes.to_vec());
            Ok(digest)
        }
    }

    fn jpeg(seed: u8) -> Vec<u8> {
        let image = image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap_or(0) ^ seed,
                u8::try_from(y % 256).unwrap_or(0),
                seed,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        bytes
    }

    /// The engine answers one thumbnail per distinct picture, keeps it where the
    /// workspace's content is, and names the pictures it could not open without
    /// sinking the rest.
    #[test]
    fn the_action_answers_one_thumbnail_per_distinct_picture() {
        let content = Arc::new(Held::default());
        let engine = Thumbnails::new(Arc::clone(&content) as Arc<dyn Content>);
        let photograph = jpeg(1);
        let other = jpeg(2);
        let junk = b"not an image".to_vec();
        let digests: Vec<[u8; 32]> = [&photograph, &other, &photograph, &junk]
            .into_iter()
            .map(|bytes| {
                let digest = *blake3::hash(bytes).as_bytes();
                content.0.lock().unwrap().insert(digest, bytes.clone());
                digest
            })
            .collect();
        let named: Vec<String> = digests.iter().map(hex::encode).collect();
        let caller = Caller::local_person("ada").in_workspace("w");
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: tilo_actions::remote::application_action_target("w", APP, RESOURCE, ACTION),
            argument: InvokeArgument::Json(
                serde_json::json!({ "photos": named })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let mut answer = engine.invoke(&caller, request);
        let PendingPoll::Ready(Ok(receipt)) = answer.poll_ready() else {
            panic!("the engine answers at once");
        };
        let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the batch ran: {:?}", receipt.status);
        };
        assert_eq!(
            output.resources.len(),
            2,
            "one thumbnail per distinct picture"
        );
        for address in &output.resources {
            let hash = tilo_actions::content_hash(address).unwrap();
            let mut digest = [0; 32];
            hex::decode_to_slice(hash, &mut digest).unwrap();
            assert!(
                content.0.lock().unwrap().contains_key(&digest),
                "the thumbnail is kept where the workspace reads it"
            );
        }
        assert_eq!(
            output.value,
            Some(serde_json::json!({
                "failed": [{
                    "photo": hex::encode(blake3::hash(&junk).as_bytes()),
                    "reason": "is not an image",
                }],
            })),
            "the picture that opened none is named, and the rest answered"
        );
    }
}

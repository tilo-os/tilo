//! The exported **actions** and the **resource tree** the app publishes.
//!
//! ## What is an action here, and what is not
//!
//! An [`Action`] is a value that deterministically mutates the shared CRDT.
//! Sorting this app's interactions by that rule leaves a short, honest list:
//!
//! - **Actions.** Rotating, cropping, and resetting. Each is a user-authored
//!   decision about the photograph that survives a restart, replicates to a
//!   collaborator, and belongs on the undo stack. Each carries the **absolute**
//!   resulting [`Edits`] value rather than a delta, so [`Action::apply`] is a
//!   pure function of `(payload, document)` — no wall-clock, no RNG, no read of
//!   local view state — and two replicas replaying it land on the identical
//!   document. A rotate authored as "turn by one" would instead depend on how
//!   many turns the *receiving* replica had already seen, which is exactly the
//!   divergence the model exists to prevent.
//!
//! - **Not actions.** Zoom, pan, and which region the viewport happens to be
//!   showing. They are local presentation and never leave the machine.
//!   Copying to the clipboard is not one either, and that is a deliberate
//!   call rather than an omission: an action replays on *every* peer, so a
//!   clipboard write modelled as one would reach into the selection of every
//!   machine in the room the moment one person pressed Copy. It stays an
//!   affordance of the app instance the user is touching.
//!
//! ## Addressing
//!
//! Everything is named by id. The open photograph is a resource whose id is
//! derived from **its content**, and each edit is an [`ActionSpec`] with a
//! stable id (`rotate-right`, `crop`, …). No screen position appears anywhere
//! in this surface: a caller with no display enumerates by id and title, and
//! invokes by id, which is what lets the palette, a remote peer, and a
//! headless agent all drive the same operations.
//!
//! The content rather than the path, because an address is only worth having
//! if it means the same thing to everybody holding it — see [`resource_id`].

use automerge::AutoCommit;

use qos_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResourceProvider,
    ResultSpec, RetryPolicy,
};

use crate::doc::PhotosDoc;
use crate::model::{Edits, OpenImage, Rect01};

/// Registry `type_id` for every action this app exports.
pub const PHOTOS_ACTION_TYPE: &str = "photos.action";

/// The resource `kind` an open photograph publishes under.
pub const IMAGE_KIND: &str = "photos.image";
pub const PHOTOS_OPERATION_TYPE: &str = "photos.operation";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotosOperation {
    /// Bring the addressed photograph forward without changing its pixels.
    ///
    /// This is the safe canonical activation used by global search. The host
    /// has already routed the request to the Photos instance that publishes
    /// the resource, so accepting it is enough to reveal the current view.
    View,
    Open {
        path: Option<String>,
    },
    Copy,
    Save {
        path: Option<String>,
    },
}

/// Decode an operation payload owned by the Photos app.
///
/// # Errors
///
/// Returns an error when a Photos operation payload is not valid JSON for
/// [`PhotosOperation`]. Unknown operation types return `Ok(None)`.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<PhotosOperation>, String> {
    if type_id != PHOTOS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn operation(id: &str, label: &str, op: &PhotosOperation) -> Option<ActionSpec> {
    let spec =
        ActionSpec::operation(id, label, OperationKind::Effect, PHOTOS_OPERATION_TYPE, op).ok()?;
    Some(match op {
        PhotosOperation::View | PhotosOperation::Open { .. } | PhotosOperation::Copy => {
            spec.retry_policy(RetryPolicy::Idempotent)
        }
        PhotosOperation::Save { .. } => spec.retry_policy(RetryPolicy::AtMostOnce),
    })
}

/// Every collaborative operation on the open photograph.
///
/// One variant, carrying the absolute post-edit state. See the module docs for
/// why it is absolute rather than incremental.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotosAction {
    /// Replace the edits in force with exactly this value.
    SetEdits {
        /// Region of the original kept, normalized.
        crop: Rect01,
        /// Clockwise quarter turns, `0..=3`.
        rotate_quarters: u8,
    },
}

impl PhotosAction {
    /// Build the action that lands `edits`.
    pub fn set(edits: &Edits) -> Self {
        Self::SetEdits {
            crop: edits.crop,
            rotate_quarters: edits.rotate_quarters,
        }
    }

    /// The edit state this action produces, after rejecting malformed durable
    /// input rather than coercing it into a different edit.
    fn edits(&self) -> Result<Edits, ActionError> {
        match self {
            Self::SetEdits {
                crop,
                rotate_quarters,
            } if crop.is_valid_selection() && *rotate_quarters < 4 => Ok(Edits {
                crop: *crop,
                rotate_quarters: *rotate_quarters,
            }),
            Self::SetEdits { .. } => Err(ActionError::Refused(
                "photo edits must name a non-empty crop inside the image and a quarter turn from 0 to 3".into(),
            )),
        }
    }
}

impl Action for PhotosAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        PhotosDoc::set_edits(doc, &self.edits()?);
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // The edit is a small mutation of a document this client already holds;
        // there is nothing a host could do better.
        ExecutionLocus::ClientLocal
    }
}

/// Register the decoder so a replayed envelope reconstructs the concrete type.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<PhotosAction>(PHOTOS_ACTION_TYPE);
}

/// Encode into the wire form the palette and the on-screen buttons both
/// dispatch — the one place an action becomes a replayable envelope.
///
/// # Panics
///
/// Never in practice: [`PhotosAction`] holds only [`Rect01`] and `u8` fields,
/// which always serialize. `ActionSpec::prompted`'s `build` closure must
/// return an `EncodedAction` directly rather than a `Result`, so there is no
/// error path here to propagate to instead.
#[allow(
    clippy::expect_used,
    reason = "PhotosAction always serializes (Rect01/u8 fields only); the \
              prompted-builder closure this feeds must return EncodedAction \
              directly, not a Result, so there is nowhere to propagate a failure"
)]
pub fn encode(action: &PhotosAction) -> EncodedAction {
    EncodedAction::encode(PHOTOS_ACTION_TYPE, action).expect("PhotosAction is always serializable")
}

/// `ActionSpec::immediate` for a [`PhotosAction`] that lands `next` outright.
/// Every field it carries always serializes — see [`encode`].
#[allow(
    clippy::expect_used,
    reason = "PhotosAction always serializes (Rect01/u8 fields only); see encode()"
)]
fn immediate_photos_spec(id: &str, label: &str, next: Edits) -> ActionSpec {
    ActionSpec::immediate(id, label, PHOTOS_ACTION_TYPE, &PhotosAction::set(&next))
        .expect("PhotosAction is always serializable")
        .reversible()
}

/// Parse the `x,y,w,h` argument a prompted crop collects.
///
/// Accepts four comma-separated numbers in `0..1`. Returns `None` for anything
/// else, which the caller turns into "leave the photograph alone" rather than a
/// panic — this string comes from a human or a remote caller.
pub fn parse_crop_arg(arg: &str) -> Option<Rect01> {
    let parts: Vec<f64> = arg
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let [x, y, w, h] = parts.as_slice() else {
        return None;
    };
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return None;
    }
    // Refuse rather than clamp an out-of-range rectangle. Clamping `1,1,1,1`
    // used to produce a zero-area crop which later became an arbitrary
    // single pixel; a caller gets either the rectangle it requested or no
    // action at all.
    if *x < 0.0
        || *y < 0.0
        || *w <= 0.0
        || *h <= 0.0
        || *x > 1.0
        || *y > 1.0
        || *x + *w > 1.0
        || *y + *h > 1.0
    {
        return None;
    }
    Some(Rect01 {
        x: *x,
        y: *y,
        w: *w,
        h: *h,
    })
}

/// Publishes the open photograph and the edits it supports.
///
/// Enumeration is a **read**: [`resources`](ResourceProvider::resources) takes
/// the document by shared reference, builds specs from values already in it,
/// and performs no I/O — listing what is available never rotates anything, and
/// never touches the file on disk.
pub struct PhotosResources;

impl ResourceProvider for PhotosResources {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Nothing open: there is no addressed photograph yet. The shell's app
        // launcher remains the entry point for opening the first one.
        let Some(image) = PhotosDoc::image(doc) else {
            return vec![
                Resource::new("photos.library", "photos.library", "Photos")
                    .with_action(file_operation("open", "Open photograph", true)),
            ];
        };
        let edits = PhotosDoc::edits(doc);
        let (w, h) = image.edited_size(&edits);
        // Minted once and used for the resource and every child, so the parent
        // and its facts cannot drift apart.
        let id = resource_id(&image);

        let mut resource = Resource::new(id.clone(), IMAGE_KIND, &image.name);
        if let Some(spec) = operation("view", "View photograph", &PhotosOperation::View) {
            resource =
                resource.with_action(spec.with_description("Bring this photograph into view"));
        }
        resource = resource
            .with_action(
                immediate_photos_spec("rotate-right", "Rotate right", edits.rotated_right())
                    .with_description("Turn the photograph a quarter turn clockwise"),
            )
            .with_action(
                immediate_photos_spec("rotate-left", "Rotate left", edits.rotated_left())
                    .with_description("Turn the photograph a quarter turn anticlockwise"),
            )
            .with_action(
                ActionSpec::typed(
                    "crop",
                    "Crop to a region",
                    "0.25,0.25,0.5,0.5",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "region",
                        ParamType::Text,
                        "normalized x,y,width,height values between 0 and 1",
                    )
                    .with_schema(serde_json::json!({
                        "type": "string",
                        "pattern": "^[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*$"
                    }))],
                    move |args: &ActionArgs| {
                        // An unparseable argument leaves the photograph exactly
                        // as it is; it never crops to nothing.
                        let next = match args.text("region").and_then(parse_crop_arg) {
                            Some(rect) => edits.cropped_from_view(rect),
                            None => edits,
                        };
                        encode(&PhotosAction::set(&next))
                    },
                )
                .reversible()
                .with_description(
                    "Keep a region of what is currently shown, in normalized coordinates",
                ),
            );
        resource = resource.with_action(file_operation("open", "Open another photograph", true));
        for (id, label, op) in [("copy", "Copy photograph", PhotosOperation::Copy)] {
            if let Some(spec) = operation(id, label, &op) {
                resource = resource.with_action(spec);
            }
        }
        resource = resource.with_action(file_operation("save", "Save edited photograph", false));

        // Reset only appears when there is something to undo, so the surface
        // never offers an operation that would do nothing.
        if !edits.is_identity() {
            resource = resource.with_action(
                immediate_photos_spec("reset", "Reset edits", Edits::default())
                    .with_description("Discard the crop and rotation, returning to the original"),
            );
        }

        // The dimensions ride along in the label of a child resource rather
        // than in a side channel, so a caller that can only read ids and
        // titles still learns the size it is working with.
        resource = resource.with_child(Resource::new(
            format!("{id}#size"),
            "photos.size",
            format!("{w} x {h} pixels"),
        ));

        vec![resource]
    }
}

fn file_operation(id: &str, label: &str, open: bool) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "path (omit to use the system file picker or suggested save path)",
        OperationKind::Effect,
        PHOTOS_OPERATION_TYPE,
        vec![ActionParam::optional_owned(
            "path",
            ParamType::Text,
            "Direct filesystem path",
        )],
        move |args: &ActionArgs| {
            let path = args
                .text("path")
                .filter(|p| !p.trim().is_empty())
                .map(str::to_string);
            EncodedAction::encode_operation(
                PHOTOS_OPERATION_TYPE,
                &if open {
                    PhotosOperation::Open { path }
                } else {
                    PhotosOperation::Save { path }
                },
            )
            .unwrap_or_else(|error| EncodedAction {
                type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("photos file operation could not be serialized: {error}")
                    .into_bytes(),
            })
        },
    )
    .execution(ExecutionMode::Background { cancellable: false })
    .returns(ResultSpec::Job)
}

/// The address of a photograph: what it is, not where a machine put it.
///
/// # Why the path was the wrong name
///
/// This used to be `format!("{IMAGE_KIND}:{path}")`, and the path is true on
/// exactly one machine. Content handed to a room is staged by each session
/// under its own runtime directory, so one photograph had one address per
/// participant — `…/qlva…/qos-opened/<hash>.png` here, `…/qlvx…/` there,
/// `…/qlvq…/` in the third. An address is resolved by string equality
/// ([`qos_actions::door::resolve`]), so an operation invoked on one of them
/// reached exactly one session: the one whose path had won. An agent cropped
/// the editor in `qlva…` while the person watching the editor in `qlvx…` saw
/// their picture sit there unchanged. The run before, the same invocation
/// worked — because that time the id happened to name the watcher's copy.
///
/// So the name is the content's, through the hash the document already
/// carries beside the path (`OpenImage::content`). Content is the same word in
/// every session, which is the property an address has to have before anyone
/// can act through one: everybody says the same thing about the picture, and
/// the operation lands on it wherever it is open.
///
/// # Why the file's name is not in it
///
/// Because a name is a property of whoever wrote it down, exactly as the path
/// was. A participant who opened `harbour.png` off their own disk and one who
/// was handed the same bytes staged as `<hash>.png` would be back to two
/// addresses for one picture. What a person reads is the resource's **label**,
/// which is the file's own name and travels beside the id everywhere one is
/// rendered — the palette row, `qos_actions::FlatAction::resource_label`, the
/// agent's survey line, the chip on a remark.
///
/// # A photograph with no content hash
///
/// Named by its path, as it always was. A document written before the hash
/// existed parses with `content: None`, and the path is the only thing such a
/// picture has: as good an address as it ever was in the one session the path
/// is true of, and better than publishing nothing. Nothing is migrated,
/// because there is nothing stored to migrate — this tree is a pure function
/// of the document, rebuilt every frame, so a picture gains its content
/// address the moment its bytes reach the store.
pub fn resource_id(image: &OpenImage) -> String {
    match &image.content {
        Some(hash) => content_address(hash),
        None => format!("{IMAGE_KIND}:{}", image.path),
    }
}

/// The address the content named by `hash` is published under.
///
/// Split out so a caller holding the bytes but no document — the offscreen
/// renderer posing a scenario, a host staging content for an app — spells the
/// address the one way, rather than assembling a second spelling of it that
/// can drift from the one the app answers to.
pub fn content_address(hash: &str) -> String {
    format!("{IMAGE_KIND}:{hash}")
}

/// Length of a blake3 hash written as hex, which is what a content-named id
/// carries after its kind.
const CONTENT_HEX_LEN: usize = 64;

/// The content a resource id names, if it names content.
///
/// The inverse of [`resource_id`] for the ids minted from a hash, and `None`
/// for the path-shaped id a document written before content hashes still
/// produces. This is what keeps a portable address useful rather than merely
/// identical everywhere: a session that was handed the picture and never had a
/// file for it goes from the address to the bytes through
/// `qdoc::blobstore::get`, the same route [`crate::doc::bytes_of`] takes for
/// the photograph it is drawing.
pub fn content_of(id: &str) -> Option<&str> {
    let tail = id.strip_prefix(IMAGE_KIND)?.strip_prefix(':')?;
    let is_content = tail.len() == CONTENT_HEX_LEN
        && tail
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_content.then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OpenImage;
    use qos_actions::flatten;

    fn doc_with_image() -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&img));
        doc
    }

    /// The address of a photograph that names its content is that content.
    ///
    /// Asserted on the published tree rather than on `resource_id` alone, so
    /// it is the id a caller actually resolves against.
    #[test]
    fn a_photograph_that_names_its_content_is_addressed_by_it() {
        const HASH: &str = "623fdbf0c19749ddd1116f8c68b561ad648e3023ca4f304a732bf7b06d1ffb82";
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/run/user/1000/qlva3099960/qos-opened/x.png", 800, 400)
            .with_content(HASH);
        PhotosDoc::set_image(&mut doc, Some(&img));

        let res = PhotosResources.resources(&doc);
        assert_eq!(res[0].id.0, format!("photos.image:{HASH}"));
        assert_eq!(
            content_of(&res[0].id.0),
            Some(HASH),
            "the address does not lead back to the content it names"
        );
        // And the file's name is still what a person reads.
        assert_eq!(res[0].label, "x.png");
    }

    /// The path-shaped id an older document produces is not mistaken for a
    /// content address. A caller that got one has to fall back to the path,
    /// and would otherwise ask the content store for a directory name.
    #[test]
    fn a_path_shaped_address_names_no_content() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        assert_eq!(content_of(&res[0].id.0), None);
        assert_eq!(content_of("photos.image:"), None);
        assert_eq!(content_of("files.entry:/x"), None);
        // Right length, wrong alphabet: hex is what a hash is written in.
        assert_eq!(
            content_of(&format!("photos.image:{}", "z".repeat(64))),
            None
        );
    }

    #[test]
    fn nothing_open_publishes_an_openable_library_resource() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let resources = PhotosResources.resources(&doc);
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].kind, "photos.library");
        assert_eq!(resources[0].actions[0].id, "open");
    }

    #[test]
    fn the_open_photograph_is_published_with_its_name_and_size() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].kind, IMAGE_KIND);
        assert_eq!(res[0].label, "harbour.png");
        assert_eq!(
            res[0].id.0,
            "photos.image:/var/tmp/qos/home/quinten/harbour.png"
        );
        // The size is discoverable from titles alone.
        assert_eq!(res[0].children.len(), 1);
        assert_eq!(res[0].children[0].label, "800 x 400 pixels");
    }

    #[test]
    fn every_action_is_addressable_by_a_stable_id() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let ids: Vec<&str> = res[0].actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "view",
                "rotate-right",
                "rotate-left",
                "crop",
                "open",
                "copy",
                "save",
            ]
        );
    }

    #[test]
    fn reset_appears_only_once_there_is_an_edit_to_discard() {
        let mut doc = doc_with_image();
        let before = PhotosResources.resources(&doc);
        assert!(!before[0].actions.iter().any(|a| a.id == "reset"));

        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let after = PhotosResources.resources(&doc);
        assert!(after[0].actions.iter().any(|a| a.id == "reset"));
    }

    /// Enumerating must be a pure read. If listing what is on offer could
    /// mutate the document, a palette opening would silently edit the user's
    /// photograph.
    #[test]
    fn enumeration_fires_no_side_effects() {
        let mut doc = doc_with_image();
        let before_heads = doc.get_heads();
        let before_bytes = doc.save();

        let res = PhotosResources.resources(&doc);
        let flat = flatten(&res);
        // Touch every field a caller would read, including building the label
        // of each row.
        for f in &flat {
            let _ = (&f.key, &f.resource_label, &f.action.label, f.action.locus);
        }
        assert!(!flat.is_empty(), "there was nothing to enumerate");

        assert_eq!(
            doc.get_heads(),
            before_heads,
            "enumeration moved the document"
        );
        assert_eq!(doc.save(), before_bytes, "enumeration changed the document");
    }

    #[test]
    fn applying_rotate_right_advances_the_document_by_one_quarter() {
        let mut doc = doc_with_image();
        let action = PhotosAction::set(&PhotosDoc::edits(&doc).rotated_right());
        action.apply(&mut doc).unwrap();
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
    }

    /// The determinism contract: the same envelope applied to two replicas
    /// that started equal leaves them equal.
    #[test]
    fn the_same_action_replays_to_the_same_state() {
        let mut a = doc_with_image();
        let mut b = doc_with_image();
        let action = PhotosAction::SetEdits {
            crop: Rect01 {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.5,
            },
            rotate_quarters: 3,
        };
        action.apply(&mut a).unwrap();
        action.apply(&mut b).unwrap();
        assert_eq!(PhotosDoc::edits(&a), PhotosDoc::edits(&b));
    }

    #[test]
    fn an_action_round_trips_through_the_registry() {
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let original = PhotosAction::set(&Edits::default().rotated_left());
        let decoded = registry.decode(&encode(&original)).expect("decode");
        let mut doc = doc_with_image();
        decoded.apply(&mut doc).unwrap();
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 3);
    }

    #[test]
    fn a_malformed_crop_payload_from_a_peer_is_refused_without_mutating_the_document() {
        let mut doc = doc_with_image();
        PhotosAction::SetEdits {
            crop: Rect01 {
                x: -2.0,
                y: 0.5,
                w: 9.0,
                h: 9.0,
            },
            rotate_quarters: 250,
        }
        .apply(&mut doc)
        .expect_err("malformed durable edits must be refused");
        let e = PhotosDoc::edits(&doc);
        assert!(
            e.is_identity(),
            "a refused action changed the document: {e:?}"
        );
    }

    #[test]
    fn crop_arguments_parse_and_reject() {
        let r = parse_crop_arg("0.25, 0.25, 0.5, 0.5").expect("four numbers");
        assert!((r.w - 0.5).abs() < 1e-9);
        assert!(parse_crop_arg("").is_none());
        assert!(
            parse_crop_arg("0.1,0.2,0.3").is_none(),
            "three is not a rect"
        );
        assert!(parse_crop_arg("a,b,c,d").is_none());
        assert!(
            parse_crop_arg("0,0,0,0").is_none(),
            "an empty crop is refused"
        );
        assert!(parse_crop_arg("1,1,1,1").is_none(), "a crop must fit");
    }

    /// A prompted crop that cannot be parsed must leave the photograph alone
    /// rather than crop it to nothing.
    #[test]
    fn an_unparseable_crop_argument_is_a_no_op() {
        let mut doc = doc_with_image();
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let res = PhotosResources.resources(&doc);
        let crop = res[0]
            .actions
            .iter()
            .find(|a| a.id == "crop")
            .expect("crop is published");
        let qos_actions::ActionInput::Prompted { build, .. } = &crop.input else {
            panic!("crop collects an argument");
        };
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        assert!(registry.decode(&build("nonsense")).is_err());
        let e = PhotosDoc::edits(&doc);
        assert!(
            e.crop.is_full(),
            "a bad argument cropped the photograph: {:?}",
            e.crop
        );
        assert_eq!(
            e.rotate_quarters, 1,
            "and it must not undo the rotation either"
        );
    }

    #[test]
    fn a_prompted_crop_composes_onto_the_edits_in_force() {
        let mut doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let crop = res[0].actions.iter().find(|a| a.id == "crop").unwrap();
        let qos_actions::ActionInput::Prompted { build, .. } = &crop.input else {
            panic!("crop collects an argument");
        };
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
            .decode(&build("0,0,0.5,0.5"))
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let e = PhotosDoc::edits(&doc);
        assert!((e.crop.w - 0.5).abs() < 1e-9, "{:?}", e.crop);
        assert!((e.crop.h - 0.5).abs() < 1e-9, "{:?}", e.crop);
    }

    #[test]
    fn exported_photo_operations_declare_arguments_and_execution_contracts() {
        let res = PhotosResources.resources(&doc_with_image());
        let crop = res[0].actions.iter().find(|a| a.id == "crop").unwrap();
        assert_eq!(crop.params()[0].name.as_ref(), "region");
        assert!(crop.params()[0].schema.is_some());
        assert!(crop.contract.reversible);

        let open = res[0].actions.iter().find(|a| a.id == "open").unwrap();
        assert_eq!(
            open.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(open.contract.result, qos_actions::ResultSpec::Job);
        assert_eq!(open.params()[0].name.as_ref(), "path");
        let copy = res[0].actions.iter().find(|a| a.id == "copy").unwrap();
        assert_eq!(copy.contract.retry, RetryPolicy::Idempotent);
        let view = res[0].actions.iter().find(|a| a.id == "view").unwrap();
        assert_eq!(view.contract.retry, RetryPolicy::Idempotent);
    }

    #[test]
    fn every_published_action_has_a_valid_machine_readable_contract() {
        fn check(resource: &Resource) {
            for action in &resource.actions {
                assert!(
                    action.contract.validate_declaration().is_empty(),
                    "{} on {} has an invalid contract: {:?}",
                    action.id,
                    resource.id.0,
                    action.contract.validate_declaration()
                );
            }
            for child in &resource.children {
                check(child);
            }
        }

        let empty = AutoCommit::new();
        for resource in PhotosResources.resources(&empty) {
            check(&resource);
        }
        for resource in PhotosResources.resources(&doc_with_image()) {
            check(&resource);
        }
    }
}

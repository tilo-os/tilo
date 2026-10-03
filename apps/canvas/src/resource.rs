//! The artwork as a framework resource: its state, its vocabulary published
//! by name, and the views a caller with no screen uses to see it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ui::tilo_actions::{
    ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, PermissionRequirement,
    ResultSpec,
};
use ui::tilo_resource::{
    self, ActionDecl, ActionRef, Doc, JsonSchema, ModelError, Resource, SearchEntry,
};

use crate::actions::{
    ACTION_TYPE, AddLayer, Align, Arrange, CanvasAction, Draw, EditLayer, EditMarks, Paint,
    RemoveLayer, RemoveMarks, RemoveMedium, SaveMedium, SetArtwork, SetReference,
};
use crate::model::Artwork;

/// The operation type the canvas surface answers views and effects under.
pub const VIEW_TYPE: &str = "canvas.view";

/// The artwork resource.
pub struct ArtworkResource;

impl ArtworkResource {
    /// `paint`: a layer (or the top one), then the strokes.
    pub const PAINT: ActionRef<Self, (Option<String>, Vec<crate::actions::NewStroke>)> =
        ActionRef::new("paint", &["layer", "strokes"]);
    /// `draw`: a layer (or the top one), then the shapes.
    pub const DRAW: ActionRef<Self, (Option<String>, Vec<crate::actions::NewMark>)> =
        ActionRef::new("draw", &["layer", "marks"]);
    /// `remove-marks` by id.
    pub const REMOVE_MARKS: ActionRef<Self, Vec<String>> = ActionRef::new("remove-marks", &["ids"]);
    /// `add-layer`: its name and medium (empty for plain).
    pub const ADD_LAYER: ActionRef<Self, (String, String)> =
        ActionRef::new("add-layer", &["name", "medium"]);
    /// `edit-layer` showing or hiding a layer.
    pub const SET_LAYER_VISIBLE: ActionRef<Self, (String, bool)> =
        ActionRef::new("edit-layer", &["id", "visible"]);
    /// `edit-layer` moving a layer `front`, `back`, `forward` or `backward`.
    pub const MOVE_LAYER: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "to"]);
    /// `remove-layer` by id.
    pub const REMOVE_LAYER: ActionRef<Self, String> = ActionRef::new("remove-layer", &["id"]);
    /// `arrange`: marks, then `front`, `back`, `forward` or `backward`.
    pub const ARRANGE: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("arrange", &["ids", "to"]);
    /// `add-layer` with a medium, above another layer.
    pub const ADD_LAYER_ABOVE: ActionRef<Self, (String, String, String)> =
        ActionRef::new("add-layer", &["name", "medium", "above"]);
    /// `edit-layer` renaming a layer.
    pub const RENAME_LAYER: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "name"]);
    /// `edit-layer` locking or unlocking a layer.
    pub const SET_LAYER_LOCKED: ActionRef<Self, (String, bool)> =
        ActionRef::new("edit-layer", &["id", "locked"]);
    /// `edit-layer` setting a layer's opacity.
    pub const SET_LAYER_OPACITY: ActionRef<Self, (String, f64)> =
        ActionRef::new("edit-layer", &["id", "opacity"]);
    /// `edit-layer` changing the medium that paints a layer.
    pub const SET_LAYER_MEDIUM: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "medium"]);
    /// `edit-marks` setting the fill.
    pub const SET_FILL: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "fill"]);
    /// `edit-marks` setting the outline colour.
    pub const SET_STROKE: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "stroke"]);
    /// `edit-marks` setting the outline width.
    pub const SET_STROKE_WIDTH: ActionRef<Self, (Vec<String>, f64)> =
        ActionRef::new("edit-marks", &["ids", "stroke_width"]);
    /// `edit-marks` setting the opacity.
    pub const SET_OPACITY: ActionRef<Self, (Vec<String>, f64)> =
        ActionRef::new("edit-marks", &["ids", "opacity"]);
    /// `edit-marks` setting the blend mode.
    pub const SET_BLEND: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "blend"]);
    /// `edit-marks` setting the outline dashes.
    pub const SET_DASH: ActionRef<Self, (Vec<String>, Vec<f64>)> =
        ActionRef::new("edit-marks", &["ids", "dash"]);
    /// `edit-marks` setting the outline ends.
    pub const SET_CAP: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "cap"]);
    /// `edit-marks` setting the outline corners.
    pub const SET_JOIN: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "join"]);
    /// `edit-marks` setting the shadow.
    pub const SET_SHADOW: ActionRef<Self, (Vec<String>, crate::model::Shadow)> =
        ActionRef::new("edit-marks", &["ids", "shadow"]);
    /// `edit-marks` replacing a mark's geometry (a text's style).
    pub const SET_SHAPE: ActionRef<Self, (Vec<String>, crate::model::Shape)> =
        ActionRef::new("edit-marks", &["ids", "shape"]);
    /// `set-artwork` with a title.
    pub const SET_TITLE: ActionRef<Self, String> = ActionRef::new("set-artwork", &["title"]);
    /// `set-artwork` with a width.
    pub const SET_WIDTH: ActionRef<Self, f64> = ActionRef::new("set-artwork", &["width"]);
    /// `set-artwork` with a height.
    pub const SET_HEIGHT: ActionRef<Self, f64> = ActionRef::new("set-artwork", &["height"]);
    /// `set-artwork` with a ground texture.
    pub const SET_TEXTURE: ActionRef<Self, String> = ActionRef::new("set-artwork", &["texture"]);
    /// `set-artwork` with a ground colour.
    pub const SET_GROUND: ActionRef<Self, String> =
        ActionRef::new("set-artwork", &["ground_colour"]);
    /// `undo`: the caller's last step.
    pub const UNDO: ActionRef<Self, ()> = ActionRef::new("undo", &[]);
    /// `redo`: what the last undo took back.
    pub const REDO: ActionRef<Self, ()> = ActionRef::new("redo", &[]);
    /// `undo` of an agent run, whole.
    pub const UNDO_RUN: ActionRef<Self, String> = ActionRef::new("undo", &["run"]);
    /// `restore` a past version as a new change.
    pub const RESTORE: ActionRef<Self, String> = ActionRef::new("restore", &["version"]);
    /// `checkpoint`: name the current version.
    pub const CHECKPOINT: ActionRef<Self, String> = ActionRef::new("checkpoint", &["name"]);
}

/// Look at the artwork, or a region of it, as a picture.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Look {
    /// `[x, y, width, height]` in artwork units; the whole artwork when
    /// absent. A small region is a close look at detail.
    pub region: Option<[f64; 4]>,
    /// The longest side of the picture in pixels, 64 to 1536 (default 768).
    pub size: Option<u32>,
    /// `jpeg` (default) or `png` for exact pixels.
    pub format: Option<String>,
    /// Draw a labelled coordinate grid over the picture, to place marks.
    pub grid: Option<bool>,
    /// Show only these layers.
    pub layers: Option<Vec<String>>,
    /// A past version: a change id from history or a checkpoint name.
    pub version: Option<String>,
    /// Show the reference picture over the artwork; as set when absent.
    pub reference: Option<bool>,
}

/// List layers and marks with their ids, bounds and colours.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inspect {
    /// Only marks touching `[x, y, width, height]`.
    pub region: Option<[f64; 4]>,
    /// Only marks on this layer.
    pub layer: Option<String>,
    /// How many of the newest matching marks to list, 1 to 400 (default 100).
    pub limit: Option<u32>,
    /// A past version: a change id from history or a checkpoint name.
    pub version: Option<String>,
    /// Also each mark's shape, style and transform, to edit exactly.
    pub full: Option<bool>,
}

/// Render the artwork beside, over or against the reference picture, with
/// how far apart they are.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compare {
    /// `[x, y, width, height]`; the reference's frame when absent.
    pub region: Option<[f64; 4]>,
    /// `side-by-side` (default; the artwork left, the reference right),
    /// `overlay` (the reference at half strength over the artwork) or
    /// `difference` (bright where they differ).
    pub mode: Option<String>,
    /// Longest side of each picture, 64 to 1024 (default 640).
    pub size: Option<u32>,
}

/// Render the artwork at full size as a PNG.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Pixels per artwork unit, 0.25 to 4 (default 1).
    pub scale: Option<f64>,
}

/// Export the artwork, or some of its marks, as SVG.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportSvg {
    /// Only these marks or groups, cropped to them and without the ground:
    /// one icon of a set.
    pub ids: Option<Vec<String>>,
}

/// A view or effect the canvas answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ViewOp {
    Look(Look),
    Inspect(Inspect),
    Media,
    Export(Export),
    Diff(Diff),
    ExportSvg(ExportSvg),
    Compare(Compare),
}

/// What changed between two versions of the artwork.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diff {
    /// The earlier version: a change id from history or a checkpoint name.
    pub from: String,
    /// The later version (default now).
    pub to: Option<String>,
    /// Also a picture of the changed region: before left, after right.
    pub look: Option<bool>,
}

/// The parameter type a property's schema describes.
fn param_type(schema: &Value) -> ParamType {
    let named = match schema.get("type") {
        Some(Value::String(name)) => Some(name.as_str()),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .find(|name| *name != "null"),
        _ => None,
    };
    match named {
        Some("string") => ParamType::Text,
        Some("integer") => ParamType::Integer,
        Some("number") => ParamType::Number,
        Some("boolean") => ParamType::Boolean,
        Some("array") => ParamType::Array,
        _ => ParamType::Object,
    }
}

/// One declared parameter per field of `T`, typed and described from its
/// schema, so the named action and the typed value accept the same JSON.
pub fn params_of<T: JsonSchema>() -> Vec<ActionParam> {
    let schema = T::json_schema();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, property)| {
                    let description = property
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(name)
                        .to_owned();
                    let ty = param_type(property);
                    // An optional field may also be sent as null, which is
                    // how many callers say "not given".
                    let param = if required.contains(&name.as_str()) {
                        ActionParam::required_owned(name.clone(), ty, description)
                    } else {
                        ActionParam::optional_owned(name.clone(), ty, description).nullable()
                    };
                    param.with_schema(property.clone())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The arguments as `T`, with an error naming what is wrong.
fn read_args<T: serde::de::DeserializeOwned>(
    args: &ui::tilo_actions::ActionArgs,
) -> Result<T, String> {
    let mut fields = args.to_json();
    fields.retain(|_, value| !value.is_null());
    serde_json::from_value(Value::Object(fields)).map_err(|error| error.to_string())
}

/// Read the picture at `path` where the action is authored and keep it in
/// the content store, so the action names it by content and every replica
/// shows the same picture.
fn store_picture(path: &str) -> Result<String, String> {
    let bytes = read_file(path, crate::render::MAX_REFERENCE_BYTES)?;
    crate::render::decode_picture(&bytes)
        .map_err(|error| format!("{path} is not a PNG or JPEG: {error}"))?;
    store(&bytes)
}

/// The bytes of the file at `path`, at most `limit` of them.
///
/// # Errors
/// Returns why the file cannot be read, or that it is too large.
pub fn read_file(path: &str, limit: usize) -> Result<Vec<u8>, String> {
    let cannot = |error: tilo_files::FsError| format!("cannot read {path}: {error}");
    let (dir, name) = tilo_files::file_at(std::path::Path::new(path)).map_err(cannot)?;
    let size = tilo_files::blocking::metadata(&dir, &name)
        .map_err(cannot)?
        .len;
    if usize::try_from(size).map_or(true, |size| size > limit) {
        return Err(format!("{path} is over {limit} bytes"));
    }
    tilo_files::blocking::read(&dir, &name).map_err(cannot)
}

/// Keep `bytes` in the app's content store; answers their address.
///
/// # Errors
/// Returns the store's failure.
pub fn store(bytes: &[u8]) -> Result<String, String> {
    let hash = tilo_content::ContentStore::for_app()
        .put(bytes)
        .map_err(|error| format!("the content could not be stored: {error}"))?;
    ui::tilo_actions::content_resource(&hash).ok_or_else(|| "the content has no address".to_owned())
}

/// A short random id, minted where an action is authored.
fn minted(prefix: &str) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", id.get(..8).unwrap_or_default())
}

fn decl<T: JsonSchema + serde::de::DeserializeOwned + 'static>(
    id: &'static str,
    label: &str,
    description: &str,
    wrap: fn(T) -> CanvasAction,
) -> ActionDecl<ArtworkResource> {
    ActionDecl::new(id, label, params_of::<T>(), move |args, _now| {
        read_args::<T>(args).map(wrap)
    })
    .describe(description)
}

impl Resource for ArtworkResource {
    const KIND: &'static str = "canvas.artwork";
    const LABEL: &'static str = "Artwork";
    const ACTION_TYPE: &'static str = ACTION_TYPE;

    type State = Artwork;
    type Action = CanvasAction;

    fn read(doc: &Doc) -> Result<Artwork, ModelError> {
        Ok(crate::doc::read(doc))
    }

    /// A new artwork is an empty sheet of paper with one plain layer, so the
    /// first stroke has somewhere to go.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        use ui::tilo_actions::Action as _;
        CanvasAction::AddLayer(AddLayer {
            id: Some("layer-1".into()),
            name: "Layer 1".into(),
            ..AddLayer::default()
        })
        .apply(doc)
        .map_err(|error| ModelError::Invalid {
            path: "layers".into(),
            message: error.to_string(),
        })?;
        // Created here, not by the first step that writes into them: a step
        // that created a shared map cannot be undone once others wrote in it.
        for key in [crate::doc::MARKS, crate::doc::MEDIA] {
            crate::doc::ensure(doc, &ui::automerge::ROOT, key).map_err(|error| {
                ModelError::Invalid {
                    path: key.into(),
                    message: error.to_string(),
                }
            })?;
        }
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            decl::<Paint>(
                "paint",
                "Paint strokes",
                "Paint brush strokes on a layer in one step. On an oil or watercolour layer the medium simulates real paint; on a plain layer strokes are ink. Each stroke: points [[x,y] or [x,y,pressure]], size, colour (#rrggbb or pigment:<name>).",
                CanvasAction::Paint,
            ),
            decl::<Draw>(
                "draw",
                "Draw shapes",
                "Draw shapes on a layer in one step: rect, ellipse, polygon, line, path (SVG d), text or stroke, each with fill, gradient, stroke and opacity. Give ids to address marks later. On a painted layer filled shapes are laid in with the brush.",
                CanvasAction::Draw,
            ),
            decl::<EditMarks>(
                "edit-marks",
                "Edit marks",
                "Change marks by id: colours, opacity, name, move_by [dx,dy], scale and rotate about their centre, or move them to another layer.",
                CanvasAction::edit,
            ),
            decl::<RemoveMarks>(
                "remove-marks",
                "Remove marks",
                "Remove marks by id, or every mark of a layer.",
                CanvasAction::RemoveMarks,
            ),
            decl::<Arrange>(
                "arrange",
                "Arrange marks",
                "Bring marks to the front or back of their layer, one step forward or backward, or directly above or below another mark or group.",
                CanvasAction::Arrange,
            ),
            ActionDecl::new("add-layer", "Add layer", params_of::<AddLayer>(), |args, _now| {
                let mut add: AddLayer = read_args(args)?;
                if add.id.is_none() {
                    add.id = Some(minted("layer"));
                }
                Ok(CanvasAction::AddLayer(add))
            })
            .describe("Add a layer on top (or above another). Give it a medium (see media) to paint in oil or watercolour; without one it holds crisp vector art."),
            decl::<EditLayer>(
                "edit-layer",
                "Edit layer",
                "Rename a layer, change its medium, opacity, visibility or lock, or move it to the front, back, forward or backward.",
                CanvasAction::EditLayer,
            ),
            decl::<RemoveLayer>(
                "remove-layer",
                "Remove layer",
                "Remove a layer and every mark on it.",
                CanvasAction::RemoveLayer,
            ),
            decl::<SetArtwork>(
                "set-artwork",
                "Set artwork",
                "Set the title, size (units) or ground: texture (smooth, paper, cold-press, rough, linen, panel) and colour.",
                CanvasAction::SetArtwork,
            ),
            decl::<SaveMedium>(
                "save-medium",
                "Save medium",
                "Save a medium preset in the artwork: an engine (oil or watercolour) with parameters, a ground and a pigment palette. Layers can then use its id.",
                CanvasAction::SaveMedium,
            ),
            decl::<RemoveMedium>(
                "remove-medium",
                "Remove medium",
                "Remove a medium saved in this artwork.",
                CanvasAction::RemoveMedium,
            ),
            decl::<Align>(
                "align",
                "Align marks",
                "Align marks or groups to an edge or centre line, and/or distribute them with equal gaps, within their own extent, the artwork or another mark.",
                CanvasAction::Align,
            ),
            ActionDecl::new("set-reference", "Set reference", params_of::<SetReference>(), |args, _now| {
                let mut set: SetReference = read_args(args)?;
                if let Some(image) = &set.image
                    && image.starts_with('/')
                {
                    set.image = Some(store_picture(image)?);
                }
                Ok(CanvasAction::SetReference(set))
            })
            .describe("Set the reference picture to recreate or trace: shown over the artwork at its opacity, seen by look (reference: false hides it) and compare; never exported."),
            ActionDecl::new("import-svg", "Import SVG", params_of::<crate::svg::ImportSvg>(), |args, _now| {
                let request: crate::svg::ImportSvg = read_args(args)?;
                crate::svg::import(&request).map(CanvasAction::Draw)
            })
            .describe("Import an SVG (markup, file path or content address) as one draw of path and text marks in a group: fills, gradients, outlines, opacity, blends and clip paths."),
        ]
    }

    fn action_schema() -> Option<Value> {
        Some(CanvasAction::json_schema())
    }

    fn local_operations() -> Vec<ActionSpec> {
        let view = |id: &str, label: &str, params: Vec<ActionParam>, wrap: Wrap| {
            operation(id, label, OperationKind::View, params, wrap)
        };
        let mut look = view("look", "Look at the artwork", params_of::<Look>(), |args| {
            ViewOp::Look(read_args(args).unwrap_or_default())
        })
        .with_description(
            "See the artwork as a picture: the whole of it, or a region up close. Answers with the image and the revision it shows. grid: true overlays labelled coordinates for placing marks.",
        );
        look.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut inspect = view("inspect", "Inspect marks", params_of::<Inspect>(), |args| {
            ViewOp::Inspect(read_args(args).unwrap_or_default())
        })
        .with_description(
            "List the layers and the newest marks (id, kind, name, layer, bounds [x,y,w,h], colours), optionally only in a region or on one layer.",
        );
        inspect.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut media = view("media", "List media", Vec::new(), |_| ViewOp::Media).with_description(
            "List the media layers can use (oil, watercolour presets and those saved here) with their engines' parameters and pigment palettes.",
        );
        media.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut export = ActionSpec::typed_operation(
            "export",
            "Export PNG",
            "Export PNG",
            OperationKind::Effect,
            VIEW_TYPE,
            params_of::<Export>(),
            |args| {
                EncodedAction::encode_operation(
                    VIEW_TYPE,
                    &ViewOp::Export(read_args(args).unwrap_or_default()),
                )
                .unwrap_or_else(|_| EncodedAction {
                    type_id: VIEW_TYPE.into(),
                    payload: Vec::new(),
                })
            },
        )
        .requiring(PermissionRequirement::Read)
        .with_description(
            "Render the artwork at full size as a PNG and answer with its content address.",
        );
        export.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut diff = view("diff", "Compare versions", params_of::<Diff>(), |args| {
            ViewOp::Diff(read_args(args).unwrap_or_default())
        })
        .with_description(
            "Compare two versions: marks and layers added, removed or changed by id, the changed region, and with look a before|after picture of it.",
        );
        diff.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let [compare, svg] = vector_operations();
        vec![look, inspect, media, export, diff, compare, svg]
    }

    fn search(art: &Artwork) -> Vec<SearchEntry> {
        let mut entries = vec![SearchEntry {
            anchor: None,
            title: if art.title.is_empty() {
                "Artwork".into()
            } else {
                art.title.clone()
            },
            text: art
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }];
        entries.extend(
            art.marks
                .iter()
                .filter(|mark| !mark.name.is_empty())
                .map(|mark| SearchEntry {
                    anchor: Some(mark.id.clone()),
                    title: mark.name.clone(),
                    text: mark.shape.kind().into(),
                }),
        );
        entries
    }
}

/// How a view's arguments become the operation it asks for.
type Wrap = fn(&ui::tilo_actions::ActionArgs) -> ViewOp;

/// A read-only operation answered by the surface.
fn operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    params: Vec<ActionParam>,
    wrap: Wrap,
) -> ActionSpec {
    ActionSpec::typed_operation(id, label, label, kind, VIEW_TYPE, params, move |args| {
        EncodedAction::encode_operation(VIEW_TYPE, &wrap(args)).unwrap_or_else(|_| EncodedAction {
            type_id: VIEW_TYPE.into(),
            payload: Vec::new(),
        })
    })
    .requiring(PermissionRequirement::Read)
}

/// The views that compare against the reference and export SVG.
fn vector_operations() -> [ActionSpec; 2] {
    let mut compare = operation("compare", "Compare with reference", OperationKind::View, params_of::<Compare>(), |args| {
            ViewOp::Compare(read_args(args).unwrap_or_default())
        })
        .with_description(
            "See the artwork beside (or over, or minus) the reference picture, with a difference score (mean colour distance 0..1 in the reference's frame; lower is closer) and the regions that differ most, to iterate toward it.",
        );
    compare.contract.result = ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    let mut svg = operation(
        "export-svg",
        "Export SVG",
        OperationKind::Effect,
        params_of::<ExportSvg>(),
        |args| ViewOp::ExportSvg(read_args(args).unwrap_or_default()),
    )
        .with_description(
            "Export the artwork as SVG: plain layers as exact vector art, painted layers as embedded pictures. Answers with its content address and, when small, the markup.",
        );
    svg.contract.result = ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    [compare, svg]
}

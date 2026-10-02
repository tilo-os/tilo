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
    ACTION_TYPE, AddLayer, Arrange, CanvasAction, Draw, EditLayer, EditMarks, Paint, RemoveLayer,
    RemoveMarks, RemoveMedium, SaveMedium, SetArtwork,
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
    /// `arrange`: marks, then `front`, `back`, `forward` or `backward`.
    pub const ARRANGE: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("arrange", &["ids", "to"]);
}

/// Look at the artwork, or a region of it, as a picture.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Look {
    /// `[x, y, width, height]` in artwork units; the whole artwork when
    /// absent. A small region is a close look at detail.
    pub region: Option<[f64; 4]>,
    /// The longest side of the picture in pixels, 64 to 1536 (default 1024).
    pub size: Option<u32>,
    /// Draw a labelled coordinate grid over the picture, to place marks.
    pub grid: Option<bool>,
    /// Show only these layers.
    pub layers: Option<Vec<String>>,
}

/// List layers and marks with their ids, bounds and colours.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inspect {
    /// Only marks touching `[x, y, width, height]`.
    pub region: Option<[f64; 4]>,
    /// Only marks on this layer.
    pub layer: Option<String>,
}

/// Render the artwork at full size as a PNG.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Pixels per artwork unit, 0.25 to 4 (default 1).
    pub scale: Option<f64>,
}

/// A view or effect the canvas answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ViewOp {
    Look(Look),
    Inspect(Inspect),
    Media,
    Export(Export),
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
        })
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
                CanvasAction::EditMarks,
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
                "Bring marks to the front or back of their layer, or one step forward or backward.",
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
                "Rename a layer, change its medium, opacity or visibility, or move it to the front, back, forward or backward.",
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
                "Set the title, size (units) or ground: texture (smooth, paper, cold-press, rough, linen) and colour.",
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
        ]
    }

    fn action_schema() -> Option<Value> {
        Some(CanvasAction::json_schema())
    }

    fn local_operations() -> Vec<ActionSpec> {
        let view = |id: &str,
                    label: &str,
                    params: Vec<ActionParam>,
                    wrap: fn(&ui::tilo_actions::ActionArgs) -> ViewOp| {
            ActionSpec::typed_operation(
                id,
                label,
                label,
                OperationKind::View,
                VIEW_TYPE,
                params,
                move |args| {
                    EncodedAction::encode_operation(VIEW_TYPE, &wrap(args)).unwrap_or_else(|_| {
                        EncodedAction {
                            type_id: VIEW_TYPE.into(),
                            payload: Vec::new(),
                        }
                    })
                },
            )
            .requiring(PermissionRequirement::Read)
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
            "List the layers and the marks (id, kind, name, layer, bounds [x,y,w,h], colours), optionally only in a region or on one layer.",
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
        vec![look, inspect, media, export]
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

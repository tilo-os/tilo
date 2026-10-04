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
    /// `add-layer` with its id, name and medium, above another layer or on
    /// top.
    pub const ADD_LAYER_NAMED: ActionRef<Self, (String, String, String, Option<String>)> =
        ActionRef::new("add-layer", &["id", "name", "medium", "above"]);
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
    /// `edit-marks` setting a gradient fill.
    pub const SET_GRADIENT: ActionRef<Self, (Vec<String>, crate::model::Gradient)> =
        ActionRef::new("edit-marks", &["ids", "gradient"]);
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
    /// `edit-marks` setting strokes' brush preset.
    pub const SET_BRUSH: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "brush"]);
    /// `edit-marks` setting strokes' width.
    pub const SET_SIZE: ActionRef<Self, (Vec<String>, f64)> =
        ActionRef::new("edit-marks", &["ids", "size"]);
    /// `edit-marks` setting strokes' brush dynamics or a medium's parameters.
    pub const SET_PARAMS: ActionRef<Self, (Vec<String>, std::collections::BTreeMap<String, f64>)> =
        ActionRef::new("edit-marks", &["ids", "params"]);
    /// `edit-marks` smoothing strokes' paths.
    pub const SMOOTH: ActionRef<Self, (Vec<String>, f64)> =
        ActionRef::new("edit-marks", &["ids", "smooth"]);
    /// `edit-marks` simplifying strokes' paths.
    pub const SIMPLIFY: ActionRef<Self, (Vec<String>, f64)> =
        ActionRef::new("edit-marks", &["ids", "simplify"]);
    /// `edit-marks` joining strokes into the first.
    pub const MERGE: ActionRef<Self, (Vec<String>, bool)> =
        ActionRef::new("edit-marks", &["ids", "merge"]);
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
    /// `use-selection`: what to make of it, on a layer, in a colour.
    pub const USE_SELECTION: ActionRef<Self, (String, Option<String>, Option<String>)> =
        ActionRef::new("use-selection", &["do", "layer", "colour"]);
    /// `select` inverting the selection.
    pub const SELECT_INVERT: ActionRef<Self, bool> = ActionRef::new("select", &["invert"]);
    /// `select` with a mode alone: `none` selects nothing.
    pub const SELECT_MODE: ActionRef<Self, String> = ActionRef::new("select", &["mode"]);
    /// `combine`: marks, then `union`, `subtract`, `intersect` or `exclude`.
    pub const COMBINE: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("combine", &["ids", "op"]);
    /// `edit-layer` giving a layer a mask (`reveal`, `hide`) or removing it.
    pub const SET_LAYER_MASK: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "mask"]);
    /// `edit-layer` inverting a layer's mask.
    pub const SET_MASK_INVERT: ActionRef<Self, (String, bool)> =
        ActionRef::new("edit-layer", &["id", "invert"]);
    /// `edit-layer` clipping a layer to the one below.
    pub const SET_LAYER_CLIP: ActionRef<Self, (String, bool)> =
        ActionRef::new("edit-layer", &["id", "clip"]);
    /// `checkpoint`: name the current version.
    pub const CHECKPOINT: ActionRef<Self, String> = ActionRef::new("checkpoint", &["name"]);
    /// `edit-marks` changing colour and tone adjustments.
    pub const SET_ADJUST: ActionRef<Self, (Vec<String>, crate::model::Adjust)> =
        ActionRef::new("edit-marks", &["ids", "adjust"]);
    /// `edit-marks` cropping a picture: fractions, or `[]` for all of it.
    pub const SET_CROP: ActionRef<Self, (Vec<String>, Vec<f64>)> =
        ActionRef::new("edit-marks", &["ids", "crop"]);
    /// `edit-marks` straightening a picture, in degrees.
    pub const SET_STRAIGHTEN: ActionRef<Self, (Vec<String>, f64)> =
        ActionRef::new("edit-marks", &["ids", "straighten"]);
    /// `edit-marks` mirroring marks `horizontal` or `vertical`.
    pub const FLIP: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("edit-marks", &["ids", "flip"]);
    /// `set-artwork` cropping the artwork to what a mark shows.
    pub const FIT_ARTWORK: ActionRef<Self, String> = ActionRef::new("set-artwork", &["fit"]);
    /// `edit-layer` changing a layer's adjustments.
    pub const SET_LAYER_ADJUST: ActionRef<Self, (String, Box<crate::model::Adjust>)> =
        ActionRef::new("edit-layer", &["id", "adjust"]);
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
    /// Marks or groups to measure as the eye sees them: ink box, ink area,
    /// centre of mass and its offset from the box's middle (for balancing
    /// icons optically), and a text's letter boxes.
    pub measure: Option<Vec<String>>,
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

/// Render the artwork at full size as a picture.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Pixels per artwork unit, 0.05 to 4 (default 1).
    pub scale: Option<f64>,
    /// `png` (default), `jpeg` or `webp` (lossless).
    pub format: Option<String>,
    /// JPEG quality, 1 to 100 (default 90).
    pub quality: Option<u8>,
}

/// Export the artwork, or some of its marks, as SVG.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportSvg {
    /// Only these marks or groups, cropped to them and without the ground:
    /// one icon of a set.
    pub ids: Option<Vec<String>>,
    /// Write text as outlines, which look the same without its fonts.
    /// Text along a path is always outlined.
    pub outline_text: Option<bool>,
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

/// A picture named by an absolute path, a content address or a `data:`
/// URI, kept in the content store where the action is authored: the action
/// names it by content, so every replica shows the same picture. Answers
/// its address and size in pixels.
///
/// # Errors
/// Returns why the picture cannot be read or is not a PNG, JPEG or WebP.
pub fn keep_picture(src: &str) -> Result<(String, (u32, u32)), String> {
    use crate::picture::MAX_PICTURE_BYTES;
    let bytes = if src.starts_with('/') {
        read_file(src, MAX_PICTURE_BYTES)?
    } else if src.starts_with("data:") {
        use base64::Engine as _;
        let encoded = src
            .split_once(";base64,")
            .map(|(_, encoded)| encoded.trim())
            .ok_or("an inline picture is data:image/png;base64,… (or jpeg, webp)")?;
        if encoded.len() > MAX_PICTURE_BYTES.saturating_mul(4) / 3 {
            return Err(format!("the picture is over {MAX_PICTURE_BYTES} bytes"));
        }
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| format!("the inline picture is not base64: {error}"))?
    } else if ui::tilo_actions::content_hash(src).is_some() {
        let bytes = crate::picture::bytes_of(src)
            .ok_or_else(|| format!("{src} is not in this app's content store"))?;
        let size = picture_size(src, &bytes)?;
        return Ok((src.to_owned(), size));
    } else {
        return Err(format!(
            "{src:?} must be an absolute path to a PNG, JPEG or WebP, a content address, or a data: URI"
        ));
    };
    keep_picture_bytes(src, &bytes)
}

/// Picture bytes kept in the content store: their address and size in
/// pixels. `named` says where they came from, in a refusal.
///
/// # Errors
/// Returns why the bytes are not a PNG, JPEG or WebP, or cannot be kept.
pub fn keep_picture_bytes(named: &str, bytes: &[u8]) -> Result<(String, (u32, u32)), String> {
    let size = picture_size(named, bytes)?;
    Ok((store(bytes)?, size))
}

/// The edit that opens a picture file's bytes as a new artwork, titled by
/// the file's name.
///
/// # Errors
/// Returns why the bytes are not a picture or cannot be kept.
pub fn open_picture(name: &str, bytes: &[u8]) -> Result<CanvasAction, String> {
    let (src, (width, height)) = keep_picture_bytes(name, bytes)?;
    Ok(CanvasAction::OpenPicture(crate::actions::Open {
        src,
        pixels: [width, height],
        title: std::path::Path::new(name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned(),
    }))
}

/// The size of the picture in `bytes`, refusing what is not one.
fn picture_size(src: &str, bytes: &[u8]) -> Result<(u32, u32), String> {
    let named = if src.starts_with('/') {
        src
    } else {
        "the picture"
    };
    let format = image::guess_format(bytes)
        .map_err(|error| format!("{named} is not a PNG, JPEG or WebP: {error}"))?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err(format!("{named} is {format:?}, not a PNG, JPEG or WebP"));
    }
    let (width, height) = crate::picture::dimensions(bytes)
        .map_err(|error| format!("{named} cannot be read: {error}"))?;
    if width.max(height) > crate::picture::MAX_PICTURE_SIDE || width == 0 || height == 0 {
        return Err(format!(
            "{named} is {width}x{height}; pictures are at most {} pixels a side",
            crate::picture::MAX_PICTURE_SIDE
        ));
    }
    Ok((width, height))
}

/// An image shape as authored: its picture kept by content and its frame
/// filled in from the picture's size where it was left out.
fn kept_image(shape: &mut crate::model::Shape) -> Result<(), String> {
    let crate::model::Shape::Image {
        src,
        width,
        height,
        crop,
        ..
    } = shape
    else {
        return Ok(());
    };
    let (address, (pw, ph)) = keep_picture(src)?;
    *src = address;
    let [.., cw, ch] = crop.unwrap_or([0.0, 0.0, 1.0, 1.0]);
    let (natural_w, natural_h) = (f64::from(pw) * cw, f64::from(ph) * ch);
    match (*width > 0.0, *height > 0.0) {
        (false, false) => (*width, *height) = (natural_w, natural_h),
        (true, false) => *height = *width * natural_h / natural_w.max(1e-9),
        (false, true) => *width = *height * natural_w / natural_h.max(1e-9),
        (true, true) => {}
    }
    Ok(())
}

/// The title a picture's file gives a new artwork: its name without the
/// extension.
fn title_of(path: &str) -> String {
    if !path.starts_with('/') {
        return String::new();
    }
    std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// Open a picture as a new artwork.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenPath {
    /// A PNG, JPEG or WebP: an absolute path, a content address or a data:
    /// URI.
    pub path: String,
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
        [
            decl::<Paint>(
                "paint",
                "Paint strokes",
                "Paint brush strokes on a layer in one step. On an oil or watercolour layer the medium simulates real paint; on a plain layer the brush preset draws them: pen for clean line art (pressure swells the line), pencil or charcoal to shade (light pressure, many strokes; charcoal catches the paper), marker for flat see-through tone, airbrush for soft glows, calligraphy for lettering. brush eraser (no colour) clears paint under it on its layer; remove-marks along takes whole strokes. Each stroke: points [[x,y] or [x,y,pressure]], size, colour (#rrggbb or pigment:<name>). One stroke can be many: forks grows a branch or bare tree; dabs scatters foliage, flowers or (with dab_length, angle -90) grass. Work like a painter, looking close after each passage: big shapes first; then forms in a few values (dark mass, middle, lit side); small lights and accents last.",
                CanvasAction::Paint,
            ),
            ActionDecl::new("draw", "Draw shapes", params_of::<Draw>(), |args, _now| {
                let mut draw: Draw = read_args(args)?;
                for mark in &mut draw.marks {
                    kept_image(&mut mark.shape)?;
                }
                Ok(CanvasAction::Draw(draw))
            })
            .describe("Draw shapes on a layer in one step: rect, ellipse, polygon, line, path (SVG d), text, stroke or image (src: a file path, content address or data: URI; a unit per pixel unless sized), each with fill, gradient, stroke and opacity. Give ids to address marks later. On a painted layer filled shapes are laid in with the brush: paint a building or small figure as filled planes (lit and shadow side) with a small brush, not as many strokes."),
            ActionDecl::new("edit-marks", "Edit marks", params_of::<EditMarks>(), |args, _now| {
                let mut edit: EditMarks = read_args(args)?;
                if let Some(shape) = &mut edit.shape {
                    kept_image(&mut shape.0)?;
                }
                Ok(CanvasAction::edit(edit))
            })
            .describe("Change marks by id: colours, opacity, name, move_by [dx,dy], scale, rotate and flip about their centre, or move them to another layer. Strokes: re-brush (brush, size, params), smooth, simplify, extend, split at a point, merge. Pictures: crop, straighten, adjust (exposure, white balance, …; look to judge)."),
            decl::<RemoveMarks>(
                "remove-marks",
                "Remove marks",
                "Remove marks by id, every mark of a layer, or the strokes a path along crosses.",
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
                "Rename a layer, change its medium, opacity, visibility, lock or adjustments (grading all its marks), or move it to the front, back, forward or backward.",
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
                "Set the title, size (units) or ground: texture (smooth, paper, cold-press, rough, linen, panel) and colour; or crop the artwork to a region or to what a mark shows (fit).",
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
        ]
        .into_iter()
        .chain(picture_declarations())
        .chain(area_declarations())
        .collect()
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
            "List the layers and the newest marks (id, kind, name, layer, bounds [x,y,w,h], colours), optionally only in a region or on one layer; measure weighs marks' ink as the eye does, to balance icons and type.",
        );
        inspect.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut media = view("media", "List media", Vec::new(), |_| ViewOp::Media).with_description(
            "List the media layers can use (oil, watercolour presets and those saved here) with their engines' parameters and pigment palettes, and the fonts text can use.",
        );
        media.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut export = ActionSpec::typed_operation(
            "export",
            "Export picture",
            "Export picture",
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
            "Render the artwork at full size (or scaled) as a PNG, JPEG or WebP and answer with its content address.",
        );
        export.contract.result = ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        };
        let mut diff = view("diff", "Compare versions", params_of::<Diff>(), |args| {
            ViewOp::Diff(read_args(args).unwrap_or_default())
        })
        .with_description(
            "Compare two versions: marks and layers added, removed or changed by id (with what changed: stacking, shape, fill, …), the changed region, and with look a before|after picture of it.",
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

/// The actions that bring pictures in: a reference to work from, an SVG
/// as vector marks, a picture as a new artwork.
fn picture_declarations() -> Vec<ActionDecl<ArtworkResource>> {
    vec![
            ActionDecl::new("set-reference", "Set reference", params_of::<SetReference>(), |args, _now| {
                let mut set: SetReference = read_args(args)?;
                if let Some(image) = &set.image {
                    set.image = Some(keep_picture(image)?.0);
                }
                Ok(CanvasAction::SetReference(set))
            })
            .describe("Set the reference picture to recreate or trace: shown over the artwork at its opacity, seen by look (reference: false hides it) and compare; never exported."),
            ActionDecl::new("import-svg", "Import SVG", params_of::<crate::svg::ImportSvg>(), |args, _now| {
                let request: crate::svg::ImportSvg = read_args(args)?;
                crate::svg::import(&request).map(CanvasAction::Draw)
            })
            .describe("Import an SVG (markup, file path or content address) as one draw of path and text marks in a group: fills, gradients, outlines, opacity, blends and clip paths."),
            ActionDecl::new("open", "Open picture", params_of::<OpenPath>(), |args, _now| {
                let open: OpenPath = read_args(args)?;
                let (src, (width, height)) = keep_picture(&open.path)?;
                Ok(CanvasAction::OpenPicture(crate::actions::Open {
                    src,
                    pixels: [width, height],
                    title: title_of(&open.path),
                }))
            })
            .describe("Open a PNG, JPEG or WebP as a new artwork the size of the picture (a unit per pixel), the picture its first mark, `picture`. Replaces what the artwork held (undo brings it back)."),
    ]
}

/// Selecting areas, using the selection and combining shapes.
fn area_declarations() -> Vec<ActionDecl<ArtworkResource>> {
    vec![
            ActionDecl::new("select", "Select area", params_of::<crate::selection::Select>(), |args, _now| {
            let mut select: crate::selection::Select = read_args(args)?;
            crate::author::resolve(&mut select)?;
            Ok(CanvasAction::Select(Box::new(select)))
        })
        .describe("Select an area: shapes, marks, magic wand (at) or colour; add, subtract, intersect, grow, feather, invert. Then use-selection, or clip: \"selection\" in paint or draw. Bucket fill: select at, use-selection fill. look outlines it"),
        decl::<crate::selection::UseSelection>(
            "use-selection",
            "Use selection",
            "Fill or stroke the selection as a mark on a layer, mask the layer to it, or clear it from the layer (by its mask, a layer of grey marks).",
            CanvasAction::UseSelection,
        ),
        decl::<crate::selection::Combine>(
            "combine",
            "Combine shapes",
            "Combine shapes into one path mark, in the bottom one's place and style.",
            CanvasAction::Combine,
        ),
    ]
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

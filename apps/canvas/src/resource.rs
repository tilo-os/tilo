//! The artwork as a framework resource: its state, its vocabulary published
//! by name, and the views a caller with no screen uses to see it.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{
    self, ActionCx, ActionRef, Doc, Invoke, JsonSchema, ModelError, Place, Refusal, SearchEntry,
    resource,
};

// The verbs' argument structs, named apart from the verbs' own generated
// parameter types.
use crate::actions::{
    AddLayer as NewLayer, Align as Alignment, Arrange as Arrangement, Draw as DrawShapes,
    EditLayer as LayerEdit, EditMarks as MarkEdit, NewArtwork as BlankArtwork,
    Open as PictureToOpen, OpenArtwork as SavedArtwork, Paint as PaintStrokes,
    RemoveLayer as LayerRemoval, RemoveMarks as MarkRemoval, RemoveMedium as MediumRemoval,
    SaveMedium as MediumToSave, SetArtwork as ArtworkSettings, SetReference as ReferenceSettings,
};
use crate::model::Artwork;
use crate::selection::{Combine as Combination, Select as Selecting, UseSelection as SelectionUse};
use crate::svg::ImportSvg as SvgImport;

/// Parts of the artwork's verbs that controls bind.
impl Artwork {
    /// `remove-marks` by id.
    pub const REMOVE_MARKS_BY_ID: ActionRef<Self, Vec<String>> =
        ActionRef::new("remove-marks", &["ids"]);
    /// `add-layer`: its name and medium (empty for plain).
    pub const ADD_LAYER_OF: ActionRef<Self, (String, String)> =
        ActionRef::new("add-layer", &["name", "medium"]);
    /// `edit-layer` showing or hiding a layer.
    pub const SET_LAYER_VISIBLE: ActionRef<Self, (String, bool)> =
        ActionRef::new("edit-layer", &["id", "visible"]);
    /// `edit-layer` moving a layer `front`, `back`, `forward` or `backward`.
    pub const MOVE_LAYER: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "to"]);
    /// `remove-layer` by id.
    pub const REMOVE_LAYER_BY_ID: ActionRef<Self, String> = ActionRef::new("remove-layer", &["id"]);
    /// `arrange`: marks, then `front`, `back`, `forward` or `backward`.
    pub const ARRANGE_TO: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("arrange", &["ids", "to"]);
    /// `edit-layer` renaming a layer.
    pub const RENAME_LAYER: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "name"]);
    /// `edit-layer` placing a layer in an organisational folder.
    pub const SET_LAYER_GROUP: ActionRef<Self, (String, String)> =
        ActionRef::new("edit-layer", &["id", "group"]);
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
    /// `select` inverting the selection.
    pub const SELECT_INVERT: ActionRef<Self, bool> = ActionRef::new("select", &["invert"]);
    /// `select` growing (or, negative, shrinking) the selection by units.
    pub const SELECT_GROW: ActionRef<Self, f64> = ActionRef::new("select", &["grow"]);
    /// `select` softening the selection's edge over units.
    pub const SELECT_FEATHER: ActionRef<Self, f64> = ActionRef::new("select", &["feather"]);
    /// `select` with a mode alone: `none` selects nothing.
    pub const SELECT_MODE: ActionRef<Self, String> = ActionRef::new("select", &["mode"]);
    /// `combine`: marks, then `union`, `subtract`, `intersect` or `exclude`.
    pub const COMBINE_AS: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("combine", &["ids", "op"]);
    /// `align` spacing marks evenly, `horizontal` or `vertical`.
    pub const DISTRIBUTE: ActionRef<Self, (Vec<String>, String)> =
        ActionRef::new("align", &["ids", "distribute"]);
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
    pub const SET_PIXELATED: ActionRef<Self, (Vec<String>, bool)> =
        ActionRef::new("edit-marks", &["ids", "pixelated"]);
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
    /// Include literal fill, stroke and gradient colours with mark counts,
    /// scoped by region/layer, independent of the newest-mark limit.
    pub palette: Option<bool>,
    /// Only marks touching `[x, y, width, height]`.
    pub region: Option<[f64; 4]>,
    /// Only marks on this layer.
    pub layer: Option<String>,
    /// How many of the newest matching marks to list, 0 to 400 (default
    /// 100).
    pub limit: Option<u32>,
    /// A past version: a change id from history or a checkpoint name.
    pub version: Option<String>,
    /// Also each mark's shape, style and transform, to edit exactly.
    pub full: Option<bool>,
}

/// Measure marks' ink as the eye sees it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Measure {
    /// Marks or groups to measure.
    pub ids: Vec<String>,
    /// A past version: a change id from history or a checkpoint name.
    pub version: Option<String>,
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
    /// Omit the artwork ground for a transparent PNG or WebP.
    pub transparent: Option<bool>,
    /// Pixels per artwork unit, 0.05 to 4 (default 1).
    pub scale: Option<f64>,
    /// `png` (default, or the path's), `jpeg` or `webp` (lossless).
    pub format: Option<String>,
    /// JPEG quality (default 90).
    #[schema(range(min = 1, max = 100))]
    pub quality: Option<u8>,
    /// An absolute `.png`, `.jpg` or `.webp` path to write it to.
    pub path: Option<String>,
}

/// Find an area in the picture of the artwork: like colour touching a
/// point, a colour everywhere, the subject, or the selected edge fitted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindArea {
    /// Find the main subject on-device; with at, the object there.
    pub subject: Option<bool>,
    /// Edge reach in artwork units; subject defaults to 6 image pixels.
    pub refine: Option<f64>,
    /// `[x, y]` in artwork units: alone, the magic wand (like colour
    /// touching it); with subject, the object there.
    pub at: Option<[f64; 2]>,
    /// `#rrggbb`: that colour everywhere.
    pub colour: Option<String>,
    /// How far a colour may stray for at or colour, 0..1 (default 0.15).
    pub tolerance: Option<f64>,
    /// Find on this layer alone; otherwise use all visible layers.
    pub layer: Option<String>,
    /// `replace` (default), `add`, `subtract` or `intersect`.
    pub mode: Option<String>,
    /// Feather in artwork units.
    pub feather: Option<f64>,
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
    /// An absolute `.svg` path to write it to.
    pub path: Option<String>,
}

/// Save the artwork as a `.artwork` file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveArtwork {
    /// An absolute `.artwork` path to write; else kept as content.
    pub path: Option<String>,
}

/// What the canvas answers to look at, measure, compare and write out the
/// artwork: views read it as it is or was, effects write a picture or a
/// file of it. Each takes its fields as named arguments, as its schema
/// describes them, and answers with JSON.
#[derive(Clone, Debug, PartialEq, ui::tilo_resource::Effect)]
#[effect(operation = "canvas.view")]
pub enum ViewOp {
    /// Look at the artwork.
    ///
    /// See the artwork as a picture: the whole of it, or a region up close.
    /// Answers with the image and the revision it shows. grid: true overlays
    /// labelled coordinates for placing marks.
    #[effect(job, returns, idempotent, kind = "read")]
    Look {
        #[param(flatten)]
        look: Look,
    },
    /// Inspect marks.
    ///
    /// List layers and newest marks (ids, bounds, colours), scoped by
    /// region/layer. palette: true also counts literal fill, stroke and
    /// gradient colours across all matching marks, independent of limit
    /// (first 32 colours, `palette_colours` is the total). Named pigments come
    /// from media. measure weighs ink to balance icons and type.
    #[effect(view, returns, idempotent)]
    Inspect {
        #[param(flatten)]
        inspect: Inspect,
    },
    /// Measure ink.
    ///
    /// Weigh how marks or groups put down ink, as the eye sees it: each
    /// one's ink box, ink area, centre of mass and its offset from the box's
    /// middle (two icons balance when areas and offsets match), and a
    /// text's letter boxes. Answers once their pictures are rendered.
    #[effect(job, returns, idempotent, kind = "read")]
    Measure {
        #[param(flatten)]
        measure: Measure,
    },
    /// List media.
    ///
    /// List the media layers can use (oil, watercolour presets and those
    /// saved here) with their engines' parameters and pigment palettes, and
    /// the fonts text can use.
    #[effect(view, returns, idempotent)]
    Media,
    /// Export picture.
    ///
    /// Render the artwork at full size (or scaled) as a PNG, JPEG or WebP,
    /// written to path or kept as content.
    #[effect(job, returns, kind = "read", outside_with = "path")]
    Export {
        #[param(flatten)]
        export: Export,
    },
    /// Compare versions.
    ///
    /// Compare two versions: marks and layers added, removed or changed by id
    /// (with what changed: stacking, shape, fill, …), the changed region, and
    /// with look a before|after picture of it.
    #[effect(job, returns, idempotent, kind = "read")]
    CompareVersions {
        #[param(flatten)]
        diff: Diff,
    },
    /// Export SVG.
    ///
    /// Export the artwork as SVG (plain layers as exact vectors, painted ones
    /// as pictures), written to path or kept as content (small ones
    /// answered).
    #[effect(job, returns, kind = "read", outside_with = "path")]
    ExportSvg {
        #[param(flatten)]
        svg: ExportSvg,
    },
    /// Compare with reference.
    ///
    /// See the artwork beside (or over, or minus) the reference picture, with
    /// a difference score (mean colour distance 0..1 in the reference's
    /// frame; lower is closer) and the regions that differ most, to iterate
    /// toward it.
    #[effect(job, returns, idempotent, kind = "read")]
    Compare {
        #[param(flatten)]
        compare: Compare,
    },
    /// Save artwork.
    ///
    /// Save the artwork as a .artwork file open-artwork reads: to a path, or
    /// kept as content.
    #[effect(returns, kind = "read", outside_with = "path")]
    SaveArtwork {
        #[param(flatten)]
        save: SaveArtwork,
    },
    /// Find image area.
    ///
    /// Select what a picture of the artwork shows: the magic wand (at:
    /// colour touching it), a colour everywhere (colour), a photo's subject
    /// (subject: true) or the selected edge refined (refine: units), as one
    /// undo step. Then use-selection fill, mask or clear. Accepts select's
    /// add/subtract/intersect, layer and feather options. The artwork must
    /// remain unchanged while the area is found.
    #[effect(job, returns, kind = "undoable")]
    FindArea {
        #[param(flatten)]
        area: FindArea,
    },
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
        let bytes = crate::picture::bytes_of(&crate::content::store(), src)
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
pub fn open_picture(name: &str, bytes: &[u8]) -> Result<Invoke<Artwork>, String> {
    let (src, _) = keep_picture_bytes(name, bytes)?;
    Ok(Artwork::OPEN_PICTURE.with(crate::actions::Open {
        path: src,
        title: Some(
            std::path::Path::new(name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned(),
        ),
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

/// The bytes of the artwork file `file` names: its JSON itself, stored
/// content, or a file (a package, or JSON).
///
/// # Errors
/// Returns why the file cannot be had.
fn artwork_bytes(file: &str) -> Result<Vec<u8>, String> {
    let trimmed = file.trim_start();
    if trimmed.starts_with('{') {
        return Ok(trimmed.as_bytes().to_vec());
    }
    if let Some(hash) = ui::tilo_actions::content_hash(trimmed) {
        return crate::content::store()
            .read(hash)
            .ok_or_else(|| format!("no stored content {trimmed}"));
    }
    if trimmed.starts_with('/') {
        return read_file(trimmed, crate::document::MAX_FILE_BYTES);
    }
    Err("file is an absolute path to a .artwork file, a content address, or its JSON".into())
}

/// The bytes of the file at `path`, at most `limit` of them.
///
/// # Errors
/// Returns why the file cannot be read, or that it is too large.
pub fn read_file(path: &str, limit: usize) -> Result<Vec<u8>, String> {
    let cannot = |error: tilo_files::FsError| format!("cannot read {path}: {error}");
    let (dir, name) = tilo_files::file_at(std::path::Path::new(path)).map_err(cannot)?;
    tilo_files::blocking::read_bounded(&dir, &name, limit)
        .map_err(cannot)?
        .ok_or_else(|| format!("{path} is over {limit} bytes"))
}

/// Keep `bytes` in the app's content store; answers their address.
///
/// # Errors
/// Returns the store's failure.
pub fn store(bytes: &[u8]) -> Result<String, String> {
    let hash = crate::content::store()
        .put(bytes)
        .map_err(|error| format!("the content could not be stored: {error}"))?;
    ui::tilo_actions::content_resource(&hash).ok_or_else(|| "the content has no address".to_owned())
}

/// What the verbs resolve where they are invoked, from the world outside:
/// files and inline pictures kept as content, what the window shows. The
/// invocation then carries the result, so every replica applies the same.
mod prepare {
    use super::{artwork, keep_picture, kept_image, title_of};

    /// A drawn picture given by path or inline is kept as content.
    pub(super) fn draw(params: &mut artwork::Draw) -> Result<(), String> {
        for mark in &mut params.shapes.marks {
            kept_image(&mut mark.shape)?;
        }
        Ok(())
    }

    /// A picture an edit gives by path or inline is kept as content.
    pub(super) fn edit_marks(params: &mut artwork::EditMarks) -> Result<(), String> {
        if let Some(shape) = &mut params.edit.shape {
            kept_image(&mut shape.0)?;
        }
        Ok(())
    }

    /// The reference picture is kept as content.
    pub(super) fn set_reference(params: &mut artwork::SetReference) -> Result<(), String> {
        let set = &mut params.set;
        if let Some(image) = &set.image {
            set.image = Some(keep_picture(image)?.0);
        }
        Ok(())
    }

    /// The picture is kept as content, with its size and its file's name.
    pub(super) fn open(params: &mut artwork::OpenPicture) -> Result<(), String> {
        let open = &mut params.picture;
        let (src, _) = keep_picture(&open.path)?;
        if open.title.is_none() {
            open.title = Some(title_of(&open.path));
        }
        open.path = src;
        Ok(())
    }

    /// The saved artwork as its JSON, its pictures kept as content.
    pub(super) fn open_artwork(params: &mut artwork::OpenArtwork) -> Result<(), String> {
        let open = &mut params.open;
        let bytes = super::artwork_bytes(&open.file)?;
        let file = crate::document::unpack(&bytes, &crate::content::store())?;
        open.file = String::from_utf8_lossy(&file.to_json()).into_owned();
        Ok(())
    }

    /// The SVG as its markup.
    pub(super) fn import_svg(params: &mut artwork::ImportSvg) -> Result<(), String> {
        params.import.svg = crate::svg::source(&params.import.svg)?;
        Ok(())
    }

    /// The matte's pictures defringed and kept as content.
    pub(super) fn use_selection(params: &mut artwork::UseSelection) -> Result<(), String> {
        crate::subject::clean(&mut params.using)
    }
}

/// The artwork's verbs: everything that changes it beyond one field.
///
/// Each takes its documented argument struct flat, applies to the artwork
/// as read, and is refused whole when any part of it is.
#[resource(
    kind = "canvas.artwork",
    label = "Artwork",
    initialize = Artwork::genesis,
    load = Artwork::loaded,
    lands = Artwork::lands,
    search = Artwork::entries,
    answer = crate::answer::answer,
    effects = ViewOp,
    version = 3,
    migrate = crate::model::upgrade
)]
impl Artwork {
    /// Paint strokes.
    ///
    /// Paint brush strokes on a layer in one step. On an oil or watercolour
    /// layer the medium simulates real paint; on a plain layer the brush
    /// preset draws them: pen for clean line art (pressure swells the line),
    /// pencil or charcoal to shade (pressure sets darkness; charcoal catches
    /// the paper), marker for flat see-through tone, airbrush for soft glows,
    /// calligraphy for lettering. brush eraser (no colour) clears paint under
    /// it on its layer; remove-marks along takes whole strokes. Each stroke:
    /// points `[[x,y] or [x,y,pressure]]`, size, colour (#rrggbb or
    /// `pigment:<name>`). Answers the new ids. One stroke can be many: forks
    /// grows a branch or bare tree; dabs scatters foliage, flowers or (with
    /// `dab_length`, angle -90) grass; hatch shades the area its points
    /// enclose. Work like a painter, looking close after each passage: big
    /// shapes first; then forms in a few values (dark mass, middle, lit
    /// side); small lights and accents last.
    #[action(answers)]
    pub fn paint(&mut self, #[param(flatten)] strokes: PaintStrokes) -> Result<(), Refusal> {
        let art = self.as_read();
        strokes.apply(self, &art)
    }

    /// Draw shapes.
    ///
    /// Draw shapes on a layer in one step: rect, ellipse, polygon, line, path
    /// (SVG d), text, stroke or image (src: a file path, content address or
    /// data: URI; a unit per pixel unless sized), each with fill, gradient,
    /// stroke and opacity. Answers the new ids; or give your own. On a
    /// painted layer filled shapes are laid in with the brush: paint a
    /// building or small figure as filled planes (lit and shadow side) with a
    /// small brush, not as many strokes.
    #[action(answers, prepare = prepare::draw)]
    pub fn draw(&mut self, #[param(flatten)] shapes: DrawShapes) -> Result<(), Refusal> {
        let art = self.as_read();
        shapes.apply(self, &art)
    }

    /// Draw chart.
    ///
    /// Bar chart from numbers; use it, not hand-computed rects. Equal bars,
    /// one baseline, lengths exactly value/max of box.
    #[action(answers)]
    pub fn chart(&mut self, #[param(flatten)] chart: crate::chart::Chart) -> Result<(), Refusal> {
        let art = self.as_read();
        chart.apply(self, &art)
    }

    /// Edit marks.
    ///
    /// Change marks by id: colours, opacity, name, `move_by` `[dx,dy]`, scale,
    /// rotate and flip about their centre, or move them to another layer.
    /// Strokes: re-brush (brush, size, params), smooth, simplify, extend,
    /// split at a point, merge. Pictures: crop, straighten, adjust (exposure,
    /// white balance, …; look to judge).
    #[action(prepare = prepare::edit_marks)]
    pub fn edit_marks(&mut self, #[param(flatten)] edit: MarkEdit) -> Result<(), Refusal> {
        let art = self.as_read();
        edit.apply(self, &art)
    }

    /// Remove marks.
    ///
    /// Remove marks by id, every mark of a layer, or the strokes a path along
    /// crosses.
    #[action]
    pub fn remove_marks(&mut self, #[param(flatten)] remove: MarkRemoval) -> Result<(), Refusal> {
        let art = self.as_read();
        crate::actions::remove_marks(self, &art, &remove)
    }

    /// Arrange marks.
    ///
    /// Bring marks to the front or back of their layer, one step forward or
    /// backward, or directly above or below another mark or group.
    #[action]
    pub fn arrange(&mut self, #[param(flatten)] arrange: Arrangement) -> Result<(), Refusal> {
        let art = self.as_read();
        crate::actions::arrange_marks(self, &art, &arrange)
    }

    /// Add layer.
    ///
    /// Add a layer on top (or above another). Give it a medium (see media) to
    /// paint in oil or watercolour; without one it holds crisp vector art.
    #[action]
    pub fn add_layer(
        &mut self,
        cx: &mut ActionCx,
        #[param(flatten)] add: NewLayer,
    ) -> Result<(), Refusal> {
        let mut add = add;
        if add.id.is_none() {
            let minted = cx.uuid().simple().to_string();
            add.id = Some(format!("layer-{}", minted.get(..8).unwrap_or_default()));
        }
        let art = self.as_read();
        crate::actions::add_layer(self, &art, &add)
    }

    /// Edit layer.
    ///
    /// Rename or place a layer in an organisational folder (group; empty
    /// removes it), change its medium, opacity, visibility, lock or
    /// adjustments (grading all its marks), or move it to the front, back,
    /// forward or backward.
    #[action]
    pub fn edit_layer(&mut self, #[param(flatten)] edit: LayerEdit) -> Result<(), Refusal> {
        let art = self.as_read();
        crate::actions::edit_layer(self, &art, &edit)
    }

    /// Remove layer.
    ///
    /// Remove a layer and every mark on it.
    #[action]
    pub fn remove_layer(&mut self, #[param(flatten)] remove: LayerRemoval) -> Result<(), Refusal> {
        let art = self.as_read();
        crate::actions::remove_layer(self, &art, &remove)
    }

    /// Set artwork.
    ///
    /// Set the title, size (units) or ground: texture (smooth, paper,
    /// cold-press, rough, linen, panel) and colour; or crop the artwork to a
    /// region or to what a mark shows (fit).
    #[action]
    pub fn set_artwork(&mut self, #[param(flatten)] set: ArtworkSettings) -> Result<(), Refusal> {
        let art = self.as_read();
        crate::actions::set_artwork(self, &art, &set)
    }

    /// Save medium.
    ///
    /// Save a medium preset in the artwork: an engine (oil or watercolour)
    /// with parameters, a ground and a pigment palette. Layers can then use
    /// its id.
    #[action]
    pub fn save_medium(&mut self, #[param(flatten)] save: MediumToSave) -> Result<(), Refusal> {
        save.medium.validate().map_err(Refusal)?;
        self.media.insert(save.medium.id.clone(), save.medium);
        Ok(())
    }

    /// Remove medium.
    ///
    /// Remove a medium saved in this artwork.
    #[action]
    pub fn remove_medium(
        &mut self,
        #[param(flatten)] remove: MediumRemoval,
    ) -> Result<(), Refusal> {
        if self.media.remove(&remove.id).is_none() {
            return Err(Refusal(format!(
                "no medium {:?} is saved in this artwork",
                remove.id
            )));
        }
        Ok(())
    }

    /// Align marks.
    ///
    /// Align marks or groups to an edge or centre line, and/or distribute
    /// them with equal gaps, within their own extent, the artwork or another
    /// mark.
    #[action]
    pub fn align(&mut self, #[param(flatten)] align: Alignment) -> Result<(), Refusal> {
        let art = self.as_read();
        align.apply(self, &art)
    }

    /// Set reference.
    ///
    /// Set the reference picture to recreate or trace: shown over the artwork
    /// at its opacity, seen by look (reference: false hides it) and compare;
    /// never exported.
    #[action(prepare = prepare::set_reference)]
    pub fn set_reference(
        &mut self,
        #[param(flatten)] set: ReferenceSettings,
    ) -> Result<(), Refusal> {
        let art = self.as_read();
        crate::actions::set_reference(self, &art, &set)
    }

    /// Import SVG.
    ///
    /// Import an SVG (markup, file path or content address) as one draw of
    /// path and text marks in a group: fills, gradients, outlines, opacity,
    /// blends and clip paths.
    #[action(answers, prepare = prepare::import_svg)]
    pub fn import_svg(&mut self, #[param(flatten)] import: SvgImport) -> Result<(), Refusal> {
        let draw = crate::svg::import(&import).map_err(Refusal)?;
        let art = self.as_read();
        draw.apply(self, &art)
    }

    /// Open picture.
    ///
    /// Open a PNG, JPEG or WebP as a new artwork the size of the picture (a
    /// unit per pixel), the picture its first mark, `picture`. Replaces what
    /// the artwork held (undo brings it back).
    #[action(prepare = prepare::open)]
    pub fn open_picture(
        &mut self,
        #[param(flatten)] picture: PictureToOpen,
    ) -> Result<(), Refusal> {
        // The picture is named by its content, which every replica reads
        // from its own store.
        if ui::tilo_actions::content_hash(&picture.path).is_none() {
            return Err(Refusal(format!(
                "{:?} is not kept as content: open names a picture by its content address",
                picture.path
            )));
        }
        let (_, (width, height)) = keep_picture(&picture.path).map_err(Refusal)?;
        let art = self.as_read();
        picture.apply(self, &art, [width, height])
    }

    /// New artwork.
    ///
    /// Start a blank artwork (one undo away). size: screen, a4, letter,
    /// square, poster, icon (landscape turns it). start: paper, white,
    /// watercolour, oil, panel, dark.
    #[action]
    pub fn new_artwork(&mut self, #[param(flatten)] new: BlankArtwork) -> Result<(), Refusal> {
        crate::actions::replace(self, &new.file()?)
    }

    /// Open artwork.
    ///
    /// Replace the artwork with a saved .artwork file (path, content address
    /// or JSON), one undo away.
    #[action(prepare = prepare::open_artwork)]
    pub fn open_artwork(&mut self, #[param(flatten)] open: SavedArtwork) -> Result<(), Refusal> {
        let file = crate::document::ArtworkFile::parse(open.file.as_bytes()).map_err(Refusal)?;
        crate::actions::replace(self, &file)
    }

    /// Select area.
    ///
    /// Select an area: shapes or marks; add, subtract, intersect, grow,
    /// feather, invert. For the magic wand, a colour, the subject or edge
    /// refinement invoke find-area (a job, as it reads a picture). Then
    /// use-selection, or clip: "selection" in paint or draw. Bucket fill:
    /// find-area at, use-selection fill. Cut-out: find-area subject,
    /// use-selection mask. look outlines it
    #[action(answers)]
    pub fn select(&mut self, #[param(flatten)] select: Selecting) -> Result<(), Refusal> {
        let art = self.as_read();
        select.apply(self, &art)
    }

    /// Use selection.
    ///
    /// Fill or stroke the selection as a mark on a layer, mask the layer to
    /// it, or clear it from the layer (by its mask, a layer of grey marks).
    #[action(answers, prepare = prepare::use_selection)]
    pub fn use_selection(&mut self, #[param(flatten)] using: SelectionUse) -> Result<(), Refusal> {
        let art = self.as_read();
        using.apply(self, &art)
    }

    /// Combine shapes.
    ///
    /// Combine shapes into one path mark, in the bottom one's place and
    /// style.
    #[action]
    pub fn combine(&mut self, #[param(flatten)] combine: Combination) -> Result<(), Refusal> {
        let art = self.as_read();
        combine.apply(self, &art)
    }
}

impl Artwork {
    /// Where selecting leaves the participant who asked for it: the marks
    /// `select` names, as a range over the artwork's marks, in the order
    /// every collaborator holds them — so a peer draws the selection on
    /// their own screen, and a participant with no pointer of its own shows
    /// what it picked.
    fn lands(
        state: &Self,
        action: &<Self as ui::tilo_resource::Resource>::Action,
    ) -> Option<Place> {
        let ArtworkAction::Select(select) = &action.operation else {
            return None;
        };
        let named = select.select.marks.as_ref()?;
        let picked: Vec<usize> = state
            .marks
            .iter()
            .enumerate()
            .filter(|(_, mark)| named.iter().any(|id| *id == mark.id || *id == mark.group))
            .map(|(index, _)| index)
            .collect();
        let (first, last) = (*picked.first()?, picked.last()?);
        Some(Place::Range {
            item: crate::surface::peers::SELECTION_ITEM.to_owned(),
            start: first,
            end: last.saturating_add(1),
        })
    }

    /// A new artwork is an empty sheet of paper with one plain layer, so the
    /// first stroke has somewhere to go.
    fn genesis() -> Self {
        let mut art = Self::default();
        let empty = art.clone();
        let added = crate::actions::add_layer(
            &mut art,
            &empty,
            &NewLayer {
                id: Some("layer-1".into()),
                name: "Layer 1".into(),
                ..NewLayer::default()
            },
        );
        debug_assert!(
            added.is_ok(),
            "the first layer of a blank artwork: {added:?}"
        );
        art
    }

    /// The artwork as read: settled, and what the next action authored here
    /// finds in it, even before the window has shown it.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the signature the resource's load hook is called with"
    )]
    fn loaded(
        art: &mut Self,
        _doc: &Doc,
        _demand: &tilo_resource::Demand,
    ) -> Result<(), ModelError> {
        art.settle();
        crate::author::read(art);
        Ok(())
    }

    /// The artwork by its title and layers, and its named marks.
    fn entries(&self) -> Vec<SearchEntry> {
        let mut entries = vec![SearchEntry {
            anchor: None,
            title: if self.title.is_empty() {
                "Artwork".into()
            } else {
                self.title.clone()
            },
            text: self
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            action: None,
            ..Default::default()
        }];
        entries.extend(
            self.marks
                .iter()
                .filter(|mark| !mark.name.is_empty())
                .map(|mark| SearchEntry {
                    anchor: Some(mark.id.clone()),
                    title: mark.name.clone(),
                    text: mark.shape.kind().into(),
                    action: None,
                    ..Default::default()
                }),
        );
        entries
    }
}

impl Artwork {
    /// Apply `invoke` to `doc` through the door, as a replica of it does. A
    /// blank document is initialized first — its genesis and its storage
    /// version — as a door's record is before its first change.
    #[cfg(test)]
    pub(crate) fn apply_to(doc: &mut Doc, invoke: impl Into<Invoke<Self>>) {
        use tilo_resource::__private::ReadDoc as _;
        use tilo_resource::Resource as _;
        if doc.length(&ui::automerge::ROOT) == 0 {
            Artwork::initialize(doc).expect("a blank artwork initializes");
        }
        let mut replica = tilo_resource::Replica::<Self>::from_doc(std::mem::take(doc));
        replica.apply(invoke).expect("the change applies");
        *doc = std::mem::take(replica.doc_mut());
    }

    /// The artwork settled as a verb reads it, and its state before the verb.
    fn as_read(&mut self) -> Self {
        self.settle();
        self.clone()
    }
}

#[cfg(test)]
mod export_reads {
    use ui::tilo_actions::ActionKind;
    use ui::tilo_resource::Effect as _;

    /// Looking at the artwork and keeping a picture, an SVG or the artwork
    /// file of it as content only read it, so whoever may read the artwork,
    /// an agent among them, may run them; told a path to write, an export
    /// reaches outside and asks first.
    #[test]
    fn looks_and_exports_kept_as_content_are_reads_and_a_path_reaches_outside() {
        let specs = super::ViewOp::specs();
        let spec = |id: &str| {
            specs
                .iter()
                .find(|spec| spec.id == id)
                .unwrap_or_else(|| panic!("`{id}` is published"))
        };
        assert_eq!(spec("look").contract.kind, ActionKind::Read);
        for id in ["export", "export-svg", "save-artwork"] {
            let export = spec(id);
            assert_eq!(export.contract.kind, ActionKind::Read, "{id}");
            let need = |arguments: serde_json::Value| {
                export
                    .need_for(Some(arguments.as_object().expect("an object")), None)
                    .kind
            };
            assert_eq!(need(serde_json::json!({})), ActionKind::Read, "{id}");
            assert_eq!(
                need(serde_json::json!({ "path": "/tmp/artwork.out" })),
                ActionKind::Outside,
                "{id}"
            );
        }
    }
}

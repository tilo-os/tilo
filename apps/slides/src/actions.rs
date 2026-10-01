//! Slides' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every structural / geometric / stylistic operation Slides performs on the
//! shared deck is a [`SlidesAction`]: a small serializable value that
//! deterministically mutates the CRDT (`root["deck"]` — see [`crate::doc`]).
//! Because the deck already lives in the shared document, an action replicates
//! and replays for free, and feeds straight back into the editor on the next
//! `sync`. The exact same [`EncodedAction`] is what the on-screen affordances
//! (the toolbar's insert tools, the inspector swatches, the slide-rail add /
//! duplicate / delete, a canvas drag's drop, the right-click menu) build and
//! dispatch and what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! ## The split
//!
//! Document-mutating → an action: add / delete / duplicate / reorder a slide,
//! set a slide's layout or background, set the deck theme; add / delete an
//! element, move or resize it (its **final** geometry on drop, never per drag
//! frame), restyle it (fill / stroke / align / bold / italic / size / radius),
//! raise or lower it in the z-order.
//!
//! Flowing **text** → never an action: the deck title, a slide's speaker notes
//! and a text element's body stay on the native Automerge `Text` splice path
//! (`SlidesDoc::splice_*`) so two peers get character-grained convergence, the
//! same reason Sheets keeps a cell *body* out of the action model would be the
//! opposite (a cell is a discrete value; a slide's body is a rich-text run).
//!
//! Presentation → never a document action: the current slide, the selection,
//! zoom / drag preview, active tool, panel flaps and present mode are local.
//! Start/stop and output-policy commands are view operations so the resource
//! surface can expose their user-visible lifecycle without sharing hardware
//! choices with collaborators.
//!
//! External → never an action: `.odp` import / export (a whole-deck swap, not a
//! stored mutation) and the per-peer image decode cache.
//!
//! ## Typed operations
//!
//! The deck is a [`ui::tilo_resource::Resource`] ([`SlidesDeck`]) whose action
//! is [`SlidesAction`]. Its declarations — add, move/resize, restyle and
//! delete an element, move a slide — take their targets and values as typed
//! arguments, so an agent or a script can make any layout or styling edit a
//! person makes on the canvas, by stable id, through the same permission,
//! history and undo path. Bound document ops — add a slide (rank from the
//! live deck) and set the accent theme — ride
//! [`Resource::bound_actions`](ui::tilo_resource::Resource::bound_actions) so
//! `tilo_resource::publish` includes them without a post-publish bolt-on.
//! `apply` (any [`SlidesAction`] as JSON) and `group` (several operations as
//! one undo step) are published beside them.
//!
//! ## Determinism contract
//!
//! Ids (slide + element uuids) are minted at **authoring** time and carried in
//! the payload; every enum is carried in its document string form. So
//! [`Action::apply`] is a pure function of `(payload, base document)` — no RNG,
//! no wall-clock, no app state. Where an operation is relative to the current
//! deck (legacy positional actions only) it recomputes from the document's
//! base state. Newly authored insert, reorder, and arrange operations carry
//! absolute rank/z values, so receivers never derive placement from a
//! concurrently changed view. A malformed or stale payload is refused before
//! it changes the document, so it cannot create a successful-looking history
//! entry that silently did nothing.

#![allow(
    clippy::expect_used,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    reason = "the operation builder deliberately owns a serializable value; enum serialization cannot fail for these derived primitive payloads"
)]

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionAddress, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry,
    ActionSpec, Doc, EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType,
    Resource, ResultSpec, RetryPolicy, RiskLevel,
};
use ui::tilo_resource::{ActionCx, ActionDecl};

use crate::doc::SlidesDoc;
use crate::model::{
    Background, Element, ElementKind, FILL_ROLES, Layout, PaletteKind, SLIDE_H, SLIDE_W,
    STROKE_ROLES, Slide, TextAlign, new_id,
};

/// Registry `type_id` for every Slides action.
pub const SLIDES_ACTION_TYPE: &str = "slides.action";
pub const SLIDES_OPERATION_TYPE: &str = "slides.operation";

pub fn slide_resource_id(slide: &str) -> String {
    format!("slides.slide:{slide}")
}

pub fn element_resource_id(element: &str) -> String {
    format!("slides.element:{element}")
}

pub fn text_resource_id(element: &str) -> String {
    format!("slides.text:{element}")
}

pub fn notes_resource_id(slide: &str) -> String {
    format!("slides.notes:{slide}")
}

pub fn open_slide_address(slide: &str) -> ActionAddress {
    ActionAddress::new(slide_resource_id(slide), "open")
}

pub fn open_element_address(element: &str) -> ActionAddress {
    ActionAddress::new(element_resource_id(element), "open")
}

pub fn open_text_address(element: &str) -> ActionAddress {
    ActionAddress::new(text_resource_id(element), "open")
}

pub fn open_notes_address(slide: &str) -> ActionAddress {
    ActionAddress::new(notes_resource_id(slide), "open")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlidesOperation {
    ShowEditor,
    ShowOverview,
    StartPresentation,
    /// Starts presenting with an explicit local output policy. This is view
    /// state, not a CRDT mutation: each collaborator keeps control of their
    /// own physical displays.
    StartPresentationOn {
        output: PresentationOutput,
    },
    EndPresentation,
    RevealSlide {
        slide: String,
    },
    RevealElement {
        slide: String,
        element: String,
        edit_text: bool,
    },
    RevealNotes {
        slide: String,
    },
    Open {
        path: Option<String>,
    },
    SaveAs {
        path: Option<String>,
    },
    /// The deck's [outline](crate::outline).
    Read,
}

/// User-visible audience-output policies. Kept serializable at the action
/// boundary instead of leaking host monitor indices into documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresentationOutput {
    LargestExternal,
    PrimaryExternal,
}

#[cfg(test)]
mod file_action_tests {
    use super::*;
    #[test]
    fn path_argument_binds_into_background_save() {
        let spec = file_operation("save-as", "Save presentation", true);
        let encoded = spec
            .build_json(
                serde_json::json!({"path":"/tmp/a.odp"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(SlidesOperation::SaveAs {
                path: Some("/tmp/a.odp".into())
            })
        );
        assert!(matches!(
            spec.contract.execution,
            ExecutionMode::Background { .. }
        ));
        assert_eq!(spec.contract.result, ResultSpec::Job);
    }
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<SlidesOperation>, String> {
    if type_id != SLIDES_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn operation(id: &str, label: &str, kind: OperationKind, value: SlidesOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, SLIDES_OPERATION_TYPE, &value)
        .expect("SlidesOperation is always serializable");
    match value {
        SlidesOperation::ShowEditor
        | SlidesOperation::ShowOverview
        | SlidesOperation::RevealSlide { .. }
        | SlidesOperation::RevealElement { .. }
        | SlidesOperation::RevealNotes { .. } => spec
            .with_contract(ActionContract::view())
            .retry_policy(RetryPolicy::Idempotent),
        SlidesOperation::StartPresentation
        | SlidesOperation::StartPresentationOn { .. }
        | SlidesOperation::EndPresentation => spec.with_contract(ActionContract::view()),
        SlidesOperation::Open { .. } | SlidesOperation::SaveAs { .. } => spec.destructive(),
        SlidesOperation::Read => spec.with_contract(ActionContract {
            result: ResultSpec::Json {
                schema: crate::outline::result_schema(),
            },
            ..ActionContract::view()
        }),
    }
}

fn file_operation(id: &str, label: &str, save: bool) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "optional absolute path",
        OperationKind::Effect,
        SLIDES_OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "ODP path; omit to use the system picker",
        )],
        move |args: &ActionArgs| {
            let path = args
                .text("path")
                .filter(|p| !p.trim().is_empty())
                .map(str::to_string);
            let operation = if save {
                SlidesOperation::SaveAs { path }
            } else {
                SlidesOperation::Open { path }
            };
            EncodedAction {
                type_id: SLIDES_OPERATION_TYPE.into(),
                payload: serde_json::to_vec(&operation).expect("SlidesOperation serializes"),
            }
        },
    )
    .with_contract(ActionContract {
        risk: tilo_actions::RiskLevel::Destructive,
        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        ..ActionContract::default()
    })
}

// ===========================================================================
// Serializable snapshots — the document shape, so `apply` is a pure write.
// ===========================================================================

/// A serializable snapshot of an element's fields in exactly the representation
/// the document stores (every enum as its `as_str` form). Carrying the document
/// shape keeps [`Action::apply`] a pure write with no re-derivation, and makes
/// [`Self::from_element`] / [`Self::to_element`] lossless round-trips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementData {
    pub id: String,
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub text: String,
    pub src: String,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub font_size: f64,
    pub align: String,
    pub bold: bool,
    pub italic: bool,
    pub radius: f64,
    pub z: i64,
    /// A text element's words as written (paragraphs with their marks);
    /// empty for one described by `text` alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<tilo_document::ClipBlock>,
}

impl ElementData {
    pub fn from_element(e: &Element) -> Self {
        Self {
            id: e.id.clone(),
            kind: e.kind.as_str().to_string(),
            x: e.x,
            y: e.y,
            w: e.w,
            h: e.h,
            text: e.text.clone(),
            src: e.src.clone(),
            fill: e.fill.clone(),
            stroke: e.stroke.clone(),
            stroke_width: e.stroke_width,
            font_size: e.font_size,
            align: e.align.as_str().to_string(),
            bold: e.bold,
            italic: e.italic,
            radius: e.radius,
            z: e.z,
            blocks: e
                .blocks
                .iter()
                .map(tilo_rich_text::clip_of_content)
                .collect(),
        }
    }

    pub fn to_element(&self) -> Element {
        let mut element = Element {
            id: self.id.clone(),
            kind: ElementKind::from_str(&self.kind),
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
            text: self.text.clone(),
            src: self.src.clone(),
            fill: self.fill.clone(),
            stroke: self.stroke.clone(),
            stroke_width: self.stroke_width,
            font_size: self.font_size,
            align: TextAlign::from_str(&self.align),
            bold: self.bold,
            italic: self.italic,
            radius: self.radius,
            z: self.z,
            blocks: Vec::new(),
        };
        if !self.blocks.is_empty() {
            element.set_blocks(
                self.blocks
                    .iter()
                    .map(tilo_rich_text::content_of_clip)
                    .collect(),
            );
        }
        element
    }
}

/// A serializable snapshot of a whole slide, in the document's shape. Carried by
/// [`SlidesAction::InsertSlide`] so a create-blank or a duplicate replays as a
/// pure write from its payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlideData {
    pub id: String,
    pub layout: String,
    pub background: String,
    pub notes: String,
    pub elements: Vec<ElementData>,
}

impl SlideData {
    pub fn from_slide(s: &Slide) -> Self {
        Self {
            id: s.id.clone(),
            layout: s.layout.as_str().to_string(),
            background: s.background.as_str().to_string(),
            notes: s.notes.clone(),
            elements: s.elements.iter().map(ElementData::from_element).collect(),
        }
    }

    pub fn to_slide(&self) -> Slide {
        Slide {
            id: self.id.clone(),
            layout: Layout::from_str(&self.layout),
            background: Background::from_str(&self.background),
            notes: self.notes.clone(),
            elements: self.elements.iter().map(ElementData::to_element).collect(),
        }
    }

    /// A copy of this slide with a fresh slide id and fresh element ids — the
    /// pure "duplicate" payload. Ids are minted **here**, at authoring time, and
    /// baked into the returned data, so the resulting action replays identically
    /// on every peer.
    #[must_use]
    pub fn reminted(&self) -> Self {
        let mut copy = self.clone();
        copy.id = new_id();
        for e in &mut copy.elements {
            e.id = new_id();
        }
        copy
    }
}

/// A partial restyle of one element: only the `Some` fields are written. One
/// action covers every stylistic property (fill / stroke / alignment / weight /
/// size / radius) so a control that changes several at once (e.g. adding a
/// stroke also seeds its width) is a single undo boundary and a single change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StylePatch {
    pub fill: Option<String>,
    pub stroke: Option<String>,
    pub align: Option<String>,
    pub font_size: Option<f64>,
    pub stroke_width: Option<f64>,
    pub radius: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

/// A new element described by what its author chooses: kind, rectangle,
/// optional body text or image asset, and any style overrides. Everything
/// else — its id, its place in the z-order, its kind's default style — is
/// derived when the action applies, so an agent or a script never has to
/// invent an id to add something.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewElement {
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub src: String,
    #[serde(default)]
    pub style: StylePatch,
}

// ===========================================================================
// The action enum
// ===========================================================================

/// Every collaborative operation Slides exports on the shared deck.
///
/// Determinism contract: ids and enums are carried in the payload;
/// [`Action::apply`] never touches the RNG or the wall-clock. The two variants
/// that are relative to the current deck ([`Self::InsertSlide`],
/// [`Self::MoveSlide`], [`Self::RaiseElement`], and [`Self::LowerElement`])
/// are retained only for legacy decoding. Newly-authored insert/reorder/arrange
/// operations use absolute scalar values. Malformed or stale payloads are
/// refused atomically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SlidesAction {
    /// Insert a whole slide (a blank create *or* a duplicate) at index `at`.
    /// One variant covers both: "add" and "duplicate"
    /// differ only in the payload their builders mint, exactly as a create and
    /// an edit are one `Upsert` elsewhere.
    InsertSlide { at: usize, slide: SlideData },
    /// Insert an immutable slide snapshot with its author-computed absolute
    /// rank. New creation/duplicate paths use this, never a positional slot.
    InsertSlideWithOrder { slide: SlideData, rank: i64 },
    /// Delete the slide identified by `id` at `idx`. Refused when it is the
    /// deck's last slide or when the index no longer names that stable id.
    DeleteSlide { idx: usize, id: String },
    /// Move the slide at `from` so it lands at index `to`, carrying its whole
    /// content. Legacy decoding only; new actions use `SetSlideOrder`.
    MoveSlide { from: usize, to: usize, id: String },
    /// Assign one stable slide its author-computed sparse order key. Unlike a
    /// positional destination, every replica applies this exact scalar even
    /// when concurrent inserts have changed its local list order.
    SetSlideOrder { slide_id: String, rank: i64 },
    /// Set slide `slide`'s layout.
    SetSlideLayout {
        slide: usize,
        id: String,
        layout: String,
    },
    /// Set slide `slide`'s background treatment.
    SetSlideBackground {
        slide: usize,
        id: String,
        background: String,
    },
    /// Set the deck's accent theme (palette). Deck-wide, so it carries no slide
    /// index.
    SetDeckPalette { palette: String },
    /// Set the deck title (committed rename, not a per-keystroke splice).
    SetDeckTitle { title: String },
    /// Add an element (a fresh insert, a paste, a duplicate, or a picked image)
    /// to slide `slide`. The element's id is minted at authoring and carried in
    /// the payload.
    AddElement {
        slide: usize,
        slide_id: String,
        element: ElementData,
    },
    /// Delete element `id` from slide `slide`.
    DeleteElement { slide: usize, id: String },
    /// Move element `id` to its **final** top-left after a drag drop or a
    /// keyboard nudge (never the intermediate per-frame drag positions).
    MoveElement {
        slide: usize,
        id: String,
        x: f64,
        y: f64,
    },
    /// Resize (and reposition) element `id` to its **final** rect after a drag
    /// drop.
    ResizeElement {
        slide: usize,
        id: String,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    /// Restyle element `id`: write only the patch's `Some` fields.
    RestyleElement {
        slide: usize,
        id: String,
        patch: StylePatch,
    },
    /// Bring element `id` to the front of slide `slide` (z above every sibling).
    /// The new z is recomputed from the document's base state.
    RaiseElement { slide: usize, id: String },
    /// Send element `id` to the back of slide `slide` (z below every sibling).
    /// The new z is recomputed from the document's base state.
    LowerElement { slide: usize, id: String },
    /// Set an element's author-computed absolute z value, resolving both
    /// durable identities rather than a receiver-local slide position.
    SetElementZ {
        slide_id: String,
        element_id: String,
        z: i64,
    },
    /// Add a new element on top of slide `slide_id`. Unlike
    /// [`Self::AddElement`], the payload carries no id: the id is minted
    /// from the action's deterministic context ([`ActionCx`]) — the pinned
    /// actor, the heads it applies at and the payload — so the origin and
    /// every replica create the same element, and the z is one above the
    /// slide's top element in that same base document.
    CreateElement {
        slide_id: String,
        element: NewElement,
    },
    /// Edit the words of text element `element_id` on slide `slide_id`: one
    /// rich-text edit (typing, a split, a mark) applied to its body.
    EditElementText {
        slide_id: String,
        element_id: String,
        edit: tilo_document::RichTextAction,
    },
    /// Replace the words of a slide's title (its first text box) and/or body
    /// (its second), a paragraph per line. `slide` is the slide's id, or its
    /// number counting from 1 when no slide has that id.
    SetSlideText {
        slide: String,
        title: Option<String>,
        body: Option<String>,
    },
    /// Rewrite the deck's first-format text elements as rich text, in place
    /// and once (see [`SlidesDoc::upgrade`]).
    UpgradeText,
}

const MAX_DECK_SLIDES: usize = 10_000;
const MAX_ELEMENTS_PER_SLIDE: usize = 10_000;
const MAX_TEXT_CHARS: usize = 100_000;
pub const MAX_TOTAL_SLIDES: usize = 1_000;
pub const MAX_TOTAL_ELEMENTS: usize = 20_000;
pub const MAX_TOTAL_TEXT_CHARS: usize = 2_000_000;
pub const MAX_TOTAL_ASSET_REFS: usize = 2_048;

pub fn deck_within_aggregate_limits(deck: &crate::model::Deck) -> bool {
    if deck.slides.len() > MAX_TOTAL_SLIDES {
        return false;
    }
    let mut elements = 0_usize;
    let mut text = deck.title.chars().count();
    let mut assets = 0_usize;
    for slide in &deck.slides {
        elements = elements.saturating_add(slide.elements.len());
        text = text.saturating_add(slide.notes.chars().count());
        for element in &slide.elements {
            text = text.saturating_add(element.text.chars().count());
            assets = assets.saturating_add(usize::from(element.kind == ElementKind::Image));
        }
    }
    elements <= MAX_TOTAL_ELEMENTS && text <= MAX_TOTAL_TEXT_CHARS && assets <= MAX_TOTAL_ASSET_REFS
}

fn refused(reason: &str) -> ActionError {
    ActionError::Refused(reason.to_string())
}

fn exact_layout(value: &str) -> bool {
    Layout::ALL.iter().any(|layout| layout.as_str() == value)
}

fn exact_background(value: &str) -> bool {
    Background::ALL
        .iter()
        .any(|background| background.as_str() == value)
}

fn exact_palette(value: &str) -> bool {
    PaletteKind::ALL
        .iter()
        .any(|palette| palette.as_str() == value)
}

fn exact_kind(value: &str) -> bool {
    ["text", "rect", "ellipse", "line", "image"].contains(&value)
}

fn exact_align(value: &str) -> bool {
    TextAlign::ALL.iter().any(|align| align.as_str() == value)
}

/// The palette has more neutral roles than the compact inspector swatch row,
/// and custom picker colours are canonical `#RRGGBB[AA]` literals. Do not let
/// an unknown string take the renderer's permissive fallback: direct/replayed
/// actions must have the same bounded vocabulary as the UI.
fn exact_paint(value: &str, allow_none: bool) -> bool {
    (allow_none && value == "none")
        || FILL_ROLES.contains(&value)
        || matches!(
            value,
            "accent-deep" | "surface-dim" | "text" | "text-dim" | "on-accent" | "white"
        )
        || (value.starts_with('#') && ui::color::parse_hex(value).is_some())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_whitespace)
}

fn valid_geometry(kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> bool {
    x.is_finite()
        && y.is_finite()
        && w.is_finite()
        && h.is_finite()
        && (-SLIDE_W..=SLIDE_W * 2.0).contains(&x)
        && (-SLIDE_H..=SLIDE_H * 2.0).contains(&y)
        && match kind {
            // Lines are stored as the bounding rectangle of their endpoints.
            // A horizontal or vertical line deliberately has one zero extent.
            ElementKind::Line => {
                (0.0..=SLIDE_W * 2.0).contains(&w)
                    && (0.0..=SLIDE_H * 2.0).contains(&h)
                    && (w > 0.0 || h > 0.0)
            }
            _ => (1.0..=SLIDE_W * 2.0).contains(&w) && (1.0..=SLIDE_H * 2.0).contains(&h),
        }
}

fn validate_element(data: &ElementData) -> Result<(), ActionError> {
    if !valid_id(&data.id)
        || !exact_kind(&data.kind)
        || !valid_geometry(
            ElementKind::from_str(&data.kind),
            data.x,
            data.y,
            data.w,
            data.h,
        )
    {
        return Err(refused("element id, kind, or geometry is invalid"));
    }
    if data.text.chars().count() > MAX_TEXT_CHARS
        || data.blocks.len() > MAX_TEXT_CHARS
        || data
            .blocks
            .iter()
            .map(|block| block.text.chars().count())
            .sum::<usize>()
            > MAX_TEXT_CHARS
        || (data.kind != "text" && !data.blocks.is_empty())
        || data.src.len() > 4_096
        || (data.kind == "image" && !crate::assets::is_asset_id(&data.src))
        || (data.kind != "image" && !data.src.is_empty())
        || !exact_paint(&data.fill, true)
        || (!STROKE_ROLES.contains(&data.stroke.as_str()) && !exact_paint(&data.stroke, true))
        || !exact_align(&data.align)
        || !data.stroke_width.is_finite()
        || !data.font_size.is_finite()
        || !data.radius.is_finite()
        || !(0.0..=64.0).contains(&data.stroke_width)
        || !(4.0..=256.0).contains(&data.font_size)
        || !(0.0..=720.0).contains(&data.radius)
    {
        return Err(refused("element style or content is invalid"));
    }
    Ok(())
}

fn deck_for_action(doc: &Doc) -> Result<crate::model::Deck, ActionError> {
    SlidesDoc::load(doc).ok_or_else(|| refused("the shared deck is unavailable"))
}

/// [`SlidesAction::SetSlideText`]: every text is checked and every text box
/// found before the first one is rewritten.
fn set_slide_text(doc: &mut Doc, slide: &str, texts: [Option<&str>; 2]) -> Result<(), ActionError> {
    let deck = deck_for_action(doc)?;
    let target = deck
        .slides
        .iter()
        .find(|candidate| candidate.id == slide)
        .or_else(|| {
            let number = slide.parse::<usize>().ok()?;
            deck.slides.get(number.checked_sub(1)?)
        })
        .ok_or_else(|| refused("no slide has that id or number"))?;
    let boxes: Vec<&str> = target
        .elements
        .iter()
        .filter(|element| element.kind == ElementKind::Text)
        .map(|element| element.id.as_str())
        .collect();
    let mut edits = Vec::new();
    for (index, text) in texts.into_iter().enumerate() {
        let Some(text) = text else { continue };
        if text.trim().is_empty() || text.chars().count() > MAX_TEXT_CHARS {
            return Err(refused(
                "slide text must hold 1 to 100000 characters; delete-element removes a text box",
            ));
        }
        let element = boxes.get(index).ok_or_else(|| {
            refused(if index == 0 {
                "the slide has no text box for a title; add one with add-element kind text"
            } else {
                "the slide has no second text box for a body; add one with add-element kind text"
            })
        })?;
        let body = SlidesDoc::element_body(doc, &target.id, element)
            .ok_or_else(|| refused("the text box is not rich text"))?;
        edits.push((body, paragraphs_markdown(text)));
    }
    if edits.is_empty() {
        return Err(refused("give a title, a body or both"));
    }
    for (body, markdown) in edits {
        tilo_document::RichTextAction::ReplaceBody { markdown }.apply_under(doc, &body)?;
    }
    Ok(())
}

/// `text` as Markdown that imports to one plain paragraph per line: every
/// ASCII punctuation mark is escaped, so nothing in it reads as syntax.
fn paragraphs_markdown(text: &str) -> String {
    let mut markdown = String::with_capacity(text.len() * 2);
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        for ch in line.chars() {
            if ch.is_ascii_punctuation() {
                markdown.push('\\');
            }
            markdown.push(ch);
        }
    }
    markdown
}

/// Resolve a durable element by its canonical id. The positional field in the
/// serialized action is a backwards-compatible fast path, never identity: a
/// collaborator can insert a slide before this action is applied.
fn live_element<'a>(
    deck: &'a crate::model::Deck,
    slide: usize,
    id: &str,
) -> Result<(usize, &'a Element), ActionError> {
    deck.slides
        .get(slide)
        .and_then(|candidate| candidate.element(id))
        .map(|element| (slide, element))
        .or_else(|| {
            deck.slides
                .iter()
                .enumerate()
                .find_map(|(index, candidate)| {
                    candidate.element(id).map(|element| (index, element))
                })
        })
        .ok_or_else(|| refused("the target element no longer exists"))
}

/// Resolve a slide by stable resource id, using its serialized index only as a
/// fast path. This prevents a remote insert/reorder from redirecting a local
/// mutation to whichever slide has inherited the old numeric position.
fn live_slide(deck: &crate::model::Deck, index: usize, id: &str) -> Result<usize, ActionError> {
    deck.slides
        .get(index)
        .filter(|slide| slide.id == id)
        .map(|_| index)
        .or_else(|| deck.slides.iter().position(|slide| slide.id == id))
        .ok_or_else(|| refused("the target slide no longer exists"))
}

/// Refuse a style patch any field of which is outside the vocabulary the
/// inspector itself can produce.
fn validate_patch(patch: &StylePatch) -> Result<(), ActionError> {
    if patch
        .fill
        .as_deref()
        .is_some_and(|value| !exact_paint(value, true))
        || patch
            .stroke
            .as_deref()
            .is_some_and(|value| !STROKE_ROLES.contains(&value) && !exact_paint(value, true))
        || patch
            .align
            .as_deref()
            .is_some_and(|value| !exact_align(value))
        || patch
            .font_size
            .is_some_and(|value| !value.is_finite() || !(4.0..=256.0).contains(&value))
        || patch
            .stroke_width
            .is_some_and(|value| !value.is_finite() || !(0.0..=64.0).contains(&value))
        || patch
            .radius
            .is_some_and(|value| !value.is_finite() || !(0.0..=720.0).contains(&value))
    {
        return Err(refused("element style patch is invalid"));
    }
    Ok(())
}

/// Write `patch`'s fields onto an element that is not yet in the document.
fn patched(mut element: Element, patch: &StylePatch) -> Element {
    if let Some(v) = &patch.fill {
        element.fill.clone_from(v);
    }
    if let Some(v) = &patch.stroke {
        element.stroke.clone_from(v);
    }
    if let Some(v) = &patch.align {
        element.align = TextAlign::from_str(v);
    }
    if let Some(v) = patch.font_size {
        element.font_size = v;
    }
    if let Some(v) = patch.stroke_width {
        element.stroke_width = v;
    }
    if let Some(v) = patch.radius {
        element.radius = v;
    }
    if let Some(v) = patch.bold {
        element.bold = v;
    }
    if let Some(v) = patch.italic {
        element.italic = v;
    }
    element
}

/// Add `element` to slide `index` once it passes every check an insert must:
/// room on the slide, a valid element, an id not already in the deck, and
/// the deck's aggregate limits. Nothing is written when a check fails.
fn add_element_checked(
    doc: &mut Doc,
    deck: crate::model::Deck,
    index: usize,
    element: &ElementData,
) -> Result<(), ActionError> {
    let target = deck
        .slides
        .get(index)
        .ok_or_else(|| refused("the target slide no longer exists"))?;
    if target.elements.len() >= MAX_ELEMENTS_PER_SLIDE {
        return Err(refused("the target slide has too many elements"));
    }
    validate_element(element)?;
    if deck
        .slides
        .iter()
        .any(|existing| existing.elements.iter().any(|live| live.id == element.id))
    {
        return Err(refused("element id already exists"));
    }
    let mut projected = deck;
    let target = projected
        .slides
        .get_mut(index)
        .ok_or_else(|| refused("the target slide no longer exists"))?;
    target.elements.push(element.to_element());
    if !deck_within_aggregate_limits(&projected) {
        return Err(refused("deck aggregate limit would be exceeded"));
    }
    SlidesDoc::add_element(doc, index, &element.to_element());
    Ok(())
}

impl SlidesAction {
    /// The element a [`Self::CreateElement`] adds to slide `index`: its
    /// kind's defaults with the author's overrides, an id from the action's
    /// deterministic context, and a z above the slide's top element.
    fn created_element(
        &self,
        doc: &mut Doc,
        deck: &crate::model::Deck,
        index: usize,
        element: &NewElement,
    ) -> Result<ElementData, ActionError> {
        if !exact_kind(&element.kind) {
            return Err(refused("element kind is invalid"));
        }
        validate_patch(&element.style)?;
        let slide = deck
            .slides
            .get(index)
            .ok_or_else(|| refused("the target slide no longer exists"))?;
        let z = slide
            .elements
            .iter()
            .map(|existing| existing.z)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| refused("the slide has no room above its top element"))?;
        let payload = serde_json::to_vec(self).map_err(ActionError::Encode)?;
        let mut cx = ActionCx::for_apply(doc, &payload, 0);
        let mut built = patched(
            Element::of_kind(
                cx.id().as_str().to_owned(),
                ElementKind::from_str(&element.kind),
                element.x,
                element.y,
                element.w,
                element.h,
            ),
            &element.style,
        );
        built.text.clone_from(&element.text);
        built.src.clone_from(&element.src);
        built.z = z;
        Ok(ElementData::from_element(&built))
    }
}

impl Action for SlidesAction {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive action match is the atomic validation boundary: every branch validates its complete payload before it can write the shared deck"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Typing reaches only the element it edits: no deck read per keystroke.
        match self {
            SlidesAction::EditElementText {
                slide_id,
                element_id,
                edit,
            } => {
                let body = SlidesDoc::element_body(doc, slide_id, element_id)
                    .ok_or_else(|| refused("the text element no longer exists"))?;
                return edit.apply_under(doc, &body);
            }
            SlidesAction::UpgradeText => {
                if !SlidesDoc::upgrade(doc) {
                    return Err(refused("the deck's text is already rich text"));
                }
                return Ok(());
            }
            SlidesAction::SetSlideText { slide, title, body } => {
                return set_slide_text(doc, slide, [title.as_deref(), body.as_deref()]);
            }
            _ => {}
        }
        let deck = deck_for_action(doc)?;
        match self {
            SlidesAction::EditElementText { .. }
            | SlidesAction::SetSlideText { .. }
            | SlidesAction::UpgradeText => {}
            SlidesAction::InsertSlide { at, slide } => {
                if *at > deck.slides.len() || deck.slides.len() >= MAX_DECK_SLIDES {
                    return Err(refused("slide insertion position or deck size is invalid"));
                }
                if !valid_id(&slide.id)
                    || !exact_layout(&slide.layout)
                    || !exact_background(&slide.background)
                    || slide.notes.chars().count() > MAX_TEXT_CHARS
                    || slide.elements.len() > MAX_ELEMENTS_PER_SLIDE
                    || deck.slides.iter().any(|existing| existing.id == slide.id)
                {
                    return Err(refused("new slide data is invalid or stale"));
                }
                let mut ids = std::collections::BTreeSet::new();
                for element in &slide.elements {
                    validate_element(element)?;
                    if !ids.insert(&element.id)
                        || deck.slides.iter().any(|existing| {
                            existing.elements.iter().any(|live| live.id == element.id)
                        })
                    {
                        return Err(refused("new element id already exists"));
                    }
                }
                let mut projected = deck;
                projected.slides.insert(*at, slide.to_slide());
                if !deck_within_aggregate_limits(&projected) {
                    return Err(refused("deck aggregate limit would be exceeded"));
                }
                SlidesDoc::insert_slide(doc, *at, &slide.to_slide());
            }
            SlidesAction::InsertSlideWithOrder { slide, rank } => {
                if deck.slides.len() >= MAX_DECK_SLIDES
                    || !valid_id(&slide.id)
                    || !exact_layout(&slide.layout)
                    || !exact_background(&slide.background)
                    || slide.notes.chars().count() > MAX_TEXT_CHARS
                    || slide.elements.len() > MAX_ELEMENTS_PER_SLIDE
                    || deck.slides.iter().any(|existing| existing.id == slide.id)
                {
                    return Err(refused("new slide data is invalid or stale"));
                }
                let mut ids = std::collections::BTreeSet::new();
                for element in &slide.elements {
                    validate_element(element)?;
                    if !ids.insert(&element.id)
                        || deck.slides.iter().any(|existing| {
                            existing.elements.iter().any(|live| live.id == element.id)
                        })
                    {
                        return Err(refused("new element id already exists"));
                    }
                }
                let mut projected = deck;
                projected.slides.push(slide.to_slide());
                if !deck_within_aggregate_limits(&projected) {
                    return Err(refused("deck aggregate limit would be exceeded"));
                }
                SlidesDoc::insert_slide_with_order(doc, &slide.to_slide(), *rank);
            }
            SlidesAction::DeleteSlide { idx, id } => {
                if deck.slides.len() <= 1 {
                    return Err(refused("the target slide cannot be deleted"));
                }
                let index = live_slide(&deck, *idx, id)
                    .map_err(|_| refused("the target slide cannot be deleted"))?;
                SlidesDoc::remove_slide(doc, index);
            }
            SlidesAction::MoveSlide { from, to, id } => {
                let source = live_slide(&deck, *from, id)?;
                if *to >= deck.slides.len() || source == *to {
                    return Err(refused("slide move source or destination is invalid"));
                }
                SlidesDoc::move_slide(doc, source, *to);
            }
            SlidesAction::SetSlideOrder { slide_id, rank } => {
                if !deck.slides.iter().any(|slide| slide.id == *slide_id)
                    || !SlidesDoc::set_slide_order(doc, slide_id, *rank)
                {
                    return Err(refused("the target slide no longer exists"));
                }
            }
            SlidesAction::SetSlideLayout { slide, id, layout } => {
                if !exact_layout(layout) {
                    return Err(refused("slide or layout is invalid"));
                }
                let index = live_slide(&deck, *slide, id)?;
                SlidesDoc::set_slide_layout(doc, index, Layout::from_str(layout));
            }
            SlidesAction::SetSlideBackground {
                slide,
                id,
                background,
            } => {
                if !exact_background(background) {
                    return Err(refused("slide or background is invalid"));
                }
                let index = live_slide(&deck, *slide, id)?;
                SlidesDoc::set_slide_background(doc, index, Background::from_str(background));
            }
            SlidesAction::SetDeckPalette { palette } => {
                if !exact_palette(palette) {
                    return Err(refused("deck palette is invalid"));
                }
                SlidesDoc::set_palette(doc, PaletteKind::from_str(palette));
            }
            SlidesAction::SetDeckTitle { title } => {
                if title.chars().count() > MAX_TEXT_CHARS {
                    return Err(refused("deck title is too long"));
                }
                SlidesDoc::set_title(doc, title);
            }
            SlidesAction::AddElement {
                slide,
                slide_id,
                element,
            } => {
                let index = live_slide(&deck, *slide, slide_id)?;
                add_element_checked(doc, deck, index, element)?;
            }
            SlidesAction::CreateElement { slide_id, element } => {
                let index = deck
                    .slides
                    .iter()
                    .position(|slide| slide.id == *slide_id)
                    .ok_or_else(|| refused("the target slide no longer exists"))?;
                let data = self.created_element(doc, &deck, index, element)?;
                add_element_checked(doc, deck, index, &data)?;
            }
            SlidesAction::DeleteElement { slide, id } => {
                let (index, _) = live_element(&deck, *slide, id)?;
                SlidesDoc::remove_element(doc, index, id);
            }
            SlidesAction::MoveElement { slide, id, x, y } => {
                let (index, current) = live_element(&deck, *slide, id)?;
                if !valid_geometry(current.kind, *x, *y, current.w, current.h) {
                    return Err(refused("element position is invalid"));
                }
                SlidesDoc::set_element_pos(doc, index, id, *x, *y);
            }
            SlidesAction::ResizeElement {
                slide,
                id,
                x,
                y,
                w,
                h,
            } => {
                let (index, current) = live_element(&deck, *slide, id)?;
                if !valid_geometry(current.kind, *x, *y, *w, *h) {
                    return Err(refused("element rectangle is invalid"));
                }
                SlidesDoc::set_element_rect(doc, index, id, *x, *y, *w, *h);
            }
            SlidesAction::RestyleElement { slide, id, patch } => {
                let (index, _) = live_element(&deck, *slide, id)?;
                if patch == &StylePatch::default() {
                    return Err(refused("element style patch is invalid"));
                }
                validate_patch(patch)?;
                // Fixed field order so the emitted change is byte-identical on
                // every replica.
                if let Some(v) = &patch.fill {
                    SlidesDoc::set_element_str(doc, index, id, "fill", v);
                }
                if let Some(v) = &patch.stroke {
                    SlidesDoc::set_element_str(doc, index, id, "stroke", v);
                }
                if let Some(v) = &patch.align {
                    SlidesDoc::set_element_str(doc, index, id, "align", v);
                }
                if let Some(v) = patch.font_size {
                    SlidesDoc::set_element_f64(doc, index, id, "font_size", v);
                }
                if let Some(v) = patch.stroke_width {
                    SlidesDoc::set_element_f64(doc, index, id, "stroke_width", v);
                }
                if let Some(v) = patch.radius {
                    SlidesDoc::set_element_f64(doc, index, id, "radius", v);
                }
                // A text box's bold and italic are marks over all its words.
                if let Some(v) = patch.bold
                    && !SlidesDoc::mark_whole_body(doc, index, id, "strong", v)
                {
                    SlidesDoc::set_element_bool(doc, index, id, "bold", v);
                }
                if let Some(v) = patch.italic
                    && !SlidesDoc::mark_whole_body(doc, index, id, "em", v)
                {
                    SlidesDoc::set_element_bool(doc, index, id, "italic", v);
                }
            }
            SlidesAction::RaiseElement { slide, id } => {
                let (index, _) = live_element(&deck, *slide, id)?;
                let z =
                    restack_z(doc, index, true).ok_or_else(|| refused("cannot raise element"))?;
                SlidesDoc::set_element_z(doc, index, id, z);
            }
            SlidesAction::LowerElement { slide, id } => {
                let (index, _) = live_element(&deck, *slide, id)?;
                let z =
                    restack_z(doc, index, false).ok_or_else(|| refused("cannot lower element"))?;
                SlidesDoc::set_element_z(doc, index, id, z);
            }
            SlidesAction::SetElementZ {
                slide_id,
                element_id,
                z,
            } => {
                let Some(index) = deck.slides.iter().position(|slide| slide.id == *slide_id) else {
                    return Err(refused("the target slide no longer exists"));
                };
                let Some(target_slide) = deck.slides.get(index) else {
                    return Err(refused("the target slide no longer exists"));
                };
                if target_slide.element(element_id).is_none() {
                    return Err(refused("the target element no longer exists"));
                }
                SlidesDoc::set_element_z(doc, index, element_id, *z);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    /// Deleting a slide or an element is destructive, exactly as the
    /// declared delete actions are, so the same value submitted through the
    /// generic `apply` or inside a `group` asks for the same confirmation.
    fn risk(&self) -> RiskLevel {
        match self {
            SlidesAction::DeleteSlide { .. } | SlidesAction::DeleteElement { .. } => {
                RiskLevel::Destructive
            }
            _ => RiskLevel::Low,
        }
    }
}

/// The z one above (or below) every element on `slide`, read from the
/// document's base state. Returns `None` when the slide has no elements. The
/// z-values are gathered and reduced order-independently (max / min), so a
/// `HashMap`/list iteration order can never make two replicas diverge.
fn restack_z(doc: &Doc, slide: usize, front: bool) -> Option<i64> {
    let deck = SlidesDoc::load(doc)?;
    let s = deck.slides.get(slide)?;
    if s.elements.is_empty() {
        return None;
    }
    let mut zs: Vec<i64> = s.elements.iter().map(|e| e.z).collect();
    zs.sort_unstable();
    if front {
        zs.last().copied()?.checked_add(1)
    } else {
        zs.first().copied()?.checked_sub(1)
    }
}

/// Compute a z scalar once at the authoring replica, resolving the durable
/// slide and element ids before serializing the absolute arrange action.
pub fn restack_z_for_author(
    doc: &AutoCommit,
    slide_id: &str,
    element_id: &str,
    front: bool,
) -> Option<i64> {
    let deck = SlidesDoc::load(doc)?;
    let slide = deck.slides.iter().find(|slide| slide.id == slide_id)?;
    slide.element(element_id)?;
    let edge = if front {
        slide.elements.iter().map(|element| element.z).max()
    } else {
        slide.elements.iter().map(|element| element.z).min()
    }?;
    if front {
        edge.checked_add(1)
    } else {
        edge.checked_sub(1)
    }
}

/// Register Slides' action decoders for palette execution + remote replay:
/// every [`SlidesAction`], and a group of them applied as one step.
pub fn register(registry: &mut ActionRegistry) {
    ui::tilo_resource::group::register::<SlidesDeck>(registry);
}

/// Encode any [`SlidesAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Slides action into a
/// replayable envelope.
///
/// # Panics
///
/// Never in practice: [`SlidesAction`] is a plain derived-`Serialize` enum
/// with no non-serializable fields, so encoding it cannot fail.
#[allow(
    clippy::expect_used,
    reason = "SlidesAction is a plain derived-Serialize enum, so encoding cannot fail; an un-encodable action has no replayable form to fall back to"
)]
pub fn encode(action: &SlidesAction) -> EncodedAction {
    EncodedAction::encode(SLIDES_ACTION_TYPE, action).expect("SlidesAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Insert a fully-specified slide at `at` (a blank create or a duplicate; the
/// caller mints the ids in `slide`).
pub fn insert_slide_action(at: usize, slide: &Slide) -> SlidesAction {
    SlidesAction::InsertSlide {
        at,
        slide: SlideData::from_slide(slide),
    }
}

/// Build the canonical absolute insertion action after rank calculation at the
/// authoring replica. `InsertSlide` remains available only to decode history
/// written by earlier app versions.
pub fn insert_slide_with_order_action(slide: &Slide, rank: i64) -> SlidesAction {
    SlidesAction::InsertSlideWithOrder {
        slide: SlideData::from_slide(slide),
        rank,
    }
}

pub fn delete_slide_action(idx: usize, id: impl Into<String>) -> SlidesAction {
    SlidesAction::DeleteSlide { idx, id: id.into() }
}

pub fn move_slide_action(from: usize, to: usize, id: impl Into<String>) -> SlidesAction {
    SlidesAction::MoveSlide {
        from,
        to,
        id: id.into(),
    }
}

/// Build the canonical, absolute reorder operation after the UI has computed
/// its destination rank against the authored document snapshot.
pub fn set_slide_order_action(slide_id: impl Into<String>, rank: i64) -> SlidesAction {
    SlidesAction::SetSlideOrder {
        slide_id: slide_id.into(),
        rank,
    }
}

pub fn set_slide_layout_action(
    slide: usize,
    id: impl Into<String>,
    layout: Layout,
) -> SlidesAction {
    SlidesAction::SetSlideLayout {
        slide,
        id: id.into(),
        layout: layout.as_str().to_string(),
    }
}

pub fn set_slide_background_action(
    slide: usize,
    id: impl Into<String>,
    background: Background,
) -> SlidesAction {
    SlidesAction::SetSlideBackground {
        slide,
        id: id.into(),
        background: background.as_str().to_string(),
    }
}

pub fn set_deck_palette_action(palette: PaletteKind) -> SlidesAction {
    SlidesAction::SetDeckPalette {
        palette: palette.as_str().to_string(),
    }
}

pub fn set_deck_title_action(title: impl Into<String>) -> SlidesAction {
    SlidesAction::SetDeckTitle {
        title: title.into(),
    }
}

pub fn add_element_action(
    slide: usize,
    slide_id: impl Into<String>,
    element: &Element,
) -> SlidesAction {
    SlidesAction::AddElement {
        slide,
        slide_id: slide_id.into(),
        element: ElementData::from_element(element),
    }
}

pub fn delete_element_action(slide: usize, id: impl Into<String>) -> SlidesAction {
    SlidesAction::DeleteElement {
        slide,
        id: id.into(),
    }
}

pub fn move_element_action(slide: usize, id: impl Into<String>, x: f64, y: f64) -> SlidesAction {
    SlidesAction::MoveElement {
        slide,
        id: id.into(),
        x,
        y,
    }
}

pub fn resize_element_action(
    slide: usize,
    id: impl Into<String>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> SlidesAction {
    SlidesAction::ResizeElement {
        slide,
        id: id.into(),
        x,
        y,
        w,
        h,
    }
}

pub fn restyle_element_action(
    slide: usize,
    id: impl Into<String>,
    patch: StylePatch,
) -> SlidesAction {
    SlidesAction::RestyleElement {
        slide,
        id: id.into(),
        patch,
    }
}

/// Restyle one string property (`"fill"`, `"stroke"`, `"align"`) — the inspector
/// swatch / alignment affordances.
pub fn restyle_str_action(slide: usize, id: impl Into<String>, key: &str, v: &str) -> SlidesAction {
    let mut patch = StylePatch::default();
    match key {
        "fill" => patch.fill = Some(v.to_string()),
        "stroke" => patch.stroke = Some(v.to_string()),
        "align" => patch.align = Some(v.to_string()),
        _ => {}
    }
    restyle_element_action(slide, id, patch)
}

/// Restyle one f64 property (`"font_size"`, `"stroke_width"`, `"radius"`).
pub fn restyle_f64_action(slide: usize, id: impl Into<String>, key: &str, v: f64) -> SlidesAction {
    let mut patch = StylePatch::default();
    match key {
        "font_size" => patch.font_size = Some(v),
        "stroke_width" => patch.stroke_width = Some(v),
        "radius" => patch.radius = Some(v),
        _ => {}
    }
    restyle_element_action(slide, id, patch)
}

/// Restyle one bool property (`"bold"`, `"italic"`).
pub fn restyle_bool_action(
    slide: usize,
    id: impl Into<String>,
    key: &str,
    v: bool,
) -> SlidesAction {
    let mut patch = StylePatch::default();
    match key {
        "bold" => patch.bold = Some(v),
        "italic" => patch.italic = Some(v),
        _ => {}
    }
    restyle_element_action(slide, id, patch)
}

pub fn raise_element_action(slide: usize, id: impl Into<String>) -> SlidesAction {
    SlidesAction::RaiseElement {
        slide,
        id: id.into(),
    }
}

pub fn lower_element_action(slide: usize, id: impl Into<String>) -> SlidesAction {
    SlidesAction::LowerElement {
        slide,
        id: id.into(),
    }
}

pub fn set_element_z_action(
    slide_id: impl Into<String>,
    element_id: impl Into<String>,
    z: i64,
) -> SlidesAction {
    SlidesAction::SetElementZ {
        slide_id: slide_id.into(),
        element_id: element_id.into(),
        z,
    }
}

/// The template for a freshly-created slide, shared by the toolbar's "New slide"
/// button and the palette's "Add slide" so both insert the byte-identical
/// structure. Fresh ids are minted on each call (a new slide is genuinely new).
pub fn new_slide_template() -> Slide {
    let mut s = Slide::new(Layout::TitleContent);
    s.add(Element::text("New slide", 96.0, 90.0, 1088.0, 100.0, 54.0).with(|e| e.bold = true));
    s.add(
        Element::shape(ElementKind::Rect, 96.0, 188.0, 150.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }),
    );
    s.add(
        Element::text("Add your content…", 96.0, 230.0, 900.0, 300.0, 32.0).with(|e| {
            e.fill = "text-dim".into();
        }),
    );
    s
}

/// A new slide with its words already in place: the starter's cover for
/// `layout: title`, else [`new_slide_template`]. Omitted text keeps the
/// template's placeholder.
pub fn authored_slide(title: Option<&str>, body: Option<&str>, layout: Option<&str>) -> Slide {
    let cover = (layout == Some("title"))
        .then(|| crate::model::starter_deck().slides.into_iter().next())
        .flatten();
    let mut slide = cover.unwrap_or_else(new_slide_template);
    let texts = slide
        .elements
        .iter_mut()
        .filter(|element| element.kind == ElementKind::Text);
    for (index, (element, text)) in texts.zip([title, body]).enumerate() {
        let Some(text) = text else { continue };
        element.text = text.to_owned();
        element.blocks.clear();
        if index == 1 && layout != Some("title") {
            // The template's body is a dimmed placeholder; real content is not.
            element.fill = "text".into();
        }
    }
    slide
}

/// The element the toolbar's insert tools add, before the user moves or
/// styles it.
pub fn inserted_element_template(kind: ElementKind) -> Element {
    match kind {
        ElementKind::Text => Element::text("Text", 440.0, 320.0, 400.0, 90.0, 40.0),
        ElementKind::Line => Element::shape(ElementKind::Line, 440.0, 360.0, 360.0, 0.0),
        ElementKind::Image => Element::shape(ElementKind::Image, 440.0, 220.0, 400.0, 280.0),
        other => Element::shape(other, 480.0, 260.0, 320.0, 220.0),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One element surfaced as a resource: its id, kind (for a label) and body text
/// (for a label snippet). Derived from the deck the app already has open.
#[derive(Debug, Clone)]
pub struct ElementResource {
    pub id: String,
    pub kind: ElementKind,
    pub text: String,
    pub z: i64,
}

/// One slide surfaced as a resource: its index, a snapshot for the duplicate
/// action, its current layout/background, and its elements.
#[derive(Debug, Clone)]
pub struct SlideResource {
    pub idx: usize,
    pub data: SlideData,
    pub elements: Vec<ElementResource>,
}

/// The current live selection. Published beside the framework root when hosted
/// — never a second copy of the deck document door. Its actions are bound to
/// the peer's selected element (or current slide when nothing is selected).
#[derive(Clone)]
pub struct SelectionResourceCtx {
    pub slide_id: String,
    pub element_id: Option<String>,
    pub label: String,
    pub actions: Vec<ActionSpec>,
}

/// The presentation context Slides hands [`provide_resources`]: the deck title,
/// the slides it currently has open (each with its elements), and the peer's
/// live selection. Deck rows come from the document; selection is local.
#[derive(Clone)]
pub struct SlidesResourceCtx {
    pub deck_title: String,
    pub slides: Vec<SlideResource>,
    /// Per-peer current slide / selected element. Not collaborative document
    /// state — rides [`live_presentation_resources`] when hosted.
    pub selection: Option<SelectionResourceCtx>,
}

impl SlidesResourceCtx {
    /// The context for `deck` as it stands: each slide in deck order, each
    /// slide's elements back to front. Selection is left unset — callers that
    /// know the peer's focus fill it in (see [`crate::app::SlidesApp::slides_resource_ctx`]).
    pub fn from_deck(deck: &crate::model::Deck) -> Self {
        Self {
            deck_title: deck.title.clone(),
            slides: deck
                .slides
                .iter()
                .enumerate()
                .map(|(idx, slide)| SlideResource {
                    idx,
                    data: SlideData::from_slide(slide),
                    elements: slide
                        .z_sorted()
                        .into_iter()
                        .map(|element| ElementResource {
                            id: element.id.clone(),
                            kind: element.kind,
                            text: element.text.clone(),
                            z: element.z,
                        })
                        .collect(),
                })
                .collect(),
            selection: None,
        }
    }
}

// ===========================================================================
// The deck as a resource
// ===========================================================================

/// The deck as a [`ui::tilo_resource::Resource`]: [`SlidesAction`] is its
/// replayable vocabulary. Declarations are the typed element/slide ops an
/// agent, a script or the palette calls by name; bound actions are add-slide
/// and theme-* (payload from the live deck). Publishing it
/// ([`provide_resources`] / [`ui::tilo_resource::publish`]) also offers the
/// generic [`APPLY`](ui::tilo_resource::APPLY) and
/// [`GROUP`](ui::tilo_resource::GROUP) every resource has.
pub struct SlidesDeck;

/// Declared operation ids, as published on the `slides.deck` resource.
pub const ADD_ELEMENT: &str = "add-element";
pub const MOVE_ELEMENT: &str = "move-element";
pub const RESTYLE_ELEMENT: &str = "restyle-element";
pub const DELETE_ELEMENT: &str = "delete-element";
pub const MOVE_SLIDE: &str = "move-slide";
pub const ADD_SLIDE: &str = "add-slide";
pub const SET_SLIDE_TEXT: &str = "set-slide-text";

impl ui::tilo_resource::Resource for SlidesDeck {
    const KIND: &'static str = "slides.deck";
    const LABEL: &'static str = "Deck";
    const ACTION_TYPE: &'static str = SLIDES_ACTION_TYPE;

    type State = SlidesResourceCtx;
    type Action = SlidesAction;

    fn read(doc: &Doc) -> Result<SlidesResourceCtx, ui::tilo_resource::ModelError> {
        SlidesDoc::load(doc)
            .map(|deck| SlidesResourceCtx::from_deck(&deck))
            .ok_or_else(|| ui::tilo_resource::ModelError::invalid("the document holds no deck"))
    }

    fn initialize(doc: &mut Doc) -> Result<(), ui::tilo_resource::ModelError> {
        if SlidesDoc::is_uninitialized(doc) {
            SlidesDoc::seed(doc, &crate::model::starter_deck());
        }
        Ok(())
    }

    fn local_operations() -> Vec<ActionSpec> {
        deck_local_operations()
    }

    fn bound_actions(state: &SlidesResourceCtx) -> Vec<ActionSpec> {
        deck_bound_document_actions(state)
    }

    fn coalesces(previous: &SlidesAction, next: &SlidesAction) -> bool {
        matches!(
            (previous, next),
            (
                SlidesAction::SetDeckTitle { .. },
                SlidesAction::SetDeckTitle { .. }
            )
        )
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let mut add_params = vec![
            slide_param(),
            ActionParam::required("kind", ParamType::Text, "What to add")
                .with_choices(["text", "rect", "ellipse", "line", "image"]),
        ];
        add_params.extend(rect_params(true));
        add_params.extend([
            ActionParam::optional(
                "text",
                ParamType::Text,
                "Body of a text element; lines are separated by \\n",
            ),
            ActionParam::optional(
                "src",
                ParamType::Text,
                "Content id (`asset:blake3:<hash>`) of an image element's picture",
            ),
        ]);
        add_params.extend(style_params());

        let mut move_params = vec![element_param()];
        move_params.extend(rect_params(false));

        let mut restyle_params = vec![element_param()];
        restyle_params.extend(style_params());

        vec![
            ActionDecl::new(ADD_ELEMENT, "Add element", add_params, |args, _| {
                build_add_element(args)
            })
            .describe(
                "Add a text box, shape, line or image to a slide, above everything on it. \
                 The new element's id is chosen when the edit applies; read the deck again to \
                 find it.",
            ),
            ActionDecl::new(
                MOVE_ELEMENT,
                "Move or resize element",
                move_params,
                |args, _| build_move_element(args),
            )
            .describe(
                "Move an element so its top-left corner is at (x, y); give w and h as well to \
                 resize it. Slide units: the slide is 1280 × 720.",
            ),
            ActionDecl::new(
                RESTYLE_ELEMENT,
                "Restyle element",
                restyle_params,
                |args, _| {
                    let id = element_id_arg(args)?;
                    let patch = style_patch(args)?;
                    if patch == StylePatch::default() {
                        return Err("name at least one style to change".into());
                    }
                    Ok(restyle_element_action(0, id, patch))
                },
            )
            .describe(
                "Change an element's colours (fill, text colour, outline), font size, \
                 alignment, weight, slant or corner radius. Only the styles given change.",
            ),
            ActionDecl::new(
                DELETE_ELEMENT,
                "Delete element",
                vec![element_param()],
                |args, _| Ok(delete_element_action(0, element_id_arg(args)?)),
            )
            .describe("Delete an element from its slide.")
            .destructive(),
            ActionDecl::new(
                MOVE_SLIDE,
                "Move slide",
                vec![
                    slide_param(),
                    ActionParam::required(
                        "position",
                        ParamType::Integer,
                        "Where the slide ends up, counting from 0 for the first slide",
                    ),
                ],
                |args, _| {
                    let id = slide_id_arg(args)?;
                    let position = args
                        .integer("position")
                        .and_then(|position| usize::try_from(position).ok())
                        .ok_or("`position` must be 0 or more")?;
                    Ok(move_slide_action(0, position, id))
                },
            )
            .describe("Reorder the deck: move a slide to another position."),
            ActionDecl::new(
                SET_SLIDE_TEXT,
                "Set slide text",
                vec![
                    ActionParam::required(
                        "slide",
                        ParamType::Text,
                        "Slide id, or its number as `read` shows it (1 for the first slide)",
                    ),
                    ActionParam::optional("title", ParamType::Text, "New title"),
                    ActionParam::optional(
                        "body",
                        ParamType::Text,
                        "New body, a paragraph per line (\\n)",
                    ),
                ],
                |args, _| {
                    let slide = args.text("slide").ok_or("name the `slide`")?;
                    let slide = slide.strip_prefix("slides.slide:").unwrap_or(slide);
                    let title = args.text("title").map(str::to_owned);
                    let body = args.text("body").map(str::to_owned);
                    if title.is_none() && body.is_none() {
                        return Err("give a `title`, a `body` or both".into());
                    }
                    Ok(SlidesAction::SetSlideText {
                        slide: slide.to_owned(),
                        title,
                        body,
                    })
                },
            )
            .describe(
                "Replace the words of a slide's title (its first text box) and/or body (its \
                 second) as plain text, keeping their place and style.",
            ),
        ]
    }

    /// The slides, each with its elements, text and notes, as nested
    /// resources exporting their actions with arguments already bound.
    fn children(state: &SlidesResourceCtx) -> Vec<Resource> {
        state
            .slides
            .iter()
            .take(MAX_EAGER_SLIDES)
            .map(|slide| slide_resource(slide, true, state))
            .collect()
    }
}

// The positional fields of the element and slide actions a declaration
// builds are only a lookup hint (see `live_element` / `live_slide`): the
// stable id is what the action targets, so a declaration passes slot 0 and
// the id resolves wherever the target is when the action applies.

fn slide_param() -> ActionParam {
    ActionParam::required(
        "slide",
        ParamType::Text,
        "Slide id (or its `slides.slide:<id>` resource id)",
    )
}

fn element_param() -> ActionParam {
    ActionParam::required(
        "element",
        ParamType::Text,
        "Element id (or its `slides.element:<id>` resource id)",
    )
}

fn rect_params(size_required: bool) -> Vec<ActionParam> {
    let size = |name: &'static str, description: &'static str| {
        if size_required {
            ActionParam::required(name, ParamType::Number, description)
        } else {
            ActionParam::optional(name, ParamType::Number, description)
        }
    };
    vec![
        ActionParam::required(
            "x",
            ParamType::Number,
            "Left edge, in slide units (the slide is 1280 wide)",
        ),
        ActionParam::required(
            "y",
            ParamType::Number,
            "Top edge, in slide units (the slide is 720 high)",
        ),
        size("w", "Width, in slide units"),
        size("h", "Height, in slide units"),
    ]
}

fn style_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "fill",
            ParamType::Text,
            "Fill colour, and a text element's text colour: a palette role (accent, \
             accent-soft, accent-deep, surface, surface-dim, deep, text, text-dim, on-accent, \
             white, success, warning, danger, info), `#RRGGBB`, or `none`. `text` and \
             `text-dim` follow the slide's background so they stay legible on it",
        ),
        ActionParam::optional(
            "text_color",
            ParamType::Text,
            "Text colour of a text element — the same value as `fill`, by its text name",
        ),
        ActionParam::optional(
            "stroke",
            ParamType::Text,
            "Outline colour: `none`, a palette role or `#RRGGBB`",
        ),
        ActionParam::optional(
            "stroke_width",
            ParamType::Number,
            "Outline width in slide units, 0 to 64",
        ),
        ActionParam::optional(
            "font_size",
            ParamType::Number,
            "Font size in slide units, 4 to 256",
        ),
        ActionParam::optional("align", ParamType::Text, "Text alignment")
            .with_choices(TextAlign::ALL.map(TextAlign::as_str)),
        ActionParam::optional("bold", ParamType::Boolean, "Bold text"),
        ActionParam::optional("italic", ParamType::Boolean, "Italic text"),
        ActionParam::optional(
            "radius",
            ParamType::Number,
            "Corner radius of a rectangle, in slide units, 0 to 720",
        ),
    ]
}

/// A durable id from argument `name`, accepting the bare id or its resource
/// id (`<prefix><id>`), since both are what a reader of the tree sees.
fn id_arg(args: &ActionArgs, name: &str, prefixes: &[&str]) -> Result<String, String> {
    let raw = args
        .text(name)
        .map(str::trim)
        .ok_or_else(|| format!("`{name}` is required"))?;
    let id = prefixes
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix))
        .unwrap_or(raw);
    if valid_id(id) {
        Ok(id.to_owned())
    } else {
        Err(format!("`{name}` is not an id"))
    }
}

fn slide_id_arg(args: &ActionArgs) -> Result<String, String> {
    id_arg(args, "slide", &["slides.slide:"])
}

fn element_id_arg(args: &ActionArgs) -> Result<String, String> {
    id_arg(args, "element", &["slides.element:", "slides.text:"])
}

fn style_patch(args: &ActionArgs) -> Result<StylePatch, String> {
    let fill = match (args.text("fill"), args.text("text_color")) {
        (Some(fill), Some(color)) if fill != color => {
            return Err("give `fill` or `text_color`, not two different colours".into());
        }
        (fill, color) => fill.or(color).map(str::to_owned),
    };
    let patch = StylePatch {
        fill,
        stroke: args.text("stroke").map(str::to_owned),
        align: args.text("align").map(str::to_owned),
        font_size: args.number("font_size"),
        stroke_width: args.number("stroke_width"),
        radius: args.number("radius"),
        bold: args.boolean("bold"),
        italic: args.boolean("italic"),
    };
    validate_patch(&patch).map_err(|error| error.to_string())?;
    Ok(patch)
}

fn required_number(args: &ActionArgs, name: &str) -> Result<f64, String> {
    args.number(name)
        .ok_or_else(|| format!("`{name}` is required"))
}

fn build_add_element(args: &ActionArgs) -> Result<SlidesAction, String> {
    let kind = args.text("kind").ok_or("`kind` is required")?.to_owned();
    let text = args.text("text").unwrap_or_default().to_owned();
    let src = args.text("src").unwrap_or_default().to_owned();
    if !text.is_empty() && kind != "text" {
        return Err("only a text element takes `text`".into());
    }
    if kind == "image" && !crate::assets::is_asset_id(&src) {
        return Err("an image needs `src`, the content id of its picture".into());
    }
    if kind != "image" && !src.is_empty() {
        return Err("only an image element takes `src`".into());
    }
    Ok(SlidesAction::CreateElement {
        slide_id: slide_id_arg(args)?,
        element: NewElement {
            kind,
            x: required_number(args, "x")?,
            y: required_number(args, "y")?,
            w: required_number(args, "w")?,
            h: required_number(args, "h")?,
            text,
            src,
            style: style_patch(args)?,
        },
    })
}

fn build_move_element(args: &ActionArgs) -> Result<SlidesAction, String> {
    let id = element_id_arg(args)?;
    let x = required_number(args, "x")?;
    let y = required_number(args, "y")?;
    match (args.number("w"), args.number("h")) {
        (None, None) => Ok(move_element_action(0, id, x, y)),
        (Some(w), Some(h)) => Ok(resize_element_action(0, id, x, y, w, h)),
        _ => Err("give both `w` and `h` to resize, or neither to only move".into()),
    }
}

/// The palette receives a compact contextual tree on every frame.  The full
/// durable deck remains addressable through [`resolve_resource`], so a large
/// presentation cannot turn an ordinary frame into an unbounded resource
/// snapshot merely because it has many slides or objects.
pub const MAX_EAGER_SLIDES: usize = 16;
const MAX_EAGER_ELEMENTS_PER_SLIDE: usize = 24;

// Same infallibility as `encode` above: `ActionSpec::immediate` can only fail
// on JSON serialization, which `SlidesAction`'s primitive fields never trigger.
#[allow(
    clippy::expect_used,
    reason = "SlidesAction always serializes; see encode()'s doc comment"
)]
fn immediate_spec(id: &str, label: &str, action: &SlidesAction) -> ActionSpec {
    let spec = ActionSpec::immediate(id, label, SLIDES_ACTION_TYPE, action)
        .expect("encode slides action")
        .reversible();
    if id.contains("delete") {
        spec.destructive()
    } else {
        spec
    }
}

/// A one-line label for an element in the palette: a text element shows its
/// first line (trimmed / capped), a shape shows its kind.
fn element_label(kind: ElementKind, text: &str) -> String {
    if kind == ElementKind::Text {
        let line = text.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            "Text".to_string()
        } else if line.chars().count() > 32 {
            let short: String = line.chars().take(32).collect();
            format!("{short}…")
        } else {
            line.to_string()
        }
    } else {
        kind.label().to_string()
    }
}

/// Build the resource tree Slides currently has open: the deck root (exporting
/// "Add slide" and a "Theme: <name>" per palette), one `slides.slide:<id>` child
/// per slide (duplicate / delete / set-layout / set-background), and under each
/// slide one `slides.element:<id>` per element (delete / bring-front /
/// send-back). Every operation is the same action the on-screen affordance
/// dispatches.
fn resource_rank_for_slot(ctx: &SlidesResourceCtx, at: usize) -> i64 {
    // Search/resource providers hold a stable deck snapshot rather than a
    // mutable Automerge handle. Derive one wide sparse key *here at authoring
    // time* from that snapshot; receivers apply this scalar verbatim.
    (at.min(ctx.slides.len()) as i64).saturating_mul(1_i64 << 32)
}

/// View and Effect operations published via [`SlidesDeck::local_operations`].
fn deck_local_operations() -> Vec<ActionSpec> {
    let mut specs = vec![
        operation(
            "editor",
            "Show editor",
            OperationKind::View,
            SlidesOperation::ShowEditor,
        ),
        operation(
            "overview",
            "Show slide overview",
            OperationKind::View,
            SlidesOperation::ShowOverview,
        ),
        operation(
            "present",
            "Start presentation",
            OperationKind::View,
            SlidesOperation::StartPresentation,
        ),
        operation(
            "present-largest-external",
            "Present on largest external display",
            OperationKind::View,
            SlidesOperation::StartPresentationOn {
                output: PresentationOutput::LargestExternal,
            },
        ),
        operation(
            "present-primary-external",
            "Present on primary external display",
            OperationKind::View,
            SlidesOperation::StartPresentationOn {
                output: PresentationOutput::PrimaryExternal,
            },
        ),
        operation(
            "end-presentation",
            "End presentation",
            OperationKind::View,
            SlidesOperation::EndPresentation,
        ),
    ];
    // System file picker with no document delta may stay an Effect.
    specs.push(file_operation("open", "Open presentation", false));
    specs.push(file_operation("save-as", "Save presentation", true));
    specs.push(read_operation());
    specs
}

/// Document ops whose payload needs the live deck: append a slide (rank after
/// the last slide) and set the accent theme. Published through
/// [`SlidesDeck::bound_actions`] so `tilo_resource::publish` owns them.
fn deck_bound_document_actions(ctx: &SlidesResourceCtx) -> Vec<ActionSpec> {
    // Deck-level: add a new slide after the whole deck. The slide's ids are
    // minted when the palette dispatches (a genuinely new slide), so this spec
    // is not asserted for byte-equality against a builder — every other action
    // here is deterministic and is.
    let rank = resource_rank_for_slot(ctx, ctx.slides.len());
    let mut specs = vec![
        ActionSpec::typed(
            ADD_SLIDE,
            "Add slide",
            "optional title and body",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::optional("title", ParamType::Text, "The slide's title"),
                ActionParam::optional(
                    "body",
                    ParamType::Text,
                    "Text below the title, a line per paragraph (a subtitle on a title slide)",
                ),
                ActionParam::optional("layout", ParamType::Text, "title: a cover slide")
                    .with_choices(["title", "title-content"]),
            ],
            move |args| {
                encode(&insert_slide_with_order_action(
                    &authored_slide(args.text("title"), args.text("body"), args.text("layout")),
                    rank,
                ))
            },
        )
        .reversible(),
    ];

    // Deck-level: set the accent theme, one row per palette. Collaborative
    // document state (not a per-peer View) — same CRDT path as the inspector.
    for kind in PaletteKind::ALL {
        specs.push(immediate_spec(
            &format!("theme-{}", kind.as_str()),
            &format!("Theme: {}", kind.label()),
            &set_deck_palette_action(kind),
        ));
    }
    specs
}

fn deck_resource(ctx: &SlidesResourceCtx) -> Resource {
    let title = ctx.deck_title.trim();
    let deck_label = if title.is_empty() { "Deck" } else { title };
    // Declared ops, bound add-slide/theme, apply, group, View/Effect
    // local_operations, and slides beneath — all from publish, no bolt-on.
    ui::tilo_resource::publish::<SlidesDeck>("slides.deck", deck_label, ctx)
}

fn slide_resource(sr: &SlideResource, eager_children: bool, ctx: &SlidesResourceCtx) -> Resource {
    let idx = sr.idx;
    let mut slide = Resource::new(
        slide_resource_id(&sr.data.id),
        "slides.slide",
        format!(
            "Slide {} · {}",
            idx + 1,
            Layout::from_str(&sr.data.layout).label()
        ),
    )
    .with_action(operation(
        "open",
        "Open slide",
        OperationKind::View,
        SlidesOperation::RevealSlide {
            slide: sr.data.id.clone(),
        },
    ))
    .with_action(immediate_spec(
        "duplicate",
        "Duplicate slide",
        &SlidesAction::InsertSlideWithOrder {
            slide: sr.data.reminted(),
            rank: resource_rank_for_slot(ctx, idx + 1),
        },
    ))
    .with_action(immediate_spec(
        "delete",
        "Delete slide",
        &delete_slide_action(idx, sr.data.id.clone()),
    ));

    for layout in Layout::ALL {
        slide = slide.with_action(immediate_spec(
            &format!("layout-{}", layout.as_str()),
            &format!("Layout: {}", layout.label()),
            &set_slide_layout_action(idx, sr.data.id.clone(), layout),
        ));
    }
    for bg in Background::ALL {
        slide = slide.with_action(immediate_spec(
            &format!("background-{}", bg.as_str()),
            &format!("Background: {}", bg.label()),
            &set_slide_background_action(idx, sr.data.id.clone(), bg),
        ));
    }
    if eager_children {
        for er in sr.elements.iter().take(MAX_EAGER_ELEMENTS_PER_SLIDE) {
            slide = slide.with_child(element_resource(sr, er, true));
        }
        if !sr.data.notes.trim().is_empty() {
            slide = slide.with_child(notes_resource(sr));
        }
    }
    slide
}

fn resource_restack_z(slide: &SlideResource, front: bool) -> i64 {
    let edge = if front {
        slide.elements.iter().map(|element| element.z).max()
    } else {
        slide.elements.iter().map(|element| element.z).min()
    }
    .unwrap_or(0);
    if front {
        edge.saturating_add(1)
    } else {
        edge.saturating_sub(1)
    }
}

fn element_resource(sr: &SlideResource, er: &ElementResource, eager_text: bool) -> Resource {
    let mut resource = Resource::new(
        element_resource_id(&er.id),
        "slides.element",
        element_label(er.kind, &er.text),
    )
    .with_action(operation(
        "open",
        "Reveal element",
        OperationKind::View,
        SlidesOperation::RevealElement {
            slide: sr.data.id.clone(),
            element: er.id.clone(),
            edit_text: false,
        },
    ))
    .with_action(immediate_spec(
        "delete",
        "Delete element",
        &delete_element_action(sr.idx, er.id.clone()),
    ))
    .with_action(immediate_spec(
        "bring-front",
        "Bring to front",
        &set_element_z_action(
            sr.data.id.clone(),
            er.id.clone(),
            resource_restack_z(sr, true),
        ),
    ))
    .with_action(immediate_spec(
        "send-back",
        "Send to back",
        &set_element_z_action(
            sr.data.id.clone(),
            er.id.clone(),
            resource_restack_z(sr, false),
        ),
    ));
    if eager_text && er.kind == ElementKind::Text && !er.text.trim().is_empty() {
        resource = resource.with_child(text_resource(sr, er));
    }
    resource
}

fn text_resource(sr: &SlideResource, er: &ElementResource) -> Resource {
    Resource::new(
        text_resource_id(&er.id),
        "slides.text",
        element_label(ElementKind::Text, &er.text),
    )
    .with_action(operation(
        "open",
        "Edit text",
        OperationKind::View,
        SlidesOperation::RevealElement {
            slide: sr.data.id.clone(),
            element: er.id.clone(),
            edit_text: true,
        },
    ))
}

fn notes_resource(sr: &SlideResource) -> Resource {
    Resource::new(
        notes_resource_id(&sr.data.id),
        "slides.notes",
        element_label(ElementKind::Text, &sr.data.notes),
    )
    .with_action(operation(
        "open",
        "Open speaker notes",
        OperationKind::View,
        SlidesOperation::RevealNotes {
            slide: sr.data.id.clone(),
        },
    ))
}

/// The deck's own edits answer with its [outline](crate::outline), so their
/// caller sees the result without reading the deck again. Only these: every
/// declared result grows the catalogue the deck publishes to the broker,
/// whose reply is capped, and a slide or element resolved by id keeps the
/// contract the catalogue listed.
const RETURNING_OUTLINE: &[&str] = &[
    "add-slide",
    ADD_ELEMENT,
    MOVE_ELEMENT,
    RESTYLE_ELEMENT,
    DELETE_ELEMENT,
    MOVE_SLIDE,
    SET_SLIDE_TEXT,
];

fn returning_outline(mut resource: Resource) -> Resource {
    if resource.id.as_str() != "slides.deck" {
        return resource;
    }
    for action in &mut resource.actions {
        if RETURNING_OUTLINE.contains(&action.id.as_str()) && action.kind == OperationKind::Document
        {
            action.contract.result = ResultSpec::Json {
                schema: crate::outline::result_schema(),
            };
        }
    }
    resource
}

fn read_operation() -> ActionSpec {
    operation(
        "read",
        "Read deck as text",
        OperationKind::Effect,
        SlidesOperation::Read,
    )
    .with_description(
        "The deck as text: each slide's number, layout and {#id}, its title (# line), body and other elements each ending in its {#id}, then its speaker notes. add-slide, set-slide-text, move-slide and the element edits return this too",
    )
}

/// Build the peer's live selection from a slide snapshot and optional element id.
/// Element-bound delete / restack / reveal ride here; slide document ops stay on
/// the durable `slides.slide:<id>` child published via [`SlidesDeck::children`].
pub fn selection_ctx_for(slide: &SlideResource, element_id: Option<&str>) -> SelectionResourceCtx {
    let (label, actions) = if let Some(element_id) = element_id {
        match slide
            .elements
            .iter()
            .find(|element| element.id == element_id)
        {
            Some(element) => {
                let resource = element_resource(slide, element, false);
                (resource.label, resource.actions)
            }
            None => (
                format!("Slide {}", slide.idx + 1),
                vec![operation(
                    "open",
                    "Open slide",
                    OperationKind::View,
                    SlidesOperation::RevealSlide {
                        slide: slide.data.id.clone(),
                    },
                )],
            ),
        }
    } else {
        (
            format!("Slide {}", slide.idx + 1),
            vec![operation(
                "open",
                "Open slide",
                OperationKind::View,
                SlidesOperation::RevealSlide {
                    slide: slide.data.id.clone(),
                },
            )],
        )
    };
    SelectionResourceCtx {
        slide_id: slide.data.id.clone(),
        element_id: element_id
            .filter(|id| slide.elements.iter().any(|element| element.id == *id))
            .map(str::to_owned),
        label,
        actions,
    }
}

/// Live selection. Published beside the framework root when hosted — never a
/// second copy of the deck document door. Selection-bound palette rows (delete /
/// restack / reveal on the peer's focus) stay addressable without republishing
/// every slide already on [`tilo_resource::publish`].
pub fn live_presentation_resources(ctx: &SlidesResourceCtx) -> Vec<Resource> {
    let Some(selection) = &ctx.selection else {
        return Vec::new();
    };
    vec![selection_resource(selection)]
}

fn selection_resource(selection: &SelectionResourceCtx) -> Resource {
    let id = match &selection.element_id {
        Some(element) => format!("slides.selection:{}:{}", selection.slide_id, element),
        None => format!("slides.selection:{}", selection.slide_id),
    };
    let mut resource = Resource::new(id, "slides.selection", selection.label.clone());
    resource.actions.extend(selection.actions.clone());
    resource
}

fn attach_live_selection(deck: &mut Resource, ctx: &SlidesResourceCtx) {
    let Some(selection) = &ctx.selection else {
        return;
    };
    let wanted = slide_resource_id(&selection.slide_id);
    let Some(slide) = deck.children.iter_mut().find(|slide| slide.id.0 == wanted) else {
        return;
    };
    slide.children.insert(0, selection_resource(selection));
}

/// Build the bounded contextual resource tree shown by the palette. Search and
/// agents resolve everything beyond this window by stable id through
/// [`resolve_resource`], which always rebuilds from the current deck mirror.
/// Standalone / `--shots` / unit tests nest the live selection under its slide;
/// when hosted, [`live_presentation_resources`] returns that slice alone.
pub fn provide_resources(ctx: &SlidesResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    // The document is the deck once it holds one. Selection stays the peer's
    // live focus and is attached beside that deck. A caller that has not
    // written a deck yet still publishes the context it passed.
    let state =
        <SlidesDeck as ui::tilo_resource::Resource>::read(doc).unwrap_or_else(|_| ctx.clone());
    let mut deck = returning_outline(deck_resource(&state));
    attach_live_selection(&mut deck, ctx);
    vec![deck]
}

/// Resolve exactly one current durable deck resource without growing the
/// eager snapshot.  A deleted or replaced id returns `None`, so stale search,
/// agent and collaboration requests are refused instead of being rebound by
/// position.
pub fn resolve_resource(ctx: &SlidesResourceCtx, id: &str) -> Option<Resource> {
    resolve_unannotated(ctx, id).map(returning_outline)
}

fn resolve_unannotated(ctx: &SlidesResourceCtx, id: &str) -> Option<Resource> {
    if id == "slides.deck" {
        return Some(deck_resource(ctx));
    }
    if let Some(slide_id) = id.strip_prefix("slides.slide:") {
        return ctx
            .slides
            .iter()
            .find(|slide| slide.data.id == slide_id)
            .map(|slide| slide_resource(slide, false, ctx));
    }
    if let Some(element_id) = id.strip_prefix("slides.element:") {
        return ctx.slides.iter().find_map(|slide| {
            slide
                .elements
                .iter()
                .find(|element| element.id == element_id)
                .map(|element| element_resource(slide, element, false))
        });
    }
    if let Some(element_id) = id.strip_prefix("slides.text:") {
        return ctx.slides.iter().find_map(|slide| {
            slide
                .elements
                .iter()
                .find(|element| element.id == element_id && element.kind == ElementKind::Text)
                .map(|element| text_resource(slide, element))
        });
    }
    if let Some(slide_id) = id.strip_prefix("slides.notes:") {
        return ctx
            .slides
            .iter()
            .find(|slide| slide.data.id == slide_id && !slide.data.notes.trim().is_empty())
            .map(notes_resource);
    }
    None
}

fn find_slides(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_slides(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::SlidesApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.slides_resource_ctx(), doc)
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        let published = provide_resources(&self.slides_resource_ctx(), doc);
        if let Some(found) = find_slides(&published, id) {
            return Some(found);
        }
        // A slide or element outside the eager page is still addressable.
        resolve_resource(&self.slides_resource_ctx(), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SlidesApp;
    use crate::model::seed_deck;
    use automerge::{AutoCommit, ROOT};
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// The demo deck synced into a fresh document, plus the app that owns it.
    /// After `sync`, the app's in-memory deck and the document carry the
    /// identical slide/element ids (the document was seeded from the deck).
    fn seeded() -> (SlidesApp, AutoCommit) {
        let mut app = SlidesApp::demo(None);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// A demo app mirroring `doc`, exactly as the live app is once `sync`
    /// has reloaded the shared deck.
    fn app_for(doc: &AutoCommit) -> SlidesApp {
        let mut app = SlidesApp::demo(None);
        let mut mirror = doc.clone();
        app.sync(&mut mirror, &ROOT);
        app
    }

    fn deck(doc: &AutoCommit) -> crate::model::Deck {
        SlidesDoc::load(doc).expect("seeded deck")
    }

    /// What the door builds for a declared deck operation, from the tree the
    /// app publishes for `doc`.
    fn through_door(doc: &AutoCommit, action: &str, args: serde_json::Value) -> EncodedAction {
        let resources = ui::resources::resources(&app_for(doc), doc);
        tilo_actions::door::encode_json(
            &resources,
            &ActionAddress::new("slides.deck", action),
            args.as_object().expect("arguments are an object"),
        )
        .unwrap_or_else(|error| panic!("`{action}` binds: {error:?}"))
    }

    fn add_text_args(doc: &AutoCommit) -> serde_json::Value {
        serde_json::json!({
            "slide": deck(doc).slides[1].id,
            "kind": "text",
            "x": 96, "y": 560, "w": 600, "h": 80,
            "text": "Added by an agent",
            "text_color": "text-dim",
            "font_size": 28,
            "bold": true,
        })
    }

    fn restyle_args(doc: &AutoCommit) -> serde_json::Value {
        serde_json::json!({
            "element": deck(doc).slides[1].elements[0].id,
            "fill": "on-accent",
            "align": "center",
            "font_size": 60,
            "italic": true,
        })
    }

    fn resize_args(doc: &AutoCommit) -> serde_json::Value {
        serde_json::json!({
            "element": element_resource_id(&deck(doc).slides[1].elements[0].id),
            "x": 120, "y": 64, "w": 900, "h": 120,
        })
    }

    fn move_slide_args(doc: &AutoCommit) -> serde_json::Value {
        serde_json::json!({ "slide": deck(doc).slides[4].id, "position": 1 })
    }

    fn group_args(doc: &AutoCommit) -> serde_json::Value {
        serde_json::json!({ "actions": [
            { "action": ADD_ELEMENT, "args": add_text_args(doc) },
            { "action": ADD_ELEMENT, "args": add_text_args(doc) },
            { "action": RESTYLE_ELEMENT, "args": restyle_args(doc) },
            { "action": MOVE_SLIDE, "args": move_slide_args(doc) },
        ]})
    }

    fn set_text_args() -> serde_json::Value {
        serde_json::json!({"slide": "2", "title": "Agenda", "body": "Goals\nTimeline"})
    }

    /// `read` and every deck edit declare the outline, and the app answers
    /// an edit with the deck it left, titled and with its handles.
    #[test]
    fn deck_edits_and_read_answer_with_the_outline() {
        let (_, mut doc) = seeded();
        let app = app_for(&doc);
        let resources = ui::resources::resources(&app, &doc);
        let published = resources
            .iter()
            .find(|resource| resource.id.as_str() == "slides.deck")
            .expect("the deck is published");
        for id in [SET_SLIDE_TEXT, ADD_ELEMENT, MOVE_SLIDE, "read"] {
            let action = published
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("`{id}` is published"));
            assert_eq!(
                action.contract.result,
                ResultSpec::Json {
                    schema: crate::outline::result_schema()
                },
                "{id}"
            );
        }

        let encoded = through_door(&doc, SET_SLIDE_TEXT, set_text_args());
        let action: SlidesAction = serde_json::from_slice(&encoded.payload).expect("decodes");
        action.apply(&mut doc).expect("set-slide-text applies");
        let second = deck(&doc).slides[1].id.clone();
        let outline = ui::resources::document_action_result(
            &app_for(&doc),
            &doc,
            &ActionAddress::new("slides.deck", SET_SLIDE_TEXT),
            &[],
        )
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("the edit answers with the outline");
        let slide = format!("\nSlide 2 (title-content) {{#{second}}}\n# Agenda {{#");
        assert!(outline.contains(&slide), "{outline}");
        assert!(outline.contains("\nGoals↵Timeline {#"), "{outline}");
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| ui::resources::resources(&app_for(doc), doc),
        palette: ["Add slide", "Delete slide", "Delete element", "Start presentation"],
        replay: [
            set_background => |doc| encode(&set_slide_background_action(
                1,
                deck(doc).slides[1].id.clone(),
                Background::Deep,
            )),
            raise_element => |doc| encode(&raise_element_action(
                1,
                deck(doc).slides[1].elements[0].id.clone(),
            )),
            add_element => |doc| encode(&add_element_action(
                0,
                deck(doc).slides[0].id.clone(),
                &Element::shape(ElementKind::Ellipse, 100.0, 100.0, 200.0, 200.0),
            )),
            door_add_element => |doc| through_door(doc, ADD_ELEMENT, add_text_args(doc)),
            door_move_element => |doc| through_door(doc, MOVE_ELEMENT, serde_json::json!({
                "element": deck(doc).slides[1].elements[0].id, "x": 10, "y": 20,
            })),
            door_resize_element => |doc| through_door(doc, MOVE_ELEMENT, resize_args(doc)),
            door_restyle_element => |doc| through_door(doc, RESTYLE_ELEMENT, restyle_args(doc)),
            door_delete_element => |doc| through_door(doc, DELETE_ELEMENT, serde_json::json!({
                "element": deck(doc).slides[1].elements[0].id,
            })),
            door_move_slide => |doc| through_door(doc, MOVE_SLIDE, move_slide_args(doc)),
            door_set_slide_text => |doc| through_door(doc, SET_SLIDE_TEXT, set_text_args()),
            door_group => |doc| through_door(doc, ui::tilo_resource::GROUP, group_args(doc)),
            door_apply => |doc| through_door(doc, ui::tilo_resource::APPLY, serde_json::json!({
                "action": { "SetSlideBackground": {
                    "slide": 1, "id": deck(doc).slides[1].id, "background": "deep",
                }},
            })),
        ],
        undo: [
            delete_element => |doc| encode(&delete_element_action(
                0,
                deck(doc).slides[0].elements[0].id.clone(),
            )),
            door_add_element => |doc| through_door(doc, ADD_ELEMENT, add_text_args(doc)),
            door_resize_element => |doc| through_door(doc, MOVE_ELEMENT, resize_args(doc)),
            door_restyle_element => |doc| through_door(doc, RESTYLE_ELEMENT, restyle_args(doc)),
            door_move_slide => |doc| through_door(doc, MOVE_SLIDE, move_slide_args(doc)),
            door_set_slide_text => |doc| through_door(doc, SET_SLIDE_TEXT, set_text_args()),
            door_group => |doc| through_door(doc, ui::tilo_resource::GROUP, group_args(doc)),
        ],
        encodes: [
            delete_slide => ("slides.slide", "delete", |doc, _| {
                encode(&delete_slide_action(0, deck(doc).slides[0].id.clone()))
            }),
            background_deep => ("slides.slide", "background-deep", |doc, _| {
                encode(&set_slide_background_action(
                    0,
                    deck(doc).slides[0].id.clone(),
                    Background::Deep,
                ))
            }),
            delete_element => ("slides.element", "delete", |_, element| {
                encode(&delete_element_action(
                    0,
                    element.id.as_str().trim_start_matches("slides.element:"),
                ))
            }),
        ],
    }

    /// Ctrl+K publishes the slides tree and nested view resources export stable broker addresses.
    #[test]
    fn slides_tree_exports_stable_broker_addresses() {
        // Case `ctrl_k_publishes_the_slides_tree`.
        // Ctrl+K on Slides yields the deck root exporting "Add slide" + a theme row
        // per palette, a `slides.slide:<id>` child per slide (duplicate / delete /
        // layout / background), and a `slides.element:<id>` per element (delete /
        // bring-front / send-back). Drives the real resource hook.
        {
            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "one deck root");
            let deck = &resources[0];
            assert_eq!(deck.kind, "slides.deck");
            let deck_actions: Vec<&str> = deck.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(deck_actions.contains(&"add-slide"), "{deck_actions:?}");
            assert!(deck_actions.contains(&"theme-emerald"), "{deck_actions:?}");
            assert!(
                deck_actions.contains(&"present-largest-external")
                    && deck_actions.contains(&"present-primary-external"),
                "presentation output policies must be available through the same resource surface: {deck_actions:?}"
            );

            assert_eq!(
                deck.children.len(),
                seed_deck().slides.len(),
                "one child per slide"
            );
            let slide = &deck.children[0];
            assert_eq!(slide.kind, "slides.slide");
            let s_actions: Vec<&str> = slide.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(s_actions.contains(&"duplicate"), "{s_actions:?}");
            assert!(s_actions.contains(&"delete"), "{s_actions:?}");
            assert!(s_actions.contains(&"layout-two-column"), "{s_actions:?}");
            assert!(s_actions.contains(&"background-deep"), "{s_actions:?}");

            let element = slide
                .children
                .first()
                .expect("a slide has at least one element");
            assert_eq!(element.kind, "slides.element");
            let e_actions: Vec<&str> = element.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(e_actions.contains(&"delete"), "{e_actions:?}");
            assert!(e_actions.contains(&"bring-front"), "{e_actions:?}");
            assert!(e_actions.contains(&"send-back"), "{e_actions:?}");
        }

        // Case `nested_view_resources_export_stable_broker_addresses`.
        {
            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            let slide = &resources[0].children[0];
            let slide_id = slide.id.0.strip_prefix("slides.slide:").unwrap();
            let open_slide = slide
                .actions
                .iter()
                .find(|action| action.id == "open")
                .unwrap();
            assert_eq!(open_slide.kind, OperationKind::View);
            assert_eq!(
                open_slide_address(slide_id),
                tilo_actions::ActionAddress::new(&slide.id.0, &open_slide.id)
            );

            let element = slide
                .children
                .iter()
                .find(|resource| resource.kind == "slides.element" && !resource.children.is_empty())
                .expect("slide has a searchable text element");
            let text = &element.children[0];
            let element_id = element.id.0.strip_prefix("slides.element:").unwrap();
            assert_eq!(open_element_address(element_id).resource, element.id.0);
            assert_eq!(open_text_address(element_id).resource, text.id.0);

            let address = open_text_address(element_id);
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .expect("text address resolves through the published resource tree");
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(SlidesOperation::RevealElement {
                    slide: slide_id.to_string(),
                    element: element_id.to_string(),
                    edit_text: true,
                })
            );
        }
    }

    /// Stale, hostile, over-limit and duplicate actions are refused without partial writes.
    #[test]
    fn hostile_or_over_limit_actions_are_refused_atomically() {
        // Case `stale_and_hostile_actions_are_refused_without_a_partial_write`.
        {
            let (_app, mut doc) = seeded();
            let before = doc.get_heads();
            let live = SlidesDoc::load(&doc).unwrap();
            let element = live.slides[0].elements[0].id.clone();

            for action in [
                // A stale numeric index is intentionally *not* hostile: the
                // canonical element id below resolves it after a remote insert.
                move_element_action(99, "stale-element", 10.0, 10.0),
                move_element_action(0, element.clone(), f64::NAN, 10.0),
                delete_element_action(0, "stale-element"),
                set_slide_layout_action(99, "stale-slide", Layout::Blank),
                set_slide_background_action(0, "stale-slide", Background::Deep),
                SlidesAction::SetDeckPalette {
                    palette: "untrusted-purple".into(),
                },
                SlidesAction::RestyleElement {
                    slide: 0,
                    id: element,
                    patch: StylePatch::default(),
                },
            ] {
                assert!(
                    matches!(action.apply(&mut doc), Err(ActionError::Refused(_))),
                    "invalid action must be visibly refused: {action:?}"
                );
                assert_eq!(doc.get_heads(), before, "a refused action is atomic");
            }
        }

        // Case `aggregate_slide_limit_refuses_atomically_at_the_boundary`.
        {
            let mut deck = crate::model::starter_deck();
            deck.slides = (0..MAX_TOTAL_SLIDES)
                .map(|_| crate::model::Slide::new(Layout::Blank))
                .collect();
            let mut doc = AutoCommit::new();
            SlidesDoc::seed(&mut doc, &deck);
            let before = doc.save();
            let action =
                insert_slide_action(deck.slides.len(), &crate::model::Slide::new(Layout::Blank));
            assert!(action.apply(&mut doc).is_err());
            assert_eq!(
                doc.save(),
                before,
                "a refused aggregate action writes nothing"
            );
        }

        // Case `duplicate_element_insert_is_refused_before_document_mutation`.
        {
            let (_app, mut doc) = seeded();
            let before = doc.get_heads();
            let duplicate = SlidesDoc::load(&doc).unwrap().slides[0].elements[0].clone();
            let slide_id = SlidesDoc::load(&doc).unwrap().slides[0].id.clone();
            let action = add_element_action(0, slide_id, &duplicate);
            assert!(matches!(
                action.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));
            assert_eq!(doc.get_heads(), before);
        }
    }

    /// Stable ids and absolute slide order/insert/z replay identically after divergent edits.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn absolute_orders_replay_identically_after_divergence() {
        // Case `stable_ids_keep_an_element_action_on_its_original_slide_after_an_insert`.
        {
            let (_app, mut doc) = seeded();
            let before = SlidesDoc::load(&doc).expect("seeded deck");
            let target_slide = before.slides[1].id.clone();
            let element = before.slides[1].elements[0].id.clone();
            let action = move_element_action(1, element.clone(), 321.0, 222.0);

            // This simulates a collaborator inserting a slide before the authored
            // numeric position while the action is in flight.
            SlidesDoc::insert_slide(&mut doc, 0, &crate::model::Slide::new(Layout::Blank));
            action
                .apply(&mut doc)
                .expect("stable id resolves after insert");

            let after = SlidesDoc::load(&doc).expect("deck remains readable");
            let target = after
                .slides
                .iter()
                .find(|slide| slide.id == target_slide)
                .expect("same canonical slide");
            let moved = target.element(&element).expect("same canonical element");
            assert_eq!((moved.x, moved.y), (321.0, 222.0));
            assert!(
                after.slides[1].element(&element).is_none(),
                "the stale numeric slot was not mutated"
            );
        }

        // Case `absolute_slide_order_replays_identically_after_divergent_inserts`.
        {
            let (_app, mut a) = seeded();
            a.commit();
            let bytes = a.save();
            let mut b = AutoCommit::load(&bytes).expect("replica loads");

            let base = SlidesDoc::load(&a).expect("seeded deck");
            let moved = base.slides[1].id.clone();
            let rank = SlidesDoc::rank_for_move(&a, 1, 3).expect("valid authored reorder");
            let encoded = encode(&set_slide_order_action(moved.clone(), rank));
            assert_eq!(encoded.type_id, SLIDES_ACTION_TYPE);
            let action: SlidesAction =
                serde_json::from_slice(&encoded.payload).expect("encoded action decodes");

            // Each receiver has independently evolved its positional view before
            // the same reorder arrives. The action carries the author-computed
            // scalar rank, so neither receiver recomputes a different destination.
            SlidesDoc::insert_slide(&mut a, 0, &crate::model::Slide::new(Layout::Blank));
            SlidesDoc::insert_slide(&mut b, 2, &crate::model::Slide::new(Layout::Blank));
            action.apply(&mut a).expect("absolute rank applies to a");
            action.apply(&mut b).expect("absolute rank applies to b");
            assert_eq!(SlidesDoc::slide_order(&a, &moved), Some(rank));
            assert_eq!(SlidesDoc::slide_order(&b, &moved), Some(rank));

            a.merge_incoming(&mut b).expect("merge into a");
            b.merge_incoming(&mut a).expect("merge into b");
            assert_eq!(
                SlidesDoc::load(&a),
                SlidesDoc::load(&b),
                "the divergent replicas converge after replaying one encoded rank"
            );
            assert_eq!(SlidesDoc::slide_order(&a, &moved), Some(rank));
        }

        // Case `absolute_slide_insert_replays_identically_after_divergent_inserts`.
        {
            let (_app, mut a) = seeded();
            a.commit();
            let bytes = a.save();
            let mut b = AutoCommit::load(&bytes).expect("replica loads");

            let inserted = crate::model::Slide::new(Layout::Blank);
            let inserted_id = inserted.id.clone();
            let rank = SlidesDoc::rank_for_insert(&a, 1);
            let encoded = encode(&insert_slide_with_order_action(&inserted, rank));
            assert_eq!(encoded.type_id, SLIDES_ACTION_TYPE);
            let action: SlidesAction =
                serde_json::from_slice(&encoded.payload).expect("encoded action decodes");

            // The receivers disagree about their current insertion slots, but the
            // new slide's encoded rank remains the exact authored scalar.
            SlidesDoc::insert_slide(&mut a, 0, &crate::model::Slide::new(Layout::Blank));
            SlidesDoc::insert_slide(&mut b, 3, &crate::model::Slide::new(Layout::Blank));
            action
                .apply(&mut a)
                .expect("absolute insertion applies to a");
            action
                .apply(&mut b)
                .expect("absolute insertion applies to b");
            assert_eq!(SlidesDoc::slide_order(&a, &inserted_id), Some(rank));
            assert_eq!(SlidesDoc::slide_order(&b, &inserted_id), Some(rank));

            a.merge_incoming(&mut b).expect("merge into a");
            b.merge_incoming(&mut a).expect("merge into b");
            assert_eq!(SlidesDoc::load(&a), SlidesDoc::load(&b));
            assert_eq!(SlidesDoc::slide_order(&a, &inserted_id), Some(rank));
        }

        // Case `absolute_element_z_replays_identically_after_divergent_arrangement`.
        {
            let (_app, mut a) = seeded();
            a.commit();
            let bytes = a.save();
            let mut b = AutoCommit::load(&bytes).expect("replica loads");

            let deck = SlidesDoc::load(&a).expect("seeded deck");
            let slide_id = deck.slides[0].id.clone();
            let target = deck.slides[0].elements[0].id.clone();
            let other_a = deck.slides[0].elements[1].id.clone();
            let other_b = deck.slides[0].elements[2].id.clone();
            let z = restack_z_for_author(&a, &slide_id, &target, true).expect("front z");
            let encoded = encode(&set_element_z_action(slide_id, target.clone(), z));
            let action: SlidesAction =
                serde_json::from_slice(&encoded.payload).expect("encoded action decodes");

            // Different local z extrema must not change the encoded target z.
            SlidesDoc::set_element_z(&mut a, 0, &other_a, 80);
            SlidesDoc::set_element_z(&mut b, 0, &other_b, -80);
            action.apply(&mut a).expect("absolute z applies to a");
            action.apply(&mut b).expect("absolute z applies to b");

            a.merge_incoming(&mut b).expect("merge into a");
            b.merge_incoming(&mut a).expect("merge into b");
            let left = SlidesDoc::load(&a).expect("merged deck");
            assert_eq!(Some(left.clone()), SlidesDoc::load(&b));
            assert_eq!(
                left.slides[0].element(&target).expect("target survives").z,
                z,
                "both replicas applied the author-computed z, not local extrema"
            );
        }
    }

    #[test]
    fn consecutive_deck_title_edits_are_one_step() {
        use ui::tilo_resource::Resource as _;
        let first = SlidesAction::SetDeckTitle { title: "Q".into() };
        let second = SlidesAction::SetDeckTitle { title: "Qu".into() };
        assert!(SlidesDeck::coalesces(&first, &second));
        assert!(!SlidesDeck::coalesces(
            &first,
            &SlidesAction::SetDeckPalette {
                palette: "ink".into(),
            },
        ));
        let mut doc = automerge::AutoCommit::new();
        SlidesDoc::seed(&mut doc, &crate::model::starter_deck());
        SlidesAction::SetDeckTitle {
            title: "Quarterly".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(SlidesDoc::load(&doc).expect("deck").title, "Quarterly");
    }
}

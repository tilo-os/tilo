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
//! Presentation → never an action: the current slide, the selection, the zoom /
//! drag preview, the active tool, which panel flap is open, present mode.
//!
//! External → never an action: `.odp` import / export (a whole-deck swap, not a
//! stored mutation) and the per-peer image decode cache.
//!
//! ## Determinism contract
//!
//! Ids (slide + element uuids) are minted at **authoring** time and carried in
//! the payload; every enum is carried in its document string form. So
//! [`Action::apply`] is a pure function of `(payload, base document)` — no RNG,
//! no wall-clock, no app state. Where an operation is relative to the current
//! deck (a z-order raise/lower, a slide reorder) it recomputes from the
//! document's base state, reading z-values order-independently, so two replicas
//! at the same heads land on the byte-identical result. A malformed payload is a
//! **no-op**, never an error, so a bad replay can't wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::SlidesDoc;
use crate::model::{
    new_id, Background, Element, ElementKind, Layout, PaletteKind, Slide, TextAlign,
};

/// Registry `type_id` for every Slides action.
pub const SLIDES_ACTION_TYPE: &str = "slides.action";

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
        }
    }

    pub fn to_element(&self) -> Element {
        Element {
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
        }
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

// ===========================================================================
// The action enum
// ===========================================================================

/// Every collaborative operation Slides exports on the shared deck.
///
/// Determinism contract: ids and enums are carried in the payload;
/// [`Action::apply`] never touches the RNG or the wall-clock. The two variants
/// that are relative to the current deck ([`Self::RaiseElement`] /
/// [`Self::LowerElement`]) recompute from the document's base state. A malformed
/// payload is a **no-op** rather than an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SlidesAction {
    /// Insert a whole slide (a blank create *or* a duplicate) at index `at`
    /// (clamped to the end). One variant covers both: "add" and "duplicate"
    /// differ only in the payload their builders mint, exactly as a create and
    /// an edit are one `Upsert` elsewhere.
    InsertSlide { at: usize, slide: SlideData },
    /// Delete slide `idx`. A no-op when it is the deck's last slide (a deck
    /// always keeps at least one slide) or when `idx` is out of range.
    DeleteSlide { idx: usize },
    /// Move the slide at `from` so it lands at index `to`, carrying its whole
    /// content. Recomputed from the document's base state.
    MoveSlide { from: usize, to: usize },
    /// Set slide `slide`'s layout.
    SetSlideLayout { slide: usize, layout: String },
    /// Set slide `slide`'s background treatment.
    SetSlideBackground { slide: usize, background: String },
    /// Set the deck's accent theme (palette). Deck-wide, so it carries no slide
    /// index.
    SetDeckPalette { palette: String },
    /// Add an element (a fresh insert, a paste, a duplicate, or a picked image)
    /// to slide `slide`. The element's id is minted at authoring and carried in
    /// the payload.
    AddElement { slide: usize, element: ElementData },
    /// Delete element `id` from slide `slide`.
    DeleteElement { slide: usize, id: String },
    /// Move element `id` to its **final** top-left after a drag drop or a
    /// keyboard nudge (never the intermediate per-frame drag positions).
    MoveElement { slide: usize, id: String, x: f64, y: f64 },
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
}

impl Action for SlidesAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            SlidesAction::InsertSlide { at, slide } => {
                SlidesDoc::insert_slide(doc, *at, &slide.to_slide());
            }
            SlidesAction::DeleteSlide { idx } => {
                // A deck always keeps at least one slide. The count is read from
                // the document's base state, so the guard evaluates identically
                // on every replica.
                if let Some(deck) = SlidesDoc::load(doc) {
                    if deck.slides.len() > 1 && *idx < deck.slides.len() {
                        SlidesDoc::remove_slide(doc, *idx);
                    }
                }
            }
            SlidesAction::MoveSlide { from, to } => {
                SlidesDoc::move_slide(doc, *from, *to);
            }
            SlidesAction::SetSlideLayout { slide, layout } => {
                SlidesDoc::set_slide_layout(doc, *slide, Layout::from_str(layout));
            }
            SlidesAction::SetSlideBackground { slide, background } => {
                SlidesDoc::set_slide_background(doc, *slide, Background::from_str(background));
            }
            SlidesAction::SetDeckPalette { palette } => {
                SlidesDoc::set_palette(doc, PaletteKind::from_str(palette));
            }
            SlidesAction::AddElement { slide, element } => {
                SlidesDoc::add_element(doc, *slide, &element.to_element());
            }
            SlidesAction::DeleteElement { slide, id } => {
                SlidesDoc::remove_element(doc, *slide, id);
            }
            SlidesAction::MoveElement { slide, id, x, y } => {
                SlidesDoc::set_element_pos(doc, *slide, id, *x, *y);
            }
            SlidesAction::ResizeElement {
                slide,
                id,
                x,
                y,
                w,
                h,
            } => {
                SlidesDoc::set_element_rect(doc, *slide, id, *x, *y, *w, *h);
            }
            SlidesAction::RestyleElement { slide, id, patch } => {
                // Fixed field order so the emitted change is byte-identical on
                // every replica.
                if let Some(v) = &patch.fill {
                    SlidesDoc::set_element_str(doc, *slide, id, "fill", v);
                }
                if let Some(v) = &patch.stroke {
                    SlidesDoc::set_element_str(doc, *slide, id, "stroke", v);
                }
                if let Some(v) = &patch.align {
                    SlidesDoc::set_element_str(doc, *slide, id, "align", v);
                }
                if let Some(v) = patch.font_size {
                    SlidesDoc::set_element_f64(doc, *slide, id, "font_size", v);
                }
                if let Some(v) = patch.stroke_width {
                    SlidesDoc::set_element_f64(doc, *slide, id, "stroke_width", v);
                }
                if let Some(v) = patch.radius {
                    SlidesDoc::set_element_f64(doc, *slide, id, "radius", v);
                }
                if let Some(v) = patch.bold {
                    SlidesDoc::set_element_bool(doc, *slide, id, "bold", v);
                }
                if let Some(v) = patch.italic {
                    SlidesDoc::set_element_bool(doc, *slide, id, "italic", v);
                }
            }
            SlidesAction::RaiseElement { slide, id } => {
                if let Some(z) = restack_z(doc, *slide, true) {
                    SlidesDoc::set_element_z(doc, *slide, id, z);
                }
            }
            SlidesAction::LowerElement { slide, id } => {
                if let Some(z) = restack_z(doc, *slide, false) {
                    SlidesDoc::set_element_z(doc, *slide, id, z);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// The z one above (or below) every element on `slide`, read from the
/// document's base state. Returns `None` when the slide has no elements. The
/// z-values are gathered and reduced order-independently (max / min), so a
/// `HashMap`/list iteration order can never make two replicas diverge.
fn restack_z(doc: &mut Doc, slide: usize, front: bool) -> Option<i64> {
    let deck = SlidesDoc::load(doc)?;
    let s = deck.slides.get(slide)?;
    if s.elements.is_empty() {
        return None;
    }
    let mut zs: Vec<i64> = s.elements.iter().map(|e| e.z).collect();
    zs.sort_unstable();
    if front {
        Some(zs.last().copied().unwrap_or(0) + 1)
    } else {
        Some(zs.first().copied().unwrap_or(0) - 1)
    }
}

/// Register Slides' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<SlidesAction>(SLIDES_ACTION_TYPE);
}

/// Encode any [`SlidesAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Slides action into a
/// replayable envelope.
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

pub fn delete_slide_action(idx: usize) -> SlidesAction {
    SlidesAction::DeleteSlide { idx }
}

pub fn move_slide_action(from: usize, to: usize) -> SlidesAction {
    SlidesAction::MoveSlide { from, to }
}

pub fn set_slide_layout_action(slide: usize, layout: Layout) -> SlidesAction {
    SlidesAction::SetSlideLayout {
        slide,
        layout: layout.as_str().to_string(),
    }
}

pub fn set_slide_background_action(slide: usize, background: Background) -> SlidesAction {
    SlidesAction::SetSlideBackground {
        slide,
        background: background.as_str().to_string(),
    }
}

pub fn set_deck_palette_action(palette: PaletteKind) -> SlidesAction {
    SlidesAction::SetDeckPalette {
        palette: palette.as_str().to_string(),
    }
}

pub fn add_element_action(slide: usize, element: &Element) -> SlidesAction {
    SlidesAction::AddElement {
        slide,
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

pub fn restyle_element_action(slide: usize, id: impl Into<String>, patch: StylePatch) -> SlidesAction {
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
pub fn restyle_bool_action(slide: usize, id: impl Into<String>, key: &str, v: bool) -> SlidesAction {
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

/// The template for a freshly-created slide, shared by the toolbar's "New slide"
/// button and the palette's "Add slide" so both insert the byte-identical
/// structure. Fresh ids are minted on each call (a new slide is genuinely new).
pub fn new_slide_template() -> Slide {
    let mut s = Slide::new(Layout::TitleContent);
    s.add(Element::text("New slide", 96.0, 90.0, 1088.0, 100.0, 54.0).with(|e| e.bold = true));
    s.add(Element::shape(ElementKind::Rect, 96.0, 188.0, 150.0, 8.0).with(|e| {
        e.fill = "accent".into();
        e.radius = 4.0;
    }));
    s.add(Element::text("Add your content…", 96.0, 230.0, 900.0, 300.0, 32.0).with(|e| {
        e.fill = "text-dim".into();
    }));
    s
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
}

/// One slide surfaced as a resource: its index, a snapshot for the duplicate
/// action, its current layout/background, and its elements.
#[derive(Debug, Clone)]
pub struct SlideResource {
    pub idx: usize,
    pub data: SlideData,
    pub elements: Vec<ElementResource>,
}

/// The presentation context Slides hands [`provide_resources`]: the deck title,
/// and the slides it currently has open (each with its elements). Everything
/// here is derived from the in-memory deck mirror, which the document already
/// holds; nothing is fabricated.
#[derive(Debug, Clone)]
pub struct SlidesResourceCtx {
    pub deck_title: String,
    pub slides: Vec<SlideResource>,
}

fn immediate_spec(id: &str, label: &str, action: &SlidesAction) -> ActionSpec {
    ActionSpec::immediate(id, label, SLIDES_ACTION_TYPE, action).expect("encode slides action")
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
pub fn provide_resources(ctx: &SlidesResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let title = ctx.deck_title.trim();
    let deck_label = if title.is_empty() { "Deck" } else { title };
    let mut deck = Resource::new("slides.deck", "slides.deck", deck_label);

    // Deck-level: add a new slide after the whole deck. The slide's ids are
    // minted when the palette dispatches (a genuinely new slide), so this spec
    // is not asserted for byte-equality against a builder — every other action
    // here is deterministic and is.
    let at = ctx.slides.len();
    deck = deck.with_action(ActionSpec::immediate(
        "add-slide",
        "Add slide",
        SLIDES_ACTION_TYPE,
        &insert_slide_action(at, &new_slide_template()),
    )
    .expect("encode add-slide"));

    // Deck-level: set the accent theme, one row per palette.
    for kind in PaletteKind::ALL {
        deck = deck.with_action(immediate_spec(
            &format!("theme-{}", kind.as_str()),
            &format!("Theme: {}", kind.label()),
            &set_deck_palette_action(kind),
        ));
    }

    for sr in &ctx.slides {
        let idx = sr.idx;
        let mut slide = Resource::new(
            format!("slides.slide:{}", sr.data.id),
            "slides.slide",
            format!(
                "Slide {} · {}",
                idx + 1,
                Layout::from_str(&sr.data.layout).label()
            ),
        );

        // Duplicate this slide immediately after it. Its ids are reminted at
        // authoring, baked into the payload, so the action replays identically.
        slide = slide.with_action(ActionSpec::immediate(
            "duplicate",
            "Duplicate slide",
            SLIDES_ACTION_TYPE,
            &SlidesAction::InsertSlide {
                at: idx + 1,
                slide: sr.data.reminted(),
            },
        )
        .expect("encode duplicate-slide"));

        slide = slide.with_action(immediate_spec("delete", "Delete slide", &delete_slide_action(idx)));

        for layout in Layout::ALL {
            slide = slide.with_action(immediate_spec(
                &format!("layout-{}", layout.as_str()),
                &format!("Layout: {}", layout.label()),
                &set_slide_layout_action(idx, layout),
            ));
        }
        for bg in Background::ALL {
            slide = slide.with_action(immediate_spec(
                &format!("background-{}", bg.as_str()),
                &format!("Background: {}", bg.label()),
                &set_slide_background_action(idx, bg),
            ));
        }

        for er in &sr.elements {
            let child = Resource::new(
                format!("slides.element:{}", er.id),
                "slides.element",
                element_label(er.kind, &er.text),
            )
            .with_action(immediate_spec(
                "delete",
                "Delete element",
                &delete_element_action(idx, er.id.clone()),
            ))
            .with_action(immediate_spec(
                "bring-front",
                "Bring to front",
                &raise_element_action(idx, er.id.clone()),
            ))
            .with_action(immediate_spec(
                "send-back",
                "Send to back",
                &lower_element_action(idx, er.id.clone()),
            ));
            slide = slide.with_child(child);
        }

        deck = deck.with_child(slide);
    }

    vec![deck]
}

impl ResourceProvider for crate::app::SlidesApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.slides_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SlidesApp;
    use crate::model::seed_deck;
    use automerge::{AutoCommit, ROOT};
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
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

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does.
    fn dispatch(doc: &mut AutoCommit, action: &SlidesAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// Ctrl+K on Slides yields the deck root exporting "Add slide" + a theme row
    /// per palette, a `slides.slide:<id>` child per slide (duplicate / delete /
    /// layout / background), and a `slides.element:<id>` per element (delete /
    /// bring-front / send-back). Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_slides_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one deck root");
        let deck = &resources[0];
        assert_eq!(deck.kind, "slides.deck");
        let deck_actions: Vec<&str> = deck.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(deck_actions.contains(&"add-slide"), "{deck_actions:?}");
        assert!(deck_actions.contains(&"theme-emerald"), "{deck_actions:?}");

        assert_eq!(deck.children.len(), seed_deck().slides.len(), "one child per slide");
        let slide = &deck.children[0];
        assert_eq!(slide.kind, "slides.slide");
        let s_actions: Vec<&str> = slide.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(s_actions.contains(&"duplicate"), "{s_actions:?}");
        assert!(s_actions.contains(&"delete"), "{s_actions:?}");
        assert!(s_actions.contains(&"layout-two-column"), "{s_actions:?}");
        assert!(s_actions.contains(&"background-deep"), "{s_actions:?}");

        let element = slide
            .children
            .iter()
            .next()
            .expect("a slide has at least one element");
        assert_eq!(element.kind, "slides.element");
        let e_actions: Vec<&str> = element.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(e_actions.contains(&"delete"), "{e_actions:?}");
        assert!(e_actions.contains(&"bring-front"), "{e_actions:?}");
        assert!(e_actions.contains(&"send-back"), "{e_actions:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("Add slide")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Delete slide")), "{labels:?}");
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut app = SlidesApp::demo(None);
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let deck = SlidesDoc::load(&state.doc).unwrap();
        let victim = deck.slides[0].elements[0].id.clone();
        let before_heads = state.doc.get_heads();
        let before = deck.slides[0].elements.len();

        let action = delete_element_action(0, victim.clone());
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads.clone(), None, None);

        let after = SlidesDoc::load(&state.doc).unwrap();
        assert_eq!(after.slides[0].elements.len(), before - 1, "the action deleted an element");
        assert!(
            !after.slides[0].elements.iter().any(|e| e.id == victim),
            "the right element is gone"
        );
        assert!(state.peek_last_undo().is_some(), "exactly one undo boundary recorded");

        state.undo(None, None).expect("an undo entry to pop");
        let reverted = SlidesDoc::load(&state.doc).unwrap();
        assert_eq!(
            reverted.slides[0].elements.len(),
            before,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            reverted.slides[0].elements.iter().any(|e| e.id == victim),
            "the element is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "precondition: same base state");

        // Set the second slide's background — a whole-value write, deterministic
        // against the shared base.
        let action = set_slide_background_action(1, Background::Deep);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "replay reproduced the identical change");
        assert_eq!(SlidesDoc::load(&b).unwrap().slides[1].background, Background::Deep);
    }

    /// A z-order raise — recomputed from the document's base state — also replays
    /// byte-identically on a peer.
    #[test]
    fn raise_element_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "precondition: same base state");

        let deck = SlidesDoc::load(&a).unwrap();
        // The first (back-most) element of a multi-element slide.
        let id = deck.slides[1].elements[0].id.clone();
        let action = raise_element_action(1, id.clone());
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "restack replayed identically");
        let ra = SlidesDoc::load(&a).unwrap().slides[1].clone();
        let raised = ra.element(&id).unwrap();
        assert!(
            ra.elements.iter().all(|e| e.id == id || e.z < raised.z),
            "the raised element is on top"
        );
    }

    /// An `AddElement` (insert / paste / duplicate / image) replays
    /// byte-identically — the create path, from an authoring-minted id.
    #[test]
    fn add_element_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let el = Element::shape(ElementKind::Ellipse, 100.0, 100.0, 200.0, 200.0);
        let id = el.id.clone();
        let action = add_element_action(0, &el);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "add replayed identically");
        assert!(SlidesDoc::load(&b).unwrap().slides[0].element(&id).is_some());
    }

    /// Single source of truth: the palette's "Delete element" / "Delete slide" /
    /// "Background" specs encode byte-identically to the builders the on-screen
    /// affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let deck = &resources[0];
        let slide = &deck.children[0];

        // Delete-slide spec == delete builder (slide 0).
        let delete_spec = slide.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_slide_action(0));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(from_spec.payload, from_builder.payload, "delete-slide is one source of truth");

        // Background spec == builder for the same treatment.
        let bg_spec = slide.actions.iter().find(|a| a.id == "background-deep").unwrap();
        let from_spec = immediate_encode(bg_spec);
        let from_builder = encode(&set_slide_background_action(0, Background::Deep));
        assert_eq!(from_spec.payload, from_builder.payload, "background is one source of truth");

        // Delete-element spec == builder, carrying the element's real id.
        let element = slide.children.iter().next().unwrap();
        let el_id = element.id.0.trim_start_matches("slides.element:").to_string();
        let del_el = element.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(del_el);
        let from_builder = encode(&delete_element_action(0, el_id));
        assert_eq!(from_spec.payload, from_builder.payload, "delete-element is one source of truth");
    }
}

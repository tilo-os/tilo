//! Deck history uses exact authored CRDT roots and a presentation-only renderer.
use crate::{
    assets::{ImageLookup, ImageState},
    model::{Deck, Palette, SLIDE_H, SLIDE_W},
};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::{
    collections::{BTreeMap, HashSet},
    rc::Rc,
};
use tilo_content::ContentStore;
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoricalAssetRequest, HistoricalAssetResponse, HistoricalAssetSource, HistoryDefinition,
    HistoryPresentation, HistoryPreview, HistoryScopeDefinition, PreviewAccessibility,
    PreviewInput, PreviewNavigation, PreviewView,
};
use ui::{TextRenderer, TextRole, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
    peniko::{BlendMode, Fill},
};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.slides",
    scopes: &[HistoryScopeDefinition {
        key: "deck",
        title: "Presentation deck",
        presentation: HistoryPresentation::DomainWithAssets {
            create: preview,
            assets: || Rc::new(DeckAssets::new(ContentStore::for_app())),
        },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "slides.deck",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "deck",
        required: true,
        expected_type: RestoreValueType::Object(ObjType::Map),
    }],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};
fn object(
    doc: &AutoCommit,
    parent: &ObjId,
    key: impl Into<automerge::Prop>,
    kind: ObjType,
) -> Result<ObjId, String> {
    match doc.get(parent, key).map_err(|e| e.to_string())? {
        Some((Value::Object(actual), id)) if actual == kind => Ok(id),
        _ => Err(format!("Recorded deck requires a {kind:?} object")),
    }
}
fn string(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<String, String> {
    match doc.get(obj, key).map_err(|e| e.to_string())? {
        Some((Value::Scalar(v), _)) => v
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{key} must be a string")),
        _ => Err(format!("{key} must be a string")),
    }
}
fn enum_field(doc: &AutoCommit, obj: &ObjId, key: &str, values: &[&str]) -> Result<(), String> {
    let value = string(doc, obj, key)?;
    if values.contains(&value.as_str()) {
        Ok(())
    } else {
        Err(format!("Unsupported {key}: {value}"))
    }
}
fn number(doc: &AutoCommit, obj: &ObjId, key: &str, min: f64, max: f64) -> Result<f64, String> {
    let value = doc
        .get(obj, key)
        .map_err(|e| e.to_string())?
        .and_then(|(v, _)| match v {
            Value::Scalar(s) => match s.as_ref() {
                ScalarValue::F64(n) => Some(*n),
                ScalarValue::Int(n) => Some(*n as f64),
                ScalarValue::Uint(n) => Some(*n as f64),
                _ => None,
            },
            Value::Object(_) => None,
        });
    value
        .filter(|n| n.is_finite() && (min..=max).contains(n))
        .ok_or_else(|| format!("Unsafe or malformed {key}"))
}
fn unique_id(doc: &AutoCommit, obj: &ObjId, ids: &mut HashSet<String>) -> Result<(), String> {
    let id = string(doc, obj, "id")?;
    if id.is_empty() || id.len() > 1024 || !ids.insert(id) {
        return Err("Missing, oversized or duplicate stable ID".into());
    }
    Ok(())
}
/// Validate every field consumed by the tolerant live loader before using it.
/// Unknown authored fields and Text marks stay in the checked whole-root graph.
fn validate(doc: &AutoCommit) -> Result<(), String> {
    if doc.get(ROOT, "deck").map_err(|e| e.to_string())?.is_none() {
        return Ok(());
    }
    let deck = object(doc, &ROOT, "deck", ObjType::Map)?;
    object(doc, &deck, "title", ObjType::Text)?;
    enum_field(
        doc,
        &deck,
        "palette",
        &["indigo", "emerald", "amber", "rose", "sky"],
    )?;
    let slides = object(doc, &deck, "slides", ObjType::List)?;
    if doc.length(&slides) > crate::actions::MAX_TOTAL_SLIDES {
        return Err("Too many recorded slides".into());
    }
    let mut slide_ids = HashSet::new();
    let mut element_ids = HashSet::new();
    for i in 0..doc.length(&slides) {
        let slide = object(doc, &slides, i, ObjType::Map)?;
        unique_id(doc, &slide, &mut slide_ids)?;
        enum_field(
            doc,
            &slide,
            "layout",
            &["title", "title-content", "two-column", "section", "blank"],
        )?;
        enum_field(doc, &slide, "background", &["surface", "accent", "deep"])?;
        object(doc, &slide, "notes", ObjType::Text)?;
        let elements = object(doc, &slide, "elements", ObjType::List)?;
        if doc.length(&elements).saturating_add(element_ids.len())
            > crate::actions::MAX_TOTAL_ELEMENTS
        {
            return Err("Too many recorded elements".into());
        }
        for j in 0..doc.length(&elements) {
            let e = object(doc, &elements, j, ObjType::Map)?;
            unique_id(doc, &e, &mut element_ids)?;
            enum_field(
                doc,
                &e,
                "kind",
                &["text", "rect", "ellipse", "line", "image"],
            )?;
            enum_field(doc, &e, "align", &["left", "center", "right", "justify"])?;
            // A text box keeps its words as rich text, or before that as a
            // plain Text; other elements have none.
            match doc.get(&e, "body").map_err(|e| e.to_string())? {
                Some((Value::Object(ObjType::Map), body)) => {
                    tilo_doc::model::open(doc, &body)
                        .map_err(|_| "Unreadable recorded text body")?;
                }
                Some(_) => return Err("Recorded text body is not rich text".into()),
                None if string(doc, &e, "kind")? == "text"
                    || doc.get(&e, "text").map_err(|e| e.to_string())?.is_some() =>
                {
                    object(doc, &e, "text", ObjType::Text)?;
                }
                None => {}
            }
            for key in ["src", "fill", "stroke"] {
                if string(doc, &e, key)?.len() > 4096 {
                    return Err(format!("Oversized {key}"));
                }
            }
            for key in ["bold", "italic"] {
                if !matches!(doc.get(&e, key).map_err(|e| e.to_string())?, Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Boolean(_)))
                {
                    return Err(format!("{key} must be Boolean"));
                }
            }
            if !matches!(doc.get(&e, "z").map_err(|e| e.to_string())?, Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Int(_)))
            {
                return Err("z must be an integer".into());
            }
            number(doc, &e, "x", -SLIDE_W, SLIDE_W * 2.)?;
            number(doc, &e, "y", -SLIDE_H, SLIDE_H * 2.)?;
            let line = string(doc, &e, "kind")? == "line";
            let w = number(doc, &e, "w", if line { 0. } else { 1. }, SLIDE_W * 2.)?;
            let h = number(doc, &e, "h", if line { 0. } else { 1. }, SLIDE_H * 2.)?;
            if w == 0. && h == 0. {
                return Err("Zero-length line".into());
            }
            number(doc, &e, "font_size", 4., 256.)?;
            number(doc, &e, "stroke_width", 0., 64.)?;
            number(doc, &e, "radius", 0., 720.)?;
        }
    }
    let projected = crate::doc::SlidesDoc::load(doc).ok_or("Recorded deck is unreadable")?;
    if !crate::actions::deck_within_aggregate_limits(&projected) {
        return Err("Recorded deck exceeds safe aggregate limits".into());
    }
    Ok(())
}

/// Resolves exact recorded content identities only, outside the renderer.
/// Recorded images come from the app's own content store: the same directory
/// the running app imports into, resolved the same way.
pub(crate) struct DeckAssets {
    content: ContentStore,
}
impl DeckAssets {
    pub(crate) fn new(content: ContentStore) -> Self {
        Self { content }
    }
}
impl HistoricalAssetSource for DeckAssets {
    fn request(&self, request: HistoricalAssetRequest, response: HistoricalAssetResponse) {
        let result = (|| {
            let doc = AutoCommit::load(request.snapshot.bytes()).map_err(|e| e.to_string())?;
            validate(&doc)?;
            let mut assets = BTreeMap::new();
            let mut total = 0usize;
            if let Some(deck) = crate::doc::SlidesDoc::load(&doc) {
                for e in deck.slides.iter().flat_map(|s| &s.elements) {
                    if e.kind != crate::model::ElementKind::Image
                        || !crate::assets::is_asset_id(&e.src)
                        || assets.contains_key(&e.src)
                    {
                        continue;
                    }
                    if let Some(bytes) = crate::assets::read_asset(&self.content, &e.src) {
                        total = total.saturating_add(bytes.len());
                        if total > 128 * 1024 * 1024 {
                            return Err("Recorded images exceed preview byte limit".into());
                        }
                        assets.insert(e.src.clone(), bytes.into());
                    }
                }
            }
            Ok(assets)
        })();
        response.complete(result);
    }
}
struct FrozenImages(BTreeMap<String, ImageState>);
impl ImageLookup for FrozenImages {
    fn get(&self, id: &str) -> ImageState {
        self.0.get(id).cloned().unwrap_or(ImageState::Failed)
    }
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&doc)?;
    let deck = crate::doc::SlidesDoc::load(&doc);
    let mut images = BTreeMap::new();
    let mut decoded_bytes = 0usize;
    if let Some(deck) = &deck {
        for e in deck.slides.iter().flat_map(|s| &s.elements) {
            if e.kind != crate::model::ElementKind::Image || images.contains_key(&e.src) {
                continue;
            }
            let state = input
                .assets
                .get(&e.src)
                .map_or(ImageState::Failed, |bytes| {
                    crate::assets::decode_bytes_with_budget(
                        bytes,
                        (96usize * 1024 * 1024).saturating_sub(decoded_bytes),
                    )
                });
            if let ImageState::Ready(image) = &state {
                decoded_bytes = decoded_bytes.saturating_add(image.data.len());
            }
            images.insert(e.src.clone(), state);
        }
    }
    let pages = deck
        .as_ref()
        .map(|d| {
            d.slides
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    (
                        s.id.clone(),
                        format!("Slide {} of {}", i + 1, d.slides.len()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut p = DeckPreview {
        deck,
        images: FrozenImages(images),
        pages,
        view: input.initial_view,
        bounds: Rect::ZERO,
        slide_rect: Rect::ZERO,
        notes_height: 0.,
        notes_width: 0.,
    };
    if !p
        .pages
        .iter()
        .any(|(id, _)| Some(id) == p.view.page.as_ref())
    {
        p.view.page = p.pages.first().map(|(id, _)| id.clone());
    }
    if !p.view.zoom.is_finite() {
        p.view.zoom = 1.;
    }
    p.view.zoom = p.view.zoom.clamp(0.25, 4.);
    Ok(Box::new(p))
}
struct DeckPreview {
    deck: Option<Deck>,
    images: FrozenImages,
    pages: Vec<(String, String)>,
    view: PreviewView,
    bounds: Rect,
    slide_rect: Rect,
    notes_height: f64,
    notes_width: f64,
}
impl DeckPreview {
    fn selected(&self) -> Option<&crate::model::Slide> {
        self.deck
            .as_ref()?
            .slides
            .iter()
            .find(|s| Some(&s.id) == self.view.page.as_ref())
    }
    fn notes(&self) -> String {
        self.selected()
            .map_or_else(String::new, |s| s.notes.clone())
    }
    fn clamp(&mut self) {
        let width = (self.bounds.width() - 32.).max(1.) * self.view.zoom;
        let height = width * SLIDE_H / SLIDE_W;
        let max_x = (width + 32. - self.bounds.width()).max(0.);
        let max_y = (height + self.notes_height + 80. - self.bounds.height()).max(0.);
        self.view.scroll.0 = if self.view.scroll.0.is_finite() {
            self.view.scroll.0.clamp(0., max_x)
        } else {
            0.
        };
        self.view.scroll.1 = if self.view.scroll.1.is_finite() {
            self.view.scroll.1.clamp(0., max_y)
        } else {
            0.
        };
        let x = self.bounds.x0 + 16. - self.view.scroll.0;
        let y = self.bounds.y0 + 16. - self.view.scroll.1;
        self.slide_rect = Rect::new(x, y, x + width, y + height);
    }
}
impl HistoryPreview for DeckPreview {
    fn layout(&mut self, bounds: Rect, tr: &mut TextRenderer, theme: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        self.notes_width = (bounds.width() - 32.).max(1.);
        let label = f64::from(theme.text(TextRole::Subheading).line_height_px());
        self.notes_height = label
            + f64::from(
                tr.measure_style(
                    &self.notes(),
                    Some(self.notes_width as f32),
                    &theme.text(TextRole::Body),
                )
                .1,
            );
        self.clamp();
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.,
            transform,
            &self.bounds,
        );
        if let (Some(deck), Some(slide)) = (&self.deck, self.selected()) {
            crate::render::draw_slide(
                &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
                crate::render::DeckAssets {
                    palette: &Palette::resolve(deck.palette, theme),
                    images: &self.images,
                },
                slide,
                self.slide_rect,
                8.,
            );
            let notes = self.notes();
            let x = self.bounds.x0 + 16.;
            let y = self.slide_rect.y1 + 24.;
            let mut ctx = ui::paint::PaintCtx::new(scene, transform, tr, theme);
            ctx.text_role(
                &crate::strings::tr("slides-speaker-notes"),
                TextRole::Subheading,
                theme.text_primary,
                Point::new(x, y),
                None,
            );
            let label = ctx.line_box(TextRole::Subheading);
            let (notes, ink) = if notes.is_empty() {
                (crate::strings::tr("slides-no-notes"), theme.text_dim)
            } else {
                (notes, theme.text_primary)
            };
            ctx.text_role(
                &notes,
                TextRole::Body,
                ink,
                Point::new(x, y + label),
                Some(self.notes_width as f32),
            );
        } else {
            let message = if self.deck.is_some() {
                crate::strings::tr("slides-history-no-slides")
            } else {
                crate::strings::tr("slides-history-no-deck")
            };
            ui::paint::PaintCtx::new(scene, transform, tr, theme).text_role(
                &message,
                TextRole::Body,
                theme.text_dim,
                Point::new(self.bounds.x0 + 16., self.bounds.y0 + 16.),
                Some((self.bounds.width() - 32.).max(1.) as f32),
            );
        }
        scene.pop_layer();
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        if let PreviewNavigation::Zoom { factor, anchor } = command {
            if !factor.is_finite() || factor <= 0. || !anchor.x.is_finite() || !anchor.y.is_finite()
            {
                return false;
            }
            self.view.zoom = (self.view.zoom * factor).clamp(0.25, 4.);
        } else if !ui::history_mode::adapter::navigate_pages(&mut self.view, command, &self.pages) {
            return false;
        }
        self.clamp();
        true
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let description = match (&self.deck, self.selected()) {
            (Some(deck), Some(slide)) => {
                let label = self
                    .pages
                    .iter()
                    .find(|(id, _)| id == &slide.id)
                    .map_or("Slide", |(_, label)| label.as_str());
                let elements = slide
                    .elements
                    .iter()
                    .map(|e| {
                        if e.kind == crate::model::ElementKind::Image {
                            if matches!(self.images.get(&e.src), ImageState::Ready(_)) {
                                "Recorded image".into()
                            } else {
                                "Recorded image unavailable".into()
                            }
                        } else {
                            e.text.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "{} — {label}\n{elements}\nSpeaker notes\n{}",
                    deck.title, slide.notes
                )
            }
            (Some(deck), None) => format!(
                "{} — Empty presentation — no slides in this version.",
                deck.title
            ),
            _ => "No deck recorded in this version. Restore is unavailable.".into(),
        };
        PreviewAccessibility {
            description,
            pages: self.pages.clone(),
        }
    }
}

//! The deck as Slides stores it, and the operations on it that are more
//! than editing one field.
//!
//! [`Deck`] is the record: its title and theme, and its slides in one
//! [`Collection`] whose order is the deck's. Each slide holds its elements
//! in a collection of its own whose order is the stacking order, back to
//! front. A text box's words are rich text, edited by field edits at their
//! path (`Deck::paths().slides.item(&slide).elements.item(&element).body`);
//! an element's place, size and style, a slide's layout, background and
//! speaker notes, the deck's title and theme are plain field edits. The
//! verbs below are what is more than that: adding, copying, moving and
//! deleting slides and elements, restacking, and replacing the whole deck
//! with a presentation file.
//!
//! The window works on [`crate::model::Deck`], the deck as it draws it,
//! made from this record by [`mirror`].

use serde::{Deserialize, Serialize};
use tilo_document::{ClipBlock, RichText, RichTextAction};
use ui::tilo_resource::{self, ActionCx, Atom, Collection, Id, Model, Refusal, resource};
use ui::widget::BlockContent;
use ui::widget::marks::Mark;

use crate::model::{
    Background, ElementKind, FILL_ROLES, Layout, PaletteKind, SLIDE_H, SLIDE_W, STROKE_ROLES,
    TextAlign,
};

/// The most slides one deck holds.
pub const MAX_SLIDES: usize = 1_000;
/// The most elements one slide holds.
pub const MAX_ELEMENTS_PER_SLIDE: usize = 10_000;

/// A deck.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Deck {
    /// Title
    #[model(label)]
    pub title: String,
    /// Theme
    pub palette: PaletteKind,
    /// Slides
    pub slides: Collection<Slide>,
}

/// One slide.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Slide {
    /// Layout
    pub layout: Layout,
    /// Background
    pub background: Background,
    /// Speaker notes
    pub notes: String,
    /// Elements, back to front.
    pub elements: Collection<Element>,
}

/// A text box, shape, line or picture on a slide.
#[derive(Model, Default, Clone, Debug, PartialEq)]
#[model(check = Element::valid)]
pub struct Element {
    /// Kind
    #[model(readonly)]
    pub kind: ElementKind,
    /// Left, in slide units (the slide is 1280 × 720).
    pub x: f64,
    /// Top
    pub y: f64,
    /// Width
    pub w: f64,
    /// Height
    pub h: f64,
    /// Text: a text box's words, read whole: a slide draws all of them.
    #[model(label)]
    #[model(whole)]
    pub body: RichText,
    /// Picture: the content id of a picture element.
    pub src: Atom,
    /// Fill colour: a role of the deck's theme, `none` or `#RRGGBB`.
    pub fill: Atom,
    /// Outline colour
    pub stroke: Atom,
    /// Outline width
    pub stroke_width: f64,
    /// Font size
    pub font_size: f64,
    /// Alignment
    pub align: TextAlign,
    /// Bold, for an element without words of its own; a text box's words
    /// carry their own marks.
    pub bold: bool,
    /// Italic
    pub italic: bool,
    /// Corner radius
    pub radius: f64,
}

impl Element {
    /// An element whose place and style are ones the inspector can make.
    fn valid(&self) -> Result<(), Refusal> {
        if !valid_geometry(self.kind, self.x, self.y, self.w, self.h) {
            return Err(Refusal::new("the element's place or size is off the slide"));
        }
        let style_ok = exact_paint(&self.fill.0, true)
            && (STROKE_ROLES.contains(&self.stroke.0.as_str())
                || exact_paint(&self.stroke.0, true))
            && self.stroke_width.is_finite()
            && (0.0..=64.0).contains(&self.stroke_width)
            && self.font_size.is_finite()
            && (4.0..=256.0).contains(&self.font_size)
            && self.radius.is_finite()
            && (0.0..=720.0).contains(&self.radius)
            && self.src.0.len() <= 4_096
            && (self.kind == ElementKind::Image) != self.src.0.is_empty()
            && (self.kind != ElementKind::Image || crate::assets::is_asset_id(&self.src.0));
        if style_ok {
            Ok(())
        } else {
            Err(Refusal::new("the element's style or picture is invalid"))
        }
    }
}

/// An element as a verb carries it whole: the window's paste, a slide's
/// copy, a file's import. `blocks` are a text box's words as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct ElementData {
    /// The id it is kept under.
    pub id: String,
    pub kind: ElementKind,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub blocks: Vec<ClipBlock>,
    #[serde(default)]
    pub src: String,
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub font_size: f64,
    pub align: TextAlign,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    pub radius: f64,
}

/// An element an agent adds: its kind and place, its words or picture, and
/// any style other than its kind's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct NewElement {
    /// What to add.
    pub kind: ElementKind,
    /// Left edge, in slide units (the slide is 1280 wide).
    pub x: f64,
    /// Top edge, in slide units (the slide is 720 high).
    pub y: f64,
    /// Width, in slide units.
    pub w: f64,
    /// Height, in slide units.
    pub h: f64,
    /// A text box's words, a paragraph per line.
    #[serde(default)]
    pub text: Option<String>,
    /// A picture's content id.
    #[serde(default)]
    pub src: Option<String>,
    /// Fill colour, and a text box's text colour: a palette role (accent,
    /// accent-soft, accent-deep, surface, surface-dim, deep, text, text-dim,
    /// on-accent, white, success, warning, danger, info), `#RRGGBB`, or
    /// `none`.
    #[serde(default)]
    pub fill: Option<String>,
    /// A text box's text colour: the same value as `fill`, by its text name.
    #[serde(default)]
    pub text_color: Option<String>,
    /// Outline colour: `none`, a palette role or `#RRGGBB`.
    #[serde(default)]
    pub stroke: Option<String>,
    /// Outline width in slide units, 0 to 64.
    #[serde(default)]
    pub stroke_width: Option<f64>,
    /// Font size in slide units, 4 to 256.
    #[serde(default)]
    pub font_size: Option<f64>,
    /// Text alignment.
    #[serde(default)]
    pub align: Option<TextAlign>,
    /// Bold text.
    #[serde(default)]
    pub bold: Option<bool>,
    /// Italic text.
    #[serde(default)]
    pub italic: Option<bool>,
    /// Corner radius of a rectangle, in slide units, 0 to 720.
    #[serde(default)]
    pub radius: Option<f64>,
}

impl NewElement {
    /// The element as a verb carries it, refused when its words, picture or
    /// colours do not fit its kind.
    fn data(&self) -> Result<ElementData, Refusal> {
        let text = self.text.clone().unwrap_or_default();
        let src = self.src.clone().unwrap_or_default();
        if !text.is_empty() && self.kind != ElementKind::Text {
            return Err(Refusal::new("only a text element takes `text`"));
        }
        if self.kind != ElementKind::Image && !src.is_empty() {
            return Err(Refusal::new("only an image element takes `src`"));
        }
        if self.kind == ElementKind::Image && !crate::assets::is_asset_id(&src) {
            return Err(Refusal::new(
                "an image needs `src`, the content id of its picture",
            ));
        }
        let fill = match (&self.fill, &self.text_color) {
            (Some(fill), Some(color)) if fill != color => {
                return Err(Refusal::new(
                    "give `fill` or `text_color`, not two different colours",
                ));
            }
            (fill, color) => fill.clone().or_else(|| color.clone()),
        };
        let mut element = crate::model::Element::of_kind(
            String::new(),
            self.kind,
            self.x,
            self.y,
            self.w,
            self.h,
        );
        element.text = text;
        element.src = src;
        if let Some(fill) = fill {
            element.fill = fill;
        }
        if let Some(stroke) = &self.stroke {
            element.stroke.clone_from(stroke);
        }
        element.stroke_width = self.stroke_width.unwrap_or(element.stroke_width);
        element.font_size = self.font_size.unwrap_or(element.font_size);
        element.align = self.align.unwrap_or(element.align);
        element.bold = self.bold.unwrap_or(element.bold);
        element.italic = self.italic.unwrap_or(element.italic);
        element.radius = self.radius.unwrap_or(element.radius);
        Ok(ElementData::of(&element))
    }
}

/// A slide as a verb carries it whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct SlideData {
    /// The id it is kept under.
    pub id: String,
    pub layout: Layout,
    pub background: Background,
    #[serde(default)]
    pub notes: String,
    /// Back to front.
    #[serde(default)]
    pub elements: Vec<ElementData>,
}

impl ElementData {
    /// `element` of the window's deck as a verb carries it.
    #[must_use]
    pub fn of(element: &crate::model::Element) -> Self {
        let text = element.kind == ElementKind::Text;
        Self {
            id: element.id.clone(),
            kind: element.kind,
            x: element.x,
            y: element.y,
            w: element.w,
            h: element.h,
            blocks: if text {
                element
                    .rich_blocks()
                    .iter()
                    .map(tilo_rich_text::clip_of_content)
                    .collect()
            } else {
                Vec::new()
            },
            src: element.src.clone(),
            fill: element.fill.clone(),
            stroke: element.stroke.clone(),
            stroke_width: element.stroke_width,
            font_size: element.font_size,
            align: element.align,
            bold: element.bold && !text,
            italic: element.italic && !text,
            radius: element.radius,
        }
    }

    /// The element as the deck stores it.
    #[must_use]
    pub fn stored(&self) -> Element {
        let mut body = RichText::default();
        if self.kind == ElementKind::Text {
            body.perform(RichTextAction::ReplaceClips {
                blocks: self.blocks.clone(),
            });
        }
        Element {
            kind: self.kind,
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
            body,
            src: Atom(self.src.clone()),
            fill: Atom(self.fill.clone()),
            stroke: Atom(self.stroke.clone()),
            stroke_width: self.stroke_width,
            font_size: self.font_size,
            align: self.align,
            bold: self.bold,
            italic: self.italic,
            radius: self.radius,
        }
    }
}

impl SlideData {
    /// `slide` of the window's deck as a verb carries it.
    #[must_use]
    pub fn of(slide: &crate::model::Slide) -> Self {
        Self {
            id: slide.id.clone(),
            layout: slide.layout,
            background: slide.background,
            notes: slide.notes.clone(),
            elements: slide.z_sorted().into_iter().map(ElementData::of).collect(),
        }
    }

    /// This slide with fresh ids for it and its elements, minted by `cx`.
    fn reminted(&self, cx: &mut ActionCx) -> Self {
        let mut copy = self.clone();
        copy.id = cx.uuid().to_string();
        for element in &mut copy.elements {
            element.id = cx.uuid().to_string();
        }
        copy
    }

    /// The slide as the deck stores it, and its id.
    fn stored(&self) -> Result<(Id, Slide), Refusal> {
        if !valid_id(&self.id) {
            return Err(Refusal::new("a slide's id is not blank"));
        }
        if self.elements.len() > MAX_ELEMENTS_PER_SLIDE {
            return Err(Refusal::new("a slide holds at most 10000 elements"));
        }
        let mut elements = Collection::new();
        for element in &self.elements {
            if !valid_id(&element.id) || elements.contains(&Id::from(element.id.as_str())) {
                return Err(Refusal::new("a slide names an element twice"));
            }
            let stored = element.stored();
            tilo_resource::Model::check(&stored)?;
            elements.push(Id::from(element.id.as_str()), stored);
        }
        Ok((
            Id::from(self.id.as_str()),
            Slide {
                layout: self.layout,
                background: self.background,
                notes: self.notes.clone(),
                elements,
            },
        ))
    }
}

#[resource(
    kind = "slides.deck",
    label = "Deck",
    initialize = Deck::starter,
    search = crate::search::entries
)]
impl Deck {
    /// Add slide
    ///
    /// A new slide after `after` (last when none), its title and body text
    /// boxes holding `title` and `body` when given. A `title` layout makes a
    /// cover slide.
    #[action]
    pub fn add_slide(
        &mut self,
        cx: &mut ActionCx,
        #[param("Title")] title: Option<String>,
        #[param("Body, a paragraph per line")] body: Option<String>,
        #[param("Layout")] layout: Option<Layout>,
        #[param("The slide it goes after; last when none")] after: Option<Id>,
    ) -> Result<(), Refusal> {
        let slide = crate::actions::authored_slide(title.as_deref(), body.as_deref(), layout);
        let data = SlideData::of(&slide).reminted(cx);
        let at = match &after {
            Some(after) => self
                .slides
                .position(after)
                .ok_or("the slide it goes after is gone")?
                .saturating_add(1),
            None => self.slides.len(),
        };
        self.insert(at, &data)
    }

    /// Insert slide
    ///
    /// The slide `slide`, whole, at position `at` (counting from 0): the
    /// window's new and pasted slides, under the ids they carry.
    #[action]
    pub fn insert_slide(
        &mut self,
        #[param("Position, counting from 0")] at: usize,
        #[param("The slide")] slide: SlideData,
    ) -> Result<(), Refusal> {
        self.insert(at, &slide)
    }

    /// Duplicate slide
    ///
    /// A copy of the slide, right after it.
    #[action(item = slides)]
    pub fn duplicate_slide(&mut self, cx: &mut ActionCx, slide: &Id) -> Result<(), Refusal> {
        let at = self.slides.position(slide).ok_or("no such slide")?;
        let original = self.slides.get(slide).ok_or("no such slide")?.clone();
        let mut copy = original;
        let mut elements = Collection::new();
        for (_, element) in copy.elements.iter() {
            elements.push(Id::from(cx.uuid().to_string()), copied(element));
        }
        copy.elements = elements;
        if self.slides.len() >= MAX_SLIDES {
            return Err(Refusal::new("a deck holds at most 1000 slides"));
        }
        self.slides
            .insert(at.saturating_add(1), Id::from(cx.uuid().to_string()), copy);
        Ok(())
    }

    /// Delete slide
    #[action(item = slides, destructive)]
    pub fn delete_slide(&mut self, slide: &Id) -> Result<(), Refusal> {
        if !self.slides.contains(slide) {
            return Err(Refusal::new("no such slide"));
        }
        if self.slides.len() <= 1 {
            return Err(Refusal::new("a deck keeps at least one slide"));
        }
        self.slides.remove(slide);
        Ok(())
    }

    /// Move slide
    ///
    /// Put the slide at `position`, counting from 0 for the first slide.
    #[action(item = slides)]
    pub fn move_slide(
        &mut self,
        slide: &Id,
        #[param("Where the slide ends up, counting from 0")] position: usize,
    ) -> Result<(), Refusal> {
        if position >= self.slides.len() {
            return Err(Refusal::new("the deck has no slide at that position"));
        }
        if !self.slides.move_to(slide, position) {
            return Err(Refusal::new("no such slide"));
        }
        Ok(())
    }

    /// Set slide text
    ///
    /// Replace the words of the slide's title (its first text box) and/or
    /// body (its second) as plain text, a paragraph per line, keeping their
    /// place and style.
    #[action(item = slides)]
    pub fn set_text(
        &mut self,
        slide: &Id,
        #[param("New title")] title: Option<String>,
        #[param("New body, a paragraph per line")] body: Option<String>,
    ) -> Result<(), Refusal> {
        let target = self.slides.get_mut(slide).ok_or("no such slide")?;
        let boxes: Vec<Id> = target
            .elements
            .iter()
            .filter(|(_, element)| element.kind == ElementKind::Text)
            .map(|(id, _)| id.clone())
            .collect();
        let mut writes = Vec::new();
        for (index, text) in [title, body].into_iter().enumerate() {
            let Some(text) = text else { continue };
            if text.trim().is_empty() {
                return Err(Refusal::new(
                    "slide text is not blank; delete-element removes a text box",
                ));
            }
            let element = boxes.get(index).ok_or(if index == 0 {
                "the slide has no text box for a title; add one with add-element"
            } else {
                "the slide has no second text box for a body; add one with add-element"
            })?;
            writes.push((element.clone(), text));
        }
        if writes.is_empty() {
            return Err(Refusal::new("give a title, a body or both"));
        }
        for (element, text) in writes {
            if let Some(element) = target.elements.get_mut(&element) {
                element.body.perform(RichTextAction::ReplaceClips {
                    blocks: crate::model::paragraphs_of(&text, false, false)
                        .iter()
                        .filter(|block| !block.text.trim().is_empty())
                        .map(tilo_rich_text::clip_of_content)
                        .collect(),
                });
            }
        }
        Ok(())
    }

    /// Add element
    ///
    /// A text box, shape, line or picture on the slide, above everything on
    /// it, styled as its kind is unless `element` names a style.
    #[action(item = slides)]
    pub fn add_element(
        &mut self,
        cx: &mut ActionCx,
        slide: &Id,
        #[param("What to add, where, and any style")] element: NewElement,
    ) -> Result<(), Refusal> {
        let mut data = element.data()?;
        data.id = cx.uuid().to_string();
        self.place_element(slide, &data)
    }

    /// Insert element
    ///
    /// The element `element`, whole, above everything on the slide: the
    /// window's pasted and new elements, under the ids they carry.
    #[action(item = slides)]
    pub fn insert_element(
        &mut self,
        slide: &Id,
        #[param("The element")] element: ElementData,
    ) -> Result<(), Refusal> {
        self.place_element(slide, &element)
    }

    /// Delete element
    #[action(item = slides, destructive)]
    pub fn delete_element(
        &mut self,
        slide: &Id,
        #[param("The element")] element: Id,
    ) -> Result<(), Refusal> {
        let target = self.slides.get_mut(slide).ok_or("no such slide")?;
        target
            .elements
            .remove(&element)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such element"))
    }

    /// Bring to front
    #[action(item = slides)]
    pub fn bring_to_front(
        &mut self,
        slide: &Id,
        #[param("The element")] element: Id,
    ) -> Result<(), Refusal> {
        let target = self.slides.get_mut(slide).ok_or("no such slide")?;
        let top = target.elements.len();
        if !target.elements.move_to(&element, top) {
            return Err(Refusal::new("no such element"));
        }
        Ok(())
    }

    /// Send to back
    #[action(item = slides)]
    pub fn send_to_back(
        &mut self,
        slide: &Id,
        #[param("The element")] element: Id,
    ) -> Result<(), Refusal> {
        let target = self.slides.get_mut(slide).ok_or("no such slide")?;
        if !target.elements.move_to(&element, 0) {
            return Err(Refusal::new("no such element"));
        }
        Ok(())
    }

    /// Replace deck
    ///
    /// Replace the whole deck — its title, theme and every slide — with
    /// another: a presentation file opened into it.
    #[action]
    pub fn replace_deck(
        &mut self,
        #[param("Title")] title: String,
        #[param("Theme")] palette: PaletteKind,
        #[param("Slides, in order")] slides: Vec<SlideData>,
    ) -> Result<(), Refusal> {
        if slides.is_empty() {
            return Err(Refusal::new("a deck needs at least one slide"));
        }
        if slides.len() > MAX_SLIDES {
            return Err(Refusal::new("a deck holds at most 1000 slides"));
        }
        if !within_limits(&title, &slides) {
            return Err(Refusal::new(
                "the deck holds more elements, words or pictures than one deck can",
            ));
        }
        let mut replaced = Collection::new();
        for slide in &slides {
            let (id, stored) = slide.stored()?;
            if replaced.contains(&id) {
                return Err(Refusal::new("the deck names a slide twice"));
            }
            replaced.push(id, stored);
        }
        self.title = title;
        self.palette = palette;
        self.slides = replaced;
        Ok(())
    }
}

impl Deck {
    /// The deck every new record begins with, under fixed ids: the genesis
    /// is the same change on every device, so two people creating the
    /// room's deck before they sync converge on one first slide.
    #[must_use]
    pub fn starter() -> Self {
        let deck = crate::model::starter_deck();
        let mut serial = 0_u32;
        let mut next = || {
            serial = serial.saturating_add(1);
            format!("00000000-0000-4000-8000-{serial:012}")
        };
        let mut slides = Collection::new();
        for slide in &deck.slides {
            let mut data = SlideData::of(slide);
            data.id = next();
            for element in &mut data.elements {
                element.id = next();
            }
            if let Ok((id, stored)) = data.stored() {
                slides.push(id, stored);
            }
        }
        Self {
            title: deck.title,
            palette: deck.palette,
            slides,
        }
    }

    fn insert(&mut self, at: usize, slide: &SlideData) -> Result<(), Refusal> {
        if self.slides.len() >= MAX_SLIDES {
            return Err(Refusal::new("a deck holds at most 1000 slides"));
        }
        let (id, stored) = slide.stored()?;
        if self.slides.contains(&id) {
            return Err(Refusal::new("a slide of that id exists"));
        }
        self.slides.insert(at, id, stored);
        Ok(())
    }

    fn place_element(&mut self, slide: &Id, element: &ElementData) -> Result<(), Refusal> {
        let id = Id::from(element.id.as_str());
        if !valid_id(&element.id) {
            return Err(Refusal::new("an element's id is not blank"));
        }
        if self
            .slides
            .values()
            .any(|existing| existing.elements.contains(&id))
        {
            return Err(Refusal::new("an element of that id exists"));
        }
        let stored = element.stored();
        tilo_resource::Model::check(&stored)?;
        let target = self.slides.get_mut(slide).ok_or("no such slide")?;
        if target.elements.len() >= MAX_ELEMENTS_PER_SLIDE {
            return Err(Refusal::new("a slide holds at most 10000 elements"));
        }
        target.elements.push(id, stored);
        Ok(())
    }
}

/// A copy of `element` with its words as written.
fn copied(element: &Element) -> Element {
    let mut copy = element.clone();
    if element.kind == ElementKind::Text {
        let blocks = blocks_of(&element.body);
        copy.body = RichText::default();
        copy.body.perform(RichTextAction::ReplaceClips {
            blocks: blocks.iter().map(tilo_rich_text::clip_of_content).collect(),
        });
    }
    copy
}

/// The paragraphs of `body` as the editor shows them.
#[must_use]
pub fn blocks_of(body: &RichText) -> Vec<BlockContent> {
    let Some(state) = body.state() else {
        return Vec::new();
    };
    state
        .body()
        .all()
        .filter_map(|id| state.block(id))
        .map(|view| tilo_rich_text::content_of(view))
        .collect()
}

/// The deck as the window draws it.
#[must_use]
pub fn mirror(deck: &Deck) -> crate::model::Deck {
    crate::model::Deck {
        title: deck.title.clone(),
        palette: deck.palette,
        slides: deck
            .slides
            .iter()
            .map(|(id, slide)| crate::model::Slide {
                id: id.to_string(),
                layout: slide.layout,
                background: slide.background,
                notes: slide.notes.clone(),
                elements: slide
                    .elements
                    .iter()
                    .enumerate()
                    .map(|(z, (id, element))| mirror_element(id, element, z))
                    .collect(),
            })
            .collect(),
    }
}

fn mirror_element(id: &Id, element: &Element, z: usize) -> crate::model::Element {
    let mut mirrored = crate::model::Element::of_kind(
        id.to_string(),
        element.kind,
        element.x,
        element.y,
        element.w,
        element.h,
    );
    mirrored.src.clone_from(&element.src.0);
    mirrored.fill.clone_from(&element.fill.0);
    mirrored.stroke.clone_from(&element.stroke.0);
    mirrored.stroke_width = element.stroke_width;
    mirrored.font_size = element.font_size;
    mirrored.align = element.align;
    mirrored.radius = element.radius;
    mirrored.z = i64::try_from(z).unwrap_or(i64::MAX);
    if element.kind == ElementKind::Text {
        // The paragraphs keep the ids the record names them by, so a style
        // set over the box's words marks them where they are.
        let blocks = blocks_of(&element.body);
        mirrored.set_blocks(blocks.clone());
        mirrored.blocks = blocks;
    } else {
        mirrored.bold = element.bold;
        mirrored.italic = element.italic;
    }
    mirrored
}

/// The object text box `element` of slide `slide` keeps its words in, in
/// `doc`.
#[must_use]
pub fn body_object(
    doc: &tilo_resource::Doc,
    slide: &str,
    element: &str,
) -> Option<automerge::ObjId> {
    let deck = <Deck as tilo_resource::Resource>::read(doc).ok()?;
    let body = deck
        .slides
        .get(&Id::from(slide))?
        .elements
        .get(&Id::from(element))?
        .body
        .state()?;
    Some(body.containers().root.clone())
}

/// The rich-text op setting or clearing `mark` over every word of a text
/// box showing `blocks`.
#[must_use]
pub fn mark_all(blocks: &[BlockContent], mark: &Mark, on: bool) -> RichTextAction {
    let (name, value) = tilo_rich_text::document_mark(mark);
    RichTextAction::SetMark {
        region: tilo_document::action::RegionKind::Body,
        spans: blocks
            .iter()
            .filter(|block| !block.text.is_empty())
            .map(|block| tilo_document::action::Span {
                block: block.id.clone(),
                start: 0,
                end: block.text.chars().count(),
            })
            .collect(),
        name: name.to_owned(),
        value: on.then_some(value),
        also_clear: Vec::new(),
    }
}

/// Whether a deck titled `title` of `slides` stays within what one deck
/// holds, counted across the deck.
fn within_limits(title: &str, slides: &[SlideData]) -> bool {
    let mut elements = 0_usize;
    let mut text = title.chars().count();
    let mut assets = 0_usize;
    for slide in slides {
        elements = elements.saturating_add(slide.elements.len());
        text = text.saturating_add(slide.notes.chars().count());
        for element in &slide.elements {
            let words = element
                .blocks
                .iter()
                .map(|block| block.text.chars().count());
            text = text.saturating_add(words.sum());
            assets = assets.saturating_add(usize::from(element.kind == ElementKind::Image));
        }
    }
    elements <= crate::actions::MAX_TOTAL_ELEMENTS
        && text <= crate::actions::MAX_TOTAL_TEXT_CHARS
        && assets <= crate::actions::MAX_TOTAL_ASSET_REFS
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_whitespace)
}

/// Whether `value` is a colour the inspector offers: a role of the theme, a
/// canonical `#RRGGBB[AA]`, or `none` where that is allowed.
#[must_use]
pub fn exact_paint(value: &str, allow_none: bool) -> bool {
    (allow_none && value == "none")
        || FILL_ROLES.contains(&value)
        || matches!(
            value,
            "accent-deep" | "surface-dim" | "deep" | "text-dim" | "on-accent" | "white"
        )
        || (value.starts_with('#') && ui::color::parse_hex(value).is_some())
}

/// Whether an element of `kind` at `x, y` sized `w × h` lies within reach of
/// the slide.
#[must_use]
pub fn valid_geometry(kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> bool {
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

#[cfg(test)]
#[path = "deck/tests.rs"]
mod tests;

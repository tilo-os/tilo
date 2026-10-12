//! What the editor, the scenarios and the tests build to change the deck.
//!
//! Every change is the [`Deck`]'s action: a field edit at a path (an
//! element's place, size or style, a slide's layout, background or notes,
//! the deck's title and theme, the words of a text box) or one of its verbs
//! (adding, copying, moving and deleting slides and elements, restacking,
//! replacing the deck). The builders here name the ones the editor makes,
//! so the window, the scenarios and the tests say the same thing the same
//! way.

use tilo_actions::EncodedAction;
use ui::tilo_resource::{Atom, Edit, Id, Resource, Stamped, items};

use crate::deck::{self, Deck, DeckAction, ElementData, SlideData};
use crate::model::{Background, Element, ElementKind, Layout, PaletteKind, Slide, TextAlign};

/// The deck's action: a field edit or a verb, stamped with when it was
/// authored.
pub type Action = <Deck as Resource>::Action;

fn stamped(operation: DeckAction) -> Action {
    Stamped {
        operation,
        now_ms: 0,
    }
}

fn edit(edit: Edit<Deck>) -> Action {
    stamped(DeckAction::FieldEdit(edit))
}

/// The wire form of `action`, for applying it to a document directly.
///
/// # Panics
/// Never: a deck action is plain data that always serializes.
#[allow(
    clippy::expect_used,
    reason = "the deck's actions serialize with serde_json over owned plain data"
)]
#[must_use]
pub fn encode(action: &Action) -> EncodedAction {
    ui::tilo_resource::encode::<Deck>(action).expect("a deck action serializes")
}

/// The resource a slide is published as: its item of the deck.
#[must_use]
pub fn slide_resource_id(slide: &str) -> String {
    items::item_resource_id::<Deck>("slides", &Id::from(slide))
}

/// The resource an element is published as: its item, within its slide.
#[must_use]
pub fn element_resource_id(slide: &str, element: &str) -> String {
    items::nested_resource_id::<Deck>(
        &[("slides", Id::from(slide))],
        "elements",
        &Id::from(element),
    )
}

/// The paths of slide `slide`.
#[must_use]
pub fn slide_paths(slide: &str) -> <deck::Slide as ui::tilo_resource::Model>::Paths<Deck> {
    Deck::paths().slides.item(&Id::from(slide))
}

/// The paths of element `element` on slide `slide`.
#[must_use]
pub fn element_paths(
    slide: &str,
    element: &str,
) -> <deck::Element as ui::tilo_resource::Model>::Paths<Deck> {
    slide_paths(slide).elements.item(&Id::from(element))
}

/// Set the deck's title.
#[must_use]
pub fn set_deck_title_action(title: impl Into<String>) -> Action {
    edit(Deck::paths().title.replace(title.into()))
}

/// Set the deck's theme.
#[must_use]
pub fn set_deck_palette_action(palette: PaletteKind) -> Action {
    edit(Deck::paths().palette.set(palette))
}

/// Set slide `slide`'s layout.
#[must_use]
pub fn set_slide_layout_action(slide: &str, layout: Layout) -> Action {
    edit(slide_paths(slide).layout.set(layout))
}

/// Set slide `slide`'s background.
#[must_use]
pub fn set_slide_background_action(slide: &str, background: Background) -> Action {
    edit(slide_paths(slide).background.set(background))
}

/// Put slide `slide` at `position`, counting from 0.
#[must_use]
pub fn move_slide_action(slide: &str, position: usize) -> Action {
    stamped(DeckAction::MoveSlide(deck::deck::MoveSlide {
        slide: Id::from(slide),
        position,
    }))
}

/// Insert slide `slide`, whole, at `at`.
#[must_use]
pub fn insert_slide_action(at: usize, slide: &Slide) -> Action {
    stamped(DeckAction::InsertSlide(deck::deck::InsertSlide {
        at,
        slide: SlideData::of(slide),
    }))
}

/// Delete slide `slide`.
#[must_use]
pub fn delete_slide_action(slide: &str) -> Action {
    stamped(DeckAction::DeleteSlide(deck::deck::DeleteSlide {
        slide: Id::from(slide),
    }))
}

/// Replace the whole deck with `deck`.
#[must_use]
pub fn replace_deck_action(deck: &crate::model::Deck) -> Action {
    stamped(DeckAction::ReplaceDeck(deck::deck::ReplaceDeck {
        title: deck.title.clone(),
        palette: deck.palette,
        slides: deck.slides.iter().map(SlideData::of).collect(),
    }))
}

/// Put `element`, whole, on top of slide `slide`.
#[must_use]
pub fn add_element_action(slide: &str, element: &Element) -> Action {
    stamped(DeckAction::InsertElement(deck::deck::InsertElement {
        slide: Id::from(slide),
        element: ElementData::of(element),
    }))
}

/// Delete element `element` of slide `slide`.
#[must_use]
pub fn delete_element_action(slide: &str, element: &str) -> Action {
    stamped(DeckAction::DeleteElement(deck::deck::DeleteElement {
        slide: Id::from(slide),
        element: Id::from(element),
    }))
}

/// Bring element `element` of slide `slide` in front of every other, or
/// send it behind every other.
#[must_use]
pub fn restack_action(slide: &str, element: &str, front: bool) -> Action {
    let (slide, element) = (Id::from(slide), Id::from(element));
    stamped(if front {
        DeckAction::BringToFront(deck::deck::BringToFront { slide, element })
    } else {
        DeckAction::SendToBack(deck::deck::SendToBack { slide, element })
    })
}

/// Move element `element` so its top-left corner is at `x, y`.
#[must_use]
pub fn move_element_actions(slide: &str, element: &str, x: f64, y: f64) -> Vec<Action> {
    let paths = element_paths(slide, element);
    vec![edit(paths.x.set(x)), edit(paths.y.set(y))]
}

/// Place element `element` at `x, y`, sized `w × h`.
#[must_use]
pub fn resize_element_actions(
    slide: &str,
    element: &str,
    rect: (f64, f64, f64, f64),
) -> Vec<Action> {
    let paths = element_paths(slide, element);
    let (x, y, w, h) = rect;
    vec![
        edit(paths.x.set(x)),
        edit(paths.y.set(y)),
        edit(paths.w.set(w)),
        edit(paths.h.set(h)),
    ]
}

/// One style of element `element`, set.
#[derive(Clone, Debug, PartialEq)]
pub enum Style {
    Fill(String),
    Stroke(String),
    Align(TextAlign),
    FontSize(f64),
    StrokeWidth(f64),
    Radius(f64),
    Bold(bool),
    Italic(bool),
}

/// Set one style of element `element`. A text box's bold and italic are
/// marks over all its words (`blocks`, as the box shows them).
#[must_use]
pub fn restyle_action(slide: &str, element: &Element, style: &Style) -> Action {
    let paths = element_paths(slide, &element.id);
    let text = element.kind == ElementKind::Text;
    let marked = |mark: ui::widget::marks::Mark, on: bool| {
        paths
            .body
            .perform(&deck::mark_all(&element.rich_blocks(), &mark, on))
            .ok()
    };
    edit(match style {
        Style::Fill(fill) => paths.fill.set(Atom(fill.clone())),
        Style::Stroke(stroke) => paths.stroke.set(Atom(stroke.clone())),
        Style::Align(align) => paths.align.set(*align),
        Style::FontSize(size) => paths.font_size.set(*size),
        Style::StrokeWidth(width) => paths.stroke_width.set(*width),
        Style::Radius(radius) => paths.radius.set(*radius),
        Style::Bold(on) if text => match marked(ui::widget::marks::Mark::Bold, *on) {
            Some(edit) => edit,
            None => paths.bold.set(*on),
        },
        Style::Italic(on) if text => match marked(ui::widget::marks::Mark::Italic, *on) {
            Some(edit) => edit,
            None => paths.italic.set(*on),
        },
        Style::Bold(on) => paths.bold.set(*on),
        Style::Italic(on) => paths.italic.set(*on),
    })
}

/// One rich-text op on the words of text box `element` of slide `slide`.
///
/// # Panics
/// Never: a rich-text op is plain data that always serializes.
#[allow(
    clippy::expect_used,
    reason = "a rich-text op is plain data that always serializes"
)]
#[must_use]
pub fn edit_text_action(
    slide: &str,
    element: &str,
    op: impl std::borrow::Borrow<tilo_document::RichTextAction>,
) -> Action {
    edit(
        element_paths(slide, element)
            .body
            .perform(op.borrow())
            .expect("a rich-text op serializes"),
    )
}

/// Set the words of slide `slide`'s title and/or body text boxes.
#[must_use]
pub fn set_slide_text_action(slide: &str, title: Option<&str>, body: Option<&str>) -> Action {
    stamped(DeckAction::SetText(deck::deck::SetText {
        slide: Id::from(slide),
        title: title.map(str::to_owned),
        body: body.map(str::to_owned),
    }))
}

/// The template for a freshly-created slide, shared by the toolbar's "New slide"
/// button and the palette's "Add slide" so both insert the identical
/// structure. Fresh ids are minted on each call (a new slide is genuinely new).
pub fn new_slide_template() -> Slide {
    let mut s = Slide::new(Layout::TitleContent);
    s.add(Element::text("New slide", 96.0, 90.0, 1088.0, 76.0, 54.0).with(|e| e.bold = true));
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
/// a `title` layout, else [`new_slide_template`]. Omitted text keeps the
/// template's placeholder.
pub fn authored_slide(title: Option<&str>, body: Option<&str>, layout: Option<Layout>) -> Slide {
    let cover = (layout == Some(Layout::Title))
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
        if index == 1 && layout != Some(Layout::Title) {
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

/// A one-line label for an element: a text element shows its first line
/// (trimmed / capped), a shape shows its kind.
pub fn element_label(kind: ElementKind, text: &str) -> String {
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

/// The most elements, across slides, one deck holds.
pub const MAX_TOTAL_ELEMENTS: usize = 20_000;
/// The most characters of words and notes one deck holds.
pub const MAX_TOTAL_TEXT_CHARS: usize = 2_000_000;
/// The most pictures one deck shows.
pub const MAX_TOTAL_ASSET_REFS: usize = 2_048;

/// Whether `deck` stays within what one deck holds: its slides, elements,
/// words and pictures, counted across the deck.
#[must_use]
pub fn deck_within_aggregate_limits(deck: &crate::model::Deck) -> bool {
    if deck.slides.len() > deck::MAX_SLIDES {
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

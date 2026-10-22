//! Shared slide rendering. The same routine paints a slide into the
//! editor canvas, a thumbnail, and the full-screen present view — only
//! the destination rect changes. Everything is expressed in the slide's
//! 1280×720 coordinate space and scaled to the destination.

use vello::Scene;
use vello::kurbo::{Affine, Ellipse, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill, ImageBrush};

use ui::paint::PaintCtx;
use ui::text_render::TextStyle;
use vello::peniko::Color;

use std::cell::RefCell;
use std::collections::HashMap;

use tilo_rich_text::RichTextView;
use ui::widget::BlockEditor;

use crate::assets::{ImageLookup, ImageState};
use crate::model::{DeckLook, Element, ElementKind, Palette, SLIDE_W, Slide, TextAlign};
pub use ui::paint::with_alpha;

/// Map a point from slide space to destination pixels.
#[inline]
fn map(dest: Rect, s: f64, x: f64, y: f64) -> Point {
    Point::new(dest.x0 + x * s, dest.y0 + y * s)
}

/// The destination rect for an element, in pixels.
pub fn element_dest(dest: Rect, s: f64, e: &Element) -> Rect {
    Rect::new(
        dest.x0 + e.x * s,
        dest.y0 + e.y * s,
        dest.x0 + (e.x + e.w) * s,
        dest.y0 + (e.y + e.h) * s,
    )
}

/// The deck-wide values a slide's colour roles and image sources resolve
/// against.
#[derive(Clone, Copy)]
pub struct DeckAssets<'a> {
    pub palette: &'a Palette,
    pub images: &'a dyn ImageLookup,
    /// Where collaborators are in the deck's words, for the canvas to show
    /// their carets and selections; `None` where nobody's place is shown (a
    /// thumbnail, a presentation).
    pub presence: Option<&'a ui::presence::PresenceCtx>,
}

/// The theme a deck's words lay out and paint against: the deck's own
/// `look`, at unit text scale, never the reader's UI theme, so a slide
/// shows the same on every screen, in Present and in a written file. Only
/// the reader's own colour comes along, for their caret and selection.
/// Built once per look and colour on a thread.
pub fn words_theme(look: DeckLook, reader: &ui::theme::Theme) -> std::rc::Rc<ui::theme::Theme> {
    type Key = (DeckLook, ui::tokens::colors::PresenceHue);
    thread_local! {
        static THEMES: RefCell<Vec<(Key, std::rc::Rc<ui::theme::Theme>)>> =
            const { RefCell::new(Vec::new()) };
    }
    let key = (look, reader.tokens.colors.you);
    THEMES.with(|themes| {
        let mut themes = themes.borrow_mut();
        if let Some((_, theme)) = themes.iter().find(|(k, _)| *k == key) {
            return theme.clone();
        }
        let preset = match look {
            DeckLook::Light => ui::theme::Preset::Light,
            DeckLook::Dark => ui::theme::Preset::Dark,
        };
        let theme = std::rc::Rc::new(ui::theme::Theme::preset(preset).with_your_colour(key.1));
        themes.push((key, theme.clone()));
        theme
    })
}

/// The colour under the middle of text element `e`: the topmost filled
/// shape beneath it on the slide, else the slide's background.
fn ground_under(palette: &Palette, slide: &Slide, e: &Element) -> Color {
    let centre = Point::new(e.x + e.w / 2.0, e.y + e.h / 2.0);
    let mut ground = palette.slide_bg(slide.background);
    for other in slide.z_sorted() {
        if other.id == e.id {
            break;
        }
        let covers = match other.kind {
            ElementKind::Rect => {
                Rect::new(other.x, other.y, other.x + other.w, other.y + other.h).contains(centre)
            }
            ElementKind::Ellipse => {
                let (rx, ry) = (other.w / 2.0, other.h / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    false
                } else {
                    let dx = (centre.x - (other.x + rx)) / rx;
                    let dy = (centre.y - (other.y + ry)) / ry;
                    dx * dx + dy * dy <= 1.0
                }
            }
            ElementKind::Text | ElementKind::Line | ElementKind::Image => false,
        };
        if covers && let Some(fill) = palette.resolve_role(&other.fill) {
            ground = fill;
        }
    }
    ground
}

/// The ink a text element is drawn in. The `text` and `text-dim` roles
/// resolve against what the text actually stands on (a shape's fill, or the
/// slide's background), so a label on a node stays legible in every theme;
/// every other role is the colour it names.
pub fn text_ink(palette: &Palette, slide: &Slide, e: &Element) -> Color {
    match e.fill.as_str() {
        "text" => palette.ink_on(ground_under(palette, slide, e), false),
        "text-dim" => palette.ink_on(ground_under(palette, slide, e), true),
        role => palette.resolve_role(role).unwrap_or(palette.text),
    }
}

/// Paint one slide into `dest`. `corner` is the slide-card corner radius
/// in pixels (0 for present mode's full-bleed render).
pub fn draw_slide(
    ctx: &mut PaintCtx<'_>,
    assets: DeckAssets<'_>,
    slide: &Slide,
    dest: Rect,
    corner: f64,
) {
    draw_slide_except(ctx, assets, slide, dest, corner, None, false);
}

/// [`draw_slide`], leaving out the words of text element `editing`: the
/// canvas draws that box's editor in their place. With `prompts`, a text
/// box with no words yet shows a dimmed prompt for them (the editor's
/// canvas); a thumbnail or a presentation shows it empty.
pub fn draw_slide_except(
    ctx: &mut PaintCtx<'_>,
    assets: DeckAssets<'_>,
    slide: &Slide,
    dest: Rect,
    corner: f64,
    editing: Option<&str>,
    prompts: bool,
) {
    let (scene, transform) = (&mut *ctx.scene, ctx.transform);
    let s = dest.width() / SLIDE_W;
    // Text roles resolve against this slide's own background.
    let palette = &assets.palette.on(slide.background);
    let bg = palette.slide_bg(slide.background);
    let card = RoundedRect::from_rect(dest, corner);
    if corner > 0.0 {
        // A framed slide (the canvas, a thumbnail, a preview) is a card:
        // its ground with an edge derived from it, so a white slide on a
        // white window keeps its outline.
        ui::surface::card(scene, transform, dest, corner, bg);
    } else {
        // Full bleed while presenting.
        scene.fill(Fill::NonZero, transform, bg, None, &card);
    }

    // Clip element painting to the slide card so overflowing shapes stay
    // crisp at the rounded edge.
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &card);
    draw_decor(scene, transform, palette, slide, dest, s);
    let assets = DeckAssets {
        palette,
        images: assets.images,
        presence: assets.presence,
    };
    // A slide's words lay out and paint against the deck's look, sized in
    // slide units whatever the reader's text scale.
    let words_theme = words_theme(palette.look, ctx.theme);
    for el in slide.z_sorted() {
        if el.kind == ElementKind::Text {
            if editing == Some(el.id.as_str()) {
                continue;
            }
            if el.is_blank_text() && !prompts {
                continue;
            }
            let theme = &*words_theme;
            let prompt = (prompts && el.is_blank_text()).then(|| prompt_for(slide, el));
            let shown = prompt.as_ref().unwrap_or(el);
            let ink = text_ink(palette, slide, shown);
            draw_text_element(
                &mut PaintCtx::new(ctx.scene, ctx.transform, ctx.text, theme),
                (ink, assets.presence),
                shown,
                dest,
                s,
            );
            continue;
        }
        draw_element(ctx, assets, el, dest, s);
    }
    ctx.scene.pop_layer();
}

/// What text box `e` on `slide` asks for while it has no words: the slide's
/// largest text box asks for a title, any other for text.
pub fn prompt_text(slide: &Slide, e: &Element) -> String {
    let largest = slide
        .elements
        .iter()
        .filter(|other| other.kind == ElementKind::Text)
        .map(|other| other.font_size)
        .fold(0.0_f64, f64::max);
    crate::strings::tr(if e.font_size >= largest {
        "slides-prompt-title"
    } else {
        "slides-prompt-text"
    })
}

/// Text box `e`, which has no words yet, as the editor shows it: a dimmed
/// prompt for its words.
fn prompt_for(slide: &Slide, e: &Element) -> Element {
    let mut prompt = e.clone();
    prompt.text = prompt_text(slide, e);
    prompt.blocks.clear();
    prompt.fill = "text-dim".into();
    prompt
}

/// Per-layout chrome: a faint footer rule + slide-number dot region so a
/// blank slide still reads as "designed". Kept subtle.
fn draw_decor(
    scene: &mut Scene,
    transform: Affine,
    palette: &Palette,
    slide: &Slide,
    dest: Rect,
    s: f64,
) {
    use crate::model::Background;
    // Footer hairline on content (surface) slides.
    if slide.background == Background::Surface {
        let y = dest.y0 + 660.0 * s;
        let line = Line::new(
            map(dest, s, 96.0, 660.0),
            Point::new(dest.x0 + 1184.0 * s, y),
        );
        scene.stroke(
            &Stroke::new((1.5 * s).max(0.5)),
            transform,
            with_alpha(palette.text_dim, 38),
            None,
            &line,
        );
    }
}

fn draw_element(ctx: &mut PaintCtx<'_>, assets: DeckAssets<'_>, e: &Element, dest: Rect, s: f64) {
    let (scene, transform, palette) = (&mut *ctx.scene, ctx.transform, assets.palette);
    let er = element_dest(dest, s, e);
    match e.kind {
        ElementKind::Rect => {
            let shape = RoundedRect::from_rect(er, (e.radius * s).max(0.0));
            if let Some(fill) = palette.resolve_role(&e.fill) {
                scene.fill(Fill::NonZero, transform, fill, None, &shape);
            }
            if let Some(stroke) = palette.resolve_role(&e.stroke) {
                scene.stroke(
                    &Stroke::new((e.stroke_width * s).max(0.5)),
                    transform,
                    stroke,
                    None,
                    &shape,
                );
            }
        }
        ElementKind::Ellipse => {
            let c = er.center();
            let shape = Ellipse::new(c, (er.width() / 2.0, er.height() / 2.0), 0.0);
            if let Some(fill) = palette.resolve_role(&e.fill) {
                scene.fill(Fill::NonZero, transform, fill, None, &shape);
            }
            if let Some(stroke) = palette.resolve_role(&e.stroke) {
                scene.stroke(
                    &Stroke::new((e.stroke_width * s).max(0.5)),
                    transform,
                    stroke,
                    None,
                    &shape,
                );
            }
        }
        ElementKind::Line => {
            let line = Line::new(map(dest, s, e.x, e.y), map(dest, s, e.x + e.w, e.y + e.h));
            let stroke_col = palette
                .resolve_role(&e.stroke)
                .or_else(|| palette.resolve_role(&e.fill))
                .unwrap_or(palette.accent);
            scene.stroke(
                &Stroke::new((e.stroke_width.max(2.0) * s).max(1.0))
                    .with_caps(vello::kurbo::Cap::Round),
                transform,
                stroke_col,
                None,
                &line,
            );
        }
        ElementKind::Image => {
            draw_image_element(ctx, assets, e, er, s);
        }
        ElementKind::Text => {
            draw_text_element(
                ctx,
                (
                    palette.resolve_role(&e.fill).unwrap_or(palette.text),
                    assets.presence,
                ),
                e,
                dest,
                s,
            );
        }
    }
}

/// Paint an image element: the real decoded picture (object-fit *cover*,
/// clipped to the element's rounded rect) once it has loaded, or a light
/// loading / broken-file state until then.
fn draw_image_element(
    ctx: &mut PaintCtx<'_>,
    assets: DeckAssets<'_>,
    e: &Element,
    er: Rect,
    s: f64,
) {
    let PaintCtx {
        scene,
        transform,
        text: tr,
        ..
    } = ctx;
    let (transform, palette, images) = (*transform, assets.palette, assets.images);
    let radius = (e.radius.max(4.0) * s).max(2.0);
    let frame = RoundedRect::from_rect(er, radius);
    match images.get(&e.src) {
        ImageState::Ready(img) => {
            let (iw, ih) = (f64::from(img.width), f64::from(img.height));
            if iw <= 0.0 || ih <= 0.0 || er.width() <= 0.0 || er.height() <= 0.0 {
                return;
            }
            // Cover: scale so the image fills the box on its short axis, centre,
            // and clip the overflow to the rounded element rect.
            let scale = (er.width() / iw).max(er.height() / ih);
            let dw = iw * scale;
            let dh = ih * scale;
            let dx = er.x0 + (er.width() - dw) * 0.5;
            let dy = er.y0 + (er.height() - dh) * 0.5;
            let brush_transform =
                Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &frame);
            scene.fill(
                Fill::NonZero,
                transform,
                &ImageBrush::new(img),
                Some(brush_transform),
                &er,
            );
            scene.pop_layer();
        }
        ImageState::Loading => {
            scene.fill(Fill::NonZero, transform, palette.surface_dim, None, &frame);
            draw_placeholder_line(
                tr,
                scene,
                transform,
                palette,
                er,
                s,
                &crate::strings::tr("slides-image-loading"),
            );
        }
        ImageState::Failed => {
            scene.fill(Fill::NonZero, transform, palette.surface_dim, None, &frame);
            scene.stroke(
                &Stroke::new((1.5 * s).max(0.75)),
                transform,
                palette.failed,
                None,
                &frame,
            );
            draw_placeholder_line(
                tr,
                scene,
                transform,
                palette,
                er,
                s,
                &crate::strings::tr("slides-image-unavailable"),
            );
        }
    }
}

/// One line centred in an image element that has no picture to show, set at
/// the slide's own scale like the rest of its content.
fn draw_placeholder_line(
    tr: &mut ui::text_render::TextRenderer,
    scene: &mut Scene,
    transform: Affine,
    palette: &Palette,
    er: Rect,
    s: f64,
    message: &str,
) {
    // A caption-sized line in slide units, scaled with the slide.
    let style = TextStyle::content((24.0 * s) as f32);
    let (tw, th) = tr.measure_style(message, None, &style);
    tr.draw_style(
        scene,
        message,
        palette.text_dim,
        transform
            * Affine::translate((
                er.center().x - f64::from(tw) / 2.0,
                er.center().y - f64::from(th) / 2.0,
            )),
        None,
        &style,
    );
}

thread_local! {
    /// Each text element's words, laid out once at the slide's size and
    /// drawn scaled wherever the slide is painted (the canvas, a thumbnail,
    /// the presentation), so all of them wrap exactly alike.
    static TEXT_VIEWS: RefCell<HashMap<String, LaidText>> = RefCell::new(HashMap::new());
}

/// A text element's words as last laid out, and how tall they came to.
struct LaidText {
    view: RichTextView,
    /// In slide units.
    height: f64,
}

/// How tall text element `id`'s words came to when last laid out, in slide
/// units: what a box that its words outgrew grows to. `None` before it was
/// first drawn.
pub fn text_height(id: &str) -> Option<f64> {
    TEXT_VIEWS.with(|views| views.borrow().get(id).map(|laid| laid.height))
}

/// How tall text box `e` shows, in slide units: its own height, or its
/// words' where they outgrow it, set as the canvas sets them.
pub fn shown_text_height(e: &Element, tr: &mut ui::text_render::TextRenderer) -> f64 {
    let reader_scale = tr.text_scale();
    tr.set_text_scale(1.0);
    let mut view = RichTextView::with_editor(text_box_editor(e));
    view.show(&e.rich_blocks());
    let words = view.layout(text_box_rect(e), &ui::theme::Theme::dark(), tr);
    tr.set_text_scale(reader_scale);
    e.h.max(words)
}

/// Every text box's shown height ([`shown_text_height`]) across `deck`, by
/// element id: the frame a file written from the deck gives each box, so
/// another app shows the box as tall as Slides does.
pub fn shown_text_heights(deck: &crate::model::Deck) -> HashMap<String, f64> {
    let mut tr = ui::text_render::TextRenderer::new();
    deck.slides
        .iter()
        .flat_map(|slide| &slide.elements)
        .filter(|e| e.kind == ElementKind::Text)
        .map(|e| (e.id.clone(), shown_text_height(e, &mut tr)))
        .collect()
}

/// The key collaborators address a paragraph of a text box's words by
/// (`block` is its document id): the one the shared editor publishes a caret
/// under while the words are edited, so that caret shows wherever the words
/// are drawn.
pub fn words_presence_key(block: &str) -> ui::presence::PresenceKey {
    use ui::framework::ResourceApp;
    ui::framework::ResourceSelection::Item(block.to_owned())
        .presence::<tilo_document::RichText>(crate::resource::SlidesShell::ID)
        .0
}

/// The most text boxes whose layout is kept between frames.
const KEPT_TEXT_VIEWS: usize = 4_096;

/// The block editor a text box is laid out with, for its words: the same
/// for the read-only views and the canvas editor.
/// The editor a text box's words are set in. An empty box shows no hint of
/// its own: the canvas prompts for its words (see [`prompt_text`]).
pub fn text_box_editor(e: &Element) -> BlockEditor {
    BlockEditor::new()
        .with_empty_hint("")
        .with_wrap_lines(true)
        .with_compact_paragraph_spacing(true)
        .with_body_size(e.font_size as f32)
        .with_text_align(text_align(e.align))
}

/// A text box's alignment as the toolkit draws it.
pub fn text_align(align: TextAlign) -> ui::TextAlign {
    match align {
        TextAlign::Left => ui::TextAlign::Left,
        TextAlign::Center => ui::TextAlign::Center,
        TextAlign::Right => ui::TextAlign::Right,
        TextAlign::Justify => ui::TextAlign::Justified,
    }
}

/// Where a text box laid out at the slide's size is drawn: into `dest` at
/// scale `s`, its `content_h` of words centred vertically in the box (or
/// from its top when they overflow it).
pub fn text_box_transform(dest: Rect, s: f64, e: &Element, content_h: f64) -> Affine {
    let dy = ((e.h - content_h) / 2.0).max(0.0);
    Affine::translate((dest.x0, dest.y0)) * Affine::scale(s) * Affine::translate((0.0, dy))
}

/// A text box's own layout rectangle, in slide units.
pub fn text_box_rect(e: &Element) -> Rect {
    Rect::new(e.x, e.y, e.x + e.w.max(1.0), e.y)
}

/// Paint text element `e` in `color`, with the collaborators' carets and
/// selections `presence` places in its words.
fn draw_text_element(
    ctx: &mut PaintCtx<'_>,
    (color, presence): (Color, Option<&ui::presence::PresenceCtx>),
    e: &Element,
    dest: Rect,
    s: f64,
) {
    let PaintCtx {
        scene,
        transform,
        text: tr,
        theme,
    } = ctx;
    let (transform, theme) = (*transform, *theme);
    // Slide text is sized in slide units, whatever the reader's text scale.
    let reader_scale = tr.text_scale();
    tr.set_text_scale(1.0);
    TEXT_VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        if views.len() > KEPT_TEXT_VIEWS && !views.contains_key(&e.id) {
            views.clear();
        }
        let laid = views.entry(e.id.clone()).or_insert_with(|| {
            let mut view = RichTextView::with_editor(text_box_editor(e));
            view.editor_mut()
                .set_presence_key(std::rc::Rc::new(words_presence_key));
            LaidText { view, height: 0.0 }
        });
        let view = &mut laid.view;
        view.editor_mut().set_body_size(e.font_size as f32);
        view.editor_mut().set_text_align(text_align(e.align));
        view.set_text_color(Some(color));
        view.show(&e.rich_blocks());
        let content_h = view.layout(text_box_rect(e), theme, tr);
        view.set_transform(text_box_transform(dest, s, e, content_h));
        match presence {
            Some(presence) => view.draw_with_presence(scene, transform, tr, theme, presence),
            None => view.draw(scene, transform, tr, theme),
        }
        laid.height = content_h;
    });
    tr.set_text_scale(reader_scale);
}

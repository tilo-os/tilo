//! Exporting the artwork as a picture someone else can open: the format,
//! how large, how much of it, and whether the ground comes along. The
//! picture is written where the person chooses in Files.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::TextRenderer;
use ui::offscreen::pixels::PixelsRequest;
use ui::prelude::*;
use ui::vello::kurbo::Rect;

use super::caption;
use crate::model::{Artwork, Shape};
use crate::render::{Options, Renderer};
use crate::tools::Tools;
use crate::view_ops::Awaiting;

/// A picture format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Png,
    Jpeg,
    Webp,
    Svg,
}

impl Format {
    pub const ALL: [Self; 4] = [Self::Png, Self::Jpeg, Self::Webp, Self::Svg];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Svg => "svg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Webp => "WebP",
            Self::Svg => "SVG",
        }
    }

    /// Whether the format keeps transparency.
    pub fn transparent(self) -> bool {
        self != Self::Jpeg
    }

    /// Whether the format is pixels, which a size applies to.
    pub fn raster(self) -> bool {
        self != Self::Svg
    }
}

/// How much of the artwork is exported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Area {
    #[default]
    Whole,
    /// What the canvas shows now.
    View,
    /// The selected marks, cropped to them.
    Selection,
}

/// A JPEG export's quality, as the `export` action's default.
const JPEG_QUALITY: u8 = 90;

/// The sizes offered, as pixels per artwork unit.
pub const SCALES: [f64; 5] = [0.5, 1.0, 2.0, 3.0, 4.0];
/// The longest side a picture may have.
pub use crate::content_geometry::MAX_PIXELS;

/// What the export dialog is set to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub format: Format,
    pub scale: f64,
    pub area: Area,
    pub transparent: bool,
}

/// The dialog's choices, kept while the window is open.
#[derive(Clone)]
pub struct Choices {
    pub format: LocalState<Format>,
    pub scale: LocalState<usize>,
    pub area: LocalState<Area>,
    pub transparent: LocalState<bool>,
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            format: LocalState::new(Format::Png),
            scale: LocalState::new(1),
            area: LocalState::new(Area::Whole),
            transparent: LocalState::new(false),
        }
    }
}

impl Choices {
    pub fn settings(&self) -> Settings {
        Settings {
            format: self.format.get(),
            scale: SCALES.get(self.scale.get()).copied().unwrap_or(1.0),
            area: self.area.get(),
            transparent: self.transparent.get(),
        }
    }
}

/// The part of `art` an export of `area` covers: the artwork to draw and
/// the region of it, or why there is none.
pub fn part(
    art: &Artwork,
    area: Area,
    view: Option<Rect>,
    selection: &[String],
) -> Result<(Artwork, Rect), String> {
    match area {
        Area::Whole => Ok((art.clone(), art.bounds())),
        Area::View => {
            let shown = view
                .map(|view| view.intersect(art.bounds()))
                .filter(|shown| shown.width() >= 1.0 && shown.height() >= 1.0)
                .ok_or_else(|| ui::i18n::I18nManager::lookup_plain("canvas-export-no-view"))?;
            Ok((art.clone(), shown))
        }
        Area::Selection => {
            if selection.is_empty() {
                return Err(ui::i18n::I18nManager::lookup_plain(
                    "canvas-export-no-selection",
                ));
            }
            let part = crate::view_ops::part(art, selection)?;
            let bounds = part.bounds();
            Ok((part, bounds))
        }
    }
}

/// The picture's size in pixels.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a side clamped to 1..=MAX_PIXELS"
)]
pub fn pixels(region: Rect, scale: f64) -> (u32, u32) {
    let side = |units: f64| (units * scale).round().clamp(1.0, MAX_PIXELS) as u32;
    (side(region.width()), side(region.height()))
}

/// The exported file's bytes, once its pictures are rendered.
pub fn encode(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    region: Rect,
    settings: Settings,
) -> Result<Awaiting<Vec<u8>>, String> {
    Ok(match draw(renderer, text, art, region, settings)? {
        Drawn::Svg(drawing) => {
            let mut drawing = Some(drawing);
            Awaiting::polled(move || {
                if !drawing.as_ref()?.ready() {
                    return None;
                }
                drawing
                    .take()
                    .map(|drawing| Ok(drawing.finish().into_bytes()))
            })
        }
        Drawn::Raster(drawing) => Awaiting::new(vec![*drawing], move |pictures| {
            let [(rgba, width, height)] = <[crate::view_ops::Rgba; 1]>::try_from(pictures)
                .map_err(|_| "the picture went missing".to_owned())?;
            encode_raster(&rgba, width, height, settings.format)
        }),
    })
}

/// An export drawn: an SVG with the pictures it embeds on their way, or a
/// picture to render and then [`encode_raster`].
pub enum Drawn {
    Svg(Box<SvgDrawing>),
    Raster(Box<crate::view_ops::Drawing>),
}

/// Draw the export of `region` of `art`.
pub fn draw(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    region: Rect,
    settings: Settings,
) -> Result<Drawn, String> {
    let bare = settings.transparent && settings.format.transparent();
    if settings.format == Format::Svg {
        return Ok(Drawn::Svg(Box::new(draw_svg(
            renderer, text, art, !bare, false,
        ))));
    }
    if region.width().max(region.height()) * settings.scale > MAX_PIXELS {
        return Err(ui::i18n::I18nManager::format(
            "canvas-export-too-large",
            &[("pixels", MAX_PIXELS.to_string().into())],
        ));
    }
    let options = Options {
        bare,
        ..Options::default()
    };
    // Drawn and encoded as the `export` action does.
    Ok(Drawn::Raster(Box::new(crate::view_ops::draw_rgba(
        renderer,
        text,
        art,
        region,
        settings.scale,
        &options,
    ))))
}

/// A rendered export's RGBA as the file's bytes in `format`.
///
/// # Errors
/// Says why the encoder failed.
pub fn encode_raster(
    rgba: &[u8],
    width: u32,
    height: u32,
    format: Format,
) -> Result<Vec<u8>, String> {
    match format {
        Format::Png => crate::view_ops::encode_png(rgba, width, height),
        Format::Jpeg => crate::view_ops::encode_jpeg_at(rgba, width, height, JPEG_QUALITY),
        Format::Webp => crate::view_ops::encode_webp(rgba, width, height),
        Format::Svg => Ok(Vec::new()),
    }
}

/// About how many bytes the export of `region` of `art` comes to, without
/// making all of it: pictures up to `SAMPLED` pixels on a side are made
/// whole; a larger one is made at two small sizes, and the way its bytes
/// grew between them carries on to its own size (flat vector art grows
/// with its edges, a painting or photo with its area).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "byte counts of pictures are far inside f64's exact range"
)]
pub fn estimate(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    region: Rect,
    settings: Settings,
) -> Result<Awaiting<u64>, String> {
    let longest = region.width().max(region.height());
    if !settings.format.raster() || longest * settings.scale <= f64::from(SAMPLED) {
        return Ok(
            encode(renderer, text, art, region, settings)?.then(|bytes| Ok(bytes.len() as u64))
        );
    }
    let large = f64::from(SAMPLED) / longest;
    let small = large / 2.0;
    let mut at = |scale: f64| encode(renderer, text, art, region, Settings { scale, ..settings });
    let small_bytes = at(small)?;
    let large_bytes = at(large)?;
    Ok(small_bytes
        .and(large_bytes)
        .then(move |(small_bytes, large_bytes)| {
            let small_bytes = (small_bytes.len() as f64).max(1.0);
            let large_bytes = (large_bytes.len() as f64).max(1.0);
            let growth = (large_bytes / small_bytes).log2().clamp(0.8, 2.2);
            Ok((large_bytes * (settings.scale / large).powf(growth)) as u64)
        }))
}

/// The longest side, in pixels, of a picture [`estimate`] makes whole.
const SAMPLED: u32 = 512; // a sampling budget in pixels, not on-screen spacing

/// An SVG export drawn, with the pictures it embeds on their way: each
/// painted layer alone, and each picture as it shows in its frame. In a
/// browser page they come a few frames after they are asked for; the
/// document is written once [`ready`](Self::ready).
pub struct SvgDrawing {
    art: Artwork,
    ground: bool,
    outlines: std::collections::BTreeMap<String, ui::vello::kurbo::BezPath>,
    dabbed: std::collections::BTreeMap<String, (Vec<u8>, Rect)>,
    painted: Vec<(String, PixelsRequest)>,
    pictures: Vec<(String, PixelsRequest)>,
}

impl SvgDrawing {
    /// Whether every picture it embeds has come.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.painted
            .iter()
            .chain(&self.pictures)
            .all(|(_, pixels)| !pixels.is_pending())
    }

    /// The SVG document. A picture that did not come, or could not be
    /// encoded, is left out, as a layer that could not be painted is.
    #[must_use]
    pub fn finish(self) -> String {
        let rgba = |pixels: &PixelsRequest| crate::view_ops::rgba_of(pixels.take()).ok();
        let mut painted: std::collections::BTreeMap<String, Vec<u8>> = self
            .painted
            .iter()
            .filter_map(|(layer, pixels)| {
                let (rgba, width, height) = rgba(pixels)?;
                let png = crate::view_ops::encode_png(&rgba, width, height).ok()?;
                Some((layer.clone(), png))
            })
            .collect();
        let pictures = self
            .pictures
            .iter()
            .filter_map(|(mark, pixels)| {
                let (rgba, width, height) = rgba(pixels)?;
                Some((mark.clone(), crate::surface::embed(&rgba, width, height)?))
            })
            .collect();
        crate::svg::export(
            &self.art,
            self.ground,
            |layer| painted.remove(&layer.id),
            self.outlines,
            pictures,
            self.dabbed,
        )
    }
}

/// `art` as SVG markup, drawn: plain layers as vector art, painted layers
/// as pictures; with its ground when `ground`, every text as outlines when
/// `outline_all` (text set along a path always is).
pub fn draw_svg(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    ground: bool,
    outline_all: bool,
) -> SvgDrawing {
    let outlines = art
        .marks
        .iter()
        .filter(|mark| match &mark.shape {
            Shape::Text { path, .. } => outline_all || path.is_some(),
            _ => false,
        })
        .map(|mark| {
            let mut outline = ui::vello::kurbo::BezPath::new();
            for letter in crate::render::letters(&mark.shape, text) {
                outline.extend(letter.path);
            }
            (mark.id.clone(), outline)
        })
        .collect();
    let pictures = art
        .marks
        .iter()
        .filter(|mark| matches!(mark.shape, Shape::Image { .. }))
        .filter_map(|mark| {
            let drawing = crate::surface::embedded_drawing(renderer, text, art, mark)?;
            Some((mark.id.clone(), drawing.request()))
        })
        .collect();
    let dabbed = renderer.dabbed_pictures(art, 2.0);
    // The painted layers the document shows as pictures of their paint.
    let painted = art
        .layers
        .iter()
        .filter(|layer| {
            layer.visible
                && !layer.medium.is_empty()
                && crate::media::resolve(art, &layer.medium).is_some()
        })
        .map(|layer| {
            let options = Options {
                layers: Some(vec![layer.id.clone()]),
                bare: true,
                ..Options::default()
            };
            let drawing =
                crate::view_ops::draw_rgba(renderer, text, art, art.bounds(), 1.0, &options);
            (layer.id.clone(), drawing.request())
        })
        .collect();
    SvgDrawing {
        art: art.clone(),
        ground,
        outlines,
        dabbed,
        painted,
        pictures,
    }
}

/// The dialog's body: format, size, how much and the background, and what
/// that comes to.
pub fn body(cx: &ViewCx<crate::model::Artwork>, tools: &Tools) -> Element {
    let choices = tools.export.clone();
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let format = ui::widget::ToggleGroup::from_state(
        Format::ALL
            .iter()
            .map(|format| {
                (
                    *format,
                    ui::widget::toggle_group::Segment::new(format.label()),
                )
            })
            .collect(),
        &choices.format,
    )
    .with_id("canvas-export-format");
    let scale = ui::widget::ToggleGroup::from_state(
        SCALES
            .iter()
            .enumerate()
            .map(|(index, scale)| {
                (
                    index,
                    ui::widget::toggle_group::Segment::new(format!("{scale} times")),
                )
            })
            .collect(),
        &choices.scale,
    )
    .with_id("canvas-export-scale");
    // Exporting the selection needs one.
    let picked = !tools.selection.with(Vec::is_empty);
    if !picked && choices.area.get() == Area::Selection {
        choices.area.set(Area::Whole);
    }
    let area = ui::widget::ToggleGroup::from_state(
        [
            (Area::Whole, "canvas-export-whole", true),
            (Area::View, "canvas-export-view", true),
            (Area::Selection, "canvas-export-selection", picked),
        ]
        .into_iter()
        .map(|(area, id, enabled)| {
            (
                area,
                ui::widget::toggle_group::Segment::new(lookup(id)).with_enabled(enabled),
            )
        })
        .collect(),
        &choices.area,
    )
    .with_id("canvas-export-area");
    let transparent = ui::widget::Checkbox::from_state(&choices.transparent)
        .with_label(lookup("canvas-export-transparent"))
        .with_id("canvas-export-transparent");
    let summary = summary(cx, tools);
    let field = |label: &str, control: Element| -> Element {
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(caption(&lookup(label)))
                .push(control),
        )
    };
    // The picture as it will be, and what it comes to, beside the choices
    // that make it.
    let shown = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(preview(cx, tools))
        .push(Box::new(summary));
    let settings = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Sibling)
        .push(field("canvas-export-format-label", Box::new(format)))
        .push(field("canvas-export-size-label", Box::new(scale)))
        .push(field("canvas-export-area-label", Box::new(area)))
        .push(Box::new(transparent));
    // In a narrow window the choices go under the preview rather than
    // squeezing every option to an ellipsis beside it.
    Box::new(
        Row::new()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .with_align_items(ui::taffy::AlignItems::Start)
            .push(super::layout::width(
                Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(Box::new(shown)),
                ),
                |metrics| metrics.inspector_width,
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(ui::tokens::Metrics::BASE.min_pointer_target * 8.0)
                    .with_child(Box::new(settings)),
            )),
    )
}

/// The picture the export makes, as it is set now: the whole artwork,
/// the part in view or the selection, on its ground or on the squares
/// that say it has none.
fn preview(cx: &ViewCx<crate::model::Artwork>, tools: &Tools) -> Element {
    let choices = tools.export.clone();
    let reads = tools.clone();
    let cut: Rc<RefCell<Option<CutKey>>> = Rc::default();
    let framing: crate::thumb::Framing = Rc::new(move |art: &Rc<Artwork>| {
        let settings = choices.settings();
        let bare = settings.transparent && settings.format.transparent();
        let view = reads.visible.get();
        if settings.area != Area::Selection {
            let region = match settings.area {
                Area::View => view?.intersect(art.bounds()),
                _ => art.bounds(),
            };
            return (region.width() >= 1.0 && region.height() >= 1.0)
                .then(|| (Rc::clone(art), region, bare));
        }
        // The selection is cut out of the artwork once for each artwork
        // and selection, not on every frame.
        let selection = reads.selection.get();
        let mut cut = cut.borrow_mut();
        let fresh = cut
            .as_ref()
            .is_none_or(|known| !Rc::ptr_eq(&known.art, art) || known.selection != selection);
        if fresh {
            let (made, region) = part(art, Area::Selection, view, &selection).ok()?;
            *cut = Some(CutKey {
                art: Rc::clone(art),
                selection,
                part: Rc::new(made),
                region,
            });
        }
        cut.as_ref()
            .map(|known| (Rc::clone(&known.part), known.region, bare))
    });
    let thumb = crate::thumb::Thumb::new(tools.renderer.clone(), None, 0.0, 0.0).framed(framing);
    super::layout::themed(cx.surface(thumb), |theme, style| {
        style.size.width = ui::taffy::Dimension::length(theme.tokens.metrics.inspector_width);
        style.size.height = ui::taffy::Dimension::length(theme.tokens.metrics.overview_width);
    })
}

/// What [`summary`] said last, and for what.
#[derive(Default)]
struct Sizes {
    key: Option<SizesKey>,
    said: Said,
    text: Option<TextRenderer>,
}

/// The summary: the picture's size and format, about how many bytes once
/// they are counted, and a note after them.
#[derive(Default)]
struct Said {
    size: String,
    counting: Option<Awaiting<u64>>,
    bytes: Option<u64>,
    note: Option<String>,
}

impl Said {
    /// Only `why` there is no picture.
    fn not(why: String) -> Self {
        Self {
            size: why,
            ..Self::default()
        }
    }

    /// Count the bytes, if their pictures came.
    fn count(&mut self) {
        if let Some(done) = self.counting.as_mut().and_then(Awaiting::poll) {
            self.counting = None;
            self.bytes = done.ok();
        }
    }

    fn text(&self) -> String {
        let mut said = self.size.clone();
        if let Some(bytes) = self.bytes {
            said.push('\n');
            said.push_str(&ui::i18n::I18nManager::format(
                "canvas-export-bytes",
                &[("size", ui::format::size(bytes).into())],
            ));
        }
        if let Some(note) = &self.note {
            said.push('\n');
            said.push_str(note);
        }
        said
    }
}

/// What the summary depends on: the artwork as it is, and the settings.
#[derive(PartialEq)]
struct SizesKey {
    /// The dialog is open: only then is the picture made to count it.
    open: bool,
    art: usize,
    settings: Settings,
    selection: Vec<String>,
    view: Option<Rect>,
}

/// The summary for `key`: the picture's size and format and about how
/// many bytes it comes to, or why there is no picture.
fn say(tools: &Tools, art: &Artwork, key: &SizesKey, text: &mut Option<TextRenderer>) -> Said {
    let settings = key.settings;
    let (part, region) = match part(art, settings.area, key.view, &key.selection) {
        Err(why) => return Said::not(why),
        Ok(found) => found,
    };
    let size = if settings.format.raster() {
        if region.width().max(region.height()) * settings.scale > MAX_PIXELS {
            return Said::not(ui::i18n::I18nManager::format(
                "canvas-export-too-large",
                &[("pixels", MAX_PIXELS.to_string().into())],
            ));
        }
        let (width, height) = pixels(region, settings.scale);
        ui::i18n::I18nManager::format(
            "canvas-export-summary",
            &[
                ("width", width.into()),
                ("height", height.into()),
                ("format", settings.format.label().into()),
            ],
        )
    } else {
        let (width, height) = pixels(region, 1.0);
        ui::i18n::I18nManager::format(
            "canvas-export-svg-summary",
            &[("width", width.into()), ("height", height.into())],
        )
    };
    // Made only while the dialog is open; in a browser page its pictures
    // come a few frames later, and the bytes are told once they did.
    let open = tools
        .sheet
        .with(|sheet| *sheet == Some(crate::tools::Sheet::Export));
    let counting = if open {
        let text = text.get_or_insert_with(TextRenderer::new);
        tools
            .renderer
            .try_borrow_mut()
            .ok()
            .and_then(|mut renderer| estimate(&mut renderer, text, &part, region, settings).ok())
    } else {
        None
    };
    let note = (settings.transparent && !settings.format.transparent())
        .then(|| ui::i18n::I18nManager::lookup_plain("canvas-export-jpeg-opaque"));
    let mut said = Said {
        size,
        counting,
        bytes: None,
        note,
    };
    said.count();
    said
}

/// The selection cut out of one artwork, kept while both stay the same.
struct CutKey {
    art: Rc<Artwork>,
    selection: Vec<String>,
    part: Rc<Artwork>,
    region: Rect,
}

/// What the export comes to: its size and format, about how many bytes,
/// or why there is none.
fn summary(cx: &ViewCx<crate::model::Artwork>, tools: &Tools) -> Text {
    let tools = tools.clone();
    let live = cx.live().clone();
    let sizes = RefCell::new(Sizes::default());
    Text::dynamic(move || {
        let settings = tools.export.settings();
        let art = live.get();
        let selection = tools.selection.get();
        let view = tools.visible.get();
        let key = SizesKey {
            open: tools
                .sheet
                .with(|sheet| *sheet == Some(crate::tools::Sheet::Export)),
            art: Rc::as_ptr(&art).addr(),
            settings,
            selection,
            view,
        };
        let mut sizes = sizes.borrow_mut();
        if sizes.key.as_ref() != Some(&key) {
            let said = say(&tools, &art, &key, &mut sizes.text);
            sizes.key = Some(key);
            sizes.said = said;
        }
        sizes.said.count();
        sizes.said.text()
    })
    .with_role(TextRole::Label)
}

#[cfg(test)]
mod tests {
    use super::{Area, Awaiting, Drawn, Format, Settings, draw, draw_svg, encode, encode_raster};
    use crate::model::Artwork;
    use crate::render::Renderer;
    use ui::TextRenderer;
    use ui::offscreen::pixels::PixelsHost;
    use ui::vello::{AaSupport, RendererOptions};

    fn decoded(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    /// Renders this thread's pixels as a browser page's frame loop does.
    fn frame_loop() -> PixelsHost {
        let gpu = ui::offscreen::shared_gpu().unwrap();
        let vello = ui::vello::Renderer::new(
            &gpu.device,
            RendererOptions {
                antialiasing_support: AaSupport::area_only(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut host = PixelsHost::new();
        host.set_gpu(gpu.device, gpu.queue, vello);
        host
    }

    /// Frames, until `host` answered everything asked of it.
    fn frames(host: &mut PixelsHost) {
        let started = std::time::Instant::now();
        while host.waiting() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "the picture never came"
            );
            host.drive();
        }
    }

    /// In a browser page the export's picture comes from the frame loop a
    /// few frames after it is asked for, and makes the same file as where
    /// it is rendered at once.
    #[test]
    fn an_export_rendered_between_frames_makes_the_same_picture() {
        crate::locales::CATALOG.ensure();
        let art = Artwork::default();
        let settings = Settings {
            format: Format::Png,
            scale: 0.1,
            area: Area::Whole,
            transparent: false,
        };
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let at_once = decoded(
            &encode(&mut renderer, &mut text, &art, art.bounds(), settings)
                .and_then(Awaiting::now)
                .unwrap(),
        );

        let mut host = frame_loop();
        let Ok(Drawn::Raster(drawing)) =
            draw(&mut renderer, &mut text, &art, art.bounds(), settings)
        else {
            panic!("a PNG export is a picture to render");
        };
        let asked = drawing.request();
        assert!(asked.is_pending(), "the frame loop renders it");
        frames(&mut host);
        let (rgba, width, height) = crate::view_ops::rgba_of(asked.take()).unwrap();
        let between_frames = decoded(&encode_raster(&rgba, width, height, Format::Png).unwrap());
        assert_eq!(between_frames.dimensions(), (120, 90));
        assert_eq!(between_frames.dimensions(), at_once.dimensions());
        assert_eq!(between_frames.get_pixel(60, 45), at_once.get_pixel(60, 45));
    }

    /// In a browser page the export dialog's size estimate is counted once
    /// the frame loop rendered its sample pictures, and comes to the same
    /// bytes as where they are rendered at once.
    #[test]
    fn an_estimate_counted_between_frames_comes_to_the_same_bytes() {
        crate::locales::CATALOG.ensure();
        let art = Artwork::default();
        let settings = Settings {
            format: Format::Png,
            scale: 1.0,
            area: Area::Whole,
            transparent: false,
        };
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let at_once = super::estimate(&mut renderer, &mut text, &art, art.bounds(), settings)
            .and_then(Awaiting::now)
            .unwrap();

        let mut host = frame_loop();
        let mut counting =
            super::estimate(&mut renderer, &mut text, &art, art.bounds(), settings).unwrap();
        assert!(counting.poll().is_none(), "its pictures are on their way");
        frames(&mut host);
        assert_eq!(counting.poll(), Some(Ok(at_once)));
    }

    /// An SVG's painted layers are pictures of their paint: in a browser
    /// page the document is written once they came, and has every one.
    #[test]
    fn an_svg_waits_for_its_painted_layers_between_frames() {
        let mut replica = ui::prelude::Replica::<Artwork>::new();
        for action in crate::sample::landscape() {
            replica.apply(action).unwrap();
        }
        let art = replica.state().unwrap();
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let at_once = draw_svg(&mut renderer, &mut text, &art, true, false).finish();
        let painted = at_once.matches("<image").count();
        assert!(painted > 0, "the landscape has painted layers");

        let mut host = frame_loop();
        let drawing = draw_svg(&mut renderer, &mut text, &art, true, false);
        assert!(!drawing.ready(), "its painted layers are on their way");
        frames(&mut host);
        assert!(drawing.ready());
        let between_frames = drawing.finish();
        assert_eq!(between_frames.matches("<image").count(), painted);
    }
}

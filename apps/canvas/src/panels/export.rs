//! Exporting the artwork as a picture someone else can open: the format,
//! how large, how much of it, and whether the ground comes along. The
//! picture is written where the person chooses in Files.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::TextRenderer;
use ui::prelude::*;
use ui::vello::kurbo::Rect;

use super::caption;
use crate::model::{Artwork, Shape};
use crate::render::{Options, Renderer};
use crate::tools::Tools;

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
///
/// # Errors
/// Says that nothing is selected, or the view shows none of the artwork.
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

/// The exported file's bytes.
///
/// # Errors
/// Says why the picture could not be drawn or encoded.
pub fn encode(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    region: Rect,
    settings: Settings,
) -> Result<Vec<u8>, String> {
    let bare = settings.transparent && settings.format.transparent();
    if settings.format == Format::Svg {
        return Ok(svg(renderer, text, art, !bare, false).into_bytes());
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
    let (rgba, width, height) =
        crate::view_ops::render_rgba(renderer, text, art, region, settings.scale, &options)?;
    match settings.format {
        Format::Png => crate::view_ops::encode_png(&rgba, width, height),
        Format::Jpeg => crate::view_ops::encode_jpeg_at(&rgba, width, height, JPEG_QUALITY),
        Format::Webp => crate::view_ops::encode_webp(&rgba, width, height),
        Format::Svg => Ok(Vec::new()),
    }
}

/// About how many bytes the export of `region` of `art` comes to, without
/// making all of it: pictures up to [`SAMPLED`] pixels on a side are made
/// whole; a larger one is made at two small sizes, and the way its bytes
/// grew between them carries on to its own size (flat vector art grows
/// with its edges, a painting or photo with its area).
///
/// # Errors
/// Says why the picture could not be drawn or encoded.
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
) -> Result<u64, String> {
    let longest = region.width().max(region.height());
    if !settings.format.raster() || longest * settings.scale <= f64::from(SAMPLED) {
        return encode(renderer, text, art, region, settings).map(|bytes| bytes.len() as u64);
    }
    let large = f64::from(SAMPLED) / longest;
    let small = large / 2.0;
    let at = |scale: f64, renderer: &mut Renderer, text: &mut TextRenderer| {
        encode(renderer, text, art, region, Settings { scale, ..settings })
            .map(|bytes| (bytes.len() as f64).max(1.0))
    };
    let small_bytes = at(small, renderer, text)?;
    let large_bytes = at(large, renderer, text)?;
    let growth = (large_bytes / small_bytes).log2().clamp(0.8, 2.2);
    Ok((large_bytes * (settings.scale / large).powf(growth)) as u64)
}

/// The longest side, in pixels, of a picture [`estimate`] makes whole.
const SAMPLED: u32 = 512; // a sampling budget in pixels, not on-screen spacing

/// `art` as SVG markup: plain layers as vector art, painted layers as
/// pictures; with its ground when `ground`, every text as outlines when
/// `outline_all` (text set along a path always is).
pub fn svg(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    ground: bool,
    outline_all: bool,
) -> String {
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
            let embedded = crate::surface::embedded_picture(renderer, text, art, mark)?;
            Some((mark.id.clone(), embedded))
        })
        .collect();
    let dabbed = renderer.dabbed_pictures(art, 2.0);
    crate::svg::export(
        art,
        ground,
        |layer| {
            let options = Options {
                layers: Some(vec![layer.id.clone()]),
                bare: true,
                ..Options::default()
            };
            crate::view_ops::render(renderer, text, art, (art.bounds(), 1.0), &options, false)
                .ok()
                .map(|picture| picture.bytes)
        },
        outlines,
        pictures,
        dabbed,
    )
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
    Box::new(
        Row::new()
            .no_wrap()
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
                    .with_min_width(0.0)
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
    said: String,
    text: Option<TextRenderer>,
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
fn say(tools: &Tools, art: &Artwork, key: &SizesKey, text: &mut Option<TextRenderer>) -> String {
    let settings = key.settings;
    let (part, region) = match part(art, settings.area, key.view, &key.selection) {
        Err(why) => return why,
        Ok(found) => found,
    };
    let mut said = if settings.format.raster() {
        if region.width().max(region.height()) * settings.scale > MAX_PIXELS {
            return ui::i18n::I18nManager::format(
                "canvas-export-too-large",
                &[("pixels", MAX_PIXELS.to_string().into())],
            );
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
    // Made only while the dialog is open, and where a picture can be made
    // without waiting on the one thread a page has.
    let open = tools
        .sheet
        .with(|sheet| *sheet == Some(crate::tools::Sheet::Export));
    let threads = ui::rt::available(ui::rt::Feature::Threads);
    let bytes = (open && threads)
        .then(|| {
            let text = text.get_or_insert_with(TextRenderer::new);
            tools
                .renderer
                .try_borrow_mut()
                .ok()
                .and_then(|mut renderer| {
                    estimate(&mut renderer, text, &part, region, settings).ok()
                })
        })
        .flatten();
    if let Some(bytes) = bytes {
        said.push('\n');
        said.push_str(&ui::i18n::I18nManager::format(
            "canvas-export-bytes",
            &[("size", ui::format::size(bytes).into())],
        ));
    }
    if settings.transparent && !settings.format.transparent() {
        said.push('\n');
        said.push_str(&ui::i18n::I18nManager::lookup_plain(
            "canvas-export-jpeg-opaque",
        ));
    }
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
        sizes.said.clone()
    })
    .with_role(TextRole::Label)
}

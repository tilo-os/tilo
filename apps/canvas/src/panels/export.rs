//! Exporting the artwork as a picture someone else can open: the format,
//! how large, how much of it, and whether the ground comes along. The
//! picture is written where the person chooses in Files.

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

/// The sizes offered, as pixels per artwork unit.
pub const SCALES: [f64; 5] = [0.5, 1.0, 2.0, 3.0, 4.0];
/// The longest side a picture may have.
pub const MAX_PIXELS: f64 = 8192.0;

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
    let (rgba, width, height) =
        crate::view_ops::pixels(region, settings.scale, bare, |scene, to| {
            renderer.draw(art, scene, to, text, &options);
        })?;
    match settings.format {
        Format::Png => crate::view_ops::encode_png(&rgba, width, height),
        Format::Jpeg => crate::view_ops::encode_jpeg(&rgba, width, height),
        Format::Webp => {
            let mut bytes = Vec::new();
            image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
                .encode(&rgba, width, height, image::ExtendedColorType::Rgba8)
                .map_err(|error| error.to_string())?;
            Ok(bytes)
        }
        Format::Svg => Ok(Vec::new()),
    }
}

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
        dabbed,
    )
}

/// The dialog's body: format, size, how much and the background, and what
/// that comes to.
pub fn body(cx: &ViewCx<crate::resource::ArtworkResource>, tools: &Tools) -> Element {
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
    .with_flexible_width()
    .with_id("canvas-export-format");
    let scale = ui::widget::ToggleGroup::from_state(
        SCALES
            .iter()
            .enumerate()
            .map(|(index, scale)| {
                (
                    index,
                    ui::widget::toggle_group::Segment::new(format!("{scale}×")),
                )
            })
            .collect(),
        &choices.scale,
    )
    .with_flexible_width()
    .with_id("canvas-export-scale");
    let area = ui::widget::ToggleGroup::from_state(
        [
            (Area::Whole, "canvas-export-whole"),
            (Area::View, "canvas-export-view"),
            (Area::Selection, "canvas-export-selection"),
        ]
        .into_iter()
        .map(|(area, id)| (area, ui::widget::toggle_group::Segment::new(lookup(id))))
        .collect(),
        &choices.area,
    )
    .with_flexible_width()
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
                .with_gap(6.0)
                .push(caption(&lookup(label)))
                .push(control),
        )
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(14.0)
            .push(field("canvas-export-format-label", Box::new(format)))
            .push(field("canvas-export-size-label", Box::new(scale)))
            .push(field("canvas-export-area-label", Box::new(area)))
            .push(Box::new(transparent))
            .push(Box::new(summary)),
    )
}

/// What the export comes to: its size and format, or why there is none.
fn summary(cx: &ViewCx<crate::resource::ArtworkResource>, tools: &Tools) -> Text {
    let tools = tools.clone();
    let live = cx.live().clone();
    Text::dynamic(move || {
        let settings = tools.export.settings();
        let art = live.get();
        let selection = tools.selection.get();
        let view = tools.visible.get();
        match part(&art, settings.area, view, &selection) {
            Err(why) => why,
            Ok((_, region)) if !settings.format.raster() => {
                let (width, height) = pixels(region, 1.0);
                ui::i18n::I18nManager::format(
                    "canvas-export-svg-summary",
                    &[("width", width.into()), ("height", height.into())],
                )
            }
            Ok((_, region)) => {
                if region.width().max(region.height()) * settings.scale > MAX_PIXELS {
                    return ui::i18n::I18nManager::format(
                        "canvas-export-too-large",
                        &[("pixels", MAX_PIXELS.to_string().into())],
                    );
                }
                let (width, height) = pixels(region, settings.scale);
                let mut said = ui::i18n::I18nManager::format(
                    "canvas-export-summary",
                    &[
                        ("width", width.into()),
                        ("height", height.into()),
                        ("format", settings.format.label().into()),
                    ],
                );
                if settings.transparent && !settings.format.transparent() {
                    said.push_str(" · ");
                    said.push_str(&ui::i18n::I18nManager::lookup_plain(
                        "canvas-export-jpeg-opaque",
                    ));
                }
                said
            }
        }
    })
    .with_role(TextRole::Label)
}

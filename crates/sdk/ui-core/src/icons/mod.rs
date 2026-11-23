//! The icon set: one SVG source per icon in `assets/icons` (see its README), on one 24 grid with one
//! 1.5 px round stroke. `build.rs` turns the sources into [`Icon`] and its outlines, which the native
//! renderer strokes and the web page draws from the same paths. Apps name icons, never files.

mod app_glyph;
mod app_image;
mod service_mark;
mod tilo_mark;

pub use tilo_mark::paint_tilo_mark;

pub use app_glyph::app_glyph;
pub use service_mark::service_mark;

pub use app_image::{
    AppImage, AppImageSource, ArtworkResolver, MAX_DEVICE_PX, MAX_PNG_BYTES, MAX_PNG_PIXELS,
    app_artwork, device_px, draw_app_image, set_app_artwork,
};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};

/// Declares [`Icon`] from the table `build.rs` generates out of `assets/icons/*.svg`, so the enum,
/// [`Icon::ALL`], [`Icon::name`] and each icon's geometry come from one source per icon.
macro_rules! icons {
    ($($(#[$doc:meta])* $icon:ident => $name:literal, $filled:literal, [$($part:expr),*],)*) => {
        /// One icon of the set, named by its source file.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($(#[$doc])* $icon,)*
        }

        impl Icon {
            /// Every icon, in its sources' order.
            pub const ALL: [Self; [$(stringify!($icon)),*].len()] = [$(Self::$icon),*];

            /// The icon's kebab name, its source's file name (`Icon::LayoutGrid` → `layout-grid`): how
            /// data that cannot name the enum (a resource's search row, a manifest) names an icon.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$icon => $name,)*
                }
            }

            /// A filled mark rather than a stroked glyph: always drawn filled, as its owner draws it.
            #[must_use]
            pub const fn is_brand(self) -> bool {
                match self {
                    $(Self::$icon => $filled,)*
                }
            }

            /// The shapes of the icon's source, in the 24 × 24 grid.
            const fn parts(self) -> &'static [Part] {
                match self {
                    $(Self::$icon => &[$($part),*],)*
                }
            }
        }
    };
}

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// One shape of an icon's source, as its SVG element gives it.
#[derive(Clone, Copy)]
enum Part {
    Path(&'static str),
    /// `x, y, width, height, rx`.
    Rect(f64, f64, f64, f64, f64),
    /// `cx, cy, r`.
    Circle(f64, f64, f64),
    /// `cx, cy, rx, ry`.
    Ellipse(f64, f64, f64, f64),
    /// `x1, y1, x2, y2`.
    Line(f64, f64, f64, f64),
}

impl Part {
    fn outline(self) -> BezPath {
        match self {
            Self::Path(d) => path_from_d(d),
            Self::Rect(x, y, width, height, radius) => rect_path(x, y, width, height, radius),
            Self::Circle(cx, cy, r) => circle_path(cx, cy, r),
            Self::Ellipse(cx, cy, rx, ry) => {
                vello::kurbo::Ellipse::new(Point::new(cx, cy), (rx, ry), 0.0)
                    .path_elements(0.05)
                    .collect()
            }
            Self::Line(x1, y1, x2, y2) => line_path(x1, y1, x2, y2),
        }
    }
}

impl Icon {
    /// The icon a lucide kebab name names, as [`Self::name`] spells it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|icon| icon.name() == name)
    }
}

/// `icon` as an app's icon artwork: the glyph stroked in white on a rounded tile of the accent
/// gradient, as a standalone SVG document.
#[must_use]
pub fn icon_svg(icon: Icon) -> String {
    let paths: String = icon_subpaths(icon)
        .iter()
        .map(|path| format!("<path d=\"{}\"/>", path.to_svg()))
        .collect();
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"128\" height=\"128\" \
         viewBox=\"0 0 128 128\"><defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"0\" \
         y2=\"1\"><stop offset=\"0\" stop-color=\"#4c6ef5\"/><stop offset=\"1\" \
         stop-color=\"#8b5cf6\"/></linearGradient></defs><rect width=\"128\" height=\"128\" \
         rx=\"30\" fill=\"url(#g)\"/><g transform=\"translate(28 28) scale(3)\" fill=\"none\" \
         stroke=\"#ffffff\" stroke-width=\"2\" stroke-linecap=\"round\" \
         stroke-linejoin=\"round\">{paths}</g></svg>"
    )
}

/// The icon grid: every source draws in a 24 × 24 box; [`draw_icon`] scales it to the requested rect.
pub const LUCIDE_VIEWBOX: f64 = 24.0;

/// The icon stroke in output pixels, at every size: a theme's `icon_stroke_px` replaces it.
pub const LUCIDE_DEFAULT_STROKE_PX: f64 = 1.5;

/// Parse an icon source's `d="…"` into a [`BezPath`].
fn path_from_d(d: &str) -> BezPath {
    #[allow(
        clippy::expect_used,
        reason = "every source path is build-time data, and a test draws every icon"
    )]
    BezPath::from_svg(d).expect("an icon source's path parses")
}

fn rect_path(left: f64, top: f64, width: f64, height: f64, radius: f64) -> BezPath {
    let rect = Rect::new(left, top, left + width, top + height);
    if radius > 0.0 {
        RoundedRect::from_rect(rect, radius)
            .path_elements(0.1)
            .collect()
    } else {
        let mut path = BezPath::new();
        path.move_to(Point::new(rect.x0, rect.y0));
        path.line_to(Point::new(rect.x1, rect.y0));
        path.line_to(Point::new(rect.x1, rect.y1));
        path.line_to(Point::new(rect.x0, rect.y1));
        path.close_path();
        path
    }
}

fn line_path(x1: f64, y1: f64, x2: f64, y2: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(Point::new(x1, y1));
    p.line_to(Point::new(x2, y2));
    p
}

fn circle_path(cx: f64, cy: f64, r: f64) -> BezPath {
    Circle::new(Point::new(cx, cy), r)
        .path_elements(0.1)
        .collect()
}

/// The outlines of `icon` in the 24 × 24 grid, made once per icon on first use.
pub(crate) fn icon_subpaths(icon: Icon) -> &'static [BezPath] {
    static OUTLINES: [std::sync::OnceLock<Vec<BezPath>>; Icon::ALL.len()] =
        [const { std::sync::OnceLock::new() }; Icon::ALL.len()];
    OUTLINES[icon as usize].get_or_init(|| icon.parts().iter().map(|part| part.outline()).collect())
}

/// Map a filesystem entry to the most appropriate [`Icon`]. Directories always resolve to
/// [`Icon::Folder`]; files are classified by (lower-cased) extension, falling back to [`Icon::File`].
pub fn icon_for_path(path: &std::path::Path, is_dir: bool) -> Icon {
    if is_dir {
        return Icon::Folder;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some(
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff"
            | "avif" | "heic",
        ) => Icon::Image,
        Some("mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "opus" | "wma") => {
            Icon::Music
        }
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "flv" | "m4v" | "mpeg" | "mpg") => {
            Icon::Film
        }
        Some(
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "lz" | "lzma",
        ) => Icon::Archive,
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cc" | "go"
            | "java" | "kt" | "rb" | "php" | "swift" | "sh" | "bash" | "fish" | "zsh" | "lua"
            | "toml" | "json" | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "scss" | "sql",
        ) => Icon::FileCode,
        Some(
            "txt" | "md" | "markdown" | "rst" | "doc" | "docx" | "odt" | "rtf" | "pdf" | "tex",
        ) => Icon::FileText,
        _ => Icon::File,
    }
}

impl Icon {
    /// Paint this glyph centred inside `rect`, stroked in `color` at the concept's default 1.5 px
    /// lucide width — equivalent to [`draw_icon`] with `self` and [`LUCIDE_DEFAULT_STROKE_PX`].
    pub fn draw(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        draw_icon(
            scene,
            transform,
            rect,
            self,
            color,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Paint the icon as a **solid fill** rather than a stroked outline — for glyphs that read as
    /// filled when "on" (starred, a selected pin). Only closed sub-paths fill meaningfully.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
    )]
    pub fn draw_filled(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        if self == Self::Tilo {
            tilo_mark::paint_tilo_mark(scene, transform, rect, tilo_mark::on_dark(color));
            return;
        }
        let size = rect.size();
        let min = size.width.min(size.height);
        if min <= 0.0 {
            return;
        }
        let scale = min / LUCIDE_VIEWBOX;
        let glyph = LUCIDE_VIEWBOX * scale;
        let tx = rect.x0 + (size.width - glyph) / 2.0;
        let ty = rect.y0 + (size.height - glyph) / 2.0;
        let local = transform * Affine::translate((tx, ty)) * Affine::scale(scale);
        if self.is_brand() {
            // A mark's shapes fill as one even-odd path, so a shape inside another (Discord's eyes)
            // is cut out of it.
            let mut mark = BezPath::new();
            for outline in icon_subpaths(self) {
                mark.extend(outline.elements().iter().copied());
            }
            scene.fill(Fill::EvenOdd, local, color, None, &mark);
            return;
        }
        for sub in icon_subpaths(self) {
            scene.fill(Fill::NonZero, local, color, None, sub);
        }
    }

    /// Same as [`Icon::draw`] but with an explicit output stroke width in pixels — use when a call site
    /// needs a heavier or lighter glyph than the 1.5 px default (e.g.
    pub fn draw_with_stroke(
        self,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        color: Color,
        stroke_px: f64,
    ) {
        draw_icon(scene, transform, rect, self, color, stroke_px);
    }
}

impl Icon {
    /// The icon as a page shows it: its outlines as SVG path data in the 24 × 24 box, in `color`,
    /// `size` pixels on a side.
    #[must_use]
    pub fn glyph(self, size: f64, color: Color, filled: bool) -> Option<crate::composition::Glyph> {
        if self == Self::Tilo || self.is_brand() {
            return None;
        }
        Some(crate::composition::Glyph {
            name: self.name(),
            paths: icon_subpaths(self).iter().map(BezPath::to_svg).collect(),
            size,
            stroke: LUCIDE_DEFAULT_STROKE_PX,
            color: crate::composition::rgba8(color),
            filled,
        })
    }
}

/// Paint `icon` stroked in `color` into `scene`, centred within `rect`. The icon is scaled from
/// `rect.size().min() / LUCIDE_VIEWBOX` so it always fits the shorter dimension, padding the longer
/// axis. `stroke_px` is the **output** width in pixels — lucide renders at 2 px on a 24-unit viewBox,
/// trimmed here to 1.5 px by default ([`LUCIDE_DEFAULT_STROKE_PX`]); it's divided by the icon's scale
/// before handing to vello so stroke width stays consistent across sizes.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
pub fn draw_icon(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    color: Color,
    stroke_px: f64,
) {
    if icon == Icon::Tilo {
        // The brand's own colours: the mark is never an ink's outline.
        tilo_mark::paint_tilo_mark(scene, transform, rect, tilo_mark::on_dark(color));
        return;
    }
    let size = rect.size();
    let min = size.width.min(size.height);
    if min <= 0.0 {
        return;
    }
    let scale = min / LUCIDE_VIEWBOX;
    let glyph_w = LUCIDE_VIEWBOX * scale;
    let glyph_h = LUCIDE_VIEWBOX * scale;
    let tx = rect.x0 + (size.width - glyph_w) / 2.0;
    let ty = rect.y0 + (size.height - glyph_h) / 2.0;
    let local = transform * Affine::translate((tx, ty)) * Affine::scale(scale);

    let stroke = Stroke::new(stroke_px / scale)
        .with_caps(Cap::Round)
        .with_join(Join::Round);

    for sub in icon_subpaths(icon) {
        scene.stroke(&stroke, local, color, None, &sub);
    }
}

/// [`draw_icon`] with a per-subpath colour. Exists for glyphs whose subpaths mean different things —
/// the Wi-Fi arcs are a *scale*, not a picture, so a two-bar signal should paint two arcs lit and two
/// dim rather than the whole glyph at one alpha (drawing four nested icons at four sizes would not line
/// up; they must come from one path table).
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
)]
pub fn draw_icon_layered(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    stroke_px: f64,
    mut color_for: impl FnMut(usize) -> Color,
) {
    let size = rect.size();
    let min = size.width.min(size.height);
    if min <= 0.0 {
        return;
    }
    let scale = min / LUCIDE_VIEWBOX;
    let glyph = LUCIDE_VIEWBOX * scale;
    let tx = rect.x0 + (size.width - glyph) / 2.0;
    let ty = rect.y0 + (size.height - glyph) / 2.0;
    let local = transform * Affine::translate((tx, ty)) * Affine::scale(scale);
    let stroke = Stroke::new(stroke_px / scale)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    for (i, sub) in icon_subpaths(icon).iter().enumerate() {
        scene.stroke(&stroke, local, color_for(i), None, &sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element_count(p: &BezPath) -> usize {
        p.elements().len()
    }

    tilo_test_env::scenario! {
        fn icons_draw_their_sources_on_the_grid;

        phase every_icon_draws_every_shape_inside_the_grid;
        {
            for icon in Icon::ALL {
                let outlines = icon_subpaths(icon);
                assert!(!outlines.is_empty(), "{icon:?} has no shapes");
                for outline in outlines {
                    assert!(element_count(outline) > 0, "{icon:?} has an empty shape");
                    let bounds = outline.bounding_box();
                    assert!(
                        bounds.x0 >= -0.5 && bounds.y0 >= -0.5 && bounds.x1 <= 24.5 && bounds.y1 <= 24.5,
                        "{icon:?} leaves the 24 grid: {bounds:?}"
                    );
                }
            }
        }

        phase an_icon_svg_is_a_document_of_the_glyph_on_a_tile;
        {
            let svg = icon_svg(Icon::FileText);
            assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
            assert!(svg.ends_with("</svg>"));
            assert_eq!(
                svg.matches("<path d=\"").count(),
                icon_subpaths(Icon::FileText).len()
            );
        }

        phase every_icon_is_found_by_its_name;
        {
            let names: std::collections::HashSet<_> = Icon::ALL.iter().map(|i| i.name()).collect();
            assert_eq!(names.len(), Icon::ALL.len(), "two icons share a name");
            for icon in Icon::ALL {
                assert_eq!(Icon::from_name(icon.name()), Some(icon));
            }
            assert_eq!(Icon::LayoutGrid.name(), "layout-grid");
            assert_eq!(Icon::Settings2.name(), "settings-2");
        }

        phase icon_for_path_classifies_by_extension;
        {
            use std::path::Path;
            assert_eq!(
                icon_for_path(Path::new("/home/user/Pictures"), true),
                Icon::Folder
            );
            assert_eq!(
                icon_for_path(Path::new("/x/archive.zip"), true),
                Icon::Folder
            );
            assert_eq!(
                icon_for_path(Path::new("a/photo.PNG"), false),
                Icon::Image,
                "case-insensitive"
            );
            assert_eq!(icon_for_path(Path::new("song.flac"), false), Icon::Music);
            assert_eq!(icon_for_path(Path::new("clip.mkv"), false), Icon::Film);
            assert_eq!(
                icon_for_path(Path::new("bundle.tar.gz"), false),
                Icon::Archive
            );
            assert_eq!(icon_for_path(Path::new("main.rs"), false), Icon::FileCode);
            assert_eq!(icon_for_path(Path::new("notes.md"), false), Icon::FileText);
            assert_eq!(
                icon_for_path(Path::new("report.docx"), false),
                Icon::FileText
            );
            assert_eq!(icon_for_path(Path::new("Makefile"), false), Icon::File);
            assert_eq!(icon_for_path(Path::new("blob.qwerty"), false), Icon::File);
        }
    }

    tilo_test_env::scenario! {
        fn icons_draw_every_subpath_as_strokes_pointing_the_named_way;

        phase draw_icon_layered_paints_every_subpath;
        {
            let mut scene = Scene::new();
            let mut seen = Vec::new();
            draw_icon_layered(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 16.0, 16.0),
                Icon::Wifi,
                1.5,
                |i| {
                    seen.push(i);
                    Color::WHITE
                },
            );
            assert_eq!(seen, vec![0, 1, 2, 3]);
        }

        phase draw_icon_does_not_panic_on_zero_rect;
        {
            let mut scene = Scene::new();
            draw_icon(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 0.0, 0.0),
                Icon::LayoutGrid,
                Color::WHITE,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }

        phase icon_draw_method_emits_strokes;
        {
            use vello_encoding::DrawTag;
            let mut scene = Scene::new();
            Icon::Check.draw(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 24.0, 24.0),
                Color::WHITE,
            );
            let color_tags = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert!(
                color_tags >= 1,
                "Icon::draw must stroke at least one subpath"
            );
        }

        phase the_skip_glyphs_point_the_way_they_are_named;
        // The two skip glyphs are mirror images, so a swap produces two icons
        // that each look fine alone and both point the wrong way. Pin the
        // direction geometrically: the bar sits on the side the triangle points at.
        {
            let bar_x = |icon: Icon| {
                let bar = &icon_subpaths(icon)[1];
                let mut xs: Vec<f64> = Vec::new();
                for el in bar.elements() {
                    if let vello::kurbo::PathEl::MoveTo(p) | vello::kurbo::PathEl::LineTo(p) = el {
                        xs.push(p.x);
                    }
                }
                xs.into_iter().fold(f64::MIN, f64::max)
            };
            assert!(
                bar_x(Icon::SkipForward) > 12.0,
                "skip-forward's bar is on the right, the side it points at"
            );
            assert!(
                bar_x(Icon::SkipBack) < 12.0,
                "skip-back's bar is on the left"
            );
            for icon in [
                Icon::WifiOff,
                Icon::VolumeX,
                Icon::EthernetPort,
                Icon::MicOff,
                Icon::SkipForward,
                Icon::SkipBack,
            ] {
                for p in icon_subpaths(icon) {
                    assert!(element_count(p) > 0, "{icon:?} subpath must emit elements");
                }
            }
        }
    }
}

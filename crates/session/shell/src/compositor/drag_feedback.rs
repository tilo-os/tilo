//! What a drag looks like while it is crossing the desktop.
//!
//! A drag that has left its source window is the one gesture no
//! application can draw. The pointer is over *another* app's surface, so
//! the source cannot paint there, and the receiver has been told a drag
//! is over it but knows nothing about what it is carrying beyond a list
//! of mime types. Between the two of them there is nobody to draw the
//! thing the user is holding — which is why a drag with no compositor
//! feedback looks, on screen and on a recording, exactly like a pointer
//! moving across a window.
//!
//! So the compositor draws it, for the same reason it draws window
//! chrome: it is the only participant that can see both ends. This
//! covers both kinds of drag — one the compositor started out of its own
//! hosted file manager, and one a client started with a
//! `wl_data_source` — because from the user's side they are the same
//! gesture and should not look different.
//!
//! Deliberately not the client's `dnd_icon` surface. A client-supplied
//! icon is painted wherever the client puts it, outside the strip layout
//! the spatial model depends on; the role is accepted and recorded so
//! the protocol stays honest, and the picture is the compositor's.

use std::sync::Arc;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect};
use vello::peniko::{Blob, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat};

use ui::TextRole;
use ui::color_compat::ColorCompat;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Height of the chip that names what is being carried — the Selector
/// height from HIG §9.6, so it matches every other pill the shell paints.
const CHIP_H: f64 = 28.0;
/// Padding inside the chip, left and right of the label.
const CHIP_PAD: f64 = 12.0;
/// The chip sits below-right of the hotspot so the cursor stays legible.
const CHIP_DX: f64 = 14.0;
const CHIP_DY: f64 = 16.0;
/// Cursor arrow, in units of its own height.
const CURSOR_H: f64 = 20.0;
const PREVIEW_MAX_W: f64 = 280.0; // Payload preview bound, not inter-control spacing.
const PREVIEW_MAX_H: f64 = 190.0; // Payload preview bound, not inter-control spacing.
const PREVIEW_MIN_W: f64 = 180.0; // Payload preview bound, not inter-control spacing.
const PREVIEW_MIN_H: f64 = 112.0; // Payload preview bound, not inter-control spacing.
const PREVIEW_ALPHA: f32 = 0.42;
const HANDLE_ALPHA: f32 = 0.5;

/// Pixel or semantic content carried by a Wayland drag. Payload reads and
/// decoding happen on workers; the frame thread only clones and paints this
/// bounded result.
#[derive(Clone, Debug)]
pub enum DragPreview {
    Image {
        rgba: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    },
    Text(String),
    Kind(DragKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    Image,
    Video,
    Audio,
    Document,
    File,
}

#[derive(Clone, Debug, Default)]
pub struct DragVisual {
    pub label: Option<String>,
    pub preview: Option<DragPreview>,
}

/// Draw the pointer and, when one is in flight, what it is carrying.
///
/// `label` is what the drag is carrying, already resolved to something a
/// person can read — a file name where the compositor knows it, a kind
/// where all it has is the offer. `None` means no drag: only the pointer
/// is drawn.
pub fn draw(
    scene: &mut Scene,
    cursor: Point,
    visual: &DragVisual,
    theme: &Theme,
    text: &mut TextRenderer,
) {
    if let Some(preview) = visual.preview.as_ref() {
        draw_preview(scene, cursor, preview, theme, text);
    }
    if let Some(label) = visual.label.as_deref() {
        // Theme type tokens already carry the user's text scale; the chip
        // paints with those resolved sizes, so the renderer must not scale
        // them again (HIG §7a) — reset for this pass, as the shell's
        // immediate-mode chrome passes do, and restore afterwards.
        let renderer_scale = text.text_scale();
        text.set_text_scale(1.0);
        draw_chip(scene, cursor, label, theme, text);
        text.set_text_scale(renderer_scale);
    }
    draw_cursor(scene, cursor, theme);
}

/// The carried-item chip: a rounded card with the item's name in it.
///
/// Deliberately not [`ui::widget::Chip::draw_at`]: a chip is a Selector
/// that sits *inside* a Panel and paints a veneer fill (`overlay_subtle`),
/// which over arbitrary app content would let whatever is being dragged
/// across show through the label. The carried item is an Object off the
/// surface — it needs an opaque ground, an elevation shadow and the Control
/// radius, none of which the chip painter offers. Nothing hit-tests it.
fn draw_chip(
    scene: &mut Scene,
    cursor: Point,
    label: &str,
    theme: &Theme,
    text: &mut TextRenderer,
) {
    // Control label scale (HIG §9.6), measured with the style it is drawn
    // in so the label is centred on its real line box rather than on a
    // `size * 1.3` guess. The caller has reset the renderer for this pass.
    let style = theme.text(TextRole::Caption);
    let (tw, th) = text.measure_style(label, None, &style);
    let w = f64::from(tw) + CHIP_PAD * 2.0;
    let x = cursor.x + CHIP_DX;
    let y = cursor.y + CHIP_DY;
    let bounds = Rect::new(x, y, x + w, y + CHIP_H);
    // Control role: a pressable-looking chip takes `radii.control`, and a carried
    // object is genuinely lifted, so its shadow comes from the elevation
    // token rather than a literal black offset copy (HIG §9.3).
    let radius = ui::chrome::ChromeRole::Control.radius(theme, CHIP_H);
    let rect = RoundedRect::from_rect(bounds, radius);
    ui::chrome::shell::panel_shadow(scene, Affine::IDENTITY, bounds, radius, theme, 1.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_primary.with_alpha_factor(HANDLE_ALPHA),
        None,
        &rect,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        Affine::IDENTITY,
        theme.tokens.colors.intent.inverted,
        None,
        &rect,
    );
    text.draw_style(
        scene,
        label,
        theme.text_primary,
        Affine::translate((x + CHIP_PAD, y + (CHIP_H - f64::from(th)) / 2.0)),
        None,
        &style,
    );
}

fn preview_rect(cursor: Point, preview: &DragPreview) -> Rect {
    let (w, h) = match preview {
        DragPreview::Image { width, height, .. } if *width > 0 && *height > 0 => {
            let source_w = f64::from(*width);
            let source_h = f64::from(*height);
            let maximum_scale = (PREVIEW_MAX_W / source_w).min(PREVIEW_MAX_H / source_h);
            let preferred_scale = (PREVIEW_MIN_W / source_w)
                .max(PREVIEW_MIN_H / source_h)
                .max(1.0);
            let scale = preferred_scale.min(maximum_scale) * 0.96;
            (source_w * scale, source_h * scale)
        }
        DragPreview::Text(_) | DragPreview::Kind(_) | DragPreview::Image { .. } => {
            (PREVIEW_MIN_W * 0.96, PREVIEW_MIN_H * 0.96)
        }
    };
    let x0 = cursor.x + CHIP_DX;
    let y0 = cursor.y + CHIP_DY + CHIP_H + 6.0;
    Rect::new(x0, y0, x0 + w, y0 + h)
}

fn draw_preview(
    scene: &mut Scene,
    cursor: Point,
    preview: &DragPreview,
    theme: &Theme,
    text: &mut TextRenderer,
) {
    let rect = preview_rect(cursor, preview);
    let radius = f64::from(theme.tokens.radii.tile);
    ui::surface::shadow(
        scene,
        Affine::IDENTITY,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    let shape = RoundedRect::from_rect(rect, radius);
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        PREVIEW_ALPHA,
        Affine::IDENTITY,
        &shape,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.content,
        None,
        &shape,
    );
    match preview {
        DragPreview::Image {
            rgba,
            width,
            height,
        } if *width > 0 && *height > 0 => {
            let image = ImageData {
                data: Blob::new(rgba.clone()),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: *width,
                height: *height,
            };
            let sx = rect.width() / f64::from(*width);
            let sy = rect.height() / f64::from(*height);
            let brush = ImageBrush::new(image).with_extend(Extend::Pad);
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine multiplication is finite floating-point transform composition"
            )]
            let image_transform =
                Affine::translate((rect.x0, rect.y0)) * Affine::scale_non_uniform(sx, sy);
            scene.fill(
                Fill::NonZero,
                image_transform,
                &brush,
                None,
                &Rect::new(0.0, 0.0, f64::from(*width), f64::from(*height)),
            );
        }
        DragPreview::Text(value) => draw_text_preview(scene, rect, value, theme, text),
        DragPreview::Kind(kind) => draw_kind_preview(scene, rect, *kind, theme),
        DragPreview::Image { .. } => {}
    }
    scene.pop_layer();
}

fn draw_text_preview(
    scene: &mut Scene,
    rect: Rect,
    value: &str,
    theme: &Theme,
    text: &mut TextRenderer,
) {
    let style = theme.text(TextRole::Caption);
    let line_h = f64::from(style.line_height_px());
    let mut y = rect.y0 + f64::from(theme.tokens.spacing.s3);
    for source_line in value.lines().filter(|line| !line.trim().is_empty()).take(5) {
        let line: String = source_line.chars().take(34).collect();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "text layout accepts f32 widths and the bounded preview is at most 560 px"
        )]
        let max_width = (rect.width() - 2.0 * f64::from(theme.tokens.spacing.s3)) as f32;
        text.draw_style(
            scene,
            &line,
            theme.tokens.colors.text_primary,
            Affine::translate((rect.x0 + f64::from(theme.tokens.spacing.s3), y)),
            Some(max_width),
            &style,
        );
        y += line_h;
        if y + line_h > rect.y1 {
            break;
        }
    }
}

fn draw_kind_preview(scene: &mut Scene, rect: Rect, kind: DragKind, theme: &Theme) {
    let icon = match kind {
        DragKind::Image => ui::icons::Icon::Image,
        DragKind::Video => ui::icons::Icon::Play,
        DragKind::Audio => ui::icons::Icon::Music,
        DragKind::Document => ui::icons::Icon::FileText,
        DragKind::File => ui::icons::Icon::File,
    };
    let side = rect.height().min(rect.width()) * 0.42;
    let centre = rect.center();
    ui::icons::draw_icon(
        scene,
        Affine::IDENTITY,
        Rect::new(
            centre.x - side / 2.0,
            centre.y - side / 2.0,
            centre.x + side / 2.0,
            centre.y + side / 2.0,
        ),
        icon,
        theme.tokens.colors.text_primary,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// Immediate semantic fallback while a worker fetches richer content.
pub fn preview_for_mimes(mimes: &[String]) -> DragPreview {
    let has = |prefix: &str| mimes.iter().any(|mime| mime.starts_with(prefix));
    if has("image/") {
        DragPreview::Kind(DragKind::Image)
    } else if has("video/") {
        DragPreview::Kind(DragKind::Video)
    } else if has("audio/") {
        DragPreview::Kind(DragKind::Audio)
    } else if mimes.iter().any(|mime| mime == "text/uri-list") {
        DragPreview::Kind(DragKind::File)
    } else if has("text/") {
        DragPreview::Kind(DragKind::Document)
    } else {
        DragPreview::Kind(DragKind::File)
    }
}

/// Decode a bounded raster preview off the compositor thread, through the
/// shared preview decoder: it reads the header first and refuses oversized
/// images before allocating a pixel, so an untrusted drag offer cannot turn a
/// thumbnail into an unbounded allocation.
pub fn image_preview(bytes: &[u8]) -> Option<DragPreview> {
    let preview = thumbnail::image_thumb(bytes, thumbnail::ThumbSize::new(560, 380))?;
    Some(DragPreview::Image {
        rgba: Arc::new(preview.rgba),
        width: preview.width,
        height: preview.height,
    })
}

pub fn text_preview(bytes: &[u8]) -> Option<DragPreview> {
    let value = String::from_utf8_lossy(bytes).trim().to_string();
    (!value.is_empty()).then(|| DragPreview::Text(value.chars().take(512).collect()))
}

/// A plain arrow pointer, hotspot at its tip.
///
/// Drawn here rather than left to the backend because only the winit and
/// headless paths have no cursor plane of their own — and headless is
/// where a drag gets photographed.
fn draw_cursor(scene: &mut Scene, at: Point, theme: &Theme) {
    let h = CURSOR_H;
    let mut p = BezPath::new();
    p.move_to((at.x, at.y));
    p.line_to((at.x, at.y + h));
    p.line_to((at.x + h * 0.27, at.y + h * 0.72));
    p.line_to((at.x + h * 0.46, at.y + h * 1.10));
    p.line_to((at.x + h * 0.62, at.y + h * 1.02));
    p.line_to((at.x + h * 0.43, at.y + h * 0.66));
    p.line_to((at.x + h * 0.72, at.y + h * 0.64));
    p.close_path();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.text_primary,
        None,
        &p,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(1.2),
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &p,
    );
}

/// A readable name for what a drag is carrying, from the mime types its
/// source offered.
///
/// Used for a drag started by a client: the compositor is holding the
/// offer, not the file, so the kind of thing is all it can say without
/// opening a pipe and reading the payload mid-gesture. Saying "Image"
/// when that is all that is known beats inventing a file name.
pub fn label_for_mimes(mimes: &[String]) -> String {
    let has = |p: &str| mimes.iter().any(|m| m.starts_with(p));
    if has("image/") {
        crate::shell::i18n::t("drag-image")
    } else if has("video/") {
        crate::shell::i18n::t("drag-video")
    } else if has("audio/") {
        crate::shell::i18n::t("drag-audio")
    } else if mimes.iter().any(|m| m == "application/pdf") {
        crate::shell::i18n::t("drag-pdf")
    } else if has("text/") {
        crate::shell::i18n::t("drag-text")
    } else {
        crate::shell::i18n::t_args("drag-items", &[("count", 1_u32.into())])
    }
}

/// The name to show for a `text/uri-list` body: the first entry's file
/// name, or "N items" when it names several.
///
/// Percent-escapes are undone, so a file with a space in its name shows
/// the name the user gave it rather than the one a URI had to spell.
/// `None` when the body names nothing usable, which leaves whatever the
/// mime types already said in place rather than blanking the chip.
pub fn name_from_uri_list(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let entries: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if entries.len() > 1 {
        return Some(crate::shell::i18n::t_args(
            "drag-items",
            &[("count", entries.len().into())],
        ));
    }
    let first = entries.first()?;
    let path = first.strip_prefix("file://").unwrap_or(first);
    // Drop a `file://host/path` authority, then take the last segment.
    let path = match path.find('/') {
        Some(0) | None => path,
        Some(i) => path.get(i..).unwrap_or(path),
    };
    let last = path.rsplit('/').find(|s| !s.is_empty())?;
    // A path segment: RFC 3986 rules, so a literal `+` stays a plus.
    let name = ui::uri::decode(last);
    (!name.is_empty()).then_some(name)
}

/// A readable name for a set of paths the compositor itself is carrying.
pub fn label_for_paths(paths: &[std::path::PathBuf]) -> String {
    match paths {
        [] => crate::shell::i18n::t_args("drag-items", &[("count", 1_u32.into())]),
        [one] => one.file_name().map_or_else(
            || one.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        many => crate::shell::i18n::t_args("drag-items", &[("count", many.len().into())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    #[test]
    fn drag_preview_shows_real_bounded_content() {
        // Phase `mime_fallback_is_visual_before_payload_arrives`.
        assert!(matches!(
            preview_for_mimes(&["image/png".into()]),
            DragPreview::Kind(DragKind::Image)
        ));
        assert!(matches!(
            preview_for_mimes(&["video/mp4".into()]),
            DragPreview::Kind(DragKind::Video)
        ));
        assert!(matches!(
            preview_for_mimes(&["application/octet-stream".into()]),
            DragPreview::Kind(DragKind::File)
        ));
        assert!(matches!(
            preview_for_mimes(&["text/uri-list".into()]),
            DragPreview::Kind(DragKind::File)
        ));
        // Phase `raster_payload_becomes_a_bounded_real_preview`.
        {
            let source = image::RgbaImage::from_pixel(32, 18, image::Rgba([12, 90, 220, 255]));
            let mut encoded = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(source)
                .write_to(&mut encoded, image::ImageFormat::Png)
                .expect("encode fixture");
            let DragPreview::Image {
                rgba,
                width,
                height,
            } = image_preview(encoded.get_ref()).expect("decode preview")
            else {
                panic!("image bytes must produce image pixels")
            };
            assert_eq!((width, height), (32, 18));
            assert_eq!(rgba.len(), 32 * 18 * 4);
            let rect = preview_rect(
                Point::new(20.0, 20.0),
                &DragPreview::Image {
                    rgba,
                    width,
                    height,
                },
            );
            assert!(rect.width() <= PREVIEW_MAX_W && rect.height() <= PREVIEW_MAX_H);
            assert!((rect.width() / rect.height() - 32.0 / 18.0).abs() < 1e-10);
        }
        // Phase `raster_payload_is_downscaled_to_the_decode_bound`.
        {
            let source = image::RgbaImage::from_pixel(800, 500, image::Rgba([12, 90, 220, 255]));
            let mut encoded = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(source)
                .write_to(&mut encoded, image::ImageFormat::Png)
                .expect("encode fixture");
            let DragPreview::Image { width, height, .. } =
                image_preview(encoded.get_ref()).expect("decode preview")
            else {
                panic!("image bytes must produce image pixels")
            };
            assert!(width <= 560 && height <= 380);
            assert_eq!((width, height), (560, 350));
        }
        // Phase `text_payload_keeps_content_not_only_a_type_label`.
        {
            let DragPreview::Text(text) = text_preview(b"A sentence selected in the editor")
                .expect("text payload must produce a visual")
            else {
                panic!("text bytes must remain text")
            };
            assert!(text.contains("selected in the editor"));
        }
    }

    #[test]
    fn drag_labels_name_every_offer_and_file() {
        // Phase `every_offer_produces_a_label`.
        // The chip has to say something for every drag, including one whose
        // offer says nothing recognisable — an empty chip is worse than a
        // vague one.
        assert_eq!(label_for_mimes(&["image/png".into()]), "Image");
        assert_eq!(
            label_for_mimes(&["text/uri-list".into(), "image/png".into()]),
            "Image",
            "the content type wins over the name list"
        );
        assert_eq!(label_for_mimes(&["application/pdf".into()]), "PDF document");
        assert_eq!(label_for_mimes(&["text/uri-list".into()]), "Text");
        assert_eq!(label_for_mimes(&[]), "1 item");
        assert_eq!(label_for_mimes(&["x-special/gnome".into()]), "1 item");
        // Phase `a_uri_list_yields_the_file_name`.
        // The name a client's drag is carrying comes out of the URI list it
        // already offers — which is the whole reason the chip can say
        // `photo.png` rather than `Image`.
        assert_eq!(
            name_from_uri_list(b"file:///home/me/photo.png\r\n").as_deref(),
            Some("photo.png")
        );
        assert_eq!(
            name_from_uri_list(b"file:///home/me/my%20photo%231.png\r\n").as_deref(),
            Some("my photo#1.png"),
            "a name a URI had to escape shows as the user wrote it"
        );
        assert_eq!(
            name_from_uri_list(b"file:///tmp/C++%20notes%2.txt\r\n").as_deref(),
            Some("C++ notes%2.txt"),
            "a plus is literal and a malformed escape is kept verbatim"
        );
        assert_eq!(
            name_from_uri_list(b"file:///tmp/caf%C3%A9%FF.txt\r\n").as_deref(),
            Some("caf\u{e9}\u{fffd}.txt"),
            "invalid UTF-8 is replaced, not dropped"
        );
        assert_eq!(
            name_from_uri_list(b"file://localhost/tmp/scan.jpg\r\n").as_deref(),
            Some("scan.jpg"),
            "an authority is not part of the name"
        );
        assert_eq!(
            name_from_uri_list(b"# comment\r\nfile:///a.png\r\nfile:///b.png\r\n").as_deref(),
            Some("2 items")
        );
        assert_eq!(
            name_from_uri_list(b""),
            None,
            "nothing usable leaves the chip alone"
        );
        assert_eq!(name_from_uri_list(b"# only a comment\r\n"), None);
        // Phase `paths_are_named_by_their_file_name`.
        // A drag the compositor owns knows the file name, and should use it.
        assert_eq!(
            label_for_paths(&[PathBuf::from("/w/Demo/photo.png")]),
            "photo.png"
        );
        assert_eq!(
            label_for_paths(&[PathBuf::from("/a.png"), PathBuf::from("/b.png")]),
            "2 items"
        );
        assert_eq!(label_for_paths(&[]), "1 item");
    }
}

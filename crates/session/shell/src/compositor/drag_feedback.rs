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

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect};
use vello::peniko::Fill;

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

/// Draw the pointer and, when one is in flight, what it is carrying.
///
/// `label` is what the drag is carrying, already resolved to something a
/// person can read — a file name where the compositor knows it, a kind
/// where all it has is the offer. `None` means no drag: only the pointer
/// is drawn.
pub fn draw(
    scene: &mut Scene,
    cursor: Point,
    label: Option<&str>,
    theme: &Theme,
    text: &mut TextRenderer,
) {
    if let Some(label) = label {
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
/// that sits *inside* a Panel and paints a veneer fill (`surface_white_5`),
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
    let size = theme.tokens.typography.scale.sm;
    let (tw, th) = text.measure(label, size, None);
    let w = f64::from(tw) + CHIP_PAD * 2.0;
    let x = cursor.x + CHIP_DX;
    let y = cursor.y + CHIP_DY;
    let bounds = Rect::new(x, y, x + w, y + CHIP_H);
    // Control role: a pressable-looking chip takes `radii.lg`, and a carried
    // object is genuinely lifted, so its shadow comes from the elevation
    // token rather than a literal black offset copy (HIG §9.3).
    let radius = ui::chrome::ChromeRole::Control.radius(theme, CHIP_H);
    let rect = RoundedRect::from_rect(bounds, radius);
    ui::chrome::shell::panel_shadow(scene, Affine::IDENTITY, bounds, radius, theme, 1.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_primary,
        None,
        &rect,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &rect,
    );
    text.draw_text(
        scene,
        label,
        size,
        theme.text_primary,
        Affine::translate((x + CHIP_PAD, y + (CHIP_H - f64::from(th)) / 2.0)),
        None,
    );
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
        "Image".to_string()
    } else if has("video/") {
        "Video".to_string()
    } else if has("audio/") {
        "Audio".to_string()
    } else if mimes.iter().any(|m| m == "application/pdf") {
        "PDF document".to_string()
    } else if has("text/") {
        "Text".to_string()
    } else {
        "1 item".to_string()
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
        return Some(format!("{} items", entries.len()));
    }
    let first = entries.first()?;
    let path = first.strip_prefix("file://").unwrap_or(first);
    // Drop a `file://host/path` authority, then take the last segment.
    let path = match path.find('/') {
        Some(0) | None => path,
        Some(i) => path.get(i..).unwrap_or(path),
    };
    let last = path.rsplit('/').find(|s| !s.is_empty())?;
    let name = percent_decode(last);
    (!name.is_empty()).then_some(name)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'%'
            && let Some(hex) = tail.get(..2)
            && let Ok(hex_str) = std::str::from_utf8(hex)
            && let Ok(v) = u8::from_str_radix(hex_str, 16)
        {
            out.push(v);
            rest = tail.get(2..).unwrap_or(&[]);
            continue;
        }
        out.push(first);
        rest = tail;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A readable name for a set of paths the compositor itself is carrying.
pub fn label_for_paths(paths: &[std::path::PathBuf]) -> String {
    match paths {
        [] => "1 item".to_string(),
        [one] => one.file_name().map_or_else(
            || one.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        many => format!("{} items", many.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The chip has to say something for every drag, including one whose
    /// offer says nothing recognisable — an empty chip is worse than a
    /// vague one.
    #[test]
    fn every_offer_produces_a_label() {
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
    }

    /// The name a client's drag is carrying comes out of the URI list it
    /// already offers — which is the whole reason the chip can say
    /// `photo.png` rather than `Image`.
    #[test]
    fn a_uri_list_yields_the_file_name() {
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
    }

    /// A drag the compositor owns knows the file name, and should use it.
    #[test]
    fn paths_are_named_by_their_file_name() {
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

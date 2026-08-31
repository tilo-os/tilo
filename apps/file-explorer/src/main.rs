// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
// Thin binary entry point — all logic lives in the library.
//
// Supports two modes:
//   * default — build a `FileExplorerWidget`, hand it to `ui::App`,
//     run the live event loop (winit window).
//   * `--shots <PATH>` — render the file explorer at a default size into
//     a PNG at `<PATH>` and exit. No window, no event loop. Used for
//     headless screenshot tests and CI snapshots of the cold-start UI.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::HashMap;

use file_explorer::FileExplorerWidget;
use taffy::prelude::*;
use ui::launch::LaunchSpec;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

/// An Automerge actor id that is actually distinct from every other replica's.
///
/// `format!("{prefix}-{pid}")` was not. Every application here runs in its own
/// PID namespace, so every instance sees itself as pid 2 and every session
/// wrote under the *same* actor — and Automerge refuses a frame whose actor
/// already used that sequence number:
///
///     blob_relay: a document frame did not parse; it is lost for this replica
///     error=duplicate seq 1 found for actor 66696c65732d32     ("files-2")
///
/// Frames dropped, silently, in the one direction that matters: a change made
/// on one machine never reaching another.
///
/// `have` is the actor `ui::persist::open` has already put on the document:
/// `app_state::DocStore` mints one at random per state directory, persists it
/// in an `actor` sidecar and takes a lock so a second instance cannot claim
/// it. That is precisely the guarantee this needs, and the app used to throw
/// it away. Prefixing keeps the readable half without inventing the unique
/// half, and it is *stable across restarts*, which a clock reading is not.
///
/// Deliberately reads no environment: the whole failure was an id that was a
/// function of things two replicas share, and a function of its arguments is
/// the only shape that can be tested for it.
fn distinct_actor(prefix: &str, have: &automerge::ActorId) -> String {
    let hex = have.to_hex_string();
    let short: String = hex.chars().take(12).collect();
    format!("{prefix}-{short}")
}

/// The name this replica is given, if anything has named it.
///
/// `QUICK_APP_ID` is how a dev run points two instances at one shell and gets
/// a history it can read. `QOS_USER_ID` is the session's own identity and
/// would be the best of both — it does not reach a sandboxed app, because the
/// launcher builds an app's environment from an allowlist and it is not on
/// it, so in the product neither of these answers and the actor comes from
/// [`distinct_actor`].
fn named_actor(prefix: &str) -> Option<String> {
    if let Ok(id) = std::env::var("QUICK_APP_ID")
        && !id.is_empty()
    {
        return Some(id);
    }
    match std::env::var("QOS_USER_ID") {
        Ok(user) if !user.is_empty() => Some(format!("{prefix}-{user}")),
        _ => None,
    }
}

/// # Errors
///
/// Returns an error if `--shots` mode fails to lay out or render the
/// offscreen frame (a taffy layout error, or writing the output PNG).
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let viewport = parse_size_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let scenario = parse_scenario_arg(&args);
        // Some scenarios seed a demo directory that must exist BEFORE the widget
        // is built (so the sidebar / workspace roots pick it up), so seed here.
        let demo_dir = match scenario.as_deref() {
            Some("sync") => Some(seed_sync_demo()),
            Some("thumbnails") => Some(seed_thumbnails_demo()),
            Some("thumbnails-fs") => Some(seed_thumbnails_fs_demo()),
            _ => None,
        };
        let mut widget = FileExplorerWidget::new();
        // Optional `--path <dir>` starts the shot in a specific directory
        // (e.g. to capture a deep breadcrumb trail), else the default
        // cold-start location is used.
        if let Some(d) = demo_dir {
            widget.navigate(d);
        } else if let Some(p) = parse_path_arg(&args) {
            widget.navigate(p);
        }
        // Pre-layout scenario setup (view mode + selection must be set before
        // the tree is built so the composed view captures them).
        match scenario.as_deref() {
            Some("grid" | "thumbnails" | "thumbnails-fs") => {
                widget.set_view_mode(file_explorer::model::ViewMode::Grid);
            }
            Some("select") => {
                widget.debug_select(&[7, 8]);
                // Surface the floating status bubble for the still (count + N
                // selected), since a single headless frame can't hover it.
                widget.arm_status_bubble_for_shots();
            }
            // Just the floating status bubble (item count) over the cold-start
            // view, with nothing selected.
            Some("status") => widget.arm_status_bubble_for_shots(),
            // Begin an inline rename before the seeding sync so the in-place
            // editor is drawn over the row (with the name selected).
            Some("rename") => {
                widget.debug_select(&[3]);
                widget.begin_rename();
            }
            // Summon the places drawer over the file view (the narrow
            // layout's hamburger action). Set BEFORE layout so `apply_layout`
            // places the revealed drawer — snapping post-layout would leave
            // the panes at their pre-reveal rects.
            Some("places") => widget.debug_open_places_drawer(),
            _ => {}
        }
        render_shot_to_png(widget, viewport, scenario.as_deref(), &out)?;
        return Ok(());
    }

    ui::launch::run(
        &LaunchSpec::new("dev.qos.file-explorer", || {
            Box::new(FileExplorerWidget::new())
        })
        .open_state(open_state),
    )?;
    Ok(())
}

/// Open persisted state while preserving the file manager's per-replica actor
/// naming. The shared runner still owns autosave, headless driving, and exit.
fn open_state(app_id: &'static str) -> (ui::State, ui::persist::Autosave) {
    let (mut state, autosave) = ui::persist::open(app_id);
    // Two windows of the file manager are two authors of the same document
    // — an inline rename is native collaborative `Text` (see
    // `actions.rs`), so concurrent edits have to be attributable to tell
    // them apart. `QUICK_APP_ID`/`QOS_USER_ID` name a known replica; a
    // sandboxed launch has neither, so the fallback has to be sound on its
    // own — see [`distinct_actor`].
    let app_id =
        named_actor("files").unwrap_or_else(|| distinct_actor("files", state.doc.get_actor()));
    ui::diag!("file-explorer: automerge actor {app_id}");
    state
        .doc
        .set_actor(automerge::ActorId::from(app_id.as_bytes()));
    (state, autosave)
}

/// Seed a demo workspace directory whose files carry real `user.qos.sync`
/// extended attributes, and point `QOS_WORKSPACE_ROOTS` at it, so a `--shots
/// --scenario sync` frame exercises the genuine badge path (`read_dir_sync` →
/// `getxattr` → `Entry.sync_state`) rather than any mock. Returns the dir.
fn seed_sync_demo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("qos-files-sync-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    // A believable spread across the three hydration states + a subfolder.
    let files: [(&str, &str); 6] = [
        ("Design brief.md", "local"),
        ("meeting-notes.txt", "local"),
        ("Q3 budget.ods", "projected"),
        ("architecture.pdf", "projected"),
        ("launch-video.mp4", "downloading"),
        ("dataset.parquet", "downloading"),
    ];
    for (name, state) in files {
        let p = dir.join(name);
        let _ = std::fs::write(&p, b"");
        let _ = xattr::set(&p, "user.qos.sync", state.as_bytes());
    }
    let _ = std::fs::create_dir_all(dir.join("shared-assets"));
    // SAFETY: called once at process startup on the main thread, before any
    // other thread exists, so there is no concurrent environment access.
    unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", &dir) };
    dir
}

/// Seed a demo directory of **real** sample files — gradient images, a genuine
/// one-page PDF, a real EPUB with a cover, a video (badge), and an assortment of
/// badge-only types — so a `--shots --scenario thumbnails` grid renders the
/// actual generator + service path (decode → downscale → cache → paint), not a
/// mock. A plain local directory (not a workspace root), so previews are
/// generated client-side by the shared service. Returns the dir.
fn seed_thumbnails_demo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("qos-files-thumbnails-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    // A subfolder (folder glyph) for visual balance.
    let _ = std::fs::create_dir_all(dir.join("Camera Roll"));

    // Real raster images across formats + patterns.
    write_image(
        &dir.join("sunset.png"),
        640,
        480,
        0,
        image::ImageFormat::Png,
    );
    write_image(
        &dir.join("harbour.jpg"),
        640,
        480,
        1,
        image::ImageFormat::Jpeg,
    );
    write_image(
        &dir.join("forest-path.png"),
        480,
        640,
        2,
        image::ImageFormat::Png,
    );
    write_image(
        &dir.join("bloom.jpg"),
        600,
        600,
        3,
        image::ImageFormat::Jpeg,
    );
    write_image(&dir.join("study.png"), 720, 480, 4, image::ImageFormat::Png);

    // A genuine one-page PDF (pdfium renders its first page) and a real EPUB.
    let _ = std::fs::write(dir.join("Field guide.pdf"), minimal_pdf("Field Guide"));
    let _ = std::fs::write(
        dir.join("The Lantern-Keeper.epub"),
        minimal_epub("The Lantern-Keeper", &gradient_png(300, 440, 3)),
    );

    // A video: no gstreamer in the hermetic build, so this shows an honest video
    // badge rather than a fabricated frame.
    let _ = std::fs::write(dir.join("launch-clip.mp4"), b"");

    // Badge-only types — each renders a category-tinted card + extension.
    for (name, body) in [
        ("release.zip", &b"PK\x03\x04"[..]),
        ("main.rs", b"fn main() {}"),
        ("README.md", b"# Readme"),
        ("theme.mp3", b""),
        ("Report.docx", b""),
        ("Budget.xlsx", b""),
        ("config.json", b"{}"),
        ("Inter.ttf", b""),
    ] {
        let _ = std::fs::write(dir.join(name), body);
    }
    dir
}

/// Seed a demo **workspace** directory whose files carry real `user.qos.thumb`
/// (host-generated preview PNG) and `user.qos.sync` extended attributes — so a
/// `--shots --scenario thumbnails-fs` grid exercises the FS-metadata path: the
/// file manager paints the host's gossiped preview (via the xattr), even for a
/// file whose content is only *projected*. Sets `QOS_WORKSPACE_ROOTS`.
fn seed_thumbnails_fs_demo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("qos-files-thumbnails-fs-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    // Files whose *content* stays empty here — the preview rides entirely on the
    // host-provided `user.qos.thumb` metadata, exactly as a peer would see a file
    // it has not yet downloaded. Each carries a real preview PNG + a sync state.
    let previews: [(&str, u8, &str); 5] = [
        ("Sunset over bay.png", 0, "local"),
        ("Harbour.jpg", 1, "projected"),
        ("Trail map.pdf", 2, "projected"),
        ("Cover art.png", 3, "downloading"),
        ("Studio.jpg", 4, "local"),
    ];
    for (name, kind, sync) in previews {
        let p = dir.join(name);
        let _ = std::fs::write(&p, b""); // content projected / not local
        // A small (metadata-sized) PNG preview, as the host generates + gossips
        // it. Kept tiny here so it also fits the demo temp filesystem's per-xattr
        // cap (btrfs bounds a single xattr to its ~16 KiB node size; the real
        // in-memory FUSE mount allows the full 64 KiB, which HOST_EDGE targets).
        let _ = xattr::set(&p, "user.qos.thumb", &gradient_png(112, 84, kind));
        let _ = xattr::set(&p, "user.qos.sync", sync.as_bytes());
    }
    // A couple of files with no host preview → sync badge + type badge.
    for (name, sync) in [("dataset.parquet", "projected"), ("notes.md", "local")] {
        let p = dir.join(name);
        let _ = std::fs::write(&p, b"");
        let _ = xattr::set(&p, "user.qos.sync", sync.as_bytes());
    }
    // SAFETY: process startup, main thread, before any other thread exists.
    unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", &dir) };
    dir
}

/// Write a gradient demo image to `path` in the given format.
fn write_image(path: &std::path::Path, w: u32, h: u32, kind: u8, fmt: image::ImageFormat) {
    let img = gradient_image(w, h, kind);
    let mut out = Vec::new();
    if image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), fmt)
        .is_ok()
    {
        let _ = std::fs::write(path, out);
    }
}

/// A distinct, believable gradient/pattern per `kind` so demo thumbnails read as
/// real, varied images rather than flat swatches.
fn gradient_image(width: u32, height: u32, kind: u8) -> image::RgbaImage {
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t.clamp(0.0, 1.0);
    let mut img = image::RgbaImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let fx = x as f32 / width as f32;
        let fy = y as f32 / height as f32;
        let (r, g, b) = match kind % 5 {
            0 => (
                lerp(255.0, 60.0, fy),
                lerp(150.0, 40.0, fy),
                lerp(70.0, 130.0, fy),
            ), // sunset
            1 => {
                let (dx, dy) = (fx - 0.5, fy - 0.42);
                let d = (dx * dx + dy * dy).sqrt() * 1.7;
                (
                    lerp(40.0, 12.0, d),
                    lerp(165.0, 55.0, d),
                    lerp(215.0, 120.0, d),
                ) // harbour
            }
            2 => {
                let t = (fx + fy) * 0.5;
                (
                    lerp(90.0, 24.0, t),
                    lerp(185.0, 95.0, t),
                    lerp(95.0, 55.0, t),
                ) // forest
            }
            3 => (
                lerp(240.0, 250.0, fx),
                lerp(90.0, 190.0, fy),
                lerp(170.0, 70.0, fx),
            ), // bloom
            _ => {
                let band = (((x / 18 + y / 18) % 2) as f32) * 34.0;
                let c = 120.0 + fy * 70.0 + band;
                (c, c * 0.98, c * 0.92) // study (soft warm gray)
            }
        };
        *px = image::Rgba([r as u8, g as u8, b as u8, 255]);
    }
    img
}

/// A gradient PNG (used for EPUB covers and host-provided preview xattrs).
fn gradient_png(w: u32, h: u32, kind: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = image::DynamicImage::ImageRgba8(gradient_image(w, h, kind))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

/// Build a minimal but valid one-page PDF (a coloured header band + title text +
/// a framed box) with correct cross-reference offsets, so pdfium rasterises its
/// first page into a real thumbnail.
fn minimal_pdf(title: &str) -> Vec<u8> {
    let esc: String = title
        .chars()
        .flat_map(|c| match c {
            '(' | ')' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect();
    let content = format!(
        "q\n0.97 0.98 0.99 rg\n0 0 300 400 re f\n\
         0.24 0.48 0.92 rg\n0 340 300 60 re f\n\
         1 1 1 rg\nBT /F1 22 Tf 20 356 Td ({esc}) Tj ET\n\
         0.30 0.33 0.38 rg\nBT /F1 11 Tf 20 314 Td (A sample document page) Tj ET\n\
         0.82 0.85 0.90 rg\n20 292 210 8 re f\n20 276 240 8 re f\n20 260 220 8 re f\n20 244 180 8 re f\n\
         0.55 0.75 0.58 rg\n20 120 118 104 re f\n0.92 0.80 0.55 rg\n146 120 114 104 re f\n\
         0.82 0.85 0.90 rg\n20 96 240 8 re f\n20 80 200 8 re f\n20 64 226 8 re f\nQ\n"
    );
    let objs: Vec<Vec<u8>> = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 400]/Resources<</Font<</F1 5 0 R>>>>/Contents 4 0 R>>".to_vec(),
        format!("<</Length {}>>\nstream\n{content}endstream", content.len()).into_bytes(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
    ];
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = vec![0usize; objs.len() + 1];
    for (i, body) in objs.iter().enumerate() {
        if let Some(slot) = offsets.get_mut(i + 1) {
            *slot = out.len();
        }
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref_pos}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Build a minimal but real EPUB (ZIP + OPF) whose manifest declares `cover_png`
/// as the cover image, so the shared generator extracts + downscales it.
#[allow(
    clippy::unwrap_used,
    reason = "every write here targets an in-memory Vec-backed Cursor with fixed, well-formed demo content and distinct entry names; the zip crate can only fail this call on I/O or duplicate-name errors, neither of which applies"
)]
fn minimal_epub(title: &str, cover_png: &[u8]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let stored =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();
        zip.start_file("META-INF/container.xml", deflated).unwrap();
        zip.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#).unwrap();
        zip.start_file("OEBPS/content.opf", deflated).unwrap();
        let opf = format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">{title}</dc:title></metadata>
  <manifest>
    <item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>
    <item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#
        );
        zip.write_all(opf.as_bytes()).unwrap();
        zip.start_file("OEBPS/ch1.xhtml", deflated).unwrap();
        zip.write_all(b"<html><body><p>Chapter one.</p></body></html>")
            .unwrap();
        zip.start_file("OEBPS/cover.png", deflated).unwrap();
        zip.write_all(cover_png).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Returns `Some((w, h))` if `--size WxH` (or `--size=WxH`) appears in
/// `args`, otherwise `None`. Lets a caller render the same offscreen frame
/// at different viewport sizes (e.g. a normal 1280×800 and a narrow width)
/// without a rebuild.
fn parse_size_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let spec = loop {
        let arg = iter.next()?;
        if arg == "--size" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--size=") {
            break rest.to_string();
        }
    };
    let (w, h) = spec.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Returns `Some(name)` if `--scenario <name>` (or `--scenario=<name>`) appears
/// in `args`. Drives headless demo shots (`grid`, `menu`, `reorder`).
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Returns `Some(dir)` if `--path <dir>` (or `--path=<dir>`) appears in
/// `args`, otherwise `None`. Only meaningful alongside `--shots`.
fn parse_path_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--path" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--path=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Apply scenario-specific state that needs the already-laid-out geometry (a
/// mid-drag reorder, an open context menu, a hovered row) — split out of
/// [`render_shot_to_png`] purely to stay under the line-count limit.
fn apply_post_layout_scenario(
    widget: &mut FileExplorerWidget,
    scenario: Option<&str>,
    (w, h): (u32, u32),
    state: &mut ui::State,
    child: NodeId,
    taffy: &TaffyTree<()>,
) {
    match scenario {
        Some("menu") => {
            // Right-click context menu + the "New File" template flyout, anchored
            // in the content pane.
            widget.debug_show_templates_menu(Point::new(f64::from(w) * 0.45, f64::from(h) * 0.34));
        }
        Some("reorder") => {
            // Grab a favourite and lift it two rows up — mid-drag.
            widget.debug_begin_favorite_drag(4, 1);
        }
        Some("pindrag") => {
            // Carry a folder out of the list toward the sidebar — mid-drag, so
            // the floating ghost shows over the Favourites section.
            let folder = widget.current_path().join("Development");
            widget.debug_begin_pin_drag(folder, Point::new(150.0, 300.0));
        }
        Some("dropmove") => {
            // Drag a file onto a folder — mid-drop, so the ghost + the folder's
            // drop-target highlight both show. Pick the first published folder
            // row as the target and the first file as the dragged item.
            let target = widget
                .debug_folder_targets()
                .into_iter()
                .next()
                .map(|(p, _)| p);
            let source = widget
                .entries()
                .iter()
                .find(|e| !e.is_dir)
                .map(|e| e.path.clone());
            if let (Some(target), Some(source)) = (target, source) {
                widget.debug_begin_move_drag(vec![source], target);
            }
        }
        Some("select") => {
            // Move the cursor over a (different) row so the hover veneer shows
            // alongside the accent selection.
            let ev = winit::event::WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(f64::from(w) * 0.35, 560.0),
            };
            let mut handled = false;
            let mut focus = None;
            let mods = winit::event::Modifiers::default();
            widget.handle_event(
                &ev,
                1.0,
                &mods,
                &mut state.doc,
                &state.root,
                &mut handled,
                &mut focus,
                child,
                taffy,
            );
        }
        _ => {}
    }
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle, then
/// applies any post-layout `scenario` (which needs the laid-out geometry —
/// e.g. a mid-drag reorder or an open context menu) and finally composites the
/// overlay layer so context menus appear in the shot.
fn render_shot_to_png(
    mut widget: FileExplorerWidget,
    viewport: (u32, u32),
    scenario: Option<&str>,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=<name>` renders the shot under any named theme (e.g.
    // `dracula`, `gruvbox`, `catppuccin-mocha`, `light`, `high-contrast`), so
    // the same frame can be graded under every palette. Defaults to dark.
    let theme = std::env::var("QOS_SHOTS_THEME")
        .ok()
        .and_then(|n| Theme::named(&n))
        .unwrap_or_else(Theme::dark);
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    // Post-layout scenarios need the laid-out geometry.
    apply_post_layout_scenario(&mut widget, scenario, (w, h), &mut state, child, &taffy);

    // Thumbnail scenarios: requests are enqueued during draw, so a throwaway
    // warm-up draw kicks off generation for the visible cells, then we wait for
    // the pool to settle before the frame we keep — so the shot captures real
    // previews, never the loading placeholders.
    if matches!(scenario, Some("thumbnails" | "thumbnails-fs")) {
        let mut warm = Scene::new();
        widget.draw_with_presence(
            &mut warm,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
        );
        widget.prewarm_thumbnails(std::time::Duration::from_secs(20));
    }

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    // Composite the overlay layer (context menu + flyout) on top.
    widget.draw_overlay(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("file-explorer: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod actor_tests {
    use super::distinct_actor;

    /// The regression, stated as the property it broke.
    ///
    /// Both ids below are minted inside one process, so anything derived from
    /// the process — a pid, which is 2 in every sandbox on this system —
    /// produces the same string twice. Automerge then refuses the second
    /// replica's frames with `duplicate seq 1 found for actor`, and a change
    /// made on one machine never reaches the other.
    #[test]
    fn two_replicas_in_one_process_do_not_share_an_actor() {
        let a = automerge::AutoCommit::new();
        let b = automerge::AutoCommit::new();
        assert_ne!(
            distinct_actor("files", a.get_actor()),
            distinct_actor("files", b.get_actor()),
            "two replicas were given the same actor id, which is the failure \
             that silently drops one of them's changes",
        );
    }

    /// And it still says what it is, so a history can be read.
    #[test]
    fn the_actor_still_names_the_application() {
        let doc = automerge::AutoCommit::new();
        assert!(distinct_actor("files", doc.get_actor()).starts_with("files-"));
    }
}

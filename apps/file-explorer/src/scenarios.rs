//! Screenshot scenarios: the folders a capture shows.
//!
//! A capture never browses the machine it is taken on. Every scenario's
//! folders are written into the capture's scratch directory before the window
//! is built — a made-up home with its standard places, and for the scenarios
//! about the Tilo filesystem, a workspace whose files carry real
//! `user.tilo.sync`, `user.tilo.thumb` and history extended attributes — so
//! the window reads them through the same listing, badge and preview paths
//! it reads a person's files with. The view state a scenario shows (a
//! selection, an open menu, a drag in flight) is staged by the surface once
//! the window is laid out ([`crate::FileExplorerWidget::stage_scenario`]).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use host_fs::attributes as xattr;
use ui::framework::Fixture;

use crate::resource::FileExplorer;

/// Every screenshot scenario.
const ALL_SCENARIOS: &[&str] = &[
    "archive",
    "archive-menu",
    "context-menu",
    "dropmove",
    "drives",
    "error",
    "grid",
    "history",
    "loading",
    "menu",
    "pindrag",
    "places",
    "recent",
    "recent-menu",
    "recent-revealed",
    "recent-forgotten",
    "remote-account",
    "rename",
    "reorder",
    "select",
    "selection-actions",
    "selection-sort",
    "selection-sorted",
    "selection-reversed",
    "status",
    "sync",
    "thumbnails",
    "thumbnails-fs",
    "undo-trash",
    "clash",
    "preview",
];

/// The scenarios a host without extended attributes can show truthfully:
/// all but the ones about the Tilo filesystem's projection (sync badges,
/// host previews, file history).
const PORTABLE_SCENARIOS: &[&str] = &[
    "archive",
    "archive-menu",
    "context-menu",
    "dropmove",
    "drives",
    "error",
    "grid",
    "loading",
    "menu",
    "pindrag",
    "places",
    "recent",
    "recent-menu",
    "recent-revealed",
    "recent-forgotten",
    "remote-account",
    "rename",
    "reorder",
    "select",
    "selection-actions",
    "selection-sort",
    "selection-sorted",
    "selection-reversed",
    "status",
    "thumbnails",
    "undo-trash",
    "clash",
    "preview",
];

/// The screenshot scenarios this host can show.
pub fn scenarios() -> &'static [&'static str] {
    if xattr::SUPPORTED {
        ALL_SCENARIOS
    } else {
        PORTABLE_SCENARIOS
    }
}

/// The folders a capture shows, written before its window is built.
#[derive(Clone, Debug)]
pub struct Scene {
    /// The made-up home: its standard places are the sidebar's.
    pub home: PathBuf,
    /// Where the window opens.
    pub start: PathBuf,
    /// The Tilo workspace roots the window shows as such.
    pub workspace_roots: Vec<PathBuf>,
    /// Where the capture's previews are cached.
    pub thumbnails: PathBuf,
}

/// Every file in a scene carries this modification time, so the dates a
/// capture shows are the same on every run.
const SCENE_TIME_SECS: u64 = 1_789_727_400;

/// Write the folders `fixture`'s scenario shows and hand them to the window.
pub fn prepare(fixture: &mut Fixture<FileExplorer>) {
    let scenario = fixture.scenario().map(str::to_owned);
    let root = match fixture.directory() {
        Ok(root) => root.to_path_buf(),
        Err(error) => {
            tracing::warn!("file-explorer: no scenario directory: {error}");
            return;
        }
    };
    match write_scene(&root, scenario.as_deref()) {
        Ok(scene) => fixture.provide(scene),
        Err(error) => tracing::warn!("file-explorer: scenario folders not written: {error}"),
    }
}

fn write_scene(root: &Path, scenario: Option<&str>) -> std::io::Result<Scene> {
    let home = root.join("home").join("alex");
    write_home(&home)?;
    let thumbnails = root.join("thumbnails");
    std::fs::create_dir_all(&thumbnails)?;
    let workspace = root.join("workspace");
    let (start, workspace_roots) = match scenario {
        Some("sync") => (write_sync(&workspace)?, vec![workspace]),
        Some("history") => (write_history(&workspace)?, vec![workspace]),
        Some("thumbnails-fs") => (write_host_previews(&workspace)?, vec![workspace]),
        Some("thumbnails") => (write_previews(&root.join("Previews"))?, Vec::new()),
        Some("archive" | "archive-menu") => (write_archive(&home)?, Vec::new()),
        _ => (home.clone(), Vec::new()),
    };
    Ok(Scene {
        home,
        start,
        workspace_roots,
        thumbnails,
    })
}

/// Write `bytes` to `path` with the scene's fixed modification time.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    std::fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(scene_time())
}

fn scene_time() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(SCENE_TIME_SECS)
}

/// Set the projection attribute `name` the Tilo filesystem would report on
/// `path`. A scratch filesystem without user attributes shows the files
/// without it, as a host without the projection would.
fn mark(path: &Path, name: &str, value: &[u8]) {
    if let Err(error) = xattr::set(path, name, value) {
        tracing::warn!(
            "file-explorer: {name} not set on {}: {error}",
            path.display()
        );
    }
}

fn make_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::File::open(path)?.set_modified(scene_time())
}

/// A believable home: the standard places, a project folder to pin, a
/// template for the "New File" menu, and a handful of documents.
fn write_home(home: &Path) -> std::io::Result<()> {
    for folder in [
        "Desktop",
        "Development",
        "Documents",
        "Downloads",
        "Music",
        "Pictures",
        "Public",
        "Templates",
        "Videos",
    ] {
        make_dir(&home.join(folder))?;
    }
    write_file(
        &home.join("Templates").join("Meeting notes.md"),
        b"# Notes\n",
    )?;
    write_file(&home.join("Templates").join("Letter.odt"), b"")?;
    for (name, body) in [
        ("Budget 2026.ods", b"".as_slice()),
        ("Holiday plan.md", b"# Lisbon in May\n"),
        ("Notes.txt", b"Pick up the keys\n"),
        ("Quarterly report.pdf", b""),
        ("Reading list.txt", b"The Lantern-Keeper\n"),
        ("Recipes.md", b"# Bread\n"),
    ] {
        write_file(&home.join(name), body)?;
    }
    write_file(
        &home.join("Screenshot 2026-09-14.png"),
        &gradient_png(320, 200, 1),
    )?;
    std::fs::File::open(home)?.set_modified(scene_time())
}

/// A workspace whose files carry the three hydration states the Tilo
/// filesystem reports, so the sync badges come from real attributes.
fn write_sync(workspace: &Path) -> std::io::Result<PathBuf> {
    make_dir(workspace)?;
    for (name, state) in [
        ("Design brief.md", "local"),
        ("meeting-notes.txt", "local"),
        ("Q3 budget.ods", "projected"),
        ("architecture.pdf", "projected"),
        ("launch-video.mp4", "downloading"),
        ("dataset.parquet", "downloading"),
    ] {
        let path = workspace.join(name);
        write_file(&path, b"")?;
        mark(&path, fs_protocol::SYNC_XATTR, state.as_bytes());
    }
    make_dir(&workspace.join("shared-assets"))?;
    Ok(workspace.to_path_buf())
}

/// A workspace file with two versions in its history projection.
fn write_history(workspace: &Path) -> std::io::Result<PathBuf> {
    make_dir(workspace)?;
    let path = workspace.join("Launch brief.md");
    write_file(&path, b"# Launch brief\n\nCurrent plan\n")?;
    let older = "11111111-1111-4111-8111-111111111111";
    let current = "22222222-2222-4222-8222-222222222222";
    let history = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "record_id": "launch-brief-record",
        "total_versions": 2,
        "versions_truncated": false,
        "heads": [current],
        "versions": [
            {"id": older, "record_id": "launch-brief-record", "parents": [],
             "actor": {"person": vec![1; 32], "device": vec![2; 32]},
             "timestamp_ms": 1_788_300_000_000_u64, "content_hash": vec![3; 32],
             "metadata_hash": vec![4; 32], "provenance": "Create"},
            {"id": current, "record_id": "launch-brief-record", "parents": [older],
             "actor": {"person": vec![5; 32], "device": vec![6; 32]},
             "timestamp_ms": 1_788_386_400_000_u64, "content_hash": vec![7; 32],
             "metadata_hash": vec![8; 32], "provenance": "Edit"}
        ],
        "conflicts_truncated": false,
        "conflicts": []
    }))
    .map_err(std::io::Error::other)?;
    mark(&path, fs_protocol::HISTORY_XATTR, &history);
    mark(&path, fs_protocol::SYNC_XATTR, b"local");
    Ok(workspace.to_path_buf())
}

/// A ZIP of a trip, browsed through the real archive reader.
fn write_archive(home: &Path) -> std::io::Result<PathBuf> {
    let path = home.join("Downloads").join("Trip archive.zip");
    let file = std::fs::File::create(&path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(
            zip::DateTime::from_date_and_time(2026, 9, 14, 10, 30, 0)
                .map_err(std::io::Error::other)?,
        );
    for (name, content) in [
        ("Photos/Coast/IMG_2048.jpg", b"photo".as_slice()),
        ("Photos/Coast/IMG_2051.jpg", b"photo".as_slice()),
        ("Documents/Itinerary.pdf", b"document".as_slice()),
        ("Documents/Packing list.txt", b"list".as_slice()),
        ("Read me.txt", b"A trip archive".as_slice()),
    ] {
        zip.start_file(name, options)
            .map_err(std::io::Error::other)?;
        zip.write_all(content)?;
    }
    zip.finish().map_err(std::io::Error::other)?;
    std::fs::File::options()
        .write(true)
        .open(&path)?
        .set_modified(scene_time())?;
    Ok(path)
}

/// Real sample files — images, a one-page PDF, an EPUB with a cover, a video
/// and badge-only types — so the grid shows the generator's actual previews.
fn write_previews(dir: &Path) -> std::io::Result<PathBuf> {
    make_dir(dir)?;
    make_dir(&dir.join("Camera Roll"))?;
    for (name, width, height, kind, format) in [
        ("sunset.png", 640, 480, 0, image::ImageFormat::Png),
        ("harbour.jpg", 640, 480, 1, image::ImageFormat::Jpeg),
        ("forest-path.png", 480, 640, 2, image::ImageFormat::Png),
        ("bloom.jpg", 600, 600, 3, image::ImageFormat::Jpeg),
        ("study.png", 720, 480, 4, image::ImageFormat::Png),
    ] {
        write_file(
            &dir.join(name),
            &encode_image(&gradient_image(width, height, kind), format),
        )?;
    }
    write_file(&dir.join("Field guide.pdf"), &minimal_pdf("Field Guide"))?;
    write_file(
        &dir.join("The Lantern-Keeper.epub"),
        &minimal_epub("The Lantern-Keeper", &gradient_png(300, 440, 3))?,
    )?;
    // No decoder for video in the hermetic build: an honest video badge.
    write_file(&dir.join("launch-clip.mp4"), b"")?;
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
        write_file(&dir.join(name), body)?;
    }
    Ok(dir.to_path_buf())
}

/// A workspace whose files' content is not here: their previews ride on the
/// host's `user.tilo.thumb` attribute, as a peer sees a file it has not
/// downloaded.
fn write_host_previews(workspace: &Path) -> std::io::Result<PathBuf> {
    make_dir(workspace)?;
    for (name, kind, sync) in [
        ("Sunset over bay.png", 0, "local"),
        ("Harbour.jpg", 1, "projected"),
        ("Trail map.pdf", 2, "projected"),
        ("Cover art.png", 3, "downloading"),
        ("Studio.jpg", 4, "local"),
    ] {
        let path = workspace.join(name);
        write_file(&path, b"")?;
        // Small, as the host makes them: a temporary filesystem bounds one
        // attribute to its node size.
        mark(
            &path,
            fs_protocol::THUMB_XATTR,
            &gradient_png(112, 84, kind),
        );
        mark(&path, fs_protocol::SYNC_XATTR, sync.as_bytes());
    }
    for (name, sync) in [("dataset.parquet", "projected"), ("notes.md", "local")] {
        let path = workspace.join(name);
        write_file(&path, b"")?;
        mark(&path, fs_protocol::SYNC_XATTR, sync.as_bytes());
    }
    Ok(workspace.to_path_buf())
}

/// `image` as a PNG, or as a JPEG (at the `image` crate's default quality)
/// when `format` asks for one, through the runtime's encoders.
fn encode_image(image: &image::RgbaImage, format: image::ImageFormat) -> Vec<u8> {
    let (width, height) = image.dimensions();
    let encoded = if format == image::ImageFormat::Jpeg {
        ui::jpeg::encode_rgba8_opaque(width, height, image.as_raw(), 75)
    } else {
        ui::png::encode_rgba8(width, height, image.as_raw())
    };
    encoded.unwrap_or_default()
}

/// A distinct gradient or pattern per `kind`, so previews read as varied
/// images rather than flat swatches.
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
            ),
            1 => {
                let (dx, dy) = (fx - 0.5, fy - 0.42);
                let d = (dx * dx + dy * dy).sqrt() * 1.7;
                (
                    lerp(40.0, 12.0, d),
                    lerp(165.0, 55.0, d),
                    lerp(215.0, 120.0, d),
                )
            }
            2 => {
                let t = (fx + fy) * 0.5;
                (
                    lerp(90.0, 24.0, t),
                    lerp(185.0, 95.0, t),
                    lerp(95.0, 55.0, t),
                )
            }
            3 => (
                lerp(240.0, 250.0, fx),
                lerp(90.0, 190.0, fy),
                lerp(170.0, 70.0, fx),
            ),
            _ => {
                let band = (((x / 18 + y / 18) % 2) as f32) * 34.0;
                let c = 120.0 + fy * 70.0 + band;
                (c, c * 0.98, c * 0.92)
            }
        };
        *px = image::Rgba([r as u8, g as u8, b as u8, 255]);
    }
    img
}

fn gradient_png(width: u32, height: u32, kind: u8) -> Vec<u8> {
    encode_image(
        &gradient_image(width, height, kind),
        image::ImageFormat::Png,
    )
}

/// A valid one-page PDF (a header band, a title, a framed body) with correct
/// cross-reference offsets, so pdfium renders its first page.
fn minimal_pdf(title: &str) -> Vec<u8> {
    let escaped: String = title
        .chars()
        .flat_map(|c| match c {
            '(' | ')' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect();
    let content = format!(
        "q\n0.97 0.98 0.99 rg\n0 0 300 400 re f\n\
         0.24 0.48 0.92 rg\n0 340 300 60 re f\n\
         1 1 1 rg\nBT /F1 22 Tf 20 356 Td ({escaped}) Tj ET\n\
         0.30 0.33 0.38 rg\nBT /F1 11 Tf 20 314 Td (A sample document page) Tj ET\n\
         0.82 0.85 0.90 rg\n20 292 210 8 re f\n20 276 240 8 re f\n20 260 220 8 re f\n20 244 180 8 re f\n\
         0.55 0.75 0.58 rg\n20 120 118 104 re f\n0.92 0.80 0.55 rg\n146 120 114 104 re f\n\
         0.82 0.85 0.90 rg\n20 96 240 8 re f\n20 80 200 8 re f\n20 64 226 8 re f\nQ\n"
    );
    let objects: Vec<Vec<u8>> = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 400]/Resources<</Font<</F1 5 0 R>>>>/Contents 4 0 R>>".to_vec(),
        format!("<</Length {}>>\nstream\n{content}endstream", content.len()).into_bytes(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
    ];
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A real EPUB (ZIP + OPF) whose manifest declares `cover_png` as its cover,
/// so the preview generator extracts it.
fn minimal_epub(title: &str, cover_png: &[u8]) -> std::io::Result<Vec<u8>> {
    use zip::write::SimpleFileOptions;
    let mut buffer = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
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
    for (name, options, body) in [
        ("mimetype", stored, b"application/epub+zip".as_slice()),
        (
            "META-INF/container.xml",
            deflated,
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#
                .as_slice(),
        ),
        ("OEBPS/content.opf", deflated, opf.as_bytes()),
        (
            "OEBPS/ch1.xhtml",
            deflated,
            b"<html><body><p>Chapter one.</p></body></html>".as_slice(),
        ),
        ("OEBPS/cover.png", deflated, cover_png),
    ] {
        zip.start_file(name, options)
            .map_err(std::io::Error::other)?;
        zip.write_all(body)?;
    }
    zip.finish().map_err(std::io::Error::other)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each scenario's folders are written into the capture's own directory
    /// with fixed times, and the projection scenarios carry real attributes.
    #[test]
    fn scenes_are_written_inside_the_capture_directory() {
        let root = tempfile::tempdir().unwrap();
        let scene = write_scene(root.path(), None).unwrap();
        assert!(scene.start.starts_with(root.path()));
        assert_eq!(scene.start, scene.home);
        assert!(scene.home.join("Development").is_dir());
        let modified = std::fs::metadata(scene.home.join("Notes.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(modified, scene_time());

        let sync = write_scene(root.path(), Some("sync")).unwrap();
        assert_eq!(sync.workspace_roots, std::slice::from_ref(&sync.start));
        let state = xattr::get(sync.start.join("Q3 budget.ods"), fs_protocol::SYNC_XATTR);
        // A temporary filesystem without user attributes cannot carry the
        // projection; the scene is still written.
        if let Ok(state) = state {
            assert_eq!(state.as_deref(), Some(b"projected".as_slice()));
        }

        let archive = write_scene(root.path(), Some("archive")).unwrap();
        assert_eq!(
            archive.start.extension().and_then(|e| e.to_str()),
            Some("zip")
        );
    }
}

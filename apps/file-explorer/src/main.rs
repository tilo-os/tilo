// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
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

use host_fs::attributes as xattr;
use std::io::{Read, Write};

mod demo_art;

use file_explorer::FileExplorerWidget;

const MAX_RECEIVED_SHARE_URI_BYTES: usize = 16 * 1024;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;

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
/// a history it can read. `TILO_USER_ID` is the session's own identity and
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
    match std::env::var("TILO_USER_ID") {
        Ok(user) if !user.is_empty() => Some(format!("{prefix}-{user}")),
        _ => None,
    }
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error if `--shots` mode fails to lay out or render the
/// offscreen frame (a taffy layout error, or writing the output PNG).
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    // Each window keeps a stable, distinct Automerge actor. The framework
    // shell does not, so two Files windows would share one replica identity.
    ui::launch::run(&standalone_launch_spec())?;
    Ok(())
}

fn standalone_launch_spec() -> LaunchSpec {
    LaunchSpec::new_with_args("so.tilo.file-explorer", live_root)
        .history(&file_explorer::framework_history::HISTORY)
        .open_state(open_state)
        .shot_handler(shot_handler)
        .allowed_options(&["--size", "--path"])
        .allowed_live_flags(&["--open-share-stdin"])
        .max_positionals(1)
        .scenarios(&[
            "archive",
            "context-menu",
            "context-menu-folder",
            "dropmove",
            "empty",
            "error",
            "grid",
            "loading",
            "menu",
            "pindrag",
            "places",
            "remote-account",
            "rename",
            "select",
            "status",
            "sync",
            "thumbnails",
            "thumbnails-fs",
            "undo-trash",
        ])
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let size = args.value("--size").and_then(parse_size);
    let viewport = shot_viewport(args.viewport, size);
    let scenario = args.scenario.as_deref();
    if !xattr::SUPPORTED && matches!(scenario, Some("sync" | "thumbnails-fs")) {
        return Err(LaunchError::Operation(
            "this scenario requires the Tilo filesystem projection".into(),
        ));
    }
    // Every capture without a folder of its own shows the same everyday home
    // folder, so the default shot is a populated folder rather than whatever
    // the capturing machine's home happens to hold.
    let demo_home = args.value("--path").is_none().then(seed_demo_home);
    let home_root = demo_home
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("tilo-files-demo-home"));
    // Some scenarios seed a demo directory that must exist BEFORE the widget
    // is built (so the sidebar / workspace roots pick it up), so seed here.
    let demo_dir = match scenario {
        Some("sync") => Some(seed_sync_demo()),
        Some("thumbnails") => Some(seed_thumbnails_demo(&home_root)),
        Some("thumbnails-fs") => Some(seed_thumbnails_fs_demo()),
        Some("archive") => Some(
            seed_archive_demo(&home_root)
                .map_err(|error| LaunchError::Operation(error.to_string()))?,
        ),
        _ => None,
    };
    // Keep screenshot state independent of persisted zoom and Places ordering.
    let mut widget = match &demo_home {
        Some(home) => FileExplorerWidget::with_persistence_dirs(
            None,
            Some(home.clone()),
            file_explorer::favorites::UserDirs::default(),
        ),
        None => FileExplorerWidget::with_persistence(None, tilo_paths::home()),
    };
    // Optional `--path <dir>` starts the shot in a specific directory
    // (e.g. to capture a deep breadcrumb trail), else the default
    // cold-start location is used.
    let start = demo_dir.or_else(|| match scenario {
        // A folder the person was in, rather than the home they started from.
        Some("error" | "loading") => demo_home.as_ref().map(|home| home.join("Documents")),
        Some("empty") => demo_home.as_ref().map(|home| home.join("Notes")),
        _ => demo_home.clone(),
    });
    if let Some(d) = start {
        widget.navigate(d);
    } else if let Some(p) = args.value("--path") {
        widget.navigate(std::path::PathBuf::from(p));
    }
    // Pre-layout scenario setup (view mode + selection must be set before
    // the tree is built so the composed view captures them).
    match scenario {
        Some("grid" | "thumbnails" | "thumbnails-fs") => {
            widget.set_view_mode(file_explorer::model::ViewMode::Grid);
        }
        // A file's own menu: right-click on a file row.
        Some("context-menu") => widget.debug_select(&[9]),
        Some("select") => {
            widget.debug_select(&[7, 8]);
            // Surface the floating status bubble for the still (count + N
            // selected), since a single headless frame can't hover it.
            widget.debug_show_status_bubble();
        }
        // Just the floating status bubble (item count) over the cold-start
        // view, with nothing selected.
        Some("status") => widget.debug_show_status_bubble(),
        // Begin an inline rename before the first sync so the in-place
        // editor is drawn over the row with its local name selected.
        Some("rename") => {
            widget.debug_select(&[3]);
            widget.begin_rename();
        }
        // An intentionally non-connected account detail state. This
        // exercises the privacy-preserving remote account surface without
        // reading a developer's real account store or vault in screenshots.
        Some("remote-account") => {
            let account = file_explorer::remote_accounts::RemoteAccount {
                id: accounts::AccountId::from("shot-remote-account"),
                kind: file_explorer::remote_accounts::RemoteAccountKind::Nextcloud,
                label: "Personal cloud".into(),
                host: "cloud.example".into(),
                health: file_explorer::remote_accounts::RemoteAccountHealth::NeedsAuthentication,
                mount: None,
            };
            widget.debug_show_remote_account(&account, true);
        }
        Some("loading") => widget.debug_show_loading_state(),
        Some("error") => widget.debug_show_error_state(),
        Some("undo-trash") => widget.debug_arm_trash_undo(),
        _ => {}
    }
    // A capture shows the status bubble only when its scenario stages one.
    if !matches!(scenario, Some("select" | "status")) {
        widget.debug_quiet_status_bubble();
    }
    render_shot_to_png(
        widget,
        viewport,
        ui::launch::shot_theme(args),
        scenario,
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(())
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::LaunchSpec::new_with_args"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let raw_args: Vec<String> = std::env::args().collect();
    let received_share = read_received_share_stdin(&raw_args);
    let mut widget = FileExplorerWidget::new();
    if received_share.is_none()
        && let Some(path) = args.positionals().first()
    {
        navigate_launch_directory(&mut widget, std::path::Path::new(path))?;
    }
    match received_share {
        // Admission talks to the recipient service; the widget runs it on
        // the app runtime and opens the mount when it is admitted.
        Some(Ok(uri)) => widget.admit_received_share(uri),
        Some(Err(error)) => widget.set_external_status(error),
        None => {}
    }
    Ok(Box::new(widget))
}

/// Resolve against the launcher's working directory before changing the Files
/// location. Missing or inaccessible directories go through the normal visible
/// navigation error instead of silently opening Home.
fn navigate_launch_directory(
    widget: &mut FileExplorerWidget,
    path: &std::path::Path,
) -> Result<(), LaunchError> {
    let path =
        std::path::absolute(path).map_err(|error| LaunchError::Operation(error.to_string()))?;
    widget.navigate(path);
    Ok(())
}

/// Open persisted state while preserving the file manager's per-replica actor
/// naming. The shared runner still owns autosave, headless driving, and exit.
fn open_state(app_id: &'static str) -> (ui::State, ui::persist::DocWriter) {
    let (mut state, autosave) = ui::persist::open(app_id);
    // Keep each window's actor stable and distinct for the host-owned
    // collaborative document, even though Files itself stores no UI state in
    // it. `QUICK_APP_ID`/`TILO_USER_ID` name a known replica; a sandboxed
    // launch has neither, so the fallback has to be sound on its own — see
    // [`distinct_actor`].
    let app_id =
        named_actor("files").unwrap_or_else(|| distinct_actor("files", state.doc.get_actor()));
    ui::diag!("file-explorer: automerge actor {app_id}");
    state
        .doc
        .set_actor(automerge::ActorId::from(app_id.as_bytes()));
    (state, autosave)
}

fn read_received_share_stdin(args: &[String]) -> Option<Result<String, String>> {
    if !args.iter().any(|arg| arg == "--open-share-stdin") {
        return None;
    }
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take((MAX_RECEIVED_SHARE_URI_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Some(Err(file_explorer::i18n::t(
            "files-received-link-read-failed",
        )));
    }
    if bytes.len() > MAX_RECEIVED_SHARE_URI_BYTES {
        return Some(Err(file_explorer::i18n::t("files-received-link-too-large")));
    }
    let Ok(uri) = String::from_utf8(bytes) else {
        return Some(Err(file_explorer::i18n::t(
            "files-received-link-invalid-text",
        )));
    };
    let uri = uri.trim_end_matches(['\r', '\n']);
    Some(if uri.is_empty() {
        Err(file_explorer::i18n::t("files-received-link-empty"))
    } else {
        Ok(uri.to_owned())
    })
}

/// Seed the everyday home folder every default capture shows: the person's
/// folders and files with fixed sizes and modification times, dated against the
/// pinned capture clock (30 June 2026, 10:42). Sizes are real (sparse) file
/// lengths, so the listing is the genuine path, not a mock.
fn seed_demo_home() -> std::path::PathBuf {
    use chrono::TimeZone;
    let dir = std::env::temp_dir().join("tilo-files-demo-home");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let now = chrono::Utc
        .with_ymd_and_hms(2026, 6, 30, 10, 42, 0)
        .single()
        .map_or(std::time::UNIX_EPOCH, std::time::SystemTime::from);
    let ago = |hours: u64| {
        now.checked_sub(std::time::Duration::from_secs(hours * 3600))
            .unwrap_or(std::time::UNIX_EPOCH)
    };
    for (name, hours) in [
        ("Documents", 5),
        ("Downloads", 1),
        ("Music", 24 * 40),
        ("Notes", 24 * 3),
        ("Pictures", 24 * 9),
        ("Projects", 26),
        ("Videos", 24 * 75),
    ] {
        let path = dir.join(name);
        let _ = std::fs::create_dir_all(&path);
        if let Ok(handle) = std::fs::File::open(&path) {
            let _ = handle.set_modified(ago(hours));
        }
    }
    for (name, bytes, hours) in [
        ("Quarterly report.pdf", 2_400_000_u64, 3),
        ("Household budget.xlsx", 184_000, 30),
        ("Meeting notes.md", 6_200, 2),
        ("Trip itinerary.docx", 96_000, 24 * 6),
        ("Holiday photos.zip", 48_300_000, 24 * 12),
        ("Recipe collection.epub", 1_800_000, 24 * 21),
        ("Wedding invitation.pdf", 540_000, 24 * 33),
        ("To do.txt", 1_100, 8),
        ("Podcast episode 12.mp3", 38_000_000, 24 * 4),
        ("Site redesign.fig", 12_700_000, 24 * 2),
    ] {
        let path = dir.join(name);
        if let Ok(handle) = std::fs::File::create(&path) {
            let _ = handle.set_len(bytes);
            let _ = handle.set_modified(ago(hours));
        }
    }
    dir
}

/// Seed a demo workspace directory whose files carry real `user.tilo.sync`
/// extended attributes, and point `TILO_WORKSPACE_ROOTS` at it, so a `--shots
/// --scenario sync` frame exercises the genuine badge path (`read_dir_sync` →
/// `getxattr` → `Entry.sync_state`) rather than any mock. Returns the dir.
#[allow(
    clippy::disallowed_methods,
    reason = "only called from the --shots handler on the main thread before the \
              widget or any of its worker threads exist, so nothing reads the \
              environment concurrently"
)]
fn seed_sync_demo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("tilo-files-sync-demo");
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
        let _ = xattr::set(&p, "user.tilo.sync", state.as_bytes());
    }
    let _ = std::fs::create_dir_all(dir.join("shared-assets"));
    // SAFETY: called once at process startup on the main thread, before any
    // other thread exists, so there is no concurrent environment access.
    unsafe { std::env::set_var("TILO_WORKSPACE_ROOTS", &dir) };
    dir
}

/// Build a believable archive hierarchy for permanent responsive screenshots.
/// The shot navigates the actual ZIP reader rather than injecting sample rows.
fn seed_archive_demo(
    home: &std::path::Path,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let downloads = home.join("Downloads");
    std::fs::create_dir_all(&downloads)?;
    let path = downloads.join("Trip archive.zip");
    let file = std::fs::File::create(&path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in [
        ("Photos/Coast/IMG_2048.jpg", b"photo".as_slice()),
        ("Photos/Coast/IMG_2051.jpg", b"photo".as_slice()),
        ("Documents/Itinerary.pdf", b"document".as_slice()),
        ("Documents/Packing list.txt", b"list".as_slice()),
        ("Read me.txt", b"A trip archive".as_slice()),
    ] {
        zip.start_file(name, options)?;
        zip.write_all(content)?;
    }
    zip.finish()?;
    Ok(path)
}

/// Seed a demo directory of **real** sample files — pictures, a genuine
/// one-page PDF, a real EPUB with a cover, a video (badge), and an assortment of
/// badge-only types — so a `--shots --scenario thumbnails` grid renders the
/// actual generator + service path (decode → downscale → cache → paint), not a
/// mock. A plain local directory (not a workspace root), so previews are
/// generated client-side by the shared service. Returns the dir.
fn seed_thumbnails_demo(home: &std::path::Path) -> std::path::PathBuf {
    let dir = home.join("Downloads");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    // A subfolder (folder glyph) for visual balance.
    let _ = std::fs::create_dir_all(dir.join("Camera Roll"));

    // Real raster images across formats: pictures of a sunset, a harbour, a
    // forest path, a flower and a mountain lake.
    for (name, w, h, kind, format) in [
        ("sunset.png", 640, 480, 0, image::ImageFormat::Png),
        ("harbour.jpg", 640, 480, 1, image::ImageFormat::Jpeg),
        ("forest path.png", 480, 640, 2, image::ImageFormat::Png),
        ("bloom.jpg", 600, 600, 3, image::ImageFormat::Jpeg),
        ("mountain lake.png", 720, 480, 4, image::ImageFormat::Png),
    ] {
        write_image(&dir.join(name), w, h, kind, format);
    }

    // A genuine one-page PDF (pdfium renders its first page) and a real EPUB.
    let _ = std::fs::write(dir.join("Field guide.pdf"), minimal_pdf("Field Guide"));
    let _ = std::fs::write(
        dir.join("The Lantern-Keeper.epub"),
        minimal_epub("The Lantern-Keeper", &demo_art::cover_png(300, 440)),
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

/// Seed a demo **workspace** directory whose files carry real `user.tilo.thumb`
/// (host-generated preview PNG) and `user.tilo.sync` extended attributes — so a
/// `--shots --scenario thumbnails-fs` grid exercises the FS-metadata path: the
/// file manager paints the host's gossiped preview (via the xattr), even for a
/// file whose content is only *projected*. Sets `TILO_WORKSPACE_ROOTS`.
#[allow(
    clippy::disallowed_methods,
    reason = "only called from the --shots handler on the main thread before the \
              widget or any of its worker threads exist, so nothing reads the \
              environment concurrently"
)]
fn seed_thumbnails_fs_demo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("tilo-files-thumbnails-fs-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    // Files whose *content* stays empty here — the preview rides entirely on the
    // host-provided `user.tilo.thumb` metadata, exactly as a peer would see a file
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
        let _ = xattr::set(&p, "user.tilo.thumb", &demo_art::scene_png(112, 84, kind));
        let _ = xattr::set(&p, "user.tilo.sync", sync.as_bytes());
    }
    // A couple of files with no host preview → sync badge + type badge.
    for (name, sync) in [("dataset.parquet", "projected"), ("notes.md", "local")] {
        let p = dir.join(name);
        let _ = std::fs::write(&p, b"");
        let _ = xattr::set(&p, "user.tilo.sync", sync.as_bytes());
    }
    // SAFETY: process startup, main thread, before any other thread exists.
    unsafe { std::env::set_var("TILO_WORKSPACE_ROOTS", &dir) };
    dir
}

/// Write a demo picture to `path` in the given format.
fn write_image(path: &std::path::Path, w: u32, h: u32, kind: u8, fmt: image::ImageFormat) {
    let img = demo_art::scene_image(w, h, kind);
    let mut out = Vec::new();
    if image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), fmt)
        .is_ok()
    {
        let _ = std::fs::write(path, out);
    }
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

fn parse_size(spec: &str) -> Option<(u32, u32)> {
    let (w, h) = spec.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn shot_viewport(viewport: Option<(u32, u32)>, size: Option<(u32, u32)>) -> (u32, u32) {
    size.or(viewport).unwrap_or(DEFAULT_SHOTS_VIEWPORT)
}

/// Apply scenario-specific state that needs the already-laid-out geometry (a
/// mid-drag move, an open context menu) — split out of
/// [`render_shot_to_png`] purely to stay under the line-count limit.
fn apply_post_layout_scenario(
    widget: &mut FileExplorerWidget,
    scenario: Option<&str>,
    (w, h): (u32, u32),
) {
    match scenario {
        Some("menu") => {
            // The toolbar's actual ⋯ menu: the capture proves it is anchored
            // directly below and right-aligned to its visible trigger.
            widget.debug_show_view_menu();
        }
        Some("context-menu" | "context-menu-folder") => {
            // Separate right-click fixture. Its location is a real pointer
            // anchor in the content pane, deliberately away from toolbar ⋯.
            // The menu opens as it does for a click: no row is armed.
            widget.debug_show_templates_menu(Point::new(f64::from(w) * 0.45, f64::from(h) * 0.25));
        }
        Some("pindrag") => {
            // Carry a folder out of the list toward the sidebar — mid-drag, so
            // the floating ghost shows over the Favourites section.
            let folder = widget.current_path().join("Projects");
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
        // Summon the places drawer over the file view (the narrow layout's
        // toggle). The shell must have laid out once to know it is a drawer;
        // the capture then lays out again with it open.
        Some("places") => widget.debug_open_places_drawer(),
        Some("undo-trash") => {
            widget.debug_open_view_menu();
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
#[allow(
    clippy::too_many_lines,
    reason = "the headless shot boundary owns one linear GPU setup-render-readback-write transaction"
)]
fn render_shot_to_png(
    mut widget: FileExplorerWidget,
    viewport: (u32, u32),
    theme: Theme,
    scenario: Option<&str>,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme);
    frame.sync(&mut widget);
    frame.layout(&mut widget)?;

    // Responsive children learn their actual width only during layout. Settle
    // one structural reflow before capturing so narrow screenshots test the
    // finished interface, not its first-frame breakpoint transition.
    if widget.take_structure_dirty() {
        frame.sync(&mut widget);
        frame.layout(&mut widget)?;
        let _settled = frame.draw(&mut widget);
    }

    // A live frame paints immediately after layout.  Prime that same phase
    // before geometry-driven demo scenarios: FileView publishes only visible
    // folder drop targets during its clipped draw, so `dropmove` must not see
    // a stale, directory-sized target registry.
    let _priming = frame.draw(&mut widget);

    // Post-layout scenarios need the laid-out geometry.
    apply_post_layout_scenario(&mut widget, scenario, viewport);
    // A scenario that changed the structure (a drawer opened) is laid out
    // again, as the live frame loop would.
    if widget.take_structure_dirty() {
        frame.sync(&mut widget);
        frame.layout(&mut widget)?;
        let _settled = frame.draw(&mut widget);
    }

    // Thumbnail scenarios: requests are enqueued during draw, so a throwaway
    // warm-up draw kicks off generation for the visible cells, then we wait for
    // the pool to settle before the frame we keep — so the shot captures real
    // previews, never the loading placeholders.
    if matches!(scenario, Some("thumbnails" | "thumbnails-fs")) {
        let _warm = frame.draw(&mut widget);
        widget.prewarm_thumbnails(std::time::Duration::from_secs(20));
    }

    let mut scene = frame.draw(&mut widget);
    // Composite the overlay layer (context menu + flyout) on top.
    frame.draw_overlay(&mut widget, &mut scene);
    frame.write_png(&scene, out);
    println!("file-explorer: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod actor_tests {
    use super::{distinct_actor, navigate_launch_directory, shot_viewport};

    /// The launch directory resolves relative paths and surfaces missing ones; the screenshot viewport honours the standard value unless overridden.
    #[test]
    fn launch_options_resolve_directory_and_viewport() {
        // --- phase: launch_directory_uses_relative_paths_and_surfaces_missing_directories ---
        {
            let cwd = std::env::current_dir().unwrap();
            let fixture = tempfile::tempdir_in(&cwd).unwrap();
            let relative = fixture.path().strip_prefix(&cwd).unwrap();
            let mut widget = file_explorer::FileExplorerWidget::with_persistence(
                None,
                Some(fixture.path().to_path_buf()),
            );
            navigate_launch_directory(&mut widget, relative).unwrap();
            assert_eq!(widget.current_path(), fixture.path());
            assert!(widget.error().is_none());
            let missing = fixture.path().join("missing-folder");
            navigate_launch_directory(&mut widget, &missing).unwrap();
            assert_eq!(widget.current_path(), missing);
            assert!(widget.error().is_some());
        }

        // --- phase: screenshot_viewport_honors_standard_value_unless_size_overrides_it ---
        {
            assert_eq!(shot_viewport(Some((480, 900)), None), (480, 900));
            assert_eq!(
                shot_viewport(Some((480, 900)), Some((640, 480))),
                (640, 480)
            );
        }
    }

    /// Two replicas in one process get distinct actors that still name the application.
    #[test]
    fn replicas_get_distinct_actors_naming_the_app() {
        // --- phase: two_replicas_in_one_process_do_not_share_an_actor ---
        // The regression, stated as the property it broke.
        //
        // Both ids below are minted inside one process, so anything derived from
        // the process — a pid, which is 2 in every sandbox on this system —
        // produces the same string twice. Automerge then refuses the second
        // replica's frames with `duplicate seq 1 found for actor`, and a change
        // made on one machine never reaches the other.
        {
            let a = automerge::AutoCommit::new();
            let b = automerge::AutoCommit::new();
            assert_ne!(
                distinct_actor("files", a.get_actor()),
                distinct_actor("files", b.get_actor()),
                "two replicas were given the same actor id, which is the failure \
             that silently drops one of them's changes",
            );
        }

        // --- phase: the_actor_still_names_the_application ---
        // And it still says what it is, so a history can be read.
        {
            let doc = automerge::AutoCommit::new();
            assert!(distinct_actor("files", doc.get_actor()).starts_with("files-"));
        }
    }
}

//! What the player can open, and how each kind arrives: a file given on the
//! command line, a path the person picked, the demo clip a capture or a first
//! run shows, and the token that keeps one source distinct from another, so a
//! resume position belongs to the clip it was left in.

use std::path::{Path, PathBuf};

use ui::portal::{FileFilter, PickRequest};

pub(super) fn video_open_request() -> PickRequest {
    PickRequest::open("Open a video", "Play")
        .thread_name("video-player-portal")
        .filter(
            FileFilter::new("Video")
                .mimetype("video/mp4")
                .mimetype("video/webm")
                .mimetype("video/x-matroska")
                .mimetype("video/quicktime")
                .glob("*.mp4")
                .glob("*.webm")
                .glob("*.mkv")
                .glob("*.mov")
                .glob("*.avi"),
        )
        .all_files()
}

/// Build a [`media::Source`] for a filesystem path: a path under the FUSE
/// workspace mount becomes a [`media::Source::Workspace`] (read-through,
/// hydrate-on-demand); anything else is a [`media::Source::LocalFile`].
pub fn source_for_path(path: &Path) -> media::Source {
    let root = media::Source::workspace_root();
    if path.starts_with(&root) {
        media::Source::Workspace(path.to_path_buf())
    } else {
        media::Source::LocalFile(path.to_path_buf())
    }
}

/// The source a launch argument names: a stream's `http(s)` URL, else a
/// path.
pub fn source_for_argument(argument: &str) -> media::Source {
    if argument.starts_with("http://") || argument.starts_with("https://") {
        media::Source::Remote {
            url: argument.to_owned(),
        }
    } else {
        source_for_path(Path::new(argument))
    }
}

/// What a source is called before anyone names it: a file's name, or a
/// stream's.
pub(super) fn source_title(source: &media::Source) -> String {
    match source {
        media::Source::LocalFile(path) | media::Source::Workspace(path) => path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("selected video")
            .to_string(),
        media::Source::Remote { .. } => "Streaming video".to_string(),
    }
}

/// The video a capture plays when its launch named none: a four-second
/// generated VP9/AAC clip (a decode fixture, `tests/fixtures`; no
/// third-party media), decoded by the system's engine like any opened file.
/// VP9 decodes with the royalty-free software decoders in the OS image, so
/// a capture shows a picture wherever it runs.
#[cfg(feature = "decode")]
pub(super) const DEMO_CLIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/vp9-aac.mp4"));

/// What the capture's demo clip is called.
pub(super) const DEMO_TITLE: &str = "Demo clip";

/// Where a capture parks the demo clip, for a stable picture.
pub(super) const DEMO_POSITION_MS: u64 = 2_000;

/// The capture's demo clip, staged as a file named by its contents, so every
/// capture names the same source.
#[cfg(feature = "decode")]
pub(super) fn demo_source() -> media::Source {
    media::Source::from_contents("Demo clip.mp4", DEMO_CLIP).unwrap_or_else(|error| {
        tracing::warn!(%error, "video player: the demo clip could not be staged");
        media::Source::LocalFile(PathBuf::from("Demo clip.mp4"))
    })
}

/// Without the system's decoder there is no clip to decode; the capture
/// says so instead of showing a picture.
#[cfg(not(feature = "decode"))]
pub(super) fn demo_source() -> media::Source {
    media::Source::LocalFile(PathBuf::from("Demo clip.mp4"))
}

/// The shareable identity of the capture's demo clip, which its scenarios'
/// shared annotations point at.
pub(crate) fn demo_media_ref() -> String {
    local_source_token(&demo_source(), 0)
}

/// Opaque, deterministic per-process identity for a local source. Resource
/// IDs and action payload guards need a stable token, but publishing a URI
/// would expose an absolute path to search, history, or a collaborator.
pub(super) fn local_source_token(source: &media::Source, generation: u64) -> String {
    // FNV-1a is used only as an opaque UI identifier, not as a security
    // boundary. The monotonic generation prevents a same-path reopen from
    // accepting stale dynamic stream/chapter actions.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, media::session::session_key(source).as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    format!("local-media-{hash:016x}-open-{generation}")
}

/// The identity a resume point is kept by. It is the same opaque token a
/// queue row is named by (and, through [`RecentMedia`]'s source, a Recent
/// row), so a video opened from a row, the picker, the file manager or a drop
/// on the window resumes at the same place.
pub(super) fn resume_key(source: &media::Source) -> String {
    local_source_token(source, 0)
}

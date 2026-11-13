//! Where a media reference resolves to, and whether its bytes are local.
//!
//! The record carries only references: a workspace-relative path plus cheap
//! metadata. The bytes live on the workspace FUSE, where they hydrate on read, so
//! a path that resolves is not a path that can be decoded. Everything that reads
//! media goes through [`resolve`], which says both.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ui::tilo_resource;

/// The workspace mount: the same tree on every peer, hydrated per peer.
pub use ::media::source::WORKSPACE_ROOT;

/// Where imported media goes: one prefix keeps the sandbox allowlist to a
/// single grant.
pub const MEDIA_PREFIX: &str = "media";

/// Names the editor's own copy of a file imported by its contents, for a host
/// with no paths to store.
pub const KEPT_PREFIX: &str = "tilo-kept";

/// Whether a reference's bytes are local. The reference itself is always in the
/// record, so a clip can be laid out, trimmed and drawn from metadata alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, tilo_resource::Model)]
pub enum Hydration {
    /// Metadata is known; the bytes have not been fetched yet.
    Projected,
    /// Present in the local cache, so it can be decoded.
    Local,
}

impl Hydration {
    pub fn as_str(self) -> &'static str {
        match self {
            Hydration::Projected => "projected",
            Hydration::Local => "local",
        }
    }
    pub fn from_name(s: &str) -> Self {
        match s {
            "local" => Hydration::Local,
            _ => Hydration::Projected,
        }
    }
    pub fn is_local(self) -> bool {
        matches!(self, Hydration::Local)
    }
}

/// The only type the decode path consumes, so the hydration contract is
/// enforced in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSource {
    /// Absolute, under [`WORKSPACE_ROOT`] or a bundled sample directory.
    pub abs_path: PathBuf,
    pub hydration: Hydration,
}

impl MediaSource {
    /// Whether the bytes can be read now.
    pub fn is_decodable(&self) -> bool {
        self.hydration.is_local()
    }
}

#[cfg(feature = "samples")]
static SAMPLE_MEDIA: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(feature = "samples")]
pub fn bundled_sample_media() -> PathBuf {
    Path::new(env!("OUT_DIR")).join("sample-media")
}

/// Resolve relative media under `dir` before anywhere else, so a capture of the
/// demo project finds its bundled stand-ins with no workspace mount. `None`
/// stops it.
#[cfg(feature = "samples")]
pub fn serve_sample_media(dir: Option<PathBuf>) {
    if let Ok(mut slot) = SAMPLE_MEDIA.lock() {
        *slot = dir;
    }
}

#[cfg(feature = "samples")]
fn sample_media() -> Option<PathBuf> {
    SAMPLE_MEDIA.lock().ok().and_then(|s| s.clone())
}

/// Where sources imported by content (`tilo.content:<reference>`) are read from:
/// a directory holding each by its address, which the render helper points at its
/// job directory. A content path resolves nowhere else.
static CONTENT_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_content_root(dir: Option<PathBuf>) {
    if let Ok(mut slot) = CONTENT_ROOT.lock() {
        *slot = dir;
    }
}

/// The file a content-addressed `path` names under the content root.
fn content_file(path: &str) -> Option<PathBuf> {
    let reference = tilo_actions::content_hash(path)?;
    let address = ui::framework::content::content_address(reference)?;
    let root = CONTENT_ROOT.lock().ok()?.clone()?;
    Some(root.join(address))
}

/// An absolute `path` is used verbatim; otherwise a content reference, then the
/// sample media, then the mount, then the working directory; then the canonical
/// mount path, projected.
///
/// The mount is asked before anything local: the reference is what replicates,
/// so two peers must resolve the same logical file even if one has a same-named
/// file beside their project. Hydration is `Local` when the resolved path
/// exists, and a missing file is a normal, drawable state rather than a panic.
pub fn resolve(path: &str) -> MediaSource {
    // An absolute path is never re-rooted under the workspace.
    let raw = Path::new(path);
    if raw.is_absolute() {
        let hydration = if raw.exists() {
            Hydration::Local
        } else {
            Hydration::Projected
        };
        return MediaSource {
            abs_path: raw.to_path_buf(),
            hydration,
        };
    }
    // A source imported from content, where a render laid its bytes out.
    if let Some(abs_path) = content_file(path) {
        let hydration = if abs_path.is_file() {
            Hydration::Local
        } else {
            Hydration::Projected
        };
        return MediaSource {
            abs_path,
            hydration,
        };
    }
    // 1. A capture's sample media.
    #[cfg(feature = "samples")]
    if let Some(dir) = sample_media() {
        let candidate = dir.join(path);
        if candidate.exists() {
            return MediaSource {
                abs_path: candidate,
                hydration: Hydration::Local,
            };
        }
    }
    // The mount is the shared meaning; the working directory is this machine's.
    let mounted = Path::new(WORKSPACE_ROOT).join(path);
    let bases = [
        Some(PathBuf::from(WORKSPACE_ROOT)),
        std::env::current_dir().ok(),
    ];
    let found = bases
        .into_iter()
        .flatten()
        .map(|base| base.join(path))
        .find(|c| c.exists());
    if let Some(abs_path) = found {
        return MediaSource {
            abs_path,
            hydration: Hydration::Local,
        };
    }
    MediaSource {
        abs_path: mounted,
        hydration: Hydration::Projected,
    }
}

/// The [`media::Source`] the decode engine consumes.
pub fn engine_source(abs_path: &Path) -> ::media::Source {
    ::media::Source::for_path(abs_path)
}

/// A file under the workspace mount is stored relative, so the reference
/// replicates and resolves on a peer; one outside is stored absolute, and a peer
/// sees it projected.
pub fn stored_path_for_import(abs: &Path) -> String {
    let root = ::media::Source::workspace_root();
    for base in [root.as_path(), Path::new(WORKSPACE_ROOT)] {
        if let Ok(rel) = abs.strip_prefix(base) {
            return rel.to_string_lossy().into_owned();
        }
    }
    abs.to_string_lossy().into_owned()
}

/// The path stored for an imported file: `"Drone.mov"` becomes
/// `"media/Drone.mov"`.
pub fn workspace_path(name: &str) -> String {
    format!("{MEDIA_PREFIX}/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(video_editor_media_env)]
    fn media_paths_resolve_and_report_hydration() {
        {
            assert_eq!(workspace_path("Drone.mov"), "media/Drone.mov");
        }
        // Both cases live in one test: the sample media is process-wide.
        {
            serve_sample_media(None);
            // A path that cannot exist resolves projected: the reference is
            // valid, the bytes are not hydrated.
            let src = resolve("media/definitely-not-here-42.mov");
            assert_eq!(src.hydration, Hydration::Projected);
            assert!(!src.is_decodable());
            assert!(src.abs_path.starts_with(WORKSPACE_ROOT));

            // With the override pointing at a directory holding the file, it
            // resolves local.
            let dir = std::env::temp_dir();
            let name = format!("tilo-ve-seam-{}.bin", std::process::id());
            let f = dir.join(&name);
            std::fs::write(&f, b"x").unwrap();
            serve_sample_media(Some(dir));
            let local = resolve(&name);
            assert_eq!(local.hydration, Hydration::Local);
            assert!(local.is_decodable());
            assert_eq!(local.abs_path, f);
            serve_sample_media(None);
            std::fs::remove_file(&f).ok();
        }
        // With nothing of that name in the working directory, the reference is
        // a workspace one: projected, not fabricated.
        {
            serve_sample_media(None);
            let away = resolve(&format!("tilo-ve-nowhere-{}/take1.mp4", std::process::id()));
            assert_eq!(away.hydration, Hydration::Projected, "{away:?}");
            assert!(away.abs_path.starts_with(WORKSPACE_ROOT), "{away:?}");
        }
        {
            for h in [Hydration::Projected, Hydration::Local] {
                assert_eq!(Hydration::from_name(h.as_str()), h);
            }
        }
        {
            let dir = std::env::temp_dir();
            let name = format!("tilo-ve-import-{}.mp4", std::process::id());
            let f = dir.join(&name);
            std::fs::write(&f, b"real bytes").unwrap();
            let src = resolve(&f.to_string_lossy());
            assert_eq!(src.abs_path, f, "absolute path is used verbatim");
            assert_eq!(src.hydration, Hydration::Local);
            assert!(src.is_decodable());
            std::fs::remove_file(&f).ok();
            let missing = resolve("/definitely/not/here-xyz.mov");
            assert_eq!(missing.abs_path, Path::new("/definitely/not/here-xyz.mov"));
            assert_eq!(missing.hydration, Hydration::Projected);
        }
        {
            let root = std::env::temp_dir().join(format!("tilo-ve-eng-{}", std::process::id()));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ROOT", &root);
            let ws = engine_source(&root.join("media/a.mp4"));
            assert!(
                matches!(ws, ::media::Source::Workspace(_)),
                "under the mount → Workspace"
            );
            let local = engine_source(Path::new("/home/u/b.mp4"));
            assert!(
                matches!(local, ::media::Source::LocalFile(_)),
                "outside the mount → LocalFile"
            );
        }
        {
            let root = std::env::temp_dir().join(format!("tilo-ve-store-{}", std::process::id()));
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ROOT", &root);
            assert_eq!(
                stored_path_for_import(&root.join("media/Clip.mp4")),
                "media/Clip.mp4"
            );
            assert_eq!(
                stored_path_for_import(Path::new("/home/u/Holiday.mov")),
                "/home/u/Holiday.mov"
            );
        }
    }
}

//! The FUSE-media seam.
//!
//! In qOS, source media lives on the **P2P FUSE filesystem mounted at
//! `/var/workspace`** (see the workspace `fs` crate), *not* in the editor's
//! CRDT. This is deliberate and load-bearing for collaboration:
//!
//!   * The **CRDT carries only references** — a workspace-relative path plus
//!     cheap metadata (duration, dimensions, palette). That is what
//!     replicates between peers over the editor's Automerge document.
//!   * The **bytes live on the FUSE** and hydrate on demand. When a second
//!     collaborator opens the project, the path references resolve against
//!     *their* mount; `fs` projects the metadata immediately (P2P gossip) and
//!     fetches content the first time a frame is actually decoded.
//!
//! So a [`crate::model::MediaItem`] stores `path` = `"footage/Drone.mov"`
//! (relative to the workspace root) and a [`Hydration`] status. The decode
//! path ([`crate::preview`], gated behind the `decode` feature) is the only
//! place that resolves a reference to real bytes — that resolution is the
//! single hydration seam, isolated here so the rest of the editor never
//! assumes a byte is local.
//!
//! ## Why a seam and not direct `std::fs`?
//!
//! `open()` on the FUSE does **not** imply bytes are present — `fs` is
//! projection-first, hydration-on-read (10s metadata TTL, 1 MB readahead).
//! Treating media access as a typed seam ([`resolve`]) keeps that contract
//! explicit: a caller gets a [`MediaSource`] describing *where* the bytes
//! are and *whether* they are local yet, never a bare assumption.

use std::path::{Path, PathBuf};

/// Root of the qOS P2P FUSE filesystem. Every collaborator mounts the same
/// logical tree here, so a workspace-relative media path resolves to the
/// same logical file for all peers (the bytes hydrate per-peer on read).
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// Sub-directory under the workspace where the editor expects project media
/// to be imported. Keeping imports under a single prefix makes the
/// per-cluster sandbox allowlist a single grant rather than per-file.
pub const MEDIA_PREFIX: &str = "media";

/// Whether a media reference's *content* is available locally yet. The CRDT
/// always carries the reference (so the clip can be laid out, trimmed and
/// drawn from metadata); the bytes are a separate, lazily-hydrated concern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hydration {
    /// Path + metadata are known (replicated via the CRDT) but the bytes
    /// have not been fetched from the FUSE yet. The timeline still renders
    /// the clip from metadata; the preview shows a representative frame.
    Projected,
    /// Content is present in the local FUSE cache and can be decoded.
    Local,
}

impl Hydration {
    pub fn as_str(self) -> &'static str {
        match self {
            Hydration::Projected => "projected",
            Hydration::Local => "local",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "local" => Hydration::Local,
            _ => Hydration::Projected,
        }
    }
    pub fn is_local(self) -> bool {
        matches!(self, Hydration::Local)
    }
}

/// A resolved media reference: the absolute on-FUSE path plus whether its
/// bytes are hydrated. This is the *only* type the decode path consumes, so
/// the hydration contract is enforced in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSource {
    /// Absolute path under [`WORKSPACE_ROOT`] (or a bundled sample path in
    /// `--shots`/dev — see [`resolve`]).
    pub abs_path: PathBuf,
    /// Whether the bytes are present locally (FUSE-hydrated or a real file).
    pub hydration: Hydration,
}

impl MediaSource {
    /// `true` when the bytes can actually be read/decoded right now.
    pub fn is_decodable(&self) -> bool {
        self.hydration.is_local()
    }
}

/// Resolve a media `path` to an absolute [`MediaSource`], reporting whether its
/// bytes are hydrated.
///
/// Resolution order:
///   0. **Absolute path** — a `path` that is already absolute (an imported
///      local file, or a path already under the mount) is used verbatim. This
///      is how a portal-imported file outside the workspace reaches the decoder.
///   1. **Bundled sample** — otherwise, if `$QOS_VE_MEDIA_DIR/<path>` exists,
///      use it. `--shots` sets `QOS_VE_MEDIA_DIR` to the crate's `sample-media/`
///      directory so headless captures decode real bytes deterministically
///      without a live FUSE mount.
///   2. **FUSE mount** — otherwise the canonical `/var/workspace/<path>`.
///
/// `hydration` is `Local` iff the resolved path exists on disk (the FUSE
/// reports a present cached file as an ordinary stat); `Projected`
/// otherwise. The editor never panics on a missing file — a projected
/// reference is a normal, drawable state.
pub fn resolve(path: &str) -> MediaSource {
    // 0. Absolute path: an imported local file, or an already-absolute mount
    //    path. Use it directly — never re-root it under the workspace.
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
    // 1. Bundled / dev override.
    if let Some(dir) = std::env::var_os("QOS_VE_MEDIA_DIR") {
        let candidate = Path::new(&dir).join(path);
        if candidate.exists() {
            return MediaSource {
                abs_path: candidate,
                hydration: Hydration::Local,
            };
        }
    }
    // 2. Canonical FUSE location.
    let abs = Path::new(WORKSPACE_ROOT).join(path);
    let hydration = if abs.exists() {
        Hydration::Local
    } else {
        Hydration::Projected
    };
    MediaSource {
        abs_path: abs,
        hydration,
    }
}

/// Turn a resolved absolute path into the shared [`media::Source`] the decode
/// engine consumes: a path under the FUSE mount becomes a
/// [`media::Source::Workspace`] (read *through* the mount so it hydrates on
/// demand), anything else a [`media::Source::LocalFile`].
pub fn engine_source(abs_path: &Path) -> ::media::Source {
    let root = ::media::Source::workspace_root();
    if abs_path.starts_with(&root) || abs_path.starts_with(WORKSPACE_ROOT) {
        ::media::Source::Workspace(abs_path.to_path_buf())
    } else {
        ::media::Source::LocalFile(abs_path.to_path_buf())
    }
}

/// The path the editor should store in the CRDT for an imported file at
/// absolute `abs`. A file already under the workspace mount is stored
/// **workspace-relative** so the reference replicates to peers (their FUSE
/// resolves the same logical file); a local file outside the workspace is
/// stored as its absolute path (it decodes locally; a peer sees it projected).
pub fn stored_path_for_import(abs: &Path) -> String {
    let root = ::media::Source::workspace_root();
    for base in [root.as_path(), Path::new(WORKSPACE_ROOT)] {
        if let Ok(rel) = abs.strip_prefix(base) {
            return rel.to_string_lossy().into_owned();
        }
    }
    abs.to_string_lossy().into_owned()
}

/// Build the canonical workspace-relative path the editor stores for an
/// imported file `name` (e.g. `"Drone.mov"` → `"media/Drone.mov"`).
pub fn workspace_path(name: &str) -> String {
    format!("{MEDIA_PREFIX}/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_path_is_prefixed() {
        assert_eq!(workspace_path("Drone.mov"), "media/Drone.mov");
    }

    // Both env-sensitive cases live in one test so there's no inter-test
    // race over the shared `QOS_VE_MEDIA_DIR` process env var.
    #[test]
    fn resolve_reports_projected_or_local_via_the_seam() {
        // Safety: single-threaded test mutation of process env.
        unsafe {
            std::env::remove_var("QOS_VE_MEDIA_DIR");
        }
        // 1. A path that cannot exist on this host resolves as Projected —
        //    the reference is valid, the bytes simply are not hydrated.
        let src = resolve("media/definitely-not-here-42.mov");
        assert_eq!(src.hydration, Hydration::Projected);
        assert!(!src.is_decodable());
        assert!(src.abs_path.starts_with(WORKSPACE_ROOT));

        // 2. With the bundled override pointing at a dir that has the file,
        //    the seam reports it Local + decodable.
        let dir = std::env::temp_dir();
        let name = format!("qos-ve-seam-{}.bin", std::process::id());
        let f = dir.join(&name);
        std::fs::write(&f, b"x").unwrap();
        unsafe {
            std::env::set_var("QOS_VE_MEDIA_DIR", &dir);
        }
        let local = resolve(&name);
        assert_eq!(local.hydration, Hydration::Local);
        assert!(local.is_decodable());
        assert_eq!(local.abs_path, f);
        unsafe {
            std::env::remove_var("QOS_VE_MEDIA_DIR");
        }
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn hydration_round_trips_through_strings() {
        for h in [Hydration::Projected, Hydration::Local] {
            assert_eq!(Hydration::from_key(h.as_str()), h);
        }
    }

    #[test]
    fn absolute_import_path_resolves_verbatim_and_reports_hydration() {
        // A real imported local file resolves to itself and reports Local.
        let dir = std::env::temp_dir();
        let name = format!("qos-ve-import-{}.mp4", std::process::id());
        let f = dir.join(&name);
        std::fs::write(&f, b"real bytes").unwrap();
        let src = resolve(&f.to_string_lossy());
        assert_eq!(src.abs_path, f, "absolute path is used verbatim");
        assert_eq!(src.hydration, Hydration::Local);
        assert!(src.is_decodable());
        std::fs::remove_file(&f).ok();
        // A non-existent absolute path stays a valid, projected reference.
        let missing = resolve("/definitely/not/here-xyz.mov");
        assert_eq!(missing.abs_path, Path::new("/definitely/not/here-xyz.mov"));
        assert_eq!(missing.hydration, Hydration::Projected);
    }

    #[test]
    fn engine_source_splits_workspace_from_local() {
        let root = std::env::temp_dir().join(format!("qos-ve-eng-{}", std::process::id()));
        // Safety: single-threaded test mutation of process env.
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOT", &root) };
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
        unsafe { std::env::remove_var("QOS_WORKSPACE_ROOT") };
    }

    #[test]
    fn stored_path_is_relative_under_mount_absolute_otherwise() {
        let root = std::env::temp_dir().join(format!("qos-ve-store-{}", std::process::id()));
        // Safety: single-threaded test mutation of process env.
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOT", &root) };
        assert_eq!(
            stored_path_for_import(&root.join("media/Clip.mp4")),
            "media/Clip.mp4"
        );
        assert_eq!(
            stored_path_for_import(Path::new("/home/u/Holiday.mov")),
            "/home/u/Holiday.mov"
        );
        unsafe { std::env::remove_var("QOS_WORKSPACE_ROOT") };
    }
}

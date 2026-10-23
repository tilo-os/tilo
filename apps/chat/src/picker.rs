//! Choosing a real file to share into a channel.
//!
//! Tilo files live in the P2P FUSE workspace ([`tilo_paths::workspace_root`]);
//! a chat attachment is imported as bounded shared bytes before it is
//! projected into a message. The composer's "+" opens this picker over the
//! workspace so a share is always a *real* file the user selected, never
//! fabricated content.
//!
//! The browsed directory is the workspace mount, overridable with
//! `TILO_CHAT_ATTACH_DIR` (for tests and for an install that mounts the shared
//! space elsewhere). In a headless environment with no workspace the listing is
//! simply empty and nothing can be attached — honest, not faked.

use std::path::{Path, PathBuf};

/// Where the picker browses: the P2P workspace mount by default.
pub fn attach_dir() -> PathBuf {
    std::env::var_os("TILO_CHAT_ATTACH_DIR")
        .map(PathBuf::from)
        .or_else(tilo_paths::workspace_root)
        .unwrap_or_else(|| PathBuf::from(tilo_paths::WORKSPACE_MOUNT))
}

/// One selectable file in the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// True when this is a video container — the composer posts it as a
    /// [`crate::model::Attachment::Video`] (inline play card) rather than an
    /// image thumbnail.
    pub is_video: bool,
}

impl PickEntry {
    /// The `ImageStore` key for this entry's thumbnail (its filesystem path).
    pub fn key(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// Image extensions the picker offers (matches the decoders enabled on the
/// `image` dependency).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

/// Video containers the picker offers. The selected local source is imported
/// into the app's content store before the inline card is posted;
/// default builds do not decode video frames (see [`crate::model::Attachment::Video`]).
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "m4v", "avi", "ogv"];

fn has_ext(name: &str, exts: &[&str]) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => exts.iter().any(|e| ext.eq_ignore_ascii_case(e)),
        None => false,
    }
}

/// Whether `name` looks like a shareable image by extension.
pub fn is_image_name(name: &str) -> bool {
    has_ext(name, IMAGE_EXTS)
}

/// Whether `name` looks like a shareable video by extension.
pub fn is_video_name(name: &str) -> bool {
    has_ext(name, VIDEO_EXTS)
}

/// List files the attachment flow can import — images, videos, and bounded
/// generic documents. The actual byte limit is checked by the worker, where it
/// cannot block the UI. This is what the composer's "+" browses.
pub fn list_attachments(dir: &Path) -> Vec<PickEntry> {
    list_filtered(dir, |_| true)
}

fn list_filtered(dir: &Path, accept: fn(&str) -> bool) -> Vec<PickEntry> {
    let mut out = Vec::new();
    collect(dir, 1, accept, &mut out);
    out.sort_by_key(|a| a.name.to_lowercase());
    out.truncate(60);
    out
}

pub fn folder_readable(dir: &Path) -> bool {
    tilo_files::Dir::open(tilo_files::Location::Native(dir.to_path_buf()))
        .and_then(|folder| tilo_files::blocking::list(&folder, &tilo_files::VPath::root()))
        .is_ok()
}

fn collect(dir: &Path, depth: usize, accept: fn(&str) -> bool, out: &mut Vec<PickEntry>) {
    let Ok(folder) = tilo_files::Dir::open(tilo_files::Location::Native(dir.to_path_buf())) else {
        return;
    };
    let Ok(entries) = tilo_files::blocking::list(&folder, &tilo_files::VPath::root()) else {
        return;
    };
    for entry in entries {
        if out.len() >= 200 {
            return;
        }
        let path = dir.join(&entry.name);
        if entry.kind == tilo_files::Kind::Dir {
            if depth > 0 {
                collect(&path, depth - 1, accept, out);
            }
            continue;
        }
        let name = entry.name;
        if !accept(&name) {
            continue;
        }
        let size = tilo_files::VPath::new(&name)
            .ok()
            .and_then(|vpath| tilo_files::blocking::metadata(&folder, &vpath).ok())
            .map_or(0, |meta| meta.len);
        let is_video = is_video_name(&name);
        out.push(PickEntry {
            path,
            name,
            size,
            is_video,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([120, 90, 200, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes.into_inner()).unwrap();
        p
    }

    #[test]
    fn listings_offer_only_their_kind_and_flag_videos() {
        // Phase: video_extension_detection.
        {
            assert!(is_video_name("demo.MP4"));
            assert!(is_video_name("capture.webm"));
            assert!(!is_video_name("photo.png"));
        }
        // Phase: list_attachments_includes_generic_files.
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("notes.md"), b"portable shared bytes").unwrap();

            let entries = list_attachments(dir.path());
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "notes.md");
            assert!(!entries[0].is_video);
        }
    }

    #[test]
    fn listing_descends_one_level_and_tolerates_missing_paths() {
        // Phase: descends_one_subdirectory_level.
        {
            let dir = tempfile::tempdir().unwrap();
            let sub = dir.path().join("designs");
            std::fs::create_dir(&sub).unwrap();
            write_png(&sub, "nested.png", 64, 64);
            let entries = list_attachments(dir.path());
            assert!(entries.iter().any(|e| e.name == "nested.png"));
        }
        // Phase: missing_directory_is_empty_not_a_panic.
        {
            assert!(list_attachments(Path::new("/definitely/not/here/at/all")).is_empty());
        }
    }
}

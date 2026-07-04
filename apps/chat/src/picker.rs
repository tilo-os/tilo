//! Choosing a real file to share into a channel.
//!
//! qOS files live in the P2P FUSE workspace mounted at `/var/workspace`; a
//! chat attachment is one of those files projected into a message (its bytes
//! hydrate on first read — see the `fs` driver). The composer's "+" opens this
//! picker over the workspace so a share is always a *real* file the user
//! selected, never fabricated content.
//!
//! The browsed directory is the workspace mount, overridable with
//! `QOS_CHAT_ATTACH_DIR` (for tests and for an install that mounts the shared
//! space elsewhere). In a headless environment with no workspace the listing is
//! simply empty and nothing can be attached — honest, not faked.

use std::path::{Path, PathBuf};

/// Where the picker browses: the P2P workspace mount by default.
pub fn attach_dir() -> PathBuf {
    std::env::var_os("QOS_CHAT_ATTACH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/workspace"))
}

/// One selectable file in the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
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

/// Whether `name` looks like a shareable image by extension.
pub fn is_image_name(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => IMAGE_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)),
        None => false,
    }
}

/// List shareable images in `dir` — its top level plus one level of
/// subdirectories — sorted by name and capped so a large workspace can't stall
/// the picker. A missing or unreadable directory yields an empty list.
pub fn list_images(dir: &Path) -> Vec<PickEntry> {
    let mut out = Vec::new();
    collect_images(dir, 1, &mut out);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.truncate(60);
    out
}

fn collect_images(dir: &Path, depth: usize, out: &mut Vec<PickEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if out.len() >= 200 {
            return;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                collect_images(&path, depth - 1, out);
            }
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_image_name(&name) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(PickEntry { path, name, size });
    }
}

/// True width/height of an image file, read from its header only (cheap — no
/// full decode). `None` when the file is missing or not a supported image.
pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    image::image_dimensions(path).ok().filter(|&(w, h)| w > 0 && h > 0)
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
    fn lists_only_images_and_reads_real_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(dir.path(), "shot.png", 320, 200);
        std::fs::write(dir.path().join("notes.md"), b"not an image").unwrap();

        let entries = list_images(dir.path());
        assert_eq!(entries.len(), 1, "only the image is offered");
        assert_eq!(entries[0].name, "shot.png");
        assert!(entries[0].size > 0);
        assert_eq!(image_dimensions(&png), Some((320, 200)), "real header dims");
    }

    #[test]
    fn descends_one_subdirectory_level() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("designs");
        std::fs::create_dir(&sub).unwrap();
        write_png(&sub, "nested.png", 64, 64);
        let entries = list_images(dir.path());
        assert!(entries.iter().any(|e| e.name == "nested.png"));
    }

    #[test]
    fn missing_directory_is_empty_not_a_panic() {
        assert!(list_images(Path::new("/definitely/not/here/at/all")).is_empty());
        assert!(image_dimensions(Path::new("/definitely/not/here.png")).is_none());
    }
}

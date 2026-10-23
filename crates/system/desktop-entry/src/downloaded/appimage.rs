//! An `AppImage`'s own name and icon, read from the image without running,
//! mounting or extracting it.
//!
//! A type 2 `AppImage` is an ELF executable (the runtime) marked `AI\x02` in
//! its identification padding, with a squashfs image appended right after
//! the ELF's last section header. That image's root holds one `.desktop`
//! file and the icon it names, as the `AppImage` specification requires.
//! Reading that image needs the `appimage` feature, which brings the
//! codecs; recognising the file does not.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

#[cfg(feature = "appimage")]
use super::squashfs::Image;
#[cfg(feature = "appimage")]
use super::{Found, Icon, MAX_ICON_BYTES};

/// Largest embedded `.desktop` file read.
#[cfg(feature = "appimage")]
const MAX_DESKTOP_BYTES: usize = 256 * 1024;

/// The ELF identification, the `AppImage` mark, and the section-header
/// fields that say where the ELF ends.
fn header(file: &mut File) -> io::Result<Option<u64>> {
    let mut raw = [0u8; 64];
    file.seek(SeekFrom::Start(0))?;
    if file.read(&mut raw)? < 52 {
        return Ok(None);
    }
    if raw.get(..4) != Some(b"\x7fELF".as_slice()) || raw.get(8..11) != Some(b"AI\x02".as_slice()) {
        return Ok(None);
    }
    let little = match raw.get(5) {
        Some(1) => true,
        Some(2) => false,
        _ => return Ok(None),
    };
    let field = |at: usize, len: usize| -> Option<u64> {
        let bytes = raw.get(at..at.checked_add(len)?)?;
        let mut out = 0u64;
        if little {
            for byte in bytes.iter().rev() {
                out = (out << 8) | u64::from(*byte);
            }
        } else {
            for byte in bytes {
                out = (out << 8) | u64::from(*byte);
            }
        }
        Some(out)
    };
    let (shoff, shentsize, shnum) = match raw.get(4) {
        Some(1) => (field(0x20, 4), field(0x2E, 2), field(0x30, 2)),
        Some(2) => (field(0x28, 8), field(0x3A, 2), field(0x3C, 2)),
        _ => return Ok(None),
    };
    let end = shoff
        .zip(shentsize)
        .zip(shnum)
        .and_then(|((off, size), count)| off.checked_add(size.checked_mul(count)?));
    Ok(end)
}

/// Whether `path` is a type 2 `AppImage`.
pub fn is_appimage(path: &Path) -> bool {
    File::open(path)
        .and_then(|mut file| header(&mut file))
        .is_ok_and(|end| end.is_some())
}

/// The image's embedded `.desktop` entry and icon; `None` when `path` is
/// not an `AppImage` or carries no entry.
///
/// # Errors
///
/// The file could not be read or its image is damaged.
#[cfg(feature = "appimage")]
pub(crate) fn read_identity(path: &Path) -> io::Result<Option<Found>> {
    let mut file = File::open(path)?;
    let Some(offset) = header(&mut file)? else {
        return Ok(None);
    };
    let mut image = Image::open(file, offset)?;
    let mut names = image.root_names()?;
    names.sort();
    let Some(desktop_name) = names
        .iter()
        .find(|name| name.ends_with(".desktop") && !name.starts_with('.'))
        .cloned()
    else {
        return Ok(None);
    };
    let Some(contents) = image.read_file(&desktop_name, MAX_DESKTOP_BYTES)? else {
        return Ok(None);
    };
    let contents = String::from_utf8_lossy(&contents);
    let id = desktop_name.trim_end_matches(".desktop").to_owned();
    let locales = crate::locale::locale_candidates();
    let Some(entry) = crate::entry::parse(&contents, &id, path, &locales) else {
        return Ok(None);
    };
    let icon = embedded_icon(&mut image, &names, entry.icon.as_deref());
    Ok(Some(Found {
        key: id,
        entry,
        icon,
    }))
}

/// The icon the embedded entry names, beside it in the image's root or in
/// its `hicolor` tree, else `.DirIcon`.
#[cfg(feature = "appimage")]
fn embedded_icon<R: Read + Seek>(
    image: &mut Image<R>,
    root: &[String],
    name: Option<&str>,
) -> Option<Icon> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(name) = name.filter(|name| !name.contains('/')) {
        for ext in ["png", "svg"] {
            let file = format!("{name}.{ext}");
            if root.contains(&file) {
                candidates.push(file);
            }
        }
        for size in [
            "scalable", "512x512", "256x256", "128x128", "64x64", "48x48",
        ] {
            for ext in ["svg", "png"] {
                candidates.push(format!("usr/share/icons/hicolor/{size}/apps/{name}.{ext}"));
            }
        }
    }
    candidates.push(".DirIcon".to_owned());
    candidates.into_iter().find_map(|path| {
        image
            .read_file(&path, MAX_ICON_BYTES)
            .ok()
            .flatten()
            .and_then(Icon::sniff)
    })
}

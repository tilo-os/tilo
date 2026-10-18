//! Isolated production-directory fixture for native evidence collection.
use std::{fs, io::Write, path::Path};

use sha2::{Digest, Sha256};

/// SHA-256 over ordered, length-delimited UTF-8 names and directory flags.
/// Ignores host-specific inode, timestamp and absolute parent path metadata.
pub(crate) fn order_hash(entries: &[crate::model::Entry]) -> String {
    let mut hash = Sha256::new();
    for entry in entries {
        hash.update((entry.name.len() as u64).to_le_bytes());
        hash.update(entry.name.as_bytes());
        hash.update([u8::from(entry.is_dir)]);
    }
    hex::encode(hash.finalize())
}

/// Create 1,200 real entries, including 299 distinct PNGs and four folders.
/// The directory must not exist; no existing file or symlink is overwritten.
/// On an I/O error a partial NEW fixture remains for diagnosis.
///
/// # Errors
/// Returns an error if directory creation, file encoding/writing or the final
/// production directory read fails.
pub fn seed(directory: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    fs::create_dir(directory)?;
    for index in 0..1200_u32 {
        if index >= 1196 {
            fs::create_dir(directory.join(format!("folder-{index:04}")))?;
            continue;
        }
        let (extension, bytes) = match index % 4 {
            0 => {
                let image = image::RgbaImage::from_fn(64, 48, |x, y| {
                    image::Rgba([
                        ((x * 3 + index) % 256) as u8,
                        ((y * 5 + index / 4) % 256) as u8,
                        ((x + y + index / 16) % 256) as u8,
                        255,
                    ])
                });
                ("png", ui::png::encode_rgba8(64, 48, image.as_raw())?)
            }
            1 => (
                "md",
                format!("# Note {index}\n\nDeterministic native Files fixture.\n").into_bytes(),
            ),
            2 => (
                "rs",
                format!("pub fn value_{index}() -> u32 {{ {index} }}\n").into_bytes(),
            ),
            _ => (
                "txt",
                format!("Entry {index}\nNative directory navigation and scrolling.\n").into_bytes(),
            ),
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("entry-{index:04}.{extension}")))?;
        file.write_all(&bytes)?;
    }
    let mut entries = crate::model::read_dir_sync(directory).map_err(std::io::Error::other)?;
    crate::model::sort_entries(
        crate::model::SortBy::Name,
        crate::model::SortOrder::Ascending,
        &mut entries,
    );
    Ok(serde_json::json!({
        "schema": 1,
        "hash_algorithm": "sha256",
        "entry_count": entries.len(),
        "image_count": 299,
        "directory_count": 4,
        "order_hash": order_hash(&entries),
    }))
}

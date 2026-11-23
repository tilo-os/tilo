//! The token a machine was handed: the one file its image or its installer
//! left for it, read once and burned once spent.
//!
//! A token in a file is only a token while it is a real one: an absent file,
//! an empty one and the placeholder an image ships are not, and a real one
//! must be readable by its owner alone, because it admits a device of the
//! person's. Both the reading and the burning live here, so every image
//! applies the same rules to it.

use std::io::{self, Write as _};
use std::path::Path;

/// The token at `path` is never longer than this; a longer file is not one.
const MAX_TOKEN_FILE: u64 = 4096;

/// The token this machine was handed, while it is a real one: `None` when the
/// file is absent, empty, or the image's own `# Replace …` placeholder.
///
/// # Errors
///
/// The file cannot be read, is not one file, is longer than a token can be, or
/// is readable by more than its owner.
pub fn given_token(path: &Path) -> io::Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(refused(format!(
            "refusing an enrollment token that is not one file: {}",
            path.display()
        )));
    }
    if metadata.len() == 0 {
        return Ok(None);
    }
    if metadata.len() > MAX_TOKEN_FILE {
        return Err(refused(format!(
            "refusing an oversized enrollment token: {}",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)?.trim().to_owned();
    if text.is_empty() || text.starts_with('#') {
        return Ok(None);
    }
    // A real token must be its owner's alone and singly linked: the same
    // checks every private file passes.
    if let Err(error) = tilo_files::native::private::read(path) {
        return Err(refused(format!(
            "enrollment token must be readable by its owner alone: {}: {error}",
            path.display()
        )));
    }
    Ok(Some(text))
}

/// Burn the token that bought this machine's certificate: overwrite it and
/// remove it, so a token left on a machine's disk admits nothing.
///
/// # Errors
///
/// The file cannot be overwritten or removed.
pub fn spend_token(path: &Path) -> io::Result<()> {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(mut file) => {
            let length = file.metadata().map_or(0, |metadata| metadata.len());
            if length <= MAX_TOKEN_FILE {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "bounded by MAX_TOKEN_FILE above"
                )]
                let length = length as usize;
                file.write_all(&vec![0_u8; length])?;
                file.sync_all()?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn refused(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

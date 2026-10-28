//! Writes avatars to the directory a public host serves at
//! `/avatars/<blake3>`. Only avatars of authors whose content a bot
//! actually sent out are published; the URL is content-addressed and
//! nothing lists the directory.
//!
//! Publishing is content-addressed, so a person who changes their picture
//! leaves the last one behind, and a bridge that stops publishing leaves
//! them all. Nothing here removes one: the host that *serves* the directory
//! owns the sweep ([`SWEEP_AGE`]), because serving is what tells a picture
//! somebody still wants from one nothing shows any more.

use std::path::PathBuf;

pub use tilo_relay_protocol::public_avatars::{MAX_AVATAR_BYTES, SWEEP_AGE, is_avatar_name, sniff};

/// Why an avatar was not published.
#[derive(Debug)]
pub enum AvatarError {
    /// Over [`MAX_AVATAR_BYTES`].
    TooLarge,
    /// Not a PNG, JPEG, GIF or WebP image.
    NotAnImage,
    /// The file could not be written.
    Io(std::io::Error),
}

impl std::fmt::Display for AvatarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "the avatar is larger than {MAX_AVATAR_BYTES} bytes"),
            Self::NotAnImage => f.write_str("the avatar is not a PNG, JPEG, GIF or WebP image"),
            Self::Io(error) => write!(f, "the avatar could not be written: {error}"),
        }
    }
}

impl std::error::Error for AvatarError {}

/// The published avatars of one public host.
#[derive(Clone, Debug)]
pub struct PublicAvatars {
    dir: PathBuf,
    base_url: String,
}

impl PublicAvatars {
    /// Avatars written to `dir` and served at `<base_url>/<name>`.
    ///
    /// # Errors
    ///
    /// `dir` cannot be created.
    pub fn new(dir: PathBuf, base_url: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Publish `bytes` and return its public URL.
    ///
    /// # Errors
    ///
    /// Too large, not a raster image, or not writable.
    pub fn publish(&self, bytes: &[u8]) -> Result<String, AvatarError> {
        if bytes.len() > MAX_AVATAR_BYTES {
            return Err(AvatarError::TooLarge);
        }
        sniff(bytes).ok_or(AvatarError::NotAnImage)?;
        let name = blake3::hash(bytes).to_hex().to_string();
        let path = self.dir.join(&name);
        if !path.is_file() {
            tilo_files::native::atomic::write(&path, bytes).map_err(AvatarError::Io)?;
        }
        Ok(format!("{}/{name}", self.base_url))
    }

    /// Where `dir` keeps the avatar `name`, when `name` is a published name
    /// and the file is there.
    #[must_use]
    pub fn served(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
        is_avatar_name(name)
            .then(|| dir.join(name))
            .filter(|path| path.is_file())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn an_avatar_is_published_once_under_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let avatars =
            PublicAvatars::new(dir.path().join("avatars"), "https://rooms.example/avatars")
                .unwrap();
        let url = avatars.publish(PNG).unwrap();
        let name = blake3::hash(PNG).to_hex().to_string();
        assert_eq!(url, format!("https://rooms.example/avatars/{name}"));
        assert_eq!(
            std::fs::read(dir.path().join("avatars").join(&name)).unwrap(),
            PNG
        );
        // Publishing it again names the same file, and writes it once.
        assert_eq!(avatars.publish(PNG).unwrap(), url);
        assert_eq!(
            std::fs::read_dir(dir.path().join("avatars"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn oversized_or_non_image_avatars_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let avatars = PublicAvatars::new(dir.path().join("avatars"), "https://x/avatars").unwrap();
        let mut big = PNG.to_vec();
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert!(matches!(avatars.publish(&big), Err(AvatarError::TooLarge)));
        assert!(matches!(
            avatars.publish(b"<svg/>"),
            Err(AvatarError::NotAnImage)
        ));
        assert_eq!(
            std::fs::read_dir(dir.path().join("avatars"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn only_a_name_that_is_a_hash_is_served() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PublicAvatars::served(dir.path(), "../etc/passwd").is_none());
        assert!(PublicAvatars::served(dir.path(), &"A".repeat(64)).is_none());
        assert!(PublicAvatars::served(dir.path(), &"ab".repeat(32)).is_none());
        std::fs::write(dir.path().join("ab".repeat(32)), PNG).unwrap();
        assert_eq!(
            PublicAvatars::served(dir.path(), &"ab".repeat(32)),
            Some(dir.path().join("ab".repeat(32)))
        );
    }
}

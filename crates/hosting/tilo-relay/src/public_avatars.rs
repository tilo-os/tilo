//! Writes avatars to the directory a public host serves at
//! `/avatars/<blake3>`. Only avatars of authors whose content a bot
//! actually sent out are published; the URL is content-addressed and
//! nothing lists the directory.

use std::path::PathBuf;

pub use tilo_relay_protocol::public_avatars::{MAX_AVATAR_BYTES, sniff};

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
        assert_eq!(avatars.publish(PNG).unwrap(), url);
    }

    #[test]
    fn oversized_or_non_image_avatars_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let avatars = PublicAvatars::new(dir.path().to_owned(), "https://x/avatars").unwrap();
        let mut big = PNG.to_vec();
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert!(matches!(avatars.publish(&big), Err(AvatarError::TooLarge)));
        assert!(matches!(
            avatars.publish(b"<svg/>"),
            Err(AvatarError::NotAnImage)
        ));
    }
}

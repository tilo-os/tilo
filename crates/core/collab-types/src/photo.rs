//! A person's or an agent's picture, as the one value every surface keeps it: a `data:` URL
//! carrying a small raster image. A profile keeps it, and an agent's persona keeps it beside its
//! name, so a face reads the same wherever an author is shown and nothing carries a second copy.

use crate::identity::AvatarBytes;

/// The most a picture's `data:` URL holds, in characters: a 96 px square encodes in a few
/// kilobytes, so this bounds one without cramping it.
pub const MAX_PHOTO_CHARS: usize = 48 * 1024;

/// The image types a picture may be.
const PHOTO_PREFIXES: [&str; 4] = [
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/webp;base64,",
    "data:image/gif;base64,",
];

/// Why a value is not a picture a profile or an agent's persona may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PhotoError {
    /// Longer than [`MAX_PHOTO_CHARS`].
    #[error("the picture is larger than {MAX_PHOTO_CHARS} characters")]
    TooLarge,
    /// Not a `data:` URL of a type pages draw.
    #[error("the picture is not a PNG, JPEG, WebP or GIF data URL")]
    NotAPicture,
}

/// `photo` as the picture a person's profile or an agent's persona keeps: a bounded base64
/// `data:` URL of an image type pages draw. An empty value is no picture, never an error.
///
/// # Errors
/// [`PhotoError::TooLarge`] or [`PhotoError::NotAPicture`].
pub fn photo(photo: &str) -> Result<Option<&str>, PhotoError> {
    if photo.is_empty() {
        return Ok(None);
    }
    if photo.len() > MAX_PHOTO_CHARS {
        return Err(PhotoError::TooLarge);
    }
    let data = PHOTO_PREFIXES.iter().find_map(|prefix| {
        let data = photo.strip_prefix(prefix)?;
        (!data.is_empty()
            && data
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')))
        .then_some(data)
    });
    data.map(|_| Some(photo)).ok_or(PhotoError::NotAPicture)
}

/// The bytes the picture `photo` carries, when it is one [`photo`] accepts: decoded base64.
#[must_use]
pub fn photo_bytes(photo: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let kept = crate::photo::photo(photo).ok()??;
    let (_, data) = kept.split_once(',')?;
    base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .ok()
        .filter(|bytes| !bytes.is_empty())
}

/// `bytes`, a small raster image, as the `data:` URL a picture is kept as: `None` when it is no
/// PNG, JPEG or WebP image.
#[must_use]
pub fn photo_url(bytes: &[u8]) -> Option<String> {
    use base64::Engine as _;
    let format = tilo_content_types::image_format(bytes)?;
    if !matches!(format.extension, "png" | "jpg" | "webp" | "gif") {
        return None;
    }
    let mime = format.mime;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let url = format!("data:{mime};base64,{encoded}");
    (url.len() <= MAX_PHOTO_CHARS).then_some(url)
}

/// The picture `photo` carries as [`AvatarBytes`], when it is one [`photo`] accepts.
#[must_use]
pub fn avatar_of(photo: &str) -> Option<AvatarBytes> {
    photo_bytes(photo).map(AvatarBytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAMAAAAoyzS7AAAAA1BMVEX///+nxBvIAAAAAXRSTlMAQObYZgAAAAxJREFUeNoBgAEAAP//AwABWgH+AAAAAElFTkSuQmCC";

    tilo_test_env::scenario! {
        /// A picture is a bounded `data:` URL of an image type pages draw, and its bytes are the
        /// image it carries.
        fn a_picture_is_a_bounded_data_url_of_an_image;

        phase a_picture_is_a_small_image_data_url;
        {
            assert_eq!(photo("").unwrap(), None, "no picture is no error");
            assert_eq!(photo(PNG).unwrap(), Some(PNG));
            assert_eq!(
                photo("data:image/svg+xml;base64,PHN2Zy8+"),
                Err(PhotoError::NotAPicture),
                "an SVG is a document, not a picture"
            );
            assert_eq!(photo("hello"), Err(PhotoError::NotAPicture));
            let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_PHOTO_CHARS));
            assert_eq!(photo(&huge), Err(PhotoError::TooLarge));
        }

        phase a_pictures_bytes_are_the_image_it_carries;
        {
            let bytes = photo_bytes(PNG).expect("the picture's bytes");
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert_eq!(avatar_of(PNG), Some(AvatarBytes(bytes.clone())));
            assert_eq!(
                photo_url(&bytes).as_deref(),
                Some(PNG),
                "an image's bytes encode back to the same URL"
            );
            assert_eq!(photo_url(b"<svg/>"), None, "an SVG is no picture");
        }
    }
}

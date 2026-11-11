//! What an image is and how large, from its header alone: no pixels are
//! decoded, on any target.

/// An image format the codec reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Png,
    Jpeg,
    WebP,
    Gif,
    Bmp,
}

impl Format {
    /// Its media type, as a browser or a mail part names it.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
        }
    }
}

/// The format `bytes` are in, read from their first bytes; `None` for one
/// the codec does not read.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    match imagesize::image_type(bytes).ok()? {
        imagesize::ImageType::Png => Some(Format::Png),
        imagesize::ImageType::Jpeg => Some(Format::Jpeg),
        imagesize::ImageType::Webp => Some(Format::WebP),
        imagesize::ImageType::Gif => Some(Format::Gif),
        imagesize::ImageType::Bmp => Some(Format::Bmp),
        _ => None,
    }
}

/// The width and height `bytes` encode, read from their header; `None` for
/// a format the codec does not read or a header too short or malformed.
#[must_use]
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    sniff(bytes)?;
    let size = imagesize::blob_size(bytes).ok()?;
    Some((
        u32::try_from(size.width).ok()?,
        u32::try_from(size.height).ok()?,
    ))
}

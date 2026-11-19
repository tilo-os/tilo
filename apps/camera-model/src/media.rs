//! The content address a capture's bytes live under.

/// The prefix a portable capture's media id carries.
pub const MEDIA_PREFIX: &str = "media:blake3:";

/// Whether `id` is a verified content reference the store can serve.
#[must_use]
pub fn is_media_id(id: &str) -> bool {
    id.strip_prefix(MEDIA_PREFIX)
        .is_some_and(tilo_content::valid_reference)
}

//! Avatars a public host publishes for the authors whose content a bot
//! sent out of the room (a Discord webhook needs a public avatar URL).
//! Only raster images: an SVG could carry script.

/// The largest avatar published.
pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;

/// The image type of `bytes` by its magic bytes, if it is one we publish.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else {
        None
    }
}

/// Whether `name` is a published avatar's name: its BLAKE3 hash in hex.
#[must_use]
pub fn is_avatar_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognised_by_their_magic_bytes_only() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0...."), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn an_avatar_name_is_a_blake3_hex_digest() {
        assert!(is_avatar_name(&"ab".repeat(32)));
        assert!(!is_avatar_name("../etc/passwd"));
        assert!(!is_avatar_name(&"AB".repeat(32)));
    }
}

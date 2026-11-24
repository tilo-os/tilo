//! Avatars a public host serves for its members, so a place outside Tilo that
//! shows a Tilo author (a Discord webhook) can fetch their picture: the room
//! record's picture is a field, and the host serves it at the public address
//! [`url`] names. Only raster images: an SVG could carry script.

/// The largest avatar published.
pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;

/// Where a host serves its members' pictures, below
/// [`crate::web::API_PREFIX`]: `/avatars/<token>`.
pub const ROUTE: &str = "/avatars/";

/// The token a person's picture is served under: a digest of the person and
/// the picture's own bytes.
///
/// It is unguessable without the picture (the picture's bytes are in it), and
/// it names one picture of one person: a picture they replace or remove leaves
/// the old address answering nothing, which is how a person revokes it.
#[must_use]
pub fn token(root: &[u8; 32], photo: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo.public-avatar.v1\0");
    hasher.update(root);
    hasher.update(photo);
    hasher.finalize().to_hex().to_string()
}

/// The public address of `root`'s picture, at this host's `origin`
/// (`https://eu1.tilo.sh`).
#[must_use]
pub fn url(origin: &str, root: &[u8; 32], photo: &[u8]) -> String {
    format!(
        "{}{}{}{}",
        origin.trim_end_matches('/'),
        crate::web::API_PREFIX,
        ROUTE,
        token(root, photo)
    )
}

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

    tilo_test_env::scenario! {
        fn a_public_avatar_is_an_image_named_by_its_digest;

        phase images_are_recognised_by_their_magic_bytes_only;
        {
            assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
            assert_eq!(sniff(b"\xff\xd8\xff\xe0...."), Some("image/jpeg"));
            assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
            assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some("image/webp"));
            assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None);
            assert_eq!(sniff(b""), None);
        }

        phase an_avatar_name_is_a_blake3_hex_digest;
        {
            assert!(is_avatar_name(&"ab".repeat(32)));
            assert!(!is_avatar_name("../etc/passwd"));
            assert!(!is_avatar_name(&"AB".repeat(32)));
        }

        phase a_pictures_token_names_the_person_and_the_picture;
        {
            let root = [7; 32];
            let png = b"\x89PNG\r\n\x1a\n....";
            let name = token(&root, png);
            assert!(is_avatar_name(&name));
            assert_eq!(
                name,
                token(&root, png),
                "the same picture of the same person is one token"
            );
            assert_ne!(
                name,
                token(&[8; 32], png),
                "another person's picture is another token"
            );
            assert_ne!(
                name,
                token(&root, b"\x89PNG\r\n\x1a\n....!"),
                "a replaced picture is another token"
            );
            assert_eq!(
                url("https://eu1.tilo.sh/", &root, png),
                format!("https://eu1.tilo.sh/peer/v1/avatars/{name}")
            );
        }
    }
}

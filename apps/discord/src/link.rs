//! Link codes (discord-bridge.md §5, "Linking"): 60 random bits shown as 12
//! base-32 characters, stored only as a SHA-256 hash.

pub const CODE_TTL_MS: i64 = 10 * 60 * 1000;
pub const MAX_FAILURES: u32 = 5;
pub const FAILURE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// A fresh code from the operating system's random source.
#[must_use]
pub fn new_code() -> String {
    let mut bytes = [0u8; 8];
    rand::fill(&mut bytes);
    code_from_bytes(bytes)
}

/// The code of the first 60 bits of `bytes`.
#[must_use]
pub fn code_from_bytes(bytes: [u8; 8]) -> String {
    // 64 bits encode as 13 base-32 characters; the first 12 carry 60 bits.
    data_encoding::BASE32_NOPAD
        .encode(&bytes)
        .chars()
        .take(12)
        .collect()
}

/// The hash the record and the event log hold. The worker hashes a
/// `/link` code with the same function before the code reaches the log.
#[must_use]
pub fn hash(code: &str) -> String {
    discord_wire::event::code_sha256(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_twelve_base32_characters() {
        let code = code_from_bytes([0xff; 8]);
        assert_eq!(code.len(), 12);
        assert!(
            code.chars()
                .all(|c| "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567".contains(c))
        );
        assert_eq!(new_code().len(), 12);
        assert_ne!(new_code(), new_code());
    }

    #[test]
    fn hashing_ignores_case_and_spaces() {
        assert_eq!(hash(" abcd efgh ijkl "), hash("ABCDEFGHIJKL"));
        assert_eq!(hash("ABCDEFGHIJKL").len(), 64);
    }
}

//! The Python behaviours the helpers' checks were defined with: what counts
//! as whitespace, how a text file's line endings read, and how strictly
//! `base64.b64decode(validate=True)` decodes.

pub use tilo_pycompat::{
    Dumps, as_int, b64decode, equal, int, is_space, repr_str, sha256_hex, split, str_of, strip,
    truthy,
};

/// What `Path.read_text()` returns for `bytes`: universal newlines, so
/// `\r\n` and a lone `\r` both read as `\n`.
#[must_use]
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;

    #[test]
    fn text_reads_with_universal_newlines() {
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }
}

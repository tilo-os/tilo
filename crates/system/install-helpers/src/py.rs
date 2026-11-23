//! The Python behaviours the helpers' checks were defined with: what counts as
//! whitespace, how a text file's line endings read, and how strictly
//! `base64.b64decode(validate=True)` decodes.

pub use tilo_pycompat::{
    Dumps, as_int, b64decode, equal, int, is_space, repr_str, sha256_hex, split, str_of, strip,
    truthy,
};

#[must_use]
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

//! The tilolang runtime's records, styles and rich text, for the tests of the core crates that
//! read them and ship without the language.
//!
//! A core crate links no language, so its own tests cannot name one. A dev-dependency on a
//! `tools/` crate is allowed (`tools/policy/check_crate_layout.py`): this one re-exports the
//! runtime the daemon links, which such a test mounts on the daemon it starts.

pub use tilolang_workspace::{host, publish, records, runtime, styles};

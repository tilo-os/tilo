#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

pub mod config_tests;

/// A genuine `.docx` fixture (a complete OOXML package: heading, styled runs, a
/// bullet list, a 2x2 table, metadata) shared by the docstore tests.
#[cfg(test)]
pub(crate) fn sample_docx() -> Vec<u8> {
    include_bytes!("fixtures/sample.docx").to_vec()
}

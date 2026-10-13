pub mod config_tests;

/// A genuine `.docx` fixture (a complete OOXML package: heading, styled runs, a
/// bullet list, a 2x2 table, metadata) shared by the docstore tests.
#[cfg(test)]
pub(crate) fn sample_docx() -> Vec<u8> {
    include_bytes!(concat!(env!("OUT_DIR"), "/sample.docx")).to_vec()
}

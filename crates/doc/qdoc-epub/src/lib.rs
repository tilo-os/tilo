//! Compatibility facade for qdoc's shared-runtime EPUB exporter.

pub use qdoc::export_epub::{DocEpubError, render_epub};

#[cfg(test)]
mod tests;

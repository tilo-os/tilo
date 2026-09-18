//! Compatibility facade for qdoc's shared-runtime HTML exporter.

pub use qdoc::export_html::{HtmlOptions, render_html};

#[cfg(test)]
mod tests;

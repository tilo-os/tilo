//! Compatibility facade for qdoc's shared-runtime RTF exporter.

pub use qdoc::export_rtf::render_rtf;

#[cfg(test)]
mod tests;

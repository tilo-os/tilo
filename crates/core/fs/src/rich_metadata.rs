//! Fast, bounded extraction of facts safe to replicate as file metadata.
//!
//! This module only sees bytes already present for a local write/preview. It
//! never opens a path or asks the hydrator for remote content.

use fs_protocol::{FilePropertiesV1, files::FILE_PROPERTIES_VERSION};

pub const MAX_METADATA_SCAN_BYTES: usize = tilo_content::FACTS_SCAN_BYTES;

/// Whether [`extract`] can find anything in a file called `name`, so a
/// caller can skip reading bytes for it.
pub fn recognizes(name: &str) -> bool {
    tilo_content::has_facts(name)
}

/// The facts the file's header states, as the metadata that replicates with it.
pub fn extract(name: &str, bytes: &[u8]) -> Option<FilePropertiesV1> {
    let facts = tilo_content::facts(name, bytes.get(..MAX_METADATA_SCAN_BYTES).unwrap_or(bytes))?;
    let value = FilePropertiesV1 {
        schema_version: FILE_PROPERTIES_VERSION,
        image_width: facts.width,
        image_height: facts.height,
        duration_ms: facts.duration_ms,
        page_count: facts.page_count,
        codec: facts.codec,
        container: facts.container,
    };
    value.validate().ok().map(|()| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_what_the_header_states_and_validates() {
        let p = extract("a.pdf", b"%PDF-1.7 /Type /Pages /Type /Page /Type /Page\n").unwrap();
        assert_eq!(p.page_count, Some(2));
        assert!(extract("photo.png", b"not a png").is_none());
    }
}

//! Content one window hands another app to open (the camera's capture for
//! Photos, a report's recording for the app that replays it), opened as the
//! file it is. The page's windows share one content store, so what the
//! handing window kept there is what the opened window reads, by hash, as
//! the workspace broker stages it for an app it launches.

use tilo_content::ContentStore;
use ui::application::{OpenFile, OpenFileReceipt};

/// The file the content `hash` in `content` opens as, named and typed by
/// what its bytes are.
///
/// # Errors
/// Content the page does not hold, or bytes no app may be handed.
pub(crate) fn opened_file(
    hash: &str,
    content: &ContentStore,
) -> Result<(OpenFile, OpenFileReceipt), String> {
    let bytes = content
        .read(hash)
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| "What this window was to open is no longer here".to_owned())?;
    let (extension, media_type) = tilo_content::opened_as(&bytes);
    OpenFile::new(&format!("{hash}.{extension}"), media_type, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handed_content_opens_as_the_file_its_bytes_are() {
        let content = ContentStore::new(tilo_content::MemoryBackend::new(
            tilo_content::ContentLimits::default(),
        ));
        let png = b"\x89PNG\r\n\x1a\n-a-capture".to_vec();
        let hash = content.put(&png).unwrap();
        let (file, _) = opened_file(&hash, &content).unwrap();
        assert_eq!(file.name(), format!("{hash}.png"));
        assert_eq!(file.media_type(), "image/png");
        assert_eq!(file.bytes(), png.as_slice());
        assert!(opened_file(&"ab".repeat(32), &content).is_err());
    }
}

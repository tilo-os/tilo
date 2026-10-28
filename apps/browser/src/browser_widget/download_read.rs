//! Reading and answering a download.
//!
//! A page's offer and a kept file both ride the download row an agent already
//! finds: answering keeps the file or discards it, and reading a kept file
//! answers its text. The bytes are kept out of [`super`] so the widget's own
//! file stays one subject.

use ui::webview::WebViewCommand;

use super::BrowserWidget;
use crate::downloads::DownloadStatus;

/// How much of a downloaded file an agent reads at once.
const DOWNLOAD_READ_LIMIT: usize = 256 * 1024;

impl BrowserWidget {
    /// A completed download's file as bounded UTF-8 text, or a note of its
    /// type and size when it holds anything else. The answer is untrusted
    /// page content (the `read-download` effect).
    pub(super) fn read_download(&self, id: &str) -> Result<String, String> {
        let path = self.completed_download_path(id)?;
        let entry = self
            .downloads
            .get(id)
            .ok_or_else(|| "download no longer exists".to_string())?;
        let name = entry.display_name().to_string();
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("could not open the downloaded file: {error}"))?;
        let size = metadata.len();
        if !metadata.is_file() {
            return Ok(format!(
                "{name} is not a file, so it has no text to read (size {size} bytes)."
            ));
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("could not read the downloaded file: {error}"))?;
        let Some(text) = decode_bounded_text(&bytes, DOWNLOAD_READ_LIMIT) else {
            return Ok(format!(
                "{name} is not UTF-8 text; it holds {size} bytes of data. Its contents cannot be read here."
            ));
        };
        Ok(text)
    }

    /// Answer the page's offer to download `id`: save it when `allow`, else
    /// discard it. The engine continues the transfer only when kept.
    pub(super) fn answer_download(&mut self, id: &str, allow: bool) -> Result<String, String> {
        let entry = self
            .downloads
            .get(id)
            .ok_or_else(|| "download no longer exists".to_string())?;
        if !matches!(
            entry.status,
            DownloadStatus::AwaitingDestination | DownloadStatus::Downloading
        ) {
            return Err("the page is no longer offering this download".into());
        }
        let name = entry.display_name().to_string();
        if let Some(native) = self.native_download_id(id) {
            self.engine
                .send(WebViewCommand::DownloadDecision { id: native, allow });
        } else if allow {
            // A saved-list row with no live engine download has no file to
            // save; the offer expired with its session.
            return Err("the page is no longer offering this download".into());
        }
        self.structure_dirty = true;
        self.persist_downloads_now();
        // Naming what the page offered and what became of it, not a running
        // announcement of routine saving.
        Ok(if allow {
            format!("{name} is kept.")
        } else {
            format!("{name} is discarded.")
        })
    }
}

/// `bytes` as UTF-8 text, at most `limit` bytes, or `None` when they are
/// not text. A multi-byte character cut by the limit is dropped whole.
fn decode_bounded_text(bytes: &[u8], limit: usize) -> Option<String> {
    let head = bytes.get(..bytes.len().min(limit))?;
    match std::str::from_utf8(head) {
        Ok(text) => Some(text.to_string()),
        Err(error) if bytes.len() > limit && error.error_len().is_none() => {
            // The limit split a character; keep what is whole.
            let whole = head.get(..error.valid_up_to())?;
            std::str::from_utf8(whole).ok().map(str::to_string)
        }
        Err(_) => None,
    }
}

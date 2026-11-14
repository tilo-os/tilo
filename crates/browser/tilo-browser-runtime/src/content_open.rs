//! Content one window hands another app to open (the camera's capture for
//! Photos, a report's recording for the app that replays it), or a file
//! dropped on the desk, opened as the file it is. A desktop's windows share
//! one content store, so what the handing window kept there is what the
//! opened window reads, by hash, as the workspace broker stages it for an app
//! it launches.

use tilo_content::ContentStore;
use ui::application::{MAX_OPEN_FILE_BYTES, OpenFile, OpenFileReceipt};

/// The file the content `hash` in `content` opens as, named `name` where the
/// opener knows the file's name, else by its hash, and typed by what its
/// bytes are. Content kept as a file of the workspace's tree (a manifest
/// naming its chunks, [`fs_content`]) opens as the file the chunks make.
///
/// # Errors
/// Content the page does not hold, or bytes no app may be handed.
pub(crate) fn opened_file(
    hash: &str,
    content: &ContentStore,
    name: Option<&str>,
) -> Result<(OpenFile, OpenFileReceipt), String> {
    let gone = || "What this window was to open is no longer here".to_owned();
    let bytes = content
        .read(hash)
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(gone)?;
    let bytes = if fs_content::is_manifest(&bytes) {
        whole(&bytes, content).ok_or_else(gone)??
    } else {
        bytes
    };
    let (extension, media_type) = tilo_content::opened_as(&bytes);
    // Bytes say a file is text, not which: its name says it is notes, and plain text reads as
    // the Markdown it is.
    let written = name
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            ["md", "markdown", "txt", "text"].contains(&extension.to_ascii_lowercase().as_str())
        });
    let media_type = if media_type == "text/plain" && written {
        "text/markdown"
    } else {
        media_type
    };
    let name = name.map_or_else(|| format!("{hash}.{extension}"), str::to_owned);
    OpenFile::new(&name, media_type, bytes)
}

/// The file the manifest `bytes` names, its chunks read from `content`:
/// `None` while a chunk is not here, refused past what an app may be handed.
fn whole(bytes: &[u8], content: &ContentStore) -> Option<Result<Vec<u8>, String>> {
    let manifest = fs_content::Manifest::decode(bytes).ok()?;
    if manifest.total_len() > MAX_OPEN_FILE_BYTES as u64 {
        return Some(Err(format!(
            "The file is larger than the {} an app may be handed",
            ui::format::memory(MAX_OPEN_FILE_BYTES as u64)
        )));
    }
    let mut file = Vec::with_capacity(usize::try_from(manifest.total_len()).unwrap_or(0));
    for chunk in manifest.chunks() {
        file.extend_from_slice(&content.read(&hex::encode(chunk.hash))?);
    }
    Some(Ok(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ContentStore {
        ContentStore::new(tilo_content::MemoryBackend::new(
            tilo_content::ContentLimits::default(),
        ))
    }

    #[test]
    fn handed_content_opens_as_the_file_its_bytes_are() {
        let content = store();
        let png = b"\x89PNG\r\n\x1a\n-a-capture".to_vec();
        let hash = content.put(&png).unwrap();
        let (file, _) = opened_file(&hash, &content, None).unwrap();
        assert_eq!(file.name(), format!("{hash}.png"));
        assert_eq!(file.media_type(), "image/png");
        assert_eq!(file.bytes(), png.as_slice());
        assert!(opened_file(&"ab".repeat(32), &content, None).is_err());
    }

    #[test]
    fn a_dropped_file_kept_as_chunks_opens_whole_under_its_own_name() {
        let content = store();
        let mut pdf = b"%PDF-1.7\n".to_vec();
        pdf.extend((0..300_000_u32).map(|n| (n % 251) as u8));
        let mut stream = fs_content::Streamed::new(pdf.len() as u64, None);
        let mut keep = |_: &fs_content::ChunkRef, bytes: &[u8]| {
            content.put_public(bytes).unwrap();
        };
        stream.push(&pdf, &mut keep).unwrap();
        let (manifest, _) = stream.finish(&mut keep).unwrap();
        assert!(manifest.chunks().len() > 1, "a file of many chunks");
        let hash = content.put_public(&manifest.encode()).unwrap();
        assert_eq!(
            hash,
            hex::encode(manifest.id()),
            "kept under the hash the tree names"
        );
        let (file, _) = opened_file(&hash, &content, Some("scan.pdf")).unwrap();
        assert_eq!(file.name(), "scan.pdf");
        assert_eq!(file.media_type(), "application/pdf");
        assert_eq!(file.bytes(), pdf.as_slice());
        // Text named as notes opens as the Markdown Notes opens.
        let note = content.put(b"# Plan\n- one\n").unwrap();
        let (file, _) = opened_file(&note, &content, Some("plan.md")).unwrap();
        assert_eq!(file.media_type(), "text/markdown");
        let (file, _) = opened_file(&note, &content, Some("todo.txt")).unwrap();
        assert_eq!(file.media_type(), "text/markdown");
        let (file, _) = opened_file(&note, &content, None).unwrap();
        assert_eq!(file.media_type(), "text/plain");
    }
}

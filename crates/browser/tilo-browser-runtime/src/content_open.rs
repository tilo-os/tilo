//! Content one window hands another app to open (the camera's capture for
//! Photos, a report's recording for the app that replays it), or a file
//! dropped on the shell, opened as the file it is. A desktop's windows share
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
    if fs_content::is_manifest(&bytes)
        && fs_content::Manifest::decode(&bytes)
            .is_ok_and(|manifest| manifest.total_len() > MAX_OPEN_FILE_BYTES as u64)
    {
        return Err(format!(
            "The file is larger than the {} an app may be handed",
            ui::format::memory(MAX_OPEN_FILE_BYTES as u64)
        ));
    }
    let bytes = fs_content::whole(bytes, MAX_OPEN_FILE_BYTES as u64, |chunk| {
        content.read(&hex::encode(chunk))
    })
    .ok_or_else(gone)?;
    let (extension, media_type) = tilo_content::opened_as(&bytes);
    // Bytes say a file is text, not which: its name says it is notes, and plain text reads as
    // the Markdown it is, or a table kept as text. An empty file named so is one not yet written.
    let named = name
        .and_then(|name| name.rsplit_once('.'))
        .and_then(
            |(_, extension)| match extension.to_ascii_lowercase().as_str() {
                "md" | "markdown" | "txt" | "text" => Some("text/markdown"),
                "csv" => Some("text/csv"),
                "tsv" => Some("text/tab-separated-values"),
                _ => None,
            },
        );
    let media_type = match named {
        Some(named) if media_type == "text/plain" || bytes.is_empty() => named,
        _ => media_type,
    };
    let name = name.map_or_else(|| format!("{hash}.{extension}"), str::to_owned);
    OpenFile::new(&name, media_type, bytes)
}

/// Keep the file whose content is `hash` in `kept` (its manifest and the chunks it lists, or its
/// bytes whole) in `windows` too, under the same addresses: a file dropped on the shell is kept in
/// the workspace tree's content, and the window it opens in reads the content windows share.
///
/// # Errors
/// What `kept` lacks of the file, or what `windows` refused to keep.
pub(crate) fn hand_over(
    kept: &ContentStore,
    windows: &ContentStore,
    hash: &str,
) -> Result<(), String> {
    let gone = || format!("{hash} is not kept here");
    let bytes = kept.read(hash).ok_or_else(gone)?;
    let mut parts = Vec::new();
    if fs_content::is_manifest(&bytes) {
        let manifest = fs_content::Manifest::decode(&bytes).map_err(|error| error.to_string())?;
        for chunk in manifest.chunks() {
            let chunk = hex::encode(chunk.hash);
            parts.push(
                kept.read(&chunk)
                    .ok_or_else(|| format!("{chunk} is not kept here"))?,
            );
        }
    }
    parts.push(bytes);
    for part in parts {
        windows
            .put_public(&part)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
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
        // A new file, made empty and named as notes, is a note not yet written.
        let stream = fs_content::Streamed::new(0, None);
        let mut keep = |_: &fs_content::ChunkRef, bytes: &[u8]| {
            content.put_public(bytes).unwrap();
        };
        let (manifest, _) = stream.finish(&mut keep).unwrap();
        let empty = content.put_public(&manifest.encode()).unwrap();
        let (file, _) = opened_file(&empty, &content, Some("plan.txt")).unwrap();
        assert_eq!(file.name(), "plan.txt");
        assert_eq!(file.media_type(), "text/markdown");
        assert!(file.bytes().is_empty());
        // A table kept as text is what its name says, for the app that opens tables.
        let table = content.put(b"name,amount\nTea,4\n").unwrap();
        let (file, _) = opened_file(&table, &content, Some("data.csv")).unwrap();
        assert_eq!(file.media_type(), "text/csv");
        let (file, _) = opened_file(&table, &content, Some("data.TSV")).unwrap();
        assert_eq!(file.media_type(), "text/tab-separated-values");
    }

    #[test]
    fn a_dropped_file_is_handed_whole_to_the_content_windows_read() {
        let (kept, windows) = (store(), store());
        let mut csv = b"name,amount\n".to_vec();
        csv.extend((0..40_000_u32).flat_map(|n| format!("item {n},{n}\n").into_bytes()));
        let mut stream = fs_content::Streamed::new(csv.len() as u64, None);
        let mut keep = |_: &fs_content::ChunkRef, bytes: &[u8]| {
            kept.put_public(bytes).unwrap();
        };
        stream.push(&csv, &mut keep).unwrap();
        let (manifest, _) = stream.finish(&mut keep).unwrap();
        let hash = kept.put_public(&manifest.encode()).unwrap();
        assert!(opened_file(&hash, &windows, Some("big.csv")).is_err());

        hand_over(&kept, &windows, &hash).unwrap();
        let (file, _) = opened_file(&hash, &windows, Some("big.csv")).unwrap();
        assert_eq!(file.media_type(), "text/csv");
        assert_eq!(file.bytes(), csv.as_slice());
        assert!(hand_over(&kept, &windows, &"ab".repeat(32)).is_err());
    }
}

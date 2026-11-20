//! Content one window hands another app to open (the camera's capture for
//! Photos, a report's recording for the app that replays it), or a file
//! dropped on the shell, opened as the file it is. A desktop's windows share
//! one content store, so what the handing window kept there is what the
//! opened window reads, by hash, as the workspace broker stages it for an app
//! it launches.

#[cfg(target_arch = "wasm32")]
use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
use tilo_content::ContentStore;
#[cfg(target_arch = "wasm32")]
use tilo_session_client::{DocumentRoute, engine::SealKeys};
use ui::application::{MAX_OPEN_FILE_BYTES, OpenFile, OpenFileReceipt};

#[cfg(target_arch = "wasm32")]
pub(crate) fn files_route(workspace: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: fs_protocol::tree::FILES_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: fs_protocol::tree::document_id(),
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn file_bytes(
    bytes: Vec<u8>,
    content: &ContentStore,
    limit: u64,
    access: Option<(u32, &str)>,
) -> Option<Vec<u8>> {
    let fetch = |chunk: &[u8; 32]| content.read(&hex::encode(chunk));
    let sealed = fs_content::Manifest::decode(&bytes)
        .ok()
        .and_then(|manifest| manifest.sealed());
    let Some(sealed) = sealed else {
        return fs_content::whole(bytes, limit, fetch);
    };
    let (peer, workspace) = access?;
    let handle = crate::peer_sync::handle_of(peer)?;
    let route = files_route(workspace);
    let workspace = route.workspace_id().ok()?;
    let group = WorkspaceId::from_u128(u128::from_be_bytes(sealed.group));
    let SealKeys::Here(keys) = handle.engine().document_content_keys(&route, group).ok()? else {
        return None;
    };
    let metadata =
        EncryptionMetadata::content_group(workspace, group, sealed.epoch, ContentRetention::Delta);
    let address = file_content_artifact(workspace);
    fs_content::whole_with(bytes, limit, fetch, |_, chunk| {
        keys.open_content(address, &metadata, chunk)
            .ok()
            .map(|bytes| bytes.to_vec())
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn file_bytes(
    bytes: Vec<u8>,
    content: &ContentStore,
    limit: u64,
    _access: Option<(u32, &str)>,
) -> Option<Vec<u8>> {
    fs_content::whole(bytes, limit, |chunk| content.read(&hex::encode(chunk)))
}

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
    workspace: Option<(u32, &str)>,
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
    let bytes =
        file_bytes(bytes, content, MAX_OPEN_FILE_BYTES as u64, workspace).ok_or_else(gone)?;
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

/// What opening the workspace's content `hash`, called `name`, in `content` comes to: the file is
/// here (or its bytes are whole), or it is still on its way.
pub(crate) enum Opening {
    /// The app that opens the file its bytes are.
    App(String),
    /// No app here opens it.
    NoApp,
    /// The file's bytes are not all here yet: the page asked the workspace for them.
    Coming,
}

/// What opening `hash`, called `name`, in `content` comes to: the file it is (a manifest and the
/// chunks it lists, or its bytes whole), and which app its bytes name.
///
/// # Errors
/// Bytes no app may be handed.
pub(crate) fn opening_app(
    content: &ContentStore,
    hash: &str,
    name: &str,
    workspace: Option<(u32, &str)>,
) -> Result<Opening, String> {
    let Some(bytes) = content.read(hash) else {
        return Ok(Opening::Coming);
    };
    // A manifest whose chunks the page has not all collected is a file still coming: reading each
    // one asks the workspace for it.
    if fs_content::is_manifest(&bytes) {
        let manifest = fs_content::Manifest::decode(&bytes).map_err(|error| error.to_string())?;
        let whole = manifest
            .chunks()
            .iter()
            .all(|chunk| content.read(&hex::encode(chunk.hash)).is_some());
        if !whole {
            return Ok(Opening::Coming);
        }
    }
    let (file, _) = opened_file(hash, content, Some(name), workspace)?;
    Ok(crate::dropped_files::opens_in(file.bytes(), file.name())
        .map_or(Opening::NoApp, Opening::App))
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
        let (file, _) = opened_file(&hash, &content, None, None).unwrap();
        assert_eq!(file.name(), format!("{hash}.png"));
        assert_eq!(file.media_type(), "image/png");
        assert_eq!(file.bytes(), png.as_slice());
        assert!(opened_file(&"ab".repeat(32), &content, None, None).is_err());
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
        let (file, _) = opened_file(&hash, &content, Some("scan.pdf"), None).unwrap();
        assert_eq!(file.name(), "scan.pdf");
        assert_eq!(file.media_type(), "application/pdf");
        assert_eq!(file.bytes(), pdf.as_slice());
        // Text named as notes opens as the Markdown Notes opens.
        let note = content.put(b"# Plan\n- one\n").unwrap();
        let (file, _) = opened_file(&note, &content, Some("plan.md"), None).unwrap();
        assert_eq!(file.media_type(), "text/markdown");
        let (file, _) = opened_file(&note, &content, Some("todo.txt"), None).unwrap();
        assert_eq!(file.media_type(), "text/markdown");
        let (file, _) = opened_file(&note, &content, None, None).unwrap();
        assert_eq!(file.media_type(), "text/plain");
        // A new file, made empty and named as notes, is a note not yet written.
        let stream = fs_content::Streamed::new(0, None);
        let mut keep = |_: &fs_content::ChunkRef, bytes: &[u8]| {
            content.put_public(bytes).unwrap();
        };
        let (manifest, _) = stream.finish(&mut keep).unwrap();
        let empty = content.put_public(&manifest.encode()).unwrap();
        let (file, _) = opened_file(&empty, &content, Some("plan.txt"), None).unwrap();
        assert_eq!(file.name(), "plan.txt");
        assert_eq!(file.media_type(), "text/markdown");
        assert!(file.bytes().is_empty());
        // A table kept as text is what its name says, for the app that opens tables.
        let table = content.put(b"name,amount\nTea,4\n").unwrap();
        let (file, _) = opened_file(&table, &content, Some("data.csv"), None).unwrap();
        assert_eq!(file.media_type(), "text/csv");
        let (file, _) = opened_file(&table, &content, Some("data.TSV"), None).unwrap();
        assert_eq!(file.media_type(), "text/tab-separated-values");
    }

    /// A file a workspace's tree names by its manifest (a file copied in through the workspace's
    /// folder, as a login mounts it) opens in the app its bytes say: the manifest names the file,
    /// never what a person wrote in it.
    #[test]
    fn an_opened_file_names_the_app_its_bytes_open_however_the_tree_kept_it() {
        let content = store();
        let mut keep = |_: &fs_content::ChunkRef, bytes: &[u8]| {
            content.put_public(bytes).unwrap();
        };
        let letter = b"Dear Ada,\n\nThe keys are under the flowerpot.\n\nGrace\n".to_vec();
        let mut stream = fs_content::Streamed::new(letter.len() as u64, None);
        stream.push(&letter, &mut keep).unwrap();
        let (manifest, _) = stream.finish(&mut keep).unwrap();
        let text = content.put_public(&manifest.encode()).unwrap();
        assert!(
            matches!(
                opening_app(&content, &text, "letter.txt", None).unwrap(),
                Opening::App(app) if app == "so.tilo.notes"
            ),
            "a text document kept as chunks opens in Notes"
        );
        // The table a command put in, kept whole, is what its name says.
        let table = content.put(b"item,cost\nrent,900\n").unwrap();
        assert!(
            matches!(
                opening_app(&content, &table, "budget.csv", None).unwrap(),
                Opening::App(app) if app == "so.tilo.sheets"
            ),
            "a table kept whole opens in Sheets"
        );
        let png = b"\x89PNG\r\n\x1a\n-a-capture".to_vec();
        let photo = content.put(&png).unwrap();
        assert!(matches!(
            opening_app(&content, &photo, "spring.png", None).unwrap(),
            Opening::App(app) if app == "so.tilo.photos"
        ));
        let blob = content.put(&[0_u8, 1, 2, 3]).unwrap();
        assert!(matches!(
            opening_app(&content, &blob, "blob.bin", None).unwrap(),
            Opening::NoApp
        ));
        assert!(
            matches!(
                opening_app(&content, &"ab".repeat(32), "gone.txt", None).unwrap(),
                Opening::Coming
            ),
            "content the page does not hold is a file still on its way"
        );
        // A manifest whose chunks have not all arrived is a file still coming, never one no app
        // opens.
        let bare = store();
        let only_manifest = bare.put_public(&manifest.encode()).unwrap();
        assert!(
            matches!(
                opening_app(&bare, &only_manifest, "letter.txt", None).unwrap(),
                Opening::Coming
            ),
            "a file whose chunks are on their way is still coming"
        );
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
        assert!(opened_file(&hash, &windows, Some("big.csv"), None).is_err());

        hand_over(&kept, &windows, &hash).unwrap();
        let (file, _) = opened_file(&hash, &windows, Some("big.csv"), None).unwrap();
        assert_eq!(file.media_type(), "text/csv");
        assert_eq!(file.bytes(), csv.as_slice());
        assert!(hand_over(&kept, &windows, &"ab".repeat(32)).is_err());
    }
}

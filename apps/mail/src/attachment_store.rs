//! Portable, verified draft-attachment content.
//!
//! A Mail draft never keeps a host pathname.  The portal gives this module an
//! already-open descriptor, which is copied into the app's content store.
//! The CRDT then carries the stable identity plus bounded presentation
//! metadata; the bytes are read back from the content store, verified against
//! that identity, only when a message is sent.

use std::io::Read;

use tilo_content::ContentStore;

use crate::model::DraftAttachment;

pub const PREFIX: &str = "mail-attachment:blake3:";
/// The portable attachment ceiling. Enforcing it before reading means Mail rejects before a chooser read/stage can report a
/// vague retention failure for a 32–64 MiB file.
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 16;
pub const MAX_ATTACHMENT_TOTAL_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_FILENAME_CHARS: usize = 180;
pub const MAX_MIME_CHARS: usize = 127;

pub fn valid_content_id(id: &str) -> bool {
    id.strip_prefix(PREFIX).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub fn safe_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = last
        .chars()
        .filter(|ch| !ch.is_control() && !matches!(ch, '/' | '\\'))
        .take(MAX_FILENAME_CHARS)
        .collect();
    match clean.trim() {
        "" | "." | ".." => "attachment".into(),
        value => value.to_string(),
    }
}

pub fn safe_mime(mime: &str) -> String {
    let value: String = mime
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_graphic() && !matches!(ch, '"' | '\\'))
        .take(MAX_MIME_CHARS)
        .collect();
    if value.contains('/') && !value.starts_with('/') && !value.ends_with('/') {
        value
    } else {
        "application/octet-stream".into()
    }
}

pub fn is_valid(attachment: &DraftAttachment) -> bool {
    valid_content_id(&attachment.content_id)
        && !attachment.filename.is_empty()
        && attachment.filename.chars().count() <= MAX_FILENAME_CHARS
        && !attachment.filename.contains(['/', '\\'])
        && !attachment.filename.chars().any(char::is_control)
        && attachment.mime.chars().count() <= MAX_MIME_CHARS
        && attachment.mime.contains('/')
        && attachment.size <= MAX_ATTACHMENT_BYTES as u64
}

pub fn bounded(attachments: &[DraftAttachment]) -> Vec<DraftAttachment> {
    let mut total = 0usize;
    let mut out: Vec<DraftAttachment> = Vec::with_capacity(attachments.len().min(MAX_ATTACHMENTS));
    for raw in attachments {
        if out.len() == MAX_ATTACHMENTS || !is_valid(raw) {
            continue;
        }
        let Ok(size) = usize::try_from(raw.size) else {
            continue;
        };
        let Some(next_total) = total.checked_add(size) else {
            continue;
        };
        if next_total > MAX_ATTACHMENT_TOTAL_BYTES
            || out.iter().any(|item| item.content_id == raw.content_id)
        {
            continue;
        }
        total = next_total;
        out.push(raw.clone());
    }
    out
}

/// Copy a descriptor already opened by the portal into `content`.  The sentinel
/// prevents a size metadata/read race from allocating unbounded memory.
///
/// # Errors
///
/// Returns an error when the descriptor is not a regular file, exceeds the
/// portable content limit, cannot be read, or the store cannot retain/verify it.
pub fn stage_file(
    content: &ContentStore,
    file: std::fs::File,
    display_name: &str,
) -> Result<DraftAttachment, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect selected file: {error}"))?;
    if !metadata.is_file() {
        return Err("the selected item is not a regular file".into());
    }
    let declared = usize::try_from(metadata.len()).map_err(|_| "selected file is too large")?;
    if declared > MAX_ATTACHMENT_BYTES {
        return Err("attachments are limited to 32 MiB".into());
    }
    let mut bytes = Vec::with_capacity(declared);
    file.take((MAX_ATTACHMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read selected file: {error}"))?;
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err("attachments are limited to 32 MiB".into());
    }
    stage_bytes(content, &bytes, display_name)
}

/// Store already bounded bytes under their canonical identity.  This is used
/// by the portal staging path and lets tests exercise send materialization
/// without reintroducing path-based attachment fixtures.
///
/// # Errors
///
/// Returns an error when bytes exceed the portable content limit or the store
/// cannot retain and verify the resulting content identity.
pub fn stage_bytes(
    content: &ContentStore,
    bytes: &[u8],
    display_name: &str,
) -> Result<DraftAttachment, String> {
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err("attachments are limited to 32 MiB".into());
    }
    let hash = content.put(bytes).map_err(|error| {
        tracing::warn!(%error, "mail: couldn't store attachment content");
        "could not retain attachment content for this draft".to_string()
    })?;
    let stored = content
        .read(&hash)
        .filter(|stored| stored == bytes && tilo_content::verify(&hash, stored))
        .ok_or_else(|| "attachment content failed integrity verification".to_string())?;
    let size = u64::try_from(stored.len()).map_err(|_| "selected file is too large")?;
    let filename = safe_filename(display_name);
    Ok(DraftAttachment {
        content_id: format!("{PREFIX}{hash}"),
        mime: safe_mime(crate::transport::mime_from_filename(&filename)),
        filename,
        size,
    })
}

/// Materialise a portable reference only after re-validating identity,
/// metadata, byte count and digest. Missing peer content deliberately fails.
pub fn materialize(
    content: &ContentStore,
    attachment: &DraftAttachment,
) -> Result<Vec<u8>, String> {
    if !is_valid(attachment) {
        return Err(format!(
            "attachment {} has invalid metadata",
            attachment.filename
        ));
    }
    let hash = attachment.content_id.strip_prefix(PREFIX).ok_or_else(|| {
        format!(
            "attachment {} has invalid content identity",
            attachment.filename
        )
    })?;
    let bytes = content.read(hash).ok_or_else(|| {
        format!(
            "attachment {} is not available on this device yet",
            attachment.filename
        )
    })?;
    if bytes.len() > MAX_ATTACHMENT_BYTES
        || u64::try_from(bytes.len()).ok() != Some(attachment.size)
        || !tilo_content::verify(hash, &bytes)
    {
        return Err(format!(
            "attachment {} failed integrity validation",
            attachment.filename
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_attachments_are_portable_and_bounded() {
        {
            let path = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(path.path(), b"portable attachment").unwrap();
            let content = ContentStore::memory();
            let staged = stage_file(
                &content,
                std::fs::File::open(path.path()).unwrap(),
                "../report.txt",
            )
            .unwrap();
            assert!(valid_content_id(&staged.content_id));
            assert_eq!(staged.filename, "report.txt");
            assert_eq!(
                materialize(&content, &staged).unwrap(),
                b"portable attachment"
            );
        }

        {
            let bytes = vec![0; MAX_ATTACHMENT_BYTES + 1];
            assert!(stage_bytes(&ContentStore::memory(), &bytes, "too-large.bin").is_err());
        }
    }
}

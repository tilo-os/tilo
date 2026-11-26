//! Bounded, revision-checked bytes for ordinary and provider-native files.

use std::io::Read;

use crate::{CloudSource, Error, ExportFormat, ItemId, ItemKind, NativeDocument, Result, Revision};

/// Choose an advertised Office representation, or SVG/PDF for Drawings.
/// Forms and Sites have no supported file representation.
pub fn preferred_export(document: &NativeDocument) -> Result<ExportFormat> {
    let (mime, extension) = match document.mime.as_str() {
        "application/vnd.google-apps.document" => (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
        ),
        "application/vnd.google-apps.spreadsheet" => (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        ),
        "application/vnd.google-apps.presentation" => (
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "pptx",
        ),
        "application/vnd.google-apps.drawing" => {
            if document
                .exports
                .iter()
                .any(|format| format.mime == "image/svg+xml")
            {
                ("image/svg+xml", "svg")
            } else {
                ("application/pdf", "pdf")
            }
        }
        _ => return Err(Error::Unsupported("native file export")),
    };
    if !document.exports.iter().any(|format| format.mime == mime) {
        return Err(Error::Unsupported("native Office export"));
    }
    // The MIME determines the suffix, not untrusted provider metadata.
    Ok(ExportFormat {
        mime: mime.into(),
        extension: extension.into(),
    })
}

/// Read or export at most `limit` bytes, verifying metadata before and after.
///
/// An explicit revision is never replaced with newer data. Without one, retry
/// up to three attempts when the provider changes during the transfer. Native
/// export endpoints generally cannot accept a conditional revision themselves.
pub fn materialize(
    source: &dyn CloudSource,
    id: &ItemId,
    expect: Option<&Revision>,
    limit: u64,
) -> Result<Vec<u8>> {
    materialize_cancellable(source, id, expect, limit, || false)
}

/// Like [`materialize`], but stops between provider calls and body reads when
/// the owning account or transfer has been revoked.
pub fn materialize_cancellable(
    source: &dyn CloudSource,
    id: &ItemId,
    expect: Option<&Revision>,
    limit: u64,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<u8>> {
    for attempt in 0..3 {
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        let before = source.stat(id)?;
        if expect.is_some() && before.revision.as_ref() != expect {
            return Err(Error::Conflict {
                current: before.revision,
            });
        }
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        let download = match &before.kind {
            ItemKind::Native(document) => source.export(id, &preferred_export(document)?),
            ItemKind::File => {
                if before.size.is_some_and(|size| size > limit) {
                    return Err(Error::TooLarge);
                }
                source.read(id, None, before.revision.as_ref())
            }
            ItemKind::Folder => return Err(Error::Unsupported("materializing folders")),
        };
        let download = match download {
            Err(Error::Conflict { .. }) if expect.is_none() && attempt < 2 => continue,
            result => result?,
        };
        if download.total.is_some_and(|size| size > limit) {
            return Err(Error::TooLarge);
        }
        let total = download.total;
        let revision = download.revision;
        let mut bytes = Vec::new();
        let mut body = download.body.take(limit.saturating_add(1));
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            if cancelled() {
                return Err(Error::SessionExpired);
            }
            let read = body.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(
                chunk
                    .get(..read)
                    .ok_or(Error::InvalidResponse("read length"))?,
            );
        }
        if u64::try_from(bytes.len()).map_err(|_| Error::TooLarge)? > limit {
            return Err(Error::TooLarge);
        }
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        let after = source.stat(id)?;
        let stable = before.revision == after.revision
            && before.kind == after.kind
            // Ordinary reads enforce `expect` in the provider, including weak
            // HTTP entity tags that refer to the same revision. Native export
            // endpoints cannot enforce that precondition themselves.
            && (before.kind == ItemKind::File || revision
                .as_ref()
                .is_none_or(|rev| Some(rev) == before.revision.as_ref()));
        if stable {
            if total.is_some_and(|size| size != bytes.len() as u64)
                || (before.kind == ItemKind::File
                    && before.size.is_some_and(|size| size != bytes.len() as u64))
            {
                return Err(Error::InvalidResponse("incomplete materialized body"));
            }
            return Ok(bytes);
        }
        if expect.is_some() || attempt == 2 {
            return Err(Error::Conflict {
                current: after.revision,
            });
        }
    }
    Err(Error::Conflict { current: None })
}

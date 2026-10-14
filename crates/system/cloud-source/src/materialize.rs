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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capabilities, Download, Item, UploadSession, UploadTarget};
    use std::ops::Range;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ChangingSource {
        stats: AtomicUsize,
        unstable_stats: usize,
        native: bool,
    }

    impl CloudSource for ChangingSource {
        fn capabilities(&self) -> Capabilities {
            Capabilities::webdav()
        }
        fn root(&self) -> ItemId {
            ItemId::new("root")
        }
        fn stat(&self, id: &ItemId) -> Result<Item> {
            let call = self.stats.fetch_add(1, Ordering::SeqCst);
            let revision = call.min(self.unstable_stats);
            Ok(Item {
                id: id.clone(),
                parents: vec![],
                name: "test".into(),
                kind: if self.native {
                    ItemKind::Native(NativeDocument {
                    mime: "application/vnd.google-apps.document".into(),
                    link_extension: "gdoc".into(), web_url: None,
                    exports: vec![ExportFormat {
                        mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
                        extension: "docx".into(),
                    }],
                })
                } else {
                    ItemKind::File
                },
                size: None,
                modified: None,
                revision: Some(Revision::new(revision.to_string())),
                hashes: vec![],
                mime: None,
            })
        }
        fn list(&self, _: &ItemId) -> Result<Vec<Item>> {
            Err(Error::Unsupported("test listing"))
        }
        fn read(
            &self,
            _: &ItemId,
            _: Option<Range<u64>>,
            _: Option<&Revision>,
        ) -> Result<Download> {
            Ok(Download {
                body: Box::new(std::io::Cursor::new(b"body")),
                range: 0..4,
                total: None,
                revision: None,
            })
        }
        fn export(&self, id: &ItemId, _: &ExportFormat) -> Result<Download> {
            self.read(id, None, None)
        }
        fn begin_upload(&self, _: UploadTarget, _: u64) -> Result<Box<dyn UploadSession>> {
            Err(Error::Unsupported("test upload"))
        }
        fn create_folder(&self, _: &ItemId, _: &str) -> Result<Item> {
            Err(Error::Unsupported("test folder creation"))
        }
        fn move_item(&self, _: &ItemId, _: &ItemId, _: &str, _: Option<&Revision>) -> Result<Item> {
            Err(Error::Unsupported("test move"))
        }
        fn delete(&self, _: &ItemId, _: Option<&Revision>) -> Result<()> {
            Err(Error::Unsupported("test delete"))
        }
    }

    #[test]
    fn cancellation_stops_before_calls_and_between_body_reads() {
        let source = ChangingSource {
            stats: AtomicUsize::new(0),
            unstable_stats: 0,
            native: true,
        };
        let id = ItemId::new("test");
        assert!(matches!(
            materialize_cancellable(&source, &id, None, 10, || true),
            Err(Error::SessionExpired)
        ));
        assert_eq!(source.stats.load(Ordering::SeqCst), 0);
        let checks = AtomicUsize::new(0);
        assert!(matches!(
            materialize_cancellable(&source, &id, None, 10, || checks
                .fetch_add(1, Ordering::SeqCst)
                >= 3),
            Err(Error::SessionExpired)
        ));
        assert_eq!(source.stats.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retries_changes_but_never_replaces_an_explicit_revision() {
        for native in [false, true] {
            let id = ItemId::new("test");
            let source = ChangingSource {
                stats: AtomicUsize::new(0),
                unstable_stats: 1,
                native,
            };
            assert_eq!(materialize(&source, &id, None, 4).unwrap(), b"body");
            assert_eq!(source.stats.load(Ordering::SeqCst), 4);
            let source = ChangingSource {
                stats: AtomicUsize::new(0),
                unstable_stats: 1,
                native,
            };
            assert!(matches!(
                materialize(&source, &id, Some(&Revision::new("0")), 4),
                Err(Error::Conflict { .. })
            ));
            assert_eq!(source.stats.load(Ordering::SeqCst), 2);
            let source = ChangingSource {
                stats: AtomicUsize::new(0),
                unstable_stats: usize::MAX,
                native,
            };
            assert!(matches!(
                materialize(&source, &id, None, 4),
                Err(Error::Conflict { .. })
            ));
            assert_eq!(source.stats.load(Ordering::SeqCst), 6);
            let source = ChangingSource {
                stats: AtomicUsize::new(0),
                unstable_stats: 0,
                native,
            };
            assert!(matches!(
                materialize(&source, &id, None, 3),
                Err(Error::TooLarge)
            ));
        }
    }

    #[test]
    fn drawings_use_advertised_svg_or_pdf_but_forms_and_sites_do_not() {
        let mut drawing = NativeDocument {
            mime: "application/vnd.google-apps.drawing".into(),
            link_extension: "gdraw".into(),
            web_url: None,
            exports: vec![ExportFormat {
                mime: "application/pdf".into(),
                extension: "wrong".into(),
            }],
        };
        assert_eq!(preferred_export(&drawing).unwrap().extension, "pdf");
        drawing.exports.push(ExportFormat {
            mime: "image/svg+xml".into(),
            extension: "wrong".into(),
        });
        assert_eq!(preferred_export(&drawing).unwrap().extension, "svg");
        for mime in [
            "application/vnd.google-apps.form",
            "application/vnd.google-apps.site",
        ] {
            drawing.mime = mime.into();
            assert!(matches!(
                preferred_export(&drawing),
                Err(Error::Unsupported(_))
            ));
        }
    }

    #[test]
    fn selects_only_advertised_office_mime_and_canonical_extension() {
        for (kind, subtype, extension) in [
            ("document", "wordprocessingml.document", "docx"),
            ("spreadsheet", "spreadsheetml.sheet", "xlsx"),
            ("presentation", "presentationml.presentation", "pptx"),
        ] {
            let mut document = NativeDocument {
                mime: format!("application/vnd.google-apps.{kind}"),
                link_extension: "link".into(),
                web_url: None,
                exports: vec![],
            };
            assert!(matches!(
                preferred_export(&document),
                Err(Error::Unsupported(_))
            ));
            document.exports.push(ExportFormat {
                mime: format!("application/vnd.openxmlformats-officedocument.{subtype}"),
                extension: "wrong".into(),
            });
            assert_eq!(preferred_export(&document).unwrap().extension, extension);
            document.mime = "application/vnd.google-apps.drawing".into();
            assert!(matches!(
                preferred_export(&document),
                Err(Error::Unsupported(_))
            ));
        }
    }
}

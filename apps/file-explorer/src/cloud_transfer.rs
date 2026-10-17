//! Maps platform transfer receipts into the unchanged Files document schema.
use crate::file_ops::{Change, Outcome};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use tilo_cloud_files::cloud::ImportEntry;
use tilo_cloud_files::{TransferOutcome, TransferReceipt};
use ui::tilo_resource::Settled;

pub use tilo_cloud_files::{DestinationIdentity, TransferProgress, pin_destination, valid_name};

fn change(receipt: TransferReceipt) -> Change {
    match receipt {
        TransferReceipt::CloudImported {
            from,
            to,
            destination,
            output,
        } => Change::CloudImported {
            from,
            to,
            destination,
            output,
        },
        TransferReceipt::CloudTrashed {
            to,
            entry,
            destination,
            output,
        } => Change::CloudTrashed {
            to,
            entry,
            destination,
            output,
        },
    }
}

fn outcome(result: TransferOutcome) -> Outcome {
    match result {
        TransferOutcome::Done(receipts) => {
            Settled::Done(receipts.into_iter().map(change).collect())
        }
        TransferOutcome::Failed { reason, partial } => Settled::Failed {
            reason,
            partial: partial.into_iter().map(change).collect(),
        },
    }
}

pub(crate) fn compensate(
    to: &str,
    destination: &DestinationIdentity,
    output: &DestinationIdentity,
) -> Result<Change, String> {
    tilo_cloud_files::compensate(to, destination, output, crate::fileops::move_into).map(change)
}

pub(crate) fn restore(
    to: &str,
    entry: &str,
    destination: &DestinationIdentity,
    output: &DestinationIdentity,
) -> Result<Change, String> {
    tilo_cloud_files::restore(to, entry, destination, output, crate::fileops::move_into).map(change)
}

// Reload at execution, not admission: an account may have become a sync mount.
fn mounts() -> Vec<PathBuf> {
    accounts::config_root()
        .map(|config| {
            crate::remote_accounts::load_in(&config, crate::model::user_home().as_deref())
                .into_iter()
                .filter_map(|account| account.mount)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn execute(
    consent: tilo_cloud_files::ImportConsent,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
) -> Outcome {
    outcome(tilo_cloud_files::execute(
        consent,
        cancel,
        progress,
        &mounts(),
    ))
}

#[cfg(test)]
fn execute_at(
    socket: &Path,
    account: &str,
    entries: &[ImportEntry],
    folder: &str,
    destination: &DestinationIdentity,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
) -> Outcome {
    let mut issuer = tilo_cloud_files::ImportAuthority::default();
    issuer
        .approve("test", account, entries, folder, destination, "test-device")
        .unwrap();
    let consent = issuer
        .consume("test", account, entries, folder, destination, "test-device")
        .unwrap();
    outcome(tilo_cloud_files::execute_at(
        socket,
        consent,
        cancel,
        progress,
        &mounts(),
    ))
}

/// Retry only top-level entries without a committed receipt. Descendant
/// progress never means that a partially staged root was published.
pub(crate) fn retain_uncommitted(
    account: &str,
    entries: &mut Vec<ImportEntry>,
    changes: &[Change],
) {
    entries.retain(|entry| {
        !changes.iter().any(|change| {
            matches!(change,
        Change::CloudImported { from, .. } if *from == format!("cloud:{account}:{}", entry.id))
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use tilo_cloud_files::cloud::{
        CLOUD_FRAMING, CloudRequest, CloudResponse, ImportError, ImportErrorKind,
    };
    use tilo_cloud_files::cloud::{ImportKind, ImportReadState, ImportReply, ImportRequest};

    fn entry(name: &str, kind: ImportKind) -> ImportEntry {
        ImportEntry {
            id: name.into(),
            name: name.into(),
            kind,
            revision: Some("revision".into()),
            size: Some(999),
            content_writable: false,
        }
    }

    fn server(
        root: &Path,
        mut reply: impl FnMut(ImportRequest) -> ImportReply + Send + 'static,
    ) -> (PathBuf, std::thread::JoinHandle<()>) {
        let socket = root.join(format!("cloud-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            while let Some(request) = CLOUD_FRAMING
                .read_value::<_, CloudRequest>(&mut stream)
                .unwrap()
            {
                let CloudRequest::Import { account, request } = request else {
                    panic!("not an import")
                };
                assert_eq!(account, "account");
                CLOUD_FRAMING
                    .write_value(&mut stream, &CloudResponse::Import(reply(request)))
                    .unwrap();
            }
        });
        (socket, handle)
    }

    fn bytes_reply(request: ImportRequest, closed: &Mutex<Vec<String>>) -> ImportReply {
        match request {
            ImportRequest::StartRead { id, revision } => {
                assert_eq!(revision.as_deref(), Some("revision"));
                ImportReply::Preparing { session: id }
            }
            ImportRequest::ReadStatus { .. } => {
                ImportReply::ReadState(ImportReadState::Ready { size: 6 })
            }
            ImportRequest::ReadChunk { offset, length, .. } => {
                assert_eq!(offset, 0);
                assert_eq!(length, 6);
                ImportReply::Chunk {
                    bytes: b"export".to_vec(),
                    eof: true,
                }
            }
            ImportRequest::CloseRead { session } => {
                closed.lock().unwrap().push(session);
                ImportReply::Closed
            }
            ImportRequest::Browse { .. } => panic!("unexpected browse request"),
        }
    }

    #[test]
    fn native_bytes_and_files_are_independent_copies_with_collision_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("report.pdf"), b"original").unwrap();
        // A pre-existing symlink must be treated as a collision, never followed.
        std::os::unix::fs::symlink(temp.path().join("outside"), dest.join("file")).unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &observed));
        let status = Mutex::new(TransferProgress::default());
        let outcome = execute_at(
            &socket,
            "account",
            &[
                entry("report.pdf", ImportKind::NativeExport),
                entry("file", ImportKind::File),
            ],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |progress| *status.lock().unwrap() = progress,
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 2);
        assert!(
            matches!(&outcome.result()[0], Change::CloudImported { to, .. } if to.ends_with("report (1).pdf"))
        );
        assert_eq!(
            std::fs::read(dest.join("report (1).pdf")).unwrap(),
            b"export"
        );
        assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"export");
        assert_eq!(std::fs::read(dest.join("report.pdf")).unwrap(), b"original");
        assert!(!temp.path().join("outside").exists());
        assert_eq!(closed.lock().unwrap().len(), 2);
        assert_eq!(status.lock().unwrap().bytes, 12);
        assert_eq!(status.lock().unwrap().completed, 2);
        let undone = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "import".into(),
            },
            Some(outcome.result()),
        );
        assert!(undone.is_done(), "{undone:?}");
        assert!(!dest.join("report (1).pdf").exists());
        assert!(!dest.join("file (1)").exists());
        let redone = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "undo-import".into(),
            },
            Some(undone.result()),
        );
        assert!(redone.is_done(), "{redone:?}");
        assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"export");
        assert!(
            redone
                .result()
                .iter()
                .all(|change| matches!(change, Change::CloudImported { .. }))
        );
        std::fs::rename(dest.join("file (1)"), dest.join("saved")).unwrap();
        std::fs::write(dest.join("file (1)"), b"unrelated").unwrap();
        let refused = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "redo-import".into(),
            },
            Some(redone.result()),
        );
        assert!(!refused.is_done(), "{refused:?}");
        assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"unrelated");
        assert_eq!(std::fs::read(dest.join("report.pdf")).unwrap(), b"original");
    }

    #[test]
    fn native_docx_collision_preserves_extension_and_office_payload() {
        use std::io::{Read as _, Write as _};
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("Essay.docx"), b"original").unwrap();
        let document = br#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Independent essay</w:t></w:r></w:p></w:body></w:document>"#;
        let mut office = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, contents) in [
            ("[Content_Types].xml", br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.as_slice()),
            ("_rels/.rels", br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.as_slice()),
            ("word/document.xml", document.as_slice()),
        ] {
            office.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            office.write_all(contents).unwrap();
        }
        let bytes = office.finish().unwrap().into_inner();
        let exported = bytes.clone();
        let (socket, server) = server(temp.path(), move |request| match request {
            ImportRequest::StartRead { id, .. } => ImportReply::Preparing { session: id },
            ImportRequest::ReadStatus { .. } => ImportReply::ReadState(ImportReadState::Ready {
                size: exported.len() as u64,
            }),
            ImportRequest::ReadChunk { offset, length, .. } => {
                assert_eq!(offset, 0);
                assert_eq!(length as usize, exported.len());
                ImportReply::Chunk {
                    bytes: exported.clone(),
                    eof: true,
                }
            }
            ImportRequest::CloseRead { .. } => ImportReply::Closed,
            ImportRequest::Browse { .. } => panic!("unexpected browse"),
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("Essay.docx", ImportKind::NativeExport)],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 1);
        let path = dest.join("Essay (1).docx");
        assert!(
            matches!(&outcome.result()[0], Change::CloudImported { to, .. } if Path::new(to) == path)
        );
        assert_eq!(path.extension().unwrap(), "docx");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut xml = Vec::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_end(&mut xml)
            .unwrap();
        assert_eq!(xml, document);
        assert_eq!(std::fs::read(dest.join("Essay.docx")).unwrap(), b"original");
    }

    #[test]
    fn full_length_unicode_collisions_create_fresh_names_with_extensions() {
        for (name, expected, kind) in [
            (
                format!("{}🦀.docx", "界".repeat(82)),
                format!("{} (2).docx", "界".repeat(82)),
                ImportKind::NativeExport,
            ),
            (
                format!("{}é.pdf", "界".repeat(83)),
                format!("{} (2).pdf", "界".repeat(82)),
                ImportKind::File,
            ),
            (
                "界".repeat(85),
                format!("{} (2)", "界".repeat(83)),
                ImportKind::Folder,
            ),
            (".hidden".into(), ".hidden (2)".into(), ImportKind::File),
            (
                "folder.with.dots".into(),
                "folder.with.dots (2)".into(),
                ImportKind::Folder,
            ),
        ] {
            if !name.is_ascii() {
                assert_eq!(name.len(), 255);
            }
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            std::fs::write(dest.join(&name), b"original").unwrap();
            let first = expected.replace(" (2)", " (1)");
            std::fs::write(dest.join(&first), b"first collision").unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) = server(temp.path(), move |request| match request {
                ImportRequest::Browse { folder, .. } => ImportReply::Page {
                    folder: folder.unwrap(),
                    entries: Vec::new(),
                    next_offset: None,
                },
                request => bytes_reply(request, &closed),
            });
            let outcome = execute_at(
                &socket,
                "account",
                &[entry(&name, kind.clone())],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &AtomicBool::new(false),
                |_| {},
            );
            server.join().unwrap();
            assert!(outcome.is_done(), "{outcome:?}");
            assert_eq!(outcome.result().len(), 1);
            assert!(
                matches!(&outcome.result()[0], Change::CloudImported { to, .. } if Path::new(to) == dest.join(&expected))
            );
            assert!(expected.len() <= 255);
            if kind == ImportKind::Folder {
                assert!(dest.join(&expected).is_dir());
            } else {
                assert_eq!(std::fs::read(dest.join(&expected)).unwrap(), b"export");
            }
            assert_eq!(std::fs::read(dest.join(&name)).unwrap(), b"original");
            assert_eq!(
                std::fs::read(dest.join(&first)).unwrap(),
                b"first collision"
            );
            assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 3);
        }
    }

    #[test]
    fn compensation_refuses_replaced_outputs_and_renamed_or_replaced_destinations() {
        for replacement in ["output", "destination", "renamed", "symlink"] {
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) =
                server(temp.path(), move |request| bytes_reply(request, &closed));
            let outcome = execute_at(
                &socket,
                "account",
                &[entry("file", ImportKind::File)],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &AtomicBool::new(false),
                |_| {},
            );
            server.join().unwrap();
            assert!(outcome.is_done(), "{outcome:?}");
            // Exercise the serialized document receipt, not an in-memory helper.
            let changes: Vec<Change> =
                serde_json::from_slice(&serde_json::to_vec(outcome.result()).unwrap()).unwrap();
            match replacement {
                "output" => {
                    std::fs::rename(dest.join("file"), dest.join("original")).unwrap();
                    std::fs::write(dest.join("file"), b"unrelated").unwrap();
                }
                "symlink" => {
                    std::fs::rename(dest.join("file"), dest.join("original")).unwrap();
                    std::fs::write(temp.path().join("unrelated"), b"unrelated").unwrap();
                    std::os::unix::fs::symlink(temp.path().join("unrelated"), dest.join("file"))
                        .unwrap();
                }
                _ => {
                    std::fs::rename(&dest, temp.path().join("original")).unwrap();
                    if replacement == "destination" {
                        std::fs::create_dir(&dest).unwrap();
                        std::fs::write(dest.join("file"), b"unrelated").unwrap();
                    }
                }
            }
            let undone = crate::file_ops::execute(
                &crate::file_ops::FileOp::Revert {
                    op: "import".into(),
                },
                Some(&changes),
            );
            assert!(!undone.is_done(), "{replacement}: {undone:?}");
            assert!(undone.result().is_empty());
            if replacement == "output" || replacement == "destination" || replacement == "symlink" {
                assert_eq!(std::fs::read(dest.join("file")).unwrap(), b"unrelated");
            }
            let original = if replacement == "output" || replacement == "symlink" {
                dest.join("original")
            } else {
                temp.path().join("original/file")
            };
            assert_eq!(std::fs::read(original).unwrap(), b"export");
        }
    }

    #[test]
    fn compensation_preserves_user_edits_to_the_imported_inode() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &closed));
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("file", ImportKind::File)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 1);
        let changes: Vec<Change> =
            serde_json::from_slice(&serde_json::to_vec(outcome.result()).unwrap()).unwrap();
        let path = temp.path().join("file");
        let before = std::fs::metadata(&path).unwrap();
        std::fs::write(&path, b"independent user edits").unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        let undone = crate::file_ops::execute(
            &crate::file_ops::FileOp::Revert {
                op: "import".into(),
            },
            Some(&changes),
        );
        assert!(!undone.is_done(), "{undone:?}");
        assert!(undone.result().is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"independent user edits");
    }

    #[test]
    fn cancellation_removes_partial_bytes_and_closes_the_session() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &observed));
        let cancel = AtomicBool::new(false);
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("partial", ImportKind::File)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &cancel,
            |progress| {
                if progress.bytes != 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert!(!temp.path().join("partial").exists());
        assert_eq!(*closed.lock().unwrap(), ["partial"]);
    }

    #[test]
    fn provider_failure_keeps_previous_copy_but_cleans_failed_entry() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| {
            if matches!(&request, ImportRequest::ReadStatus { session } if session == "bad") {
                return ImportReply::ReadState(ImportReadState::Failed(ImportError {
                    kind: ImportErrorKind::SourceChanged,
                    message: "source changed".into(),
                }));
            }
            bytes_reply(request, &observed)
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[
                entry("good", ImportKind::File),
                entry("bad", ImportKind::File),
            ],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert_eq!(outcome.result().len(), 1);
        assert_eq!(std::fs::read(temp.path().join("good")).unwrap(), b"export");
        assert!(!temp.path().join("bad").exists());
        assert_eq!(*closed.lock().unwrap(), ["good", "bad"]);
        assert_eq!(outcome.reason(), Some("bad: source changed"));
    }

    #[test]
    fn folders_are_paginated_and_recursive() {
        let temp = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let observed = closed.clone();
        let (socket, server) = server(temp.path(), move |request| match request {
            ImportRequest::Browse { folder, offset } => {
                let folder = folder.unwrap();
                let (entries, next_offset) = match (folder.as_str(), offset) {
                    ("folder", 0) => (vec![entry("nested", ImportKind::Folder)], Some(1)),
                    ("folder", 1) => (vec![entry("second", ImportKind::File)], None),
                    ("nested", 0) => (vec![entry("first", ImportKind::NativeExport)], None),
                    _ => panic!("unexpected page"),
                };
                ImportReply::Page {
                    folder,
                    entries,
                    next_offset,
                }
            }
            request => bytes_reply(request, &observed),
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("folder", ImportKind::Folder)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert_eq!(outcome.result().len(), 1);
        assert_eq!(
            std::fs::read(temp.path().join("folder/nested/first")).unwrap(),
            b"export"
        );
        assert_eq!(
            std::fs::read(temp.path().join("folder/second")).unwrap(),
            b"export"
        );
        assert_eq!(closed.lock().unwrap().len(), 2);
    }

    #[test]
    fn invalid_remote_names_remove_the_incomplete_folder() {
        let temp = tempfile::tempdir().unwrap();
        let (socket, server) = server(temp.path(), |request| match request {
            ImportRequest::Browse { .. } => ImportReply::Page {
                folder: "folder".into(),
                entries: vec![entry("../escape", ImportKind::File)],
                next_offset: None,
            },
            _ => panic!("malformed name must not start a read"),
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("folder", ImportKind::Folder)],
            temp.path().to_str().unwrap(),
            &pin_destination(temp.path().to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert!(!temp.path().join("folder").exists());
    }

    #[test]
    fn queued_import_rejects_replaced_destination_before_provider_access() {
        for replace_parent in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let parent = temp.path().join("parent");
            let dest = parent.join("dest");
            std::fs::create_dir_all(&dest).unwrap();
            let pinned = pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap();
            let replaced = if replace_parent { &parent } else { &dest };
            std::fs::rename(replaced, temp.path().join("original")).unwrap();
            std::fs::create_dir_all(&dest).unwrap();
            let outcome = execute_at(
                &temp.path().join("absent.sock"),
                "account",
                &[entry("file", ImportKind::File)],
                dest.to_str().unwrap(),
                &pinned,
                &AtomicBool::new(false),
                |_| {},
            );
            let Settled::Failed { reason, partial } = outcome else {
                panic!("replaced destination was accepted");
            };
            assert!(reason.contains("changed since admission"), "{reason}");
            assert!(partial.is_empty());
            assert!(!dest.join("file").exists());
        }
    }

    #[test]
    fn provider_failure_never_deletes_a_public_name_created_during_io() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let receiving = dest.clone();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| {
            if matches!(request, ImportRequest::ReadChunk { .. }) {
                assert!(
                    !receiving.join("file").exists(),
                    "incomplete bytes must be private"
                );
                std::fs::create_dir(receiving.join("file")).unwrap();
                std::fs::write(receiving.join("file/user"), b"user data").unwrap();
                return ImportReply::Error(ImportError {
                    kind: ImportErrorKind::SourceChanged,
                    message: "provider failed".into(),
                });
            }
            bytes_reply(request, &closed)
        });
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("file", ImportKind::File)],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert_eq!(std::fs::read(dest.join("file/user")).unwrap(), b"user data");
        assert_eq!(std::fs::read_dir(dest).unwrap().count(), 1);
    }

    #[test]
    fn failed_descendant_keeps_whole_root_private_and_prior_receipts_committed() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| match request {
            ImportRequest::Browse { folder, .. } => ImportReply::Page {
                folder: folder.unwrap(),
                entries: vec![
                    entry("copied-child", ImportKind::File),
                    entry("broken-child", ImportKind::File),
                ],
                next_offset: None,
            },
            ImportRequest::ReadStatus { session } if session == "broken-child" => {
                ImportReply::ReadState(ImportReadState::Failed(ImportError {
                    kind: ImportErrorKind::SourceChanged,
                    message: "provider failed".into(),
                }))
            }
            request => bytes_reply(request, &closed),
        });
        let entries = [
            entry("good", ImportKind::File),
            entry("folder", ImportKind::Folder),
        ];
        let outcome = execute_at(
            &socket,
            "account",
            &entries,
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(!outcome.is_done());
        assert!(outcome.reason().unwrap().contains("broken-child"));
        assert_eq!(
            outcome.reason(),
            Some("folder: broken-child: provider failed")
        );
        assert_eq!(outcome.result().len(), 1);
        assert_eq!(std::fs::read(dest.join("good")).unwrap(), b"export");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
        // The retry surface filters committed top-level IDs, not descendant
        // progress. Thus a partially copied folder restarts exactly once.
        let mut remaining = entries.to_vec();
        retain_uncommitted("account", &mut remaining, outcome.result());
        assert_eq!(remaining.len(), 1);
        let closed = Mutex::new(Vec::new());
        let (socket, server) = self::server(temp.path(), move |request| match request {
            ImportRequest::Browse { folder, .. } => ImportReply::Page {
                folder: folder.unwrap(),
                entries: vec![entry("copied-child", ImportKind::File)],
                next_offset: None,
            },
            request => bytes_reply(request, &closed),
        });
        let retry = execute_at(
            &socket,
            "account",
            &remaining,
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        );
        server.join().unwrap();
        assert!(retry.is_done(), "{retry:?}");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 2);
        assert_eq!(
            std::fs::read(dest.join("folder/copied-child")).unwrap(),
            b"export"
        );
        assert!(!dest.join("good (1)").exists());
    }

    #[test]
    fn public_replacements_during_copy_are_never_cleaned_or_overwritten() {
        for cancel_copy in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) =
                server(temp.path(), move |request| bytes_reply(request, &closed));
            let cancel = AtomicBool::new(false);
            let touched = AtomicBool::new(false);
            let outcome = execute_at(
                &socket,
                "account",
                &[entry("file", ImportKind::File)],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &cancel,
                |status| {
                    if status.bytes == 6 && !touched.swap(true, Ordering::Relaxed) {
                        assert!(!dest.join("file").exists());
                        std::fs::write(dest.join("file"), b"user replacement").unwrap();
                        cancel.store(cancel_copy, Ordering::Relaxed);
                    }
                },
            );
            server.join().unwrap();
            assert_eq!(
                std::fs::read(dest.join("file")).unwrap(),
                b"user replacement"
            );
            assert_eq!(outcome.is_done(), !cancel_copy);
            if cancel_copy {
                assert!(outcome.result().is_empty());
                assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
            } else {
                assert_eq!(std::fs::read(dest.join("file (1)")).unwrap(), b"export");
                assert_eq!(outcome.result().len(), 1);
            }
        }
    }

    #[test]
    fn tampered_private_stages_are_retained_without_deleting_user_objects() {
        for tamper in ["container", "file", "folder-child"] {
            let temp = tempfile::tempdir().unwrap();
            let dest = temp.path().join("dest");
            std::fs::create_dir(&dest).unwrap();
            let closed = Mutex::new(Vec::new());
            let (socket, server) = server(temp.path(), move |request| match request {
                ImportRequest::Browse { folder, .. } => ImportReply::Page {
                    folder: folder.unwrap(),
                    entries: vec![entry("file", ImportKind::File)],
                    next_offset: None,
                },
                request => bytes_reply(request, &closed),
            });
            let cancel = AtomicBool::new(false);
            let touched = AtomicBool::new(false);
            let outcome = execute_at(
                &socket,
                "account",
                &[entry(
                    if tamper == "folder-child" {
                        "folder"
                    } else {
                        "file"
                    },
                    if tamper == "folder-child" {
                        ImportKind::Folder
                    } else {
                        ImportKind::File
                    },
                )],
                dest.to_str().unwrap(),
                &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
                &cancel,
                |status| {
                    if status.bytes == 6 && !touched.swap(true, Ordering::Relaxed) {
                        let stage = std::fs::read_dir(&dest)
                            .unwrap()
                            .next()
                            .unwrap()
                            .unwrap()
                            .path();
                        assert!(
                            stage
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".cloud-import-")
                        );
                        match tamper {
                            "container" => {
                                std::fs::rename(&stage, dest.join("saved-stage")).unwrap();
                                std::fs::create_dir(&stage).unwrap();
                                std::fs::write(stage.join("user"), b"user data").unwrap();
                            }
                            "file" => {
                                std::fs::rename(stage.join("file"), stage.join("saved-file"))
                                    .unwrap();
                                std::fs::write(stage.join("file"), b"user data").unwrap();
                            }
                            _ => std::fs::write(stage.join("folder/user"), b"user data").unwrap(),
                        }
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            );
            server.join().unwrap();
            assert!(touched.load(Ordering::Relaxed));
            assert!(!outcome.is_done());
            assert!(outcome.result().is_empty());
            assert!(
                outcome
                    .reason()
                    .unwrap()
                    .contains("private staging retained"),
                "{outcome:?}"
            );
            let stage = std::fs::read_dir(&dest)
                .unwrap()
                .map(Result::unwrap)
                .find(|item| {
                    item.file_name()
                        .to_string_lossy()
                        .starts_with(".cloud-import-")
                })
                .unwrap()
                .path();
            let user = match tamper {
                "container" => stage.join("user"),
                "file" => stage.join("file"),
                _ => stage.join("folder/user"),
            };
            assert_eq!(std::fs::read(user).unwrap(), b"user data");
            assert!(!dest.join("file").exists());
            assert!(!dest.join("folder").exists());
        }
    }

    #[test]
    fn path_alias_introduced_during_io_cannot_receive_output_or_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("dest");
        let original = temp.path().join("original");
        let unrelated = temp.path().join("unrelated");
        std::fs::create_dir(&dest).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("file"), b"user data").unwrap();
        let closed = Mutex::new(Vec::new());
        let (socket, server) = server(temp.path(), move |request| bytes_reply(request, &closed));
        let changed = AtomicBool::new(false);
        let outcome = execute_at(
            &socket,
            "account",
            &[entry("file", ImportKind::File)],
            dest.to_str().unwrap(),
            &pin_destination(dest.to_str().unwrap(), std::iter::empty()).unwrap(),
            &AtomicBool::new(false),
            |status| {
                if status.bytes == 6 && !changed.swap(true, Ordering::Relaxed) {
                    std::fs::rename(&dest, &original).unwrap();
                    std::os::unix::fs::symlink(&unrelated, &dest).unwrap();
                }
            },
        );
        server.join().unwrap();
        assert!(changed.load(Ordering::Relaxed));
        assert!(!outcome.is_done());
        assert!(outcome.result().is_empty());
        assert_eq!(std::fs::read(unrelated.join("file")).unwrap(), b"user data");
        assert_eq!(std::fs::read_dir(original).unwrap().count(), 0);
    }

    #[test]
    fn admission_requires_existing_symlink_free_directory() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, b"original").unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        for rejected in [
            file,
            alias,
            temp.path().join("missing"),
            temp.path().join(".."),
        ] {
            assert!(pin_destination(rejected.to_str().unwrap(), std::iter::empty()).is_err());
        }
    }

    #[test]
    fn execution_rechecks_current_sync_mounts() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().to_str().unwrap();
        let pinned = pin_destination(folder, std::iter::empty()).unwrap();
        assert!(pin_destination(folder, std::iter::once(temp.path())).is_err());
        assert_eq!(pin_destination(folder, std::iter::empty()).unwrap(), pinned);
    }

    #[test]
    fn destination_components_cannot_be_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", temp.path().join("link")).unwrap();
        assert!(
            pin_destination(
                temp.path().join("link").to_str().unwrap(),
                std::iter::empty()
            )
            .is_err()
        );
    }
}

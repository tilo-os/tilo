//! Export cache checks through the volume's production inode operations.

use super::*;
use cloud_source::{
    Capabilities, Download, Error, ExportFormat, Item, ItemId, ItemKind, NativeDocument, Revision,
    UploadSession, UploadTarget,
};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

struct NativeSource {
    revision: AtomicU64,
    sheet: AtomicBool,
    exports: AtomicUsize,
    incomplete: AtomicBool,
    export_failure: AtomicUsize,
}

impl NativeSource {
    fn ordinary(&self) -> Item {
        let mut item = self.item();
        item.id = ItemId::new("ordinary");
        item.name = "ordinary.txt".into();
        item.kind = ItemKind::File;
        item.size = Some(8);
        item
    }
    fn item(&self) -> Item {
        let sheet = self.sheet.load(Ordering::SeqCst);
        let (kind, subtype, extension) = if sheet {
            ("spreadsheet", "spreadsheetml.sheet", "xlsx")
        } else {
            ("document", "wordprocessingml.document", "docx")
        };
        Item {
            id: ItemId::new("native"),
            parents: vec![self.root()],
            name: "Office".into(),
            kind: ItemKind::Native(NativeDocument {
                mime: format!("application/vnd.google-apps.{kind}"),
                link_extension: "link".into(),
                web_url: None,
                exports: vec![ExportFormat {
                    mime: format!("application/vnd.openxmlformats-officedocument.{subtype}"),
                    extension: extension.into(),
                }],
            }),
            size: Some(999_999),
            modified: None,
            revision: Some(Revision::new(
                self.revision.load(Ordering::SeqCst).to_string(),
            )),
            hashes: vec![],
            mime: None,
        }
    }
}

impl CloudSource for NativeSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities::webdav()
    }
    fn root(&self) -> ItemId {
        ItemId::new("root")
    }
    fn stat(&self, id: &ItemId) -> cloud_source::Result<Item> {
        if id == &self.root() {
            let mut root = self.item();
            root.id = self.root();
            root.kind = ItemKind::Folder;
            root.parents.clear();
            root.revision = None;
            Ok(root)
        } else if id.0 == "ordinary" {
            Ok(self.ordinary())
        } else {
            Ok(self.item())
        }
    }
    fn list(&self, _: &ItemId) -> cloud_source::Result<Vec<Item>> {
        Ok(vec![self.item(), self.ordinary()])
    }
    fn read(
        &self,
        id: &ItemId,
        range: Option<Range<u64>>,
        _: Option<&Revision>,
    ) -> cloud_source::Result<Download> {
        if id.0 != "ordinary" {
            return Err(Error::Unsupported("native content must use export"));
        }
        let range = range.unwrap_or(0..8);
        let start =
            usize::try_from(range.start).map_err(|_| Error::InvalidResponse("test range"))?;
        let end =
            usize::try_from(range.end.min(8)).map_err(|_| Error::InvalidResponse("test range"))?;
        let bytes = b"ordinary"
            .get(start..end)
            .ok_or(Error::InvalidResponse("test range"))?
            .to_vec();
        Ok(Download {
            body: Box::new(std::io::Cursor::new(bytes)),
            range,
            total: Some(8),
            revision: Some(Revision::new(
                self.revision.load(Ordering::SeqCst).to_string(),
            )),
        })
    }
    fn export(&self, _: &ItemId, format: &ExportFormat) -> cloud_source::Result<Download> {
        self.exports.fetch_add(1, Ordering::SeqCst);
        match self.export_failure.load(Ordering::SeqCst) {
            1 => return Err(Error::TooLarge),
            2 => return Err(Error::Forbidden),
            3 => return Err(Error::Offline),
            _ => {}
        }
        let bytes = if self.sheet.load(Ordering::SeqCst) {
            OFFICE_XLSX
        } else {
            OFFICE_DOCX
        };
        let expected = if self.sheet.load(Ordering::SeqCst) {
            "xlsx"
        } else {
            "docx"
        };
        if format.extension != expected {
            return Err(Error::InvalidResponse("incorrect Office export format"));
        }
        Ok(Download {
            body: Box::new(std::io::Cursor::new(bytes)),
            range: 0..bytes.len() as u64,
            total: Some(
                (bytes.len() as u64)
                    .saturating_add(u64::from(self.incomplete.load(Ordering::SeqCst))),
            ),
            revision: None,
        })
    }
    fn begin_upload(
        &self,
        _: UploadTarget,
        _: u64,
    ) -> cloud_source::Result<Box<dyn UploadSession>> {
        Err(Error::Unsupported("native upload"))
    }
    fn create_folder(&self, _: &ItemId, _: &str) -> cloud_source::Result<Item> {
        Err(Error::Unsupported("test folder"))
    }
    fn move_item(
        &self,
        _: &ItemId,
        _: &ItemId,
        _: &str,
        _: Option<&Revision>,
    ) -> cloud_source::Result<Item> {
        Err(Error::Unsupported("test move"))
    }
    fn delete(&self, _: &ItemId, _: Option<&Revision>) -> cloud_source::Result<()> {
        Err(Error::Unsupported("test deletion"))
    }
}

#[test]
fn describe_reports_cached_source_metadata_without_hydrating_office_files() {
    use cloud_source_gdrive::{
        GDrive,
        fake::{FakeDrive, SOURCE_TOKEN},
    };
    use runtime_fs::cloud::{FileDescription, FileReply, FileRequest};

    let server = FakeDrive::start();
    let folder = server.put_native(
        &server.root_id(),
        "Work",
        "application/vnd.google-apps.folder",
    );
    for kind in ["document", "spreadsheet", "presentation", "form"] {
        server.put_native(
            &folder,
            kind,
            &format!("application/vnd.google-apps.{kind}"),
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let source = GDrive::new(server.config(), bearer(SOURCE_TOKEN)).unwrap();
    let volume = oauth_volume(Arc::new(source), dir.path());
    volume.sync_once().unwrap();
    write_file(&volume, "uploaded.docx", OFFICE_DOCX);
    volume.upload_pending().unwrap();
    volume.sync_once().unwrap();
    // Reopen with persisted metadata but without a credential the provider accepts.
    drop(volume);
    let source = GDrive::new(server.config(), bearer("invalid-example-token")).unwrap();
    let volume = oauth_volume(Arc::new(source), dir.path());
    server.set_offline(true);
    let before = server.stats();
    for (kind, extension) in [
        ("document", "docx"),
        ("spreadsheet", "xlsx"),
        ("presentation", "pptx"),
    ] {
        assert_eq!(
            volume.answer(&FileRequest::Describe {
                path: format!("Work/{kind}.{extension}"),
            }),
            Ok(FileReply::Description(FileDescription {
                source_mime: Some(format!("application/vnd.google-apps.{kind}")),
                converted: true,
                content_writable: false,
            }))
        );
    }
    assert_eq!(
        volume.answer(&FileRequest::Describe {
            path: "uploaded.docx".into(),
        }),
        Ok(FileReply::Description(FileDescription {
            source_mime: Some("application/octet-stream".into()),
            converted: false,
            content_writable: true,
        }))
    );
    assert_eq!(
        volume.answer(&FileRequest::Describe {
            path: "Work/form".into(),
        }),
        Ok(FileReply::Description(FileDescription {
            source_mime: Some("application/vnd.google-apps.form".into()),
            converted: false,
            content_writable: false,
        }))
    );
    for path in [
        "../uploaded.docx",
        "Work/../uploaded.docx",
        "Work/./document.docx",
        "Work/document.docx/child",
        "missing.docx",
    ] {
        assert!(
            volume
                .answer(&FileRequest::Describe { path: path.into() })
                .is_err()
        );
    }
    assert_eq!(server.stats(), before);
}

#[test]
fn unsupported_native_items_do_not_hide_ordinary_siblings_or_fabricate_links() {
    use cloud_source_gdrive::{
        GDrive,
        fake::{FakeDrive, SOURCE_TOKEN},
    };
    let server = FakeDrive::start();
    server.put_native(
        &server.root_id(),
        "Form",
        "application/vnd.google-apps.form",
    );
    server.put_file(&server.root_id(), "ordinary.txt", b"ordinary bytes");
    let dir = tempfile::tempdir().unwrap();
    let source = GDrive::new(server.config(), bearer(SOURCE_TOKEN)).unwrap();
    let volume = oauth_volume(Arc::new(source), dir.path());
    volume.sync_once().unwrap();
    assert_eq!(names(&volume, ""), ["Form", "ordinary.txt"]);
    assert_eq!(read_all(&volume, "ordinary.txt"), b"ordinary bytes");
    let attr = volume.lookup(ROOT_INO, "Form").unwrap();
    assert_eq!(attr.size, 0);
    assert_eq!(volume.read_ino(attr.ino, 0, 100), Err(libc::EOPNOTSUPP));
}

#[test]
fn incomplete_native_export_cannot_publish_a_size_or_poison_the_cache() {
    let source = Arc::new(NativeSource {
        revision: AtomicU64::new(1),
        sheet: AtomicBool::new(false),
        exports: AtomicUsize::new(0),
        incomplete: AtomicBool::new(true),
        export_failure: AtomicUsize::new(0),
    });
    let dir = tempfile::tempdir().unwrap();
    let volume = oauth_volume(source.clone(), dir.path());
    volume.sync_once().unwrap();
    assert_eq!(volume.lookup(ROOT_INO, "Office.docx"), Err(libc::EIO));
    source.incomplete.store(false, Ordering::SeqCst);
    assert_eq!(read_all(&volume, "Office.docx"), OFFICE_DOCX);
    assert_eq!(source.exports.load(Ordering::SeqCst), 2);
}

#[test]
fn native_size_and_cache_follow_revision_and_export_format_across_restart() {
    let source = Arc::new(NativeSource {
        revision: AtomicU64::new(1),
        sheet: AtomicBool::new(false),
        exports: AtomicUsize::new(0),
        incomplete: AtomicBool::new(false),
        export_failure: AtomicUsize::new(0),
    });
    let dir = tempfile::tempdir().unwrap();
    let volume = oauth_volume(source.clone(), dir.path());
    volume.sync_once().unwrap();
    let ino = volume.resolve("Office.docx").unwrap();
    assert_eq!(volume.getattr(ino).unwrap().size, OFFICE_DOCX.len() as u64);
    assert_eq!(read_all(&volume, "Office.docx"), OFFICE_DOCX);
    assert_eq!(source.exports.load(Ordering::SeqCst), 1);
    volume.shutdown();
    drop(volume);
    let volume = oauth_volume(source.clone(), dir.path());
    assert_eq!(read_all(&volume, "Office.docx"), OFFICE_DOCX);
    assert_eq!(
        source.exports.load(Ordering::SeqCst),
        1,
        "complete export survives restart"
    );
    source.revision.store(2, Ordering::SeqCst);
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "Office.docx"), OFFICE_DOCX);
    assert_eq!(
        source.exports.load(Ordering::SeqCst),
        2,
        "new revision must re-export"
    );
    // A representation change invalidates bytes even without a revision change.
    source.sheet.store(true, Ordering::SeqCst);
    volume.sync_once().unwrap();
    let attr = volume.lookup(ROOT_INO, "Office.xlsx").unwrap();
    assert_eq!(attr.size, OFFICE_XLSX.len() as u64);
    assert_eq!(
        volume
            .read_ino(attr.ino, 0, u32::try_from(attr.size).unwrap())
            .unwrap(),
        OFFICE_XLSX
    );
    assert_eq!(source.exports.load(Ordering::SeqCst), 3);
    assert_eq!(volume.resolve("Office.docx"), Err(libc::ENOENT));
}

#[test]
fn failed_native_exports_do_not_hide_siblings() {
    for (failure, errno) in [(1, libc::EFBIG), (2, libc::EACCES), (3, libc::EIO)] {
        let source = Arc::new(NativeSource {
            revision: AtomicU64::new(1),
            sheet: AtomicBool::new(false),
            exports: AtomicUsize::new(0),
            incomplete: AtomicBool::new(false),
            export_failure: AtomicUsize::new(failure),
        });
        let dir = tempfile::tempdir().unwrap();
        let volume = oauth_volume(source.clone(), dir.path());
        volume.sync_once().unwrap();
        let entries = volume.readdir(ROOT_INO).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["Office.docx", "ordinary.txt"]
        );
        assert_eq!(source.exports.load(Ordering::SeqCst), 0);
        assert_eq!(read_all(&volume, "ordinary.txt"), b"ordinary");
        let ino = entries
            .iter()
            .find(|(name, _)| name == "Office.docx")
            .unwrap()
            .1
            .ino;
        assert_eq!(volume.getattr(ino), Err(errno));
        assert_eq!(volume.read_ino(ino, 0, 100), Err(errno));
    }
}

#[test]
fn kernel_native_conversions_are_read_only_while_uploaded_office_files_are_writable() {
    use cloud_source_gdrive::{
        GDrive,
        fake::{FakeDrive, SOURCE_TOKEN},
    };
    use std::os::unix::fs::PermissionsExt as _;
    tilo_test_env::require_capability!(
        Fuse,
        fuse_available(),
        "requires /dev/fuse and fusermount3"
    );
    let server = FakeDrive::start();
    for kind in ["document", "spreadsheet", "presentation"] {
        server.put_native(
            &server.root_id(),
            kind,
            &format!("application/vnd.google-apps.{kind}"),
        );
    }
    server.put_file(&server.root_id(), "Uploaded.docx", OFFICE_DOCX);
    let cache = tempfile::tempdir().unwrap();
    let mount = tempfile::tempdir().unwrap();
    let volume = oauth_volume(
        Arc::new(GDrive::new(server.config(), bearer(SOURCE_TOKEN)).unwrap()),
        cache.path(),
    );
    volume.sync_once().unwrap();
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::DefaultPermissions,
        fuser::MountOption::NoDev,
        fuser::MountOption::NoSuid,
    ];
    config.acl = fuser::SessionACL::Owner;
    let _session = fuser::spawn_mount(
        fs::cloud::fuse::CloudFs::new(volume.clone()),
        mount.path(),
        &config,
    )
    .unwrap();
    for (kind, suffix, bytes) in [
        ("document", "docx", OFFICE_DOCX),
        ("spreadsheet", "xlsx", OFFICE_XLSX),
        ("presentation", "pptx", OFFICE_PPTX),
    ] {
        let name = format!("{kind}.{suffix}");
        let path = mount.path().join(&name);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::write(&path, b"replacement").is_err());
        let replacement = mount.path().join("replacement.tmp");
        std::fs::write(&replacement, b"replacement").unwrap();
        assert!(std::fs::rename(&replacement, &path).is_err());
        std::fs::remove_file(replacement).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let FileReply::Description(description) = volume
            .answer(&FileRequest::Describe { path: name })
            .unwrap()
        else {
            panic!("expected mounted metadata")
        };
        assert_eq!(
            description.source_mime.as_deref(),
            Some(format!("application/vnd.google-apps.{kind}").as_str())
        );
        assert!(description.converted);
        assert!(!description.content_writable);
    }
    std::fs::write(
        mount.path().join("Uploaded.docx"),
        b"editable uploaded file",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(mount.path().join("Uploaded.docx")).unwrap(),
        b"editable uploaded file"
    );
}

#[test]
fn drawings_export_as_actual_svg() {
    use cloud_source_gdrive::{
        GDrive,
        fake::{FakeDrive, SOURCE_TOKEN},
    };
    let server = FakeDrive::start();
    server.put_native(
        &server.root_id(),
        "Drawing",
        "application/vnd.google-apps.drawing",
    );
    let dir = tempfile::tempdir().unwrap();
    let source = GDrive::new(server.config(), bearer(SOURCE_TOKEN)).unwrap();
    let volume = oauth_volume(Arc::new(source), dir.path());
    volume.sync_once().unwrap();
    assert_eq!(names(&volume, ""), ["Drawing.svg"]);
    let bytes = read_all(&volume, "Drawing.svg");
    let svg = std::str::from_utf8(&bytes).unwrap();
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.trim_end().ends_with("</svg>"));
    assert_eq!(
        volume.lookup(ROOT_INO, "Drawing.svg").unwrap().size,
        bytes.len() as u64
    );
}

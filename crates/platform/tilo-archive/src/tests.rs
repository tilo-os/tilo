use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{
    Compression, Format, Kind, Observer, Packing, children, create, extract_into, is_cancelled,
    is_unsafe, link_stays_inside, list, read_member, safe_relative, stem,
};

/// The tree every format packs: a folder with a text file, an executable
/// and a nested file.
fn tree(at: &Path) -> PathBuf {
    let top = at.join("top");
    fs::create_dir_all(top.join("sub")).unwrap();
    fs::write(top.join("a.txt"), b"alpha").unwrap();
    fs::write(top.join("sub/b.bin"), vec![7_u8; 300_000]).unwrap();
    fs::write(top.join("run.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(top.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    top
}

fn tar_bytes(top: &Path) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    builder.append_dir_all("top", top).unwrap();
    builder.into_inner().unwrap()
}

fn write_archive(at: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = at.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// `top` packed in `format`, written beside it.
fn archive_of(at: &Path, top: &Path, format: Format) -> PathBuf {
    let name = format!("bundle.{}", format.extension());
    match format {
        Format::Tar(Compression::None) => write_archive(at, &name, &tar_bytes(top)),
        Format::Tar(Compression::Gzip) => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&tar_bytes(top)).unwrap();
            write_archive(at, &name, &encoder.finish().unwrap())
        }
        // xz and 7z archives as the xz and 7z tools make them, what people
        // download: `tree`'s files under `top/` (directories 0755), made
        // with `tar --format=gnu -cf bundle.tar top && xz -1 bundle.tar`
        // and `7z a -mx=1 bundle.7z top`.
        Format::Tar(Compression::Xz) | Format::SevenZ => write_archive(at, &name, &fixture(&name)),
        Format::Tar(Compression::Zstd) => {
            let compressed = ruzstd::encoding::compress_to_vec(
                tar_bytes(top).as_slice(),
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            write_archive(at, &name, &compressed)
        }
        Format::Tar(Compression::Bzip2) => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(&tar_bytes(top)).unwrap();
            write_archive(at, &name, &encoder.finish().unwrap())
        }
        Format::Zip => {
            let path = at.join(&name);
            create(
                &[top.to_path_buf()],
                File::create_new(&path).unwrap(),
                Packing::Zip,
                &(),
            )
            .unwrap();
            path
        }
    }
}

/// A fixture archive by name, as hex: the bytes the xz and 7z tools wrote.
fn fixture(name: &str) -> Vec<u8> {
    let hex: String = match name {
        "bundle.tar.xz" => BUNDLE_TAR_XZ.concat(),
        "bundle.7z" => BUNDLE_7Z.concat(),
        _ => panic!("no fixture {name}"),
    };
    hex.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

const BUNDLE_TAR_XZ: &[&str] = &[
    "fd377a585a000004e6d6b44604c0920280e0122101100000000000005eb95282e4afff010a5d003a1bca6324",
    "0494b34ac8bccf47dec833107d335acacfd87b30da075295904e00d6b6d917d59b17274a8096ad800b1fa72f",
    "e9c4acfcfbc00aa62ca2bff34ad695ba6963fb51a3d1260e40c49420173161c2da343094ca561f0c03c7a4ad",
    "c06157e83eb1e5ff44f9bbdbecd21de099f6440fc9c3711de066cb1b381b14365791e3d6d28789bfab41bb2b",
    "9b57ea069e96740593353329c46bb7f706378167b5d078a9a59ac1b7bdc44647b289ad21f7f78ccbeb0255d9",
    "a544887b9ba2e068b0795dc166d1bdb72fbe119c9abc9da5087b366ebae683d6441472f7d93ccb855f187ec4",
    "e689906b5e8dbc3f0cf5b2102a91f22775f719c006984d98fdd8afd5900fc413cd458c0b9e78ab0f00000000",
    "62b30fbe9fec5c9d0001ae0280e012006be1cca6b1c467fb020000000004595a",
];

const BUNDLE_7Z: &[&str] = &[
    "377abcaf271c00043252de461801000000000000230000000000000037e93042e493ee00805d00309b0a6724",
    "8f02208a3a9a84a53f5c1118257177d5d029f4a7e6cf7e5372cf404c897f2e111edef53921c0dab962da1ad1",
    "eebdf673c02b87102cb62a4159cd0f5bb676835a2116e3dc7976a1e5c2b8bdeb97052a1924408f4543c5a858",
    "6812f1f1573a70e4474db6c96a32b17e7146710823016d1357344f1e9fbcdc96d90000000000813307ae0fd5",
    "2fd4d3166c291f481d98f45d7f611fef75fc78de115dd45d26e7dd582104949c2bd3c07d68b746182fa21ffd",
    "f3c877220cdd159595f37ab30006ff0976a16d41b3868831b22d5048db2f00eb6d24d8617bf3fc7f8d7418fb",
    "79d6314d8380a53f08ddd16991fe31ba656b49fb999c5007ea803d05ac88c9e54973596b0043a392435f4a22",
    "d3e30000170680880109809000070b01000123030101055d001000000c80ea0a01a37e1c6f0000",
];

const FORMATS: [Format; 7] = [
    Format::Zip,
    Format::SevenZ,
    Format::Tar(Compression::None),
    Format::Tar(Compression::Gzip),
    Format::Tar(Compression::Xz),
    Format::Tar(Compression::Zstd),
    Format::Tar(Compression::Bzip2),
];

#[test]
fn every_format_lists_extracts_and_reads_a_member() {
    for format in FORMATS {
        let temp = tempfile::tempdir().unwrap();
        let top = tree(temp.path());
        let archive = archive_of(temp.path(), &top, format);
        assert_eq!(Format::of_path(&archive), Some(format));

        let entries = list(&archive, &()).unwrap();
        let named = |path: &str| entries.iter().find(|entry| entry.path == Path::new(path));
        assert_eq!(named("top/a.txt").unwrap().kind, Kind::File, "{format:?}");
        assert_eq!(named("top/sub/b.bin").unwrap().size, 300_000, "{format:?}");
        assert!(
            named("top/a.txt").unwrap().modified.is_some(),
            "{format:?} keeps when a file was changed"
        );
        let root = children(&entries, Path::new(""));
        assert_eq!(root.len(), 1, "{format:?}: one folder at the top");
        assert_eq!(root[0].kind, Kind::Directory);

        let out = temp.path().join("out");
        fs::create_dir(&out).unwrap();
        let done = extract_into(&archive, &out, &()).unwrap();
        assert_eq!(done.files, 3, "{format:?}");
        assert_eq!(fs::read(out.join("top/a.txt")).unwrap(), b"alpha");
        assert_eq!(
            fs::read(out.join("top/sub/b.bin")).unwrap(),
            vec![7_u8; 300_000]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(out.join("top/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "{format:?} keeps the executable bit");
        }

        let mut member = Vec::new();
        read_member(&archive, Path::new("top/a.txt"), &mut member, &()).unwrap();
        assert_eq!(member, b"alpha", "{format:?}");
    }
}

/// A file named without its extension is still read by its first bytes.
#[test]
fn an_archive_without_its_extension_is_read_by_its_contents() {
    let temp = tempfile::tempdir().unwrap();
    let top = tree(temp.path());
    let archive = archive_of(temp.path(), &top, Format::Tar(Compression::Xz));
    let renamed = temp.path().join("download");
    fs::rename(&archive, &renamed).unwrap();
    assert!(
        list(&renamed, &())
            .unwrap()
            .iter()
            .any(|entry| entry.path == Path::new("top/a.txt"))
    );
}

/// A tar header naming `name` (bypassing the tar crate's own checks, as a
/// crafted archive would), of `kind`, with `link` and `data`.
fn raw_entry(
    builder: &mut tar::Builder<Vec<u8>>,
    name: &[u8],
    kind: tar::EntryType,
    link: &[u8],
    data: &[u8],
) {
    let mut header = tar::Header::new_old();
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.as_old_mut().linkname[..link.len()].copy_from_slice(link);
    header.set_entry_type(kind);
    header.set_mode(0o644);
    header.set_size(data.len() as u64);
    header.set_cksum();
    builder.append(&header, data).unwrap();
}

/// A crafted entry: its name, kind, link target and data.
type Crafted<'a> = (&'a [u8], tar::EntryType, &'a [u8], &'a [u8]);

fn crafted_tar(at: &Path, entries: &[Crafted<'_>]) -> PathBuf {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, kind, link, data) in entries {
        raw_entry(&mut builder, name, *kind, link, data);
    }
    write_archive(at, "crafted.tar", &builder.into_inner().unwrap())
}

/// Names that climb out, absolute names and links pointing out are refused
/// before anything of them is written; nothing lands beside the folder.
#[test]
fn traversal_is_refused() {
    let cases: [&[Crafted<'_>]; 5] = [
        &[
            (b"good.txt", tar::EntryType::Regular, b"", b"ok"),
            (b"../escape", tar::EntryType::Regular, b"", b"bad"),
        ],
        &[(
            b"/tmp/tilo-archive-absolute",
            tar::EntryType::Regular,
            b"",
            b"bad",
        )],
        &[(b"dir/../../escape", tar::EntryType::Regular, b"", b"bad")],
        &[(b"link", tar::EntryType::Symlink, b"../outside", b"")],
        &[(b"link", tar::EntryType::Symlink, b"/etc/passwd", b"")],
    ];
    for entries in cases {
        let temp = tempfile::tempdir().unwrap();
        let archive = crafted_tar(temp.path(), entries);
        let out = temp.path().join("out");
        fs::create_dir(&out).unwrap();
        let error = extract_into(&archive, &out, &()).unwrap_err();
        assert!(is_unsafe(&error), "{error}");
        assert!(!temp.path().join("escape").exists());
        assert!(!Path::new("/tmp/tilo-archive-absolute").exists());
        assert!(
            fs::symlink_metadata(out.join("link")).is_err(),
            "no link made"
        );
    }

    // The same in a zip.
    let temp = tempfile::tempdir().unwrap();
    let mut bytes = io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut bytes);
        writer
            .start_file("../escape", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
    }
    let archive = write_archive(temp.path(), "bad.zip", &bytes.into_inner());
    let out = temp.path().join("out");
    fs::create_dir(&out).unwrap();
    assert!(is_unsafe(&extract_into(&archive, &out, &()).unwrap_err()));
    assert!(!temp.path().join("escape").exists());
    assert!(
        is_unsafe(&list(&archive, &()).unwrap_err()),
        "listing refuses it too"
    );
}

/// A symlink pointing inside is made, after the files; one below another
/// symlink is refused, since its target was checked from the wrong place.
#[cfg(unix)]
#[test]
fn links_inside_are_made_and_links_through_links_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let archive = crafted_tar(
        temp.path(),
        &[
            (b"dir/a.txt", tar::EntryType::Regular, b"", b"alpha"),
            (b"dir/link", tar::EntryType::Symlink, b"a.txt", b""),
            (b"dir/again", tar::EntryType::Link, b"dir/a.txt", b""),
        ],
    );
    let out = temp.path().join("out");
    fs::create_dir(&out).unwrap();
    let done = extract_into(&archive, &out, &()).unwrap();
    assert_eq!(done.links, 2);
    assert_eq!(
        fs::read_link(out.join("dir/link")).unwrap(),
        Path::new("a.txt")
    );
    assert_eq!(fs::read(out.join("dir/again")).unwrap(), b"alpha");

    let temp = tempfile::tempdir().unwrap();
    let archive = crafted_tar(
        temp.path(),
        &[
            (b"here", tar::EntryType::Symlink, b".", b""),
            (b"here/out", tar::EntryType::Symlink, b"../outside", b""),
        ],
    );
    let out = temp.path().join("out");
    fs::create_dir(&out).unwrap();
    assert!(is_unsafe(&extract_into(&archive, &out, &()).unwrap_err()));
    assert!(fs::symlink_metadata(out.join("out")).is_err());
}

/// Device nodes and pipes are never made, setuid and setgid never kept,
/// and permissions never exceed rwxr-xr-x.
#[cfg(unix)]
#[test]
fn devices_are_skipped_and_permissions_capped() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    raw_entry(&mut builder, b"null", tar::EntryType::Char, b"", b"");
    raw_entry(&mut builder, b"pipe", tar::EntryType::Fifo, b"", b"");
    let mut header = tar::Header::new_gnu();
    header.set_path("setuid").unwrap();
    header.set_mode(0o6777);
    header.set_size(2);
    header.set_cksum();
    builder.append(&header, &b"hi"[..]).unwrap();
    let archive = write_archive(temp.path(), "special.tar", &builder.into_inner().unwrap());
    let out = temp.path().join("out");
    fs::create_dir(&out).unwrap();
    let done = extract_into(&archive, &out, &()).unwrap();
    assert_eq!(done.skipped, 2);
    assert!(fs::symlink_metadata(out.join("null")).is_err());
    assert!(fs::symlink_metadata(out.join("pipe")).is_err());
    let mode = fs::metadata(out.join("setuid"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o755);
}

/// Stops once it has read `after` bytes of the archive.
struct StopAfter {
    after: u64,
    read: AtomicU64,
    stopped: AtomicBool,
}

impl Observer for StopAfter {
    fn advance(&self, bytes: u64) {
        if self
            .read
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes)
            >= self.after
        {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }

    fn cancelled(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

/// Cancelling stops extraction and packing with a cancel, not a failure.
#[test]
fn cancel_stops_with_cancelled() {
    let temp = tempfile::tempdir().unwrap();
    let top = tree(temp.path());
    let archive = archive_of(temp.path(), &top, Format::Tar(Compression::None));
    let out = temp.path().join("out");
    fs::create_dir(&out).unwrap();
    let stop = StopAfter {
        after: 1024,
        read: AtomicU64::new(0),
        stopped: AtomicBool::new(false),
    };
    let error = extract_into(&archive, &out, &stop).unwrap_err();
    assert!(is_cancelled(&error), "{error}");

    for packing in [Packing::Zip, Packing::TarZst] {
        let stop = StopAfter {
            after: 1024,
            read: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        };
        let target = temp.path().join(format!("packed.{}", packing.extension()));
        let error = create(
            std::slice::from_ref(&top),
            File::create_new(&target).unwrap(),
            packing,
            &stop,
        )
        .unwrap_err();
        assert!(is_cancelled(&error), "{packing:?}: {error}");
    }
}

/// A tar.zst made here reads back with everything in it.
#[test]
fn tar_zst_round_trips() {
    let temp = tempfile::tempdir().unwrap();
    let top = tree(temp.path());
    let archive = temp.path().join("made.tar.zst");
    create(
        &[top],
        File::create_new(&archive).unwrap(),
        Packing::TarZst,
        &(),
    )
    .unwrap();
    let out = temp.path().join("out");
    fs::create_dir(&out).unwrap();
    assert_eq!(extract_into(&archive, &out, &()).unwrap().files, 3);
    assert_eq!(
        fs::read(out.join("top/sub/b.bin")).unwrap(),
        vec![7_u8; 300_000]
    );
}

/// `./` entries name the folder itself and are left out, not refused.
#[test]
fn the_folder_itself_is_left_out() {
    let temp = tempfile::tempdir().unwrap();
    let archive = crafted_tar(
        temp.path(),
        &[
            (b"./", tar::EntryType::Directory, b"", b""),
            (b"./a.txt", tar::EntryType::Regular, b"", b"a"),
        ],
    );
    let entries = list(&archive, &()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, Path::new("a.txt"));
}

#[test]
fn names_paths_and_links() {
    assert_eq!(
        Format::of_name("x.TAR.GZ"),
        Some(Format::Tar(Compression::Gzip))
    );
    assert_eq!(
        Format::of_name("factorio_2.1.23.tar.xz"),
        Some(Format::Tar(Compression::Xz))
    );
    assert_eq!(
        Format::of_name("a.tzst"),
        Some(Format::Tar(Compression::Zstd))
    );
    assert_eq!(Format::of_name("a.7z"), Some(Format::SevenZ));
    assert_eq!(Format::of_name(".zip"), None);
    assert_eq!(Format::of_name("notes.txt"), None);
    assert_eq!(stem("factorio_2.1.23.tar.xz"), "factorio_2.1.23");
    assert_eq!(stem("Photos.ZIP"), "Photos");
    assert_eq!(stem("plain"), "plain");
    assert!(safe_relative(Path::new("../x")).is_err());
    assert!(safe_relative(Path::new("/x")).is_err());
    assert_eq!(safe_relative(Path::new("./")).unwrap(), None);
    assert_eq!(
        safe_relative(Path::new("./a/./b")).unwrap(),
        Some(PathBuf::from("a/b"))
    );
    assert!(link_stays_inside(Path::new("a/b/link"), Path::new("../c")));
    assert!(link_stays_inside(Path::new("a/link"), Path::new("../c")));
    assert!(!link_stays_inside(Path::new("link"), Path::new("../c")));
    assert!(!link_stays_inside(Path::new("a/link"), Path::new("/c")));
}

#[test]
fn a_folder_named_only_by_its_contents_gets_a_row() {
    // It was last changed when the newest thing inside it was.
    let at = |seconds: u64| Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
    let entry = |path: &str, modified| crate::Entry {
        path: PathBuf::from(path),
        size: 1,
        kind: Kind::File,
        mode: None,
        modified,
        link: None,
    };
    let implied = children(
        &[
            entry("Trip/a.jpg", at(100)),
            entry("Trip/b.jpg", at(300)),
            entry("Trip/c.jpg", at(200)),
        ],
        Path::new(""),
    );
    assert_eq!(implied.len(), 1);
    assert_eq!(implied[0].kind, Kind::Directory);
    assert_eq!(implied[0].modified, at(300));

    let temp = tempfile::tempdir().unwrap();
    let archive = crafted_tar(
        temp.path(),
        &[
            (b"Photos/one.jpg", tar::EntryType::Regular, b"", b"1"),
            (b"root.txt", tar::EntryType::Regular, b"", b"r"),
        ],
    );
    let entries = list(&archive, &()).unwrap();
    let root = children(&entries, Path::new(""));
    assert_eq!(root.len(), 2);
    assert!(
        root.iter()
            .any(|row| row.path == Path::new("Photos") && row.kind == Kind::Directory)
    );
    let photos = children(&entries, Path::new("Photos"));
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].path, Path::new("Photos/one.jpg"));
    let mut bytes = Vec::new();
    File::open(&archive)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(!bytes.is_empty());
}

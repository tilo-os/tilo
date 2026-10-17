//! Explicit runnable material, separate from existing restore-only fixture.
use super::*;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

const ROOT: &str = "/run/tilo-flatpak-runnable-runtime";
const BOUND: u64 = 16 * 1024 * 1024;
pub(super) const APP_PROGRAM: &[u8] = include_bytes!("runnable/fixture-app");
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    schema: u32,
    entries: Vec<Entry>,
    native_app_executed: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    sha256: String,
    bytes: u64,
}
fn bytes(path: &Path, bound: u64, mode: u32) -> Vec<u8> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let before = file.metadata().unwrap();
    assert!(before.is_file() && before.uid() == 0 && before.gid() == 0);
    assert_eq!(before.nlink(), 1);
    assert_eq!(before.mode() & 0o7777, mode);
    assert!(before.len() > 0 && before.len() <= bound);
    let mut output = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(bound + 1)
        .read_to_end(&mut output)
        .unwrap();
    let stamp = |m: fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.mode(),
            m.uid(),
            m.gid(),
            m.nlink(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    assert_eq!(output.len() as u64, before.len());
    assert_eq!(stamp(file.metadata().unwrap()), stamp(before));
    assert_eq!(
        stamp(file.metadata().unwrap()),
        stamp(fs::symlink_metadata(path).unwrap())
    );
    output
}
pub(super) fn populate(contents: &Path, runtime: bool) {
    if runtime {
        let root = Path::new(ROOT);
        let m = fs::symlink_metadata(root).unwrap();
        assert!(m.is_dir() && m.uid() == 0 && m.gid() == 0);
        assert_eq!(m.mode() & 0o7777, 0o555);
        let metadata: Runtime =
            serde_json::from_slice(&bytes(&root.join("runtime.json"), 64 * 1024, 0o444)).unwrap();
        assert_eq!(metadata.schema, 1);
        assert!(!metadata.native_app_executed);
        assert!(!metadata.entries.is_empty() && metadata.entries.len() <= 64);
        let mut seen = std::collections::BTreeSet::new();
        let mut total = 0u64;
        for entry in metadata.entries {
            let relative = Path::new(&entry.path);
            assert!(
                relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
            );
            assert!(
                entry.path.starts_with("lib/")
                    || entry.path.starts_with("lib64/")
                    || matches!(entry.path.as_str(), "bin/bash" | "bin/sleep")
            );
            assert!(seen.insert(entry.path.clone()));
            assert!(entry.bytes > 0 && entry.bytes <= BOUND);
            total = total.checked_add(entry.bytes).unwrap();
            assert!(total <= 64 * 1024 * 1024);
            let source = root.join("usr").join(relative);
            let mut ancestor = source.parent();
            while let Some(directory) = ancestor {
                let m = fs::symlink_metadata(directory).unwrap();
                assert!(m.is_dir() && m.uid() == 0 && m.gid() == 0 && m.mode() & 0o022 == 0);
                if directory == root {
                    break;
                }
                ancestor = directory.parent();
            }
            let data = bytes(&source, BOUND, 0o555);
            assert!(data.starts_with(b"\x7fELF"));
            assert_eq!(data.len() as u64, entry.bytes);
            assert_eq!(hex::encode(Sha256::digest(&data)), entry.sha256);
            let target = contents.join(relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, &data).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(seen.contains("bin/bash"));
    }
    fs::write(contents.join("bin/fixture"), APP_PROGRAM).unwrap();
    fs::set_permissions(
        contents.join("bin/fixture"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

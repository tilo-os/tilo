//! Native fixture inputs are compiled helpers, never an interpreter or an oracle.
#![cfg(all(test, feature = "native-capture-qualification"))]

use sha2::{Digest as _, Sha256};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// A qualifier may supply an immutable SDK helper and its independently expected
/// SHA-256. Otherwise use this checkout's Cargo output (including explicit host
/// targets); missing artifacts fail with the command needed to build them.
pub(super) fn install(name: &str, destination: &Path) {
    let key = format!("TILO_NATIVE_{}", name.replace('-', "_").to_uppercase());
    let expected = std::env::var(format!("{key}_SHA256")).ok();
    let supplied = std::env::var_os(&key);
    assert_eq!(
        supplied.is_some(),
        expected.is_some(),
        "{key} and {key}_SHA256 must be supplied together"
    );
    let path = if let Some(path) = supplied {
        let path = PathBuf::from(path);
        assert!(
            path.is_absolute(),
            "{key} must name the actual absolute SDK helper path"
        );
        path
    } else {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target");
        let triple = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
        [target.join("debug").join(name), target.join(triple).join("debug").join(name)]
            .into_iter()
            .find(|path| path.is_file())
            .unwrap_or_else(|| panic!("missing compiled {name}; run cargo build --locked -p installer --bin tilo-firstboot-home-seeds -p tilo-firstboot-files --bin tilo-firstboot-files, or supply {key} and {key}_SHA256 for the qualified immutable SDK helper"))
    };
    // Read the admitted inode, not a later path lookup, and copy exactly the
    // bytes whose hash was checked. Refuse symlinks and special files.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .unwrap_or_else(|error| panic!("compiled helper {}: {error}", path.display()));
    let info = file.metadata().unwrap();
    assert!(
        info.is_file() && info.mode() & 0o111 != 0,
        "{} must be an executable regular file",
        path.display()
    );
    if expected.is_some() {
        assert_eq!(info.uid(), 0, "SDK helper must be root owned");
        assert_eq!(
            info.mode() & 0o022,
            0,
            "SDK helper must not be group/world writable"
        );
        use std::os::fd::AsRawFd as _;
        let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the descriptor is live and statvfs writes a complete structure on success.
        assert_eq!(
            unsafe { libc::fstatvfs(file.as_raw_fd(), filesystem.as_mut_ptr()) },
            0
        );
        // SAFETY: fstatvfs succeeded above.
        let filesystem = unsafe { filesystem.assume_init() };
        assert!(
            info.mode() & 0o222 == 0 || filesystem.f_flag & libc::ST_RDONLY != 0,
            "SDK helper must reside on a read-only mount or have no writable permission bits"
        );
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
    assert!(
        bytes.starts_with(b"\x7fELF"),
        "{} must be the compiled helper itself, not a script or interpreter oracle",
        path.display()
    );
    if let Some(expected) = expected {
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            expected,
            "SDK helper hash mismatch: {}",
            path.display()
        );
    }
    std::fs::write(destination, &bytes).unwrap();
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        std::fs::read(destination).unwrap(),
        bytes,
        "helper copy changed"
    );
    assert_eq!(
        std::fs::metadata(destination).unwrap().uid(),
        0,
        "fixture helper must remain root owned"
    );
}

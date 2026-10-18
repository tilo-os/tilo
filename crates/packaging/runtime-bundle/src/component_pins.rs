//! Runtime publisher pins installed by authenticated OS/component updates.
//! These paths are never supplied by an app, environment or user data store.

use std::io::Read as _;
use std::path::Path;

const PINS: [&str; 3] = [
    "/usr/share/tilo/runtime-bundle-publisher.hex",
    "/usr/share/tilo/browser-runtime-publisher.hex",
    "/usr/share/tilo/registry-runtime-publisher.hex",
];

/// Combine the operator's existing trust with immutable installed pins.
///
/// # Errors
/// A present component pin is unreadable, mutable, malformed or insecure.
pub fn load_with_components(operator_list: &Path) -> std::io::Result<Option<Vec<String>>> {
    let mut keys = match std::fs::read_to_string(operator_list) {
        Ok(text) => Some(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    for path in PINS {
        if let Some(key) = read_pin(Path::new(path))? {
            keys.get_or_insert_with(Vec::new).push(key);
        }
    }
    if let Some(keys) = &mut keys {
        keys.sort();
        keys.dedup();
    }
    Ok(keys)
}

/// An exact runtime closure already admitted by the host updater. This record
/// grants no other root or version from its publisher any additional trust.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedBundleTrust {
    pub version: String,
    pub arch: String,
    pub root_hash: String,
    pub publisher_hex: String,
}

impl RetainedBundleTrust {
    #[must_use]
    pub fn from_verified(manifest: &crate::BundleManifest) -> Option<Self> {
        Some(Self {
            version: manifest.version.clone(),
            arch: manifest.arch.clone(),
            root_hash: manifest.root_hash.clone(),
            publisher_hex: manifest.publisher_hex.clone()?,
        })
    }

    #[must_use]
    pub fn installed_path(&self) -> std::path::PathBuf {
        let identity = format!(
            "{}\0{}\0{}\0{}",
            self.version, self.arch, self.root_hash, self.publisher_hex
        );
        Path::new("/usr/share/tilo/runtime-retained").join(format!(
            "{}.json",
            blake3::hash(identity.as_bytes()).to_hex()
        ))
    }

    pub fn matches(&self, manifest: &crate::BundleManifest) -> bool {
        self.version == manifest.version
            && self.arch == manifest.arch
            && self.root_hash == manifest.root_hash
            && manifest.publisher_hex.as_deref() == Some(self.publisher_hex.as_str())
    }
}

/// Publisher policy with exact retained exceptions. Check the final verified
/// manifest, not an earlier read of a mutable bundle path.
#[derive(Debug, Clone)]
pub struct BundleTrust {
    publishers: Option<Vec<String>>,
    retained: Vec<RetainedBundleTrust>,
}

impl BundleTrust {
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.publishers
            .as_ref()
            .is_some_and(|keys| !keys.is_empty())
            || !self.retained.is_empty()
    }

    /// Apply policy to the manifest returned by the final signature/payload check.
    ///
    /// # Errors
    /// The final manifest matches neither a trusted publisher nor an exact retained identity.
    pub fn check(&self, manifest: &crate::BundleManifest) -> std::io::Result<()> {
        if !self.is_configured()
            || manifest.publisher_hex.as_ref().is_some_and(|publisher| {
                self.publishers
                    .as_ref()
                    .is_some_and(|keys| keys.iter().any(|key| key.eq_ignore_ascii_case(publisher)))
            })
            || self.retained.iter().any(|record| record.matches(manifest))
        {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "this device does not trust the runtime publisher or its exact retained identity",
            ))
        }
    }
}

/// Existing pins plus retained identities scoped to this exact signed bundle.
/// A mutable, mismatched or replaced retention record fails closed.
///
/// # Errors
/// Policy or signed bundle metadata is invalid or inaccessible.
pub fn load_for_bundle(operator_list: &Path, bundle: &Path) -> std::io::Result<BundleTrust> {
    let mut trust = BundleTrust {
        publishers: load_with_components(operator_list)?,
        retained: Vec::new(),
    };
    let manifest = crate::verify_bundle_metadata_with(
        bundle,
        &crate::VerifyOptions {
            ..Default::default()
        },
    )
    .map_err(std::io::Error::other)?;
    let Some(identity) = RetainedBundleTrust::from_verified(&manifest) else {
        return Ok(trust);
    };
    if let Some(bytes) = read_immutable(&identity.installed_path(), 8192)? {
        crate::verify_bundle_metadata_with(
            bundle,
            &crate::VerifyOptions {
                require_signature: true,
                ..Default::default()
            },
        )
        .map_err(std::io::Error::other)?;
        let retained: Vec<RetainedBundleTrust> =
            serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if retained.is_empty()
            || retained.len() > 32
            || retained.iter().any(|record| !record.matches(&manifest))
        {
            return Err(denied());
        }
        trust.retained = retained;
    }
    Ok(trust)
}

fn denied() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "installed runtime publisher pin is not immutable and root-owned",
    )
}

#[cfg(unix)]
fn read_immutable(path: &Path, bound: u64) -> std::io::Result<Option<Vec<u8>>> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(denied());
    }
    let canonical = path.canonicalize()?;
    let expected = Path::new("/usr")
        .canonicalize()?
        .join(path.strip_prefix("/usr").map_err(|_| denied())?);
    if canonical != expected {
        return Err(denied());
    }
    let mut ancestor = canonical.parent();
    while let Some(path) = ancestor {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(denied());
        }
        ancestor = path.parent();
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical)?;
    let opened = file.metadata()?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(denied());
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the owned file descriptor is live; fstatvfs initializes this struct.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the structure.
    if unsafe { filesystem.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    file.take(bound.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(std::io::Error::other)? > bound {
        return Err(denied());
    }
    Ok(Some(bytes))
}

#[cfg(not(unix))]
fn read_immutable(path: &Path, _bound: u64) -> std::io::Result<Option<Vec<u8>>> {
    if path.try_exists()? {
        Err(denied())
    } else {
        Ok(None)
    }
}

fn read_pin(path: &Path) -> std::io::Result<Option<String>> {
    read_immutable(path, 258)?
        .map(|bytes| decode(&bytes, path == Path::new(PINS[1])))
        .transpose()
        .map(Option::flatten)
}

fn decode(bytes: &[u8], optional_engine: bool) -> std::io::Result<Option<String>> {
    // An explicitly empty optional-engine pin is the existing image contract.
    if bytes.is_empty() && optional_engine {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| denied())?.trim();
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "installed runtime publisher pin is malformed",
        ));
    }
    Ok(Some(text.to_ascii_lowercase()))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::process::Command;

    fn namespace(executable: &Path, operator: &Path) -> Command {
        let mut command = Command::new("timeout");
        command.args([
            "20",
            "bwrap",
            "--die-with-parent",
            "--unshare-all",
            "--tmpfs",
            "/",
            "--dir",
            "/usr",
            "--dir",
            "/usr/share",
            "--dir",
            "/usr/share/tilo",
            "--dir",
            "/foreign",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
        ]);
        for path in ["/usr/lib64", "/usr/lib"] {
            if Path::new(path).is_dir() {
                command.args(["--ro-bind", path, path]);
            }
        }
        command.args([
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--symlink",
            "usr/lib",
            "/lib",
        ]);
        command.arg("--ro-bind").arg(executable).arg("/fixture");
        command.arg("--ro-bind").arg(operator).arg("/operator.txt");
        command
    }

    fn assert_retained_namespace_case(case: &str) {
        let old = load_for_bundle(Path::new("/operator.txt"), Path::new("/old"));
        match case {
            "mutable" | "malformed" => assert!(old.is_err()),
            "valid" => {
                let old = old.unwrap();
                let manifest = crate::verify_bundle(Path::new("/old")).unwrap();
                old.check(&manifest).unwrap();
                let other = crate::verify_bundle(Path::new("/other")).unwrap();
                let other_keys =
                    load_for_bundle(Path::new("/operator.txt"), Path::new("/other")).unwrap();
                assert!(
                    old.check(&other).is_err(),
                    "a replaced final manifest must fail"
                );
                assert!(
                    other_keys.check(&other).is_err(),
                    "retention must not trust another root from the same old publisher"
                );
                let new = crate::verify_bundle(Path::new("/new")).unwrap();
                let new_keys =
                    load_for_bundle(Path::new("/operator.txt"), Path::new("/new")).unwrap();
                new_keys.check(&new).unwrap();
                for field in ["version", "arch", "root", "publisher"] {
                    let mut replaced = manifest.clone();
                    match field {
                        "version" => replaced.version.push_str("-replaced"),
                        "arch" => replaced.arch.push_str("-replaced"),
                        "root" => replaced.root_hash = other.root_hash.clone(),
                        _ => replaced.publisher_hex = Some("33".repeat(32)),
                    }
                    assert!(old.check(&replaced).is_err(), "{field}");
                }
            }
            _ => panic!("unknown retained runtime test"),
        }
    }

    #[test]
    fn retained_runtime_trust_is_exact_and_does_not_trust_an_old_publisher_globally() {
        const CASE: &str = "TILO_TEST_RETAINED_RUNTIME";
        if let Ok(case) = std::env::var(CASE) {
            assert_retained_namespace_case(&case);
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host");
        std::fs::create_dir(&host).unwrap();
        let library = temp.path().join("library.so");
        let identity = temp.path().join("key.json");
        std::fs::write(
            &identity,
            serde_json::json!({"secret_key_hex": "11".repeat(32)}).to_string(),
        )
        .unwrap();
        let build = |name: &str, bytes: &[u8]| {
            std::fs::write(&library, bytes).unwrap();
            let path = temp.path().join(name);
            crate::build_bundle_with(
                &path,
                if name == "new" { "new" } else { "old" },
                &crate::BuildOptions {
                    from: Some(&host),
                    identity: Some(&identity),
                    extra_libs: std::slice::from_ref(&library),
                    ..Default::default()
                },
            )
            .unwrap();
            path
        };
        let old = build("old", b"old payload");
        let other = build("other", b"different payload from the same old publisher");
        std::fs::write(
            &identity,
            serde_json::json!({"secret_key_hex": "22".repeat(32)}).to_string(),
        )
        .unwrap();
        let new = build("new", b"new payload");
        let proof =
            RetainedBundleTrust::from_verified(&crate::verify_bundle(&old).unwrap()).unwrap();
        let record = temp.path().join("retained.json");
        std::fs::write(&record, serde_json::to_vec(&vec![proof.clone()]).unwrap()).unwrap();
        let malformed = temp.path().join("malformed.json");
        std::fs::write(&malformed, "not a record").unwrap();
        let pin = temp.path().join("pin");
        std::fs::write(
            &pin,
            crate::verify_bundle(&new).unwrap().publisher_hex.unwrap(),
        )
        .unwrap();
        let operator = temp.path().join("operator");
        std::fs::write(&operator, "").unwrap();
        let executable = std::env::current_exe().unwrap();
        for case in ["valid", "mutable", "malformed"] {
            let mut command = namespace(&executable, &operator);
            if std::fs::metadata(&pin).unwrap().uid() != 0 {
                // As in the pin test: the runner's own files look root-owned
                // in the namespace, as the image files the pins come from are.
                command.args(["--uid", "0", "--gid", "0"]);
            }
            command.args(["--dir", "/usr/share/tilo/runtime-retained"]);
            command.arg("--ro-bind").arg(&pin).arg(PINS[0]);
            for (source, target) in [(&old, "/old"), (&other, "/other"), (&new, "/new")] {
                command.arg("--ro-bind").arg(source).arg(target);
            }
            command
                .arg(if case == "mutable" {
                    "--bind"
                } else {
                    "--ro-bind"
                })
                .arg(if case == "malformed" {
                    &malformed
                } else {
                    &record
                })
                .arg(proof.installed_path());
            command.args(["--setenv", CASE, case, "--", "/fixture", "--exact",
                "component_pins::tests::retained_runtime_trust_is_exact_and_does_not_trust_an_old_publisher_globally", "--nocapture"]);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn installed_component_pins_use_real_readonly_files_and_fail_closed() {
        const CASE: &str = "TILO_TEST_INSTALLED_PIN_CASE";
        if let Ok(case) = std::env::var(CASE) {
            let loaded = load_with_components(Path::new("/operator.txt"));
            match case.as_str() {
                "readonly" => {
                    let keys = loaded.unwrap().unwrap();
                    assert!(keys.contains(&"11".repeat(32)));
                    assert!(keys.contains(&"22".repeat(32)));
                }
                "mutable" | "malformed" | "symlink" | "escape" | "empty" | "writebits" => {
                    assert!(loaded.is_err());
                }
                other => panic!("unexpected fixture case: {other}"),
            }
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let pin = fixture.path().join("pin");
        let malformed = fixture.path().join("malformed");
        let operator = fixture.path().join("operator");
        let empty = fixture.path().join("empty");
        let writable = fixture.path().join("writable");
        std::fs::write(&pin, "11".repeat(32)).unwrap();
        std::fs::write(&malformed, "not a publisher").unwrap();
        std::fs::write(&operator, "22".repeat(32)).unwrap();
        std::fs::write(&empty, "").unwrap();
        std::fs::write(&writable, "11".repeat(32)).unwrap();
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o666)).unwrap();
        let executable = std::env::current_exe().unwrap();
        for case in [
            "readonly",
            "mutable",
            "malformed",
            "symlink",
            "escape",
            "empty",
            "writebits",
        ] {
            let mut command = namespace(&executable, &operator);
            if std::fs::metadata(&pin).unwrap().uid() != 0 {
                // An unprivileged runner's files are its own. Map it to root in
                // the namespace so the fixture looks as the root-owned image
                // files the pins are read from do.
                command.args(["--uid", "0", "--gid", "0"]);
            }
            match case {
                "symlink" => {
                    command.args(["--symlink", "/operator.txt", PINS[0]]);
                }
                "escape" => {
                    command.args(["--tmpfs", "/usr/share"]);
                    command.args(["--symlink", "/foreign", "/usr/share/tilo"]);
                    command
                        .arg("--ro-bind")
                        .arg(&pin)
                        .arg("/foreign/runtime-bundle-publisher.hex");
                }
                _ => {
                    command.arg(if case == "mutable" {
                        "--bind"
                    } else {
                        "--ro-bind"
                    });
                    command
                        .arg(match case {
                            "malformed" => &malformed,
                            "empty" => &empty,
                            "writebits" => &writable,
                            _ => &pin,
                        })
                        .arg(PINS[0]);
                }
            }
            command.args(["--setenv", CASE, case, "--", "/fixture", "--exact",
                "component_pins::tests::installed_component_pins_use_real_readonly_files_and_fail_closed", "--nocapture"]);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "case {case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

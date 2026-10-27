//! The guest image: a signed, digest-pinned disk from the registry.
//!
//! CI builds the Linux guest (`tools/ci/publish/legacy-guest.sh`),
//! wraps its qcow2 in an OCI image, signs it with the release key and pins
//! the digest in `os/legacy-guest-snapshot.env`, which is compiled in here.
//! `skopeo` fetches by that digest, and `/etc/containers/policy.json`
//! requires the release signature for the repository. Every user's disk is
//! a copy-on-write overlay of the fetched base, so the base stays pristine
//! and is shared by all of the user's VMs of that guest OS.

use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

const SNAPSHOT: &str = include_str!("../../../../os/legacy-guest-snapshot.env");

/// The file inside the image's single layer.
pub const DISK_IN_IMAGE: &str = "disk.qcow2";

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("no Linux guest image has been published for this Tilo build; pass --image <qcow2>")]
    NotPublished,
    #[error("{program} failed: {detail}")]
    Command { program: String, detail: String },
    #[error("could not run {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("the fetched image is not a single-layer disk image")]
    Layout,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The pinned reference for this architecture, if one was published.
#[must_use]
pub fn pinned_ref(snapshot: &str, arch: &str) -> Option<String> {
    let key = format!("LEGACY_GUEST_{}_REF=", arch.to_ascii_uppercase());
    snapshot
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix(&key))
        .map(str::trim)
        .filter(|reference| valid_pinned_ref(reference))
        .map(ToString::to_string)
}

/// The one repository the guest image is published to. Its scope in
/// `/etc/containers/policy.json` requires the release key's signature; a
/// sibling under the same namespace (the rejected `tilo-dev`, `tilo-ci` and
/// `tilo-vm`, or one with no scope at all) is not the guest.
const GUEST_REPOSITORY: &str = "registry.tilo.so/tilo/legacy-guest";

/// `registry.tilo.so/tilo/legacy-guest@sha256:<64 hex>` and nothing looser.
#[must_use]
pub fn valid_pinned_ref(reference: &str) -> bool {
    let Some((repository, digest)) = reference.split_once("@sha256:") else {
        return false;
    };
    repository == GUEST_REPOSITORY
        && digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The reference compiled into this build for the running architecture.
#[must_use]
pub fn current_ref() -> Option<String> {
    pinned_ref(SNAPSHOT, std::env::consts::ARCH)
}

/// Where a fetched base for `reference` is kept.
#[must_use]
pub fn base_path(images: &Path, reference: &str) -> PathBuf {
    let digest = reference.rsplit(':').next().unwrap_or(reference);
    images.join(format!("linux-{digest}.qcow2"))
}

/// `skopeo copy docker://<ref> dir:<dir>`.
#[must_use]
pub fn fetch_argv(reference: &str, dir: &Path) -> Vec<String> {
    vec![
        "skopeo".to_string(),
        "copy".to_string(),
        "--quiet".to_string(),
        format!("docker://{reference}"),
        format!("dir:{}", dir.display()),
    ]
}

/// `qemu-img create -f qcow2 -F qcow2 -b <base> <disk>`.
#[must_use]
pub fn overlay_argv(base: &Path, disk: &Path) -> Vec<String> {
    vec![
        "qemu-img".to_string(),
        "create".to_string(),
        "-q".to_string(),
        "-f".to_string(),
        "qcow2".to_string(),
        "-F".to_string(),
        "qcow2".to_string(),
        "-b".to_string(),
        base.display().to_string(),
        disk.display().to_string(),
    ]
}

/// The one layer blob of an image copied with `skopeo copy … dir:`.
pub fn single_layer(dir: &Path) -> Result<PathBuf, ImageError> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)
            .map_err(|_| ImageError::Layout)?;
    let layers = manifest
        .get("layers")
        .and_then(serde_json::Value::as_array)
        .ok_or(ImageError::Layout)?;
    let [layer] = layers.as_slice() else {
        return Err(ImageError::Layout);
    };
    let digest = layer
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(ImageError::Layout)?;
    Ok(dir.join(digest))
}

fn run(argv: &[String]) -> Result<(), ImageError> {
    let (program, rest) = argv.split_first().ok_or(ImageError::Layout)?;
    let output = Command::new(program)
        .args(rest)
        .output()
        .map_err(|source| ImageError::Spawn {
            program: program.clone(),
            source,
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ImageError::Command {
            program: program.clone(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Fetch the pinned base into `images` unless it is already there.
pub fn ensure_base(images: &Path, reference: &str) -> Result<PathBuf, ImageError> {
    let base = base_path(images, reference);
    if base.is_file() {
        return Ok(base);
    }
    std::fs::create_dir_all(images)?;
    let staging = images.join(format!(".fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let fetched = (|| {
        run(&fetch_argv(reference, &staging))?;
        let layer = single_layer(&staging)?;
        run(&[
            "tar".to_string(),
            "-xf".to_string(),
            layer.display().to_string(),
            "-C".to_string(),
            staging.display().to_string(),
            DISK_IN_IMAGE.to_string(),
        ])?;
        let disk = staging.join(DISK_IN_IMAGE);
        // The base is never written: every VM overlays it.
        let mut permissions = std::fs::metadata(&disk)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&disk, permissions)?;
        std::fs::rename(&disk, &base)?;
        Ok(base.clone())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    fetched
}

/// Create the user's disk as an overlay of `base`.
pub fn create_disk(base: &Path, disk: &Path) -> Result<(), ImageError> {
    if let Some(parent) = disk.parent() {
        std::fs::create_dir_all(parent)?;
    }
    run(&overlay_argv(base, disk))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guest_image_is_fetched_only_by_pinned_digest() {
        let digest = "ab".repeat(32);
        // phase: only a digest pin of the signed guest repository is accepted
        {
            let snapshot = format!(
                "# pinned by CI\nLEGACY_GUEST_X86_64_REF=registry.tilo.so/tilo/legacy-guest@sha256:{digest}\n"
            );
            assert_eq!(
                pinned_ref(&snapshot, "x86_64"),
                Some(format!(
                    "registry.tilo.so/tilo/legacy-guest@sha256:{digest}"
                ))
            );
            assert_eq!(pinned_ref(&snapshot, "aarch64"), None);
            assert!(!valid_pinned_ref(
                "registry.tilo.so/tilo/legacy-guest:latest"
            ));
            assert!(!valid_pinned_ref(&format!(
                "evil.example/legacy@sha256:{digest}"
            )));
            assert!(!valid_pinned_ref("registry.tilo.so/tilo/x@sha256:abc"));
            // A sibling repository is not the signed guest scope, however
            // well-formed its pin.
            for sibling in ["tilo-dev", "tilo-vm", "tilo", "legacy-guest-dev"] {
                assert!(!valid_pinned_ref(&format!(
                    "registry.tilo.so/tilo/{sibling}@sha256:{digest}"
                )));
            }
            // Whatever is compiled in must be well-formed or absent.
            assert!(current_ref().is_none_or(|reference| valid_pinned_ref(&reference)));
        }
        // phase: fetch, layout and overlay commands are exact
        {
            let reference = format!("registry.tilo.so/tilo/legacy-guest@sha256:{digest}");
            assert_eq!(
                fetch_argv(&reference, Path::new("/c/x")),
                [
                    "skopeo".to_string(),
                    "copy".into(),
                    "--quiet".into(),
                    format!("docker://{reference}"),
                    "dir:/c/x".into()
                ]
            );
            assert_eq!(
                base_path(Path::new("/c"), &reference),
                PathBuf::from(format!("/c/linux-{digest}.qcow2"))
            );
            assert_eq!(
                overlay_argv(Path::new("/c/base.qcow2"), Path::new("/d/disk.qcow2")),
                [
                    "qemu-img",
                    "create",
                    "-q",
                    "-f",
                    "qcow2",
                    "-F",
                    "qcow2",
                    "-b",
                    "/c/base.qcow2",
                    "/d/disk.qcow2"
                ]
            );
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("manifest.json"),
                format!(r#"{{"layers":[{{"digest":"sha256:{digest}"}}]}}"#),
            )
            .unwrap();
            assert_eq!(single_layer(dir.path()).unwrap(), dir.path().join(&digest));
            std::fs::write(dir.path().join("manifest.json"), r#"{"layers":[]}"#).unwrap();
            assert!(single_layer(dir.path()).is_err());
        }
    }
}

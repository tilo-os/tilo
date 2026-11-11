//! The hosted guest images: a signed, digest-pinned Firecracker kernel and
//! root filesystem from the registry.
//!
//! CI builds them (`tools/ci/publish/hosted-guest.sh`), wraps `vmlinux` and
//! `rootfs.ext4` in one OCI image, signs it with the release key and pins the
//! digest in `os/hosted-guest-snapshot.env`, which is compiled in here.
//! `skopeo` fetches by that digest, and `/etc/containers/policy.json` requires
//! the release signature for the repository. The kernel is booted as it is
//! ([`Machine::kernel`](super::firecracker::Machine)); each VM's disk is a
//! reflinked copy of `rootfs.ext4`, then grown to the VM's size
//! ([`Machine::base_disk`](super::firecracker::Machine)), so the fetched image
//! stays pristine and every VM of a host starts from the same bytes.
//!
//! One image boots every workspace's VM: its Linux programs, and the headless
//! session with Browser and Chromium that holds the workspace's Browser
//! (`docs/hosted-browser.md`).

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::image;

const SNAPSHOT: &str = include_str!("../../../../../os/hosted-guest-snapshot.env");

/// The kernel inside an image's single layer.
pub const KERNEL_IN_IMAGE: &str = "vmlinux";
/// The root filesystem base disk inside an image's single layer.
pub const ROOTFS_IN_IMAGE: &str = "rootfs.ext4";

/// The repository the guest is published in. Its scope in
/// `/etc/containers/policy.json` requires the release key's signature, so a
/// sibling under the same namespace is not the guest.
pub const REPOSITORY: &str = "registry.tilo.so/tilo/hosted-guest";

/// The key its digest is pinned under, before the architecture.
const PIN_PREFIX: &str = "HOSTED_GUEST_";

#[derive(Debug, Error)]
pub enum GuestError {
    #[error("no hosted guest image has been published for this Tilo build; pass --ref <reference>")]
    NotPublished,
    #[error("{0} is not a digest-pinned hosted guest reference")]
    NotARef(String),
    #[error("the fetched image is not a single-layer hosted guest")]
    Layout,
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One fetched guest: what a VM boots and the disk it starts from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fetched {
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
}

/// The pinned reference of the guest for this architecture, if one was
/// published.
#[must_use]
pub fn pinned_ref(snapshot: &str, arch: &str) -> Option<String> {
    let key = format!("{PIN_PREFIX}{}_REF=", arch.to_ascii_uppercase());
    snapshot
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix(&key))
        .map(str::trim)
        .filter(|reference| valid_pinned_ref(reference))
        .map(ToString::to_string)
}

/// `registry.tilo.so/tilo/hosted-guest@sha256:<64 hex>` and nothing looser.
#[must_use]
pub fn valid_pinned_ref(reference: &str) -> bool {
    let Some((repository, digest)) = reference.split_once("@sha256:") else {
        return false;
    };
    repository == REPOSITORY
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

/// Where the image `reference` is kept: both files, under the digest that
/// names them.
#[must_use]
pub fn guest_dir(images: &Path, reference: &str) -> PathBuf {
    let digest = reference.rsplit(':').next().unwrap_or(reference);
    images.join(format!("hosted-{digest}"))
}

fn files(dir: &Path) -> Fetched {
    Fetched {
        kernel: dir.join(KERNEL_IN_IMAGE),
        rootfs: dir.join(ROOTFS_IN_IMAGE),
    }
}

/// The reference the guest is fetched by: `reference` when given, else the
/// pin compiled in. An empty reference names the pin, so a start can pass the
/// variable it holds without asking whether it is set.
///
/// # Errors
///
/// The reference is not a digest pin of the guest's repository, or nothing
/// has been published for it.
pub fn reference(reference: Option<&str>) -> Result<String, GuestError> {
    match reference.filter(|reference| !reference.is_empty()) {
        Some(reference) if valid_pinned_ref(reference) => Ok(reference.to_string()),
        Some(reference) => Err(GuestError::NotARef(reference.to_string())),
        None => current_ref().ok_or(GuestError::NotPublished),
    }
}

/// Fetch `reference` into `images` unless it is already there.
pub fn ensure_guest(images: &Path, reference: &str) -> Result<Fetched, GuestError> {
    let dir = guest_dir(images, reference);
    let found = files(&dir);
    if found.kernel.is_file() && found.rootfs.is_file() {
        return Ok(found);
    }
    std::fs::create_dir_all(images)?;
    let staging = images.join(format!(".fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let fetched = (|| {
        image::run(&image::fetch_argv(reference, &staging))?;
        let layer = image::single_layer(&staging).map_err(|_| GuestError::Layout)?;
        image::run(&[
            "tar".to_string(),
            "-xf".to_string(),
            layer.display().to_string(),
            "-C".to_string(),
            staging.display().to_string(),
            KERNEL_IN_IMAGE.to_string(),
            ROOTFS_IN_IMAGE.to_string(),
        ])?;
        let staged = files(&staging);
        // Neither file is ever written: a VM boots the kernel as it is and
        // reflinks its disk from the root filesystem.
        for path in [&staged.kernel, &staged.rootfs] {
            if !path.is_file() {
                return Err(GuestError::Layout);
            }
            let mut permissions = std::fs::metadata(path)?.permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(path, permissions)?;
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::rename(&staging, &dir)?;
        Ok(files(&dir))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    fetched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hosted_guest_is_fetched_only_by_pinned_digest() {
        let digest = "ab".repeat(32);
        let linux = format!("registry.tilo.so/tilo/hosted-guest@sha256:{digest}");
        // phase: only a digest pin of the guest's own signed repository is accepted
        {
            let snapshot = format!("# pinned by CI\nHOSTED_GUEST_X86_64_REF={linux}\n");
            assert_eq!(pinned_ref(&snapshot, "x86_64"), Some(linux.clone()));
            assert_eq!(pinned_ref(&snapshot, "aarch64"), None);
            assert_eq!(
                pinned_ref(&format!("# HOSTED_GUEST_X86_64_REF={linux}"), "x86_64"),
                None
            );
            assert!(matches!(
                reference(Some("registry.tilo.so/tilo/hosted-guest:latest")),
                Err(GuestError::NotARef(_))
            ));
            assert!(!valid_pinned_ref(&format!(
                "evil.example/hosted-guest@sha256:{digest}"
            )));
            assert!(!valid_pinned_ref("registry.tilo.so/tilo/x@sha256:abc"));
            for sibling in [
                "hosted-guest-dev",
                "hosted-browser-guest",
                "tilo-vm",
                "hosted",
            ] {
                assert!(!valid_pinned_ref(&format!(
                    "registry.tilo.so/tilo/{sibling}@sha256:{digest}"
                )));
            }
            // Whatever is compiled in must be well-formed or absent.
            assert!(current_ref().is_none_or(|reference| valid_pinned_ref(&reference)));
        }
        // phase: the guest lands under its digest, as both files the VM needs
        {
            let dir = guest_dir(Path::new("/c"), &linux);
            assert_eq!(dir, PathBuf::from(format!("/c/hosted-{digest}")));
            let found = files(&dir);
            assert_eq!(found.kernel, dir.join("vmlinux"));
            assert_eq!(found.rootfs, dir.join("rootfs.ext4"));
        }
    }
}

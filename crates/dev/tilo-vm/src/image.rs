//! Local disk files for `vm create`: what an `--image` or `--boot-esp` file
//! is, and the one-time import of an image that lives on a network path.
//!
//! A local image is either a qcow2 dev disk (what CI publishes) or a raw disk
//! with a GPT (what `os/dev/disk-image.sh --raw` and `os/dev/make-boot-esp.sh`
//! write). The VM's own disk is a qcow2 child of it, so the image is never
//! written.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::hypervisor::CommandLine;

/// The on-disk format of a local image, as `qemu-img` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Qcow2,
    Raw,
}

impl Format {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }

    /// The format recorded in a registry entry; entries written before raw
    /// images were accepted have none, and were qcow2.
    #[must_use]
    pub fn from_entry(name: Option<&str>) -> Self {
        if name == Some("raw") {
            Self::Raw
        } else {
            Self::Qcow2
        }
    }
}

const QCOW2_MAGIC: &[u8] = b"QFI\xfb";
const GPT_SIGNATURE: &[u8] = b"EFI PART";
/// The primary GPT header's signature: LBA 1 of a 512-byte-sector disk.
const GPT_AT: std::ops::Range<usize> = 512..520;

/// The format of a disk from its first bytes: a qcow2 header, or a raw disk
/// whose LBA 1 holds a GPT header.
///
/// # Errors
/// Anything else, as a sentence saying what the bytes are not.
pub fn detect(head: &[u8]) -> Result<Format, String> {
    if head.starts_with(QCOW2_MAGIC) {
        return Ok(Format::Qcow2);
    }
    match head.get(GPT_AT) {
        Some(signature) if signature == GPT_SIGNATURE => Ok(Format::Raw),
        Some(_) => Err(
            "is neither a qcow2 image nor a raw disk with a GPT (no `EFI PART` header at byte 512)"
                .into(),
        ),
        None => Err(format!(
            "is {} bytes: too small for a qcow2 header or a partition table",
            head.len()
        )),
    }
}

/// [`detect`] on a file.
///
/// # Errors
/// Read failures and files that are not a disk image.
pub fn format_of(path: &Path) -> Result<Format, String> {
    let mut head = Vec::with_capacity(GPT_AT.end);
    std::fs::File::open(path)
        .and_then(|file| file.take(GPT_AT.end as u64).read_to_end(&mut head))
        .map_err(|e| format!("cannot be read: {e}"))?;
    detect(&head)
}

/// Is `path` on another machine (a Windows UNC path such as
/// `\\wsl.localhost\Fedora\home\me\disk.raw` or `\\server\share\disk.raw`)?
/// `\\?\C:\` and `\\.\` name local files and devices.
#[must_use]
pub fn is_network_path(path: &str) -> bool {
    let unc = path.replace('/', "\\");
    if let Some(rest) = unc.strip_prefix(r"\\?\") {
        return rest.len() >= 4
            && rest
                .get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\"));
    }
    unc.starts_with(r"\\") && !unc.starts_with(r"\\.\")
}

/// Where a network image is imported to:
/// `<cache>/tilo-vm/imports/<key>.qcow2`, keyed by the source's path, size
/// and modification time, so an unchanged source is imported once and a
/// rebuilt one again.
#[must_use]
pub fn import_path(cache: &Path, source: &str, len: u64, modified_nanos: u128) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update(b"\n");
    hasher.update(len.to_le_bytes());
    hasher.update(modified_nanos.to_le_bytes());
    let key = hex::encode(hasher.finalize());
    cache
        .join("tilo-vm")
        .join("imports")
        .join(format!("{}.qcow2", key.get(..16).unwrap_or(&key)))
}

/// The partial file an import writes before it is verified.
#[must_use]
pub fn import_part(dest: &Path) -> PathBuf {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// Copy `source` into a local qcow2 at `part`, then compare every
/// guest-visible byte with the source. QEMU on Windows reads a disk behind a
/// UNC path unreliably as a backing file (whole ranges come back as zeros),
/// so a VM never runs on one: it runs on this verified local copy.
#[must_use]
pub fn import_commands(
    qemu_img: &str,
    source: &str,
    format: Format,
    part: &Path,
) -> Vec<CommandLine> {
    let part = part.display().to_string();
    vec![
        CommandLine::new(
            qemu_img,
            &["convert", "-f", format.name(), "-O", "qcow2", source, &part],
        ),
        CommandLine::new(
            qemu_img,
            &["compare", "-f", format.name(), "-F", "qcow2", source, &part],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_with_gpt() -> Vec<u8> {
        let mut disk = vec![0_u8; 1024];
        disk.get_mut(512..520)
            .unwrap()
            .copy_from_slice(GPT_SIGNATURE);
        disk
    }

    /// Formats are told apart by their first bytes; entries without a format are qcow2; UNC paths are
    /// network paths; an import is keyed by source, size and time and converts then compares every byte.
    #[test]
    fn disk_images_are_identified_and_imported_byte_exactly() {
        // Phase: formats_are_told_apart_by_their_first_bytes.
        {
            assert_eq!(detect(b"QFI\xfb\0\0\0\x03"), Ok(Format::Qcow2));
            assert_eq!(detect(&raw_with_gpt()), Ok(Format::Raw));
            let blank = detect(&[0_u8; 1024]).unwrap_err();
            assert!(blank.contains("EFI PART"), "{blank}");
            let empty = detect(&[]).unwrap_err();
            assert!(empty.contains("0 bytes"), "{empty}");

            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("disk.raw");
            std::fs::write(&file, raw_with_gpt()).unwrap();
            assert_eq!(format_of(&file), Ok(Format::Raw));
            assert!(format_of(&dir.path().join("missing")).is_err());
        }

        // Phase: entries_without_a_format_are_qcow2.
        {
            assert_eq!(Format::from_entry(None), Format::Qcow2);
            assert_eq!(Format::from_entry(Some("qcow2")), Format::Qcow2);
            assert_eq!(Format::from_entry(Some("raw")), Format::Raw);
        }

        // Phase: unc_paths_are_network_paths.
        {
            assert!(is_network_path(
                r"\\wsl.localhost\FedoraLinux-42\home\me\disk.raw"
            ));
            assert!(is_network_path(r"\\wsl$\Fedora\disk.raw"));
            assert!(is_network_path(r"\\server\share\disk.qcow2"));
            assert!(is_network_path("//server/share/disk.qcow2"));
            assert!(is_network_path(r"\\?\UNC\server\share\disk.raw"));
            assert!(!is_network_path(r"\\?\C:\vms\disk.raw"));
            assert!(!is_network_path(r"\\.\PhysicalDrive0"));
            assert!(!is_network_path(r"C:\tilo-build\disk.raw"));
            assert!(!is_network_path("/home/me/disk.raw"));
        }

        // Phase: an_import_is_keyed_by_source_size_and_time.
        {
            let cache = Path::new("/cache");
            let a = import_path(cache, r"\\wsl.localhost\F\disk.raw", 10, 1);
            assert!(a.starts_with("/cache/tilo-vm/imports"), "{}", a.display());
            assert_eq!(a.extension().unwrap(), "qcow2");
            assert_eq!(a, import_path(cache, r"\\wsl.localhost\F\disk.raw", 10, 1));
            assert_ne!(a, import_path(cache, r"\\wsl.localhost\F\disk.raw", 10, 2));
            assert_ne!(a, import_path(cache, r"\\wsl.localhost\F\disk.raw", 11, 1));
            assert_ne!(a, import_path(cache, r"\\wsl.localhost\F\other.raw", 10, 1));
            assert_eq!(
                import_part(&a).display().to_string(),
                format!("{}.part", a.display())
            );
        }

        // Phase: an_import_converts_then_compares_every_byte.
        {
            let commands = import_commands(
                "qemu-img.exe",
                r"\\wsl.localhost\F\disk.raw",
                Format::Raw,
                Path::new(r"C:\cache\x.qcow2.part"),
            );
            let shown: Vec<String> = commands.iter().map(CommandLine::display).collect();
            assert_eq!(shown.len(), 2);
            assert!(
                shown[0].starts_with("qemu-img.exe convert -f raw -O qcow2"),
                "{}",
                shown[0]
            );
            assert!(
                shown[1].starts_with("qemu-img.exe compare -f raw -F qcow2"),
                "{}",
                shown[1]
            );
            assert!(shown.iter().all(|c| c.contains("x.qcow2.part")));
        }
    }
}

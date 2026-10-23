//! Target disks for the alongside layouts, and the proof that an alongside
//! install left the existing systems byte-for-byte intact.
//!
//! The fixture is deliberately small and synthetic: a foreign EFI partition
//! and a Linux filesystem with a sentinel file stand in for the systems a
//! person keeps. Filesystems are made by the host's own tools (through
//! `os/dev/vm.sh tool`, so a distrobox setup works the same), and every
//! preserved extent is hashed before the install and again after it.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::Layout;

const MIB: u64 = 1024 * 1024;
const SECTOR: u64 = 512;
/// The target disk the scenario attaches, like the release cycle's.
pub const TARGET_BYTES: u64 = 40 * 1024 * MIB;

const EFI_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
const LINUX_TYPE: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";

/// Runs one host tool. The scenario passes `os/dev/vm.sh tool`; tests pass
/// a recorder.
pub trait Tools {
    /// # Errors
    ///
    /// The tool's own failure, as text.
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<(), String>;
}

/// One region of the target that must survive the install unchanged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Preserved {
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub sha256: String,
}

/// The partitions a layout starts with, in MiB: (name, start, size, type,
/// filesystem). `None` size means "to the end of the disk"; `None`
/// filesystem leaves the partition empty.
type Part = (
    &'static str,
    u64,
    Option<u64>,
    &'static str,
    Option<&'static str>,
);

fn partitions(layout: Layout) -> Vec<Part> {
    match layout {
        Layout::Erase => Vec::new(),
        Layout::PreparedAlongside => vec![
            ("shared EFI", 1, Some(2048), EFI_TYPE, Some("vfat")),
            ("Linux sentinel", 2049, Some(512), LINUX_TYPE, Some("ext4")),
            ("Tilo prepared root", 2561, None, LINUX_TYPE, None),
        ],
        Layout::GuidedAlongside => vec![
            ("shared EFI", 1, Some(512), EFI_TYPE, Some("vfat")),
            ("Linux sentinel", 513, Some(512), LINUX_TYPE, Some("ext4")),
        ],
    }
}

pub fn sfdisk_script(layout: Layout) -> String {
    let mut script = String::from("label: gpt\n");
    for (index, (name, start, size, kind, _)) in partitions(layout).iter().enumerate() {
        let start = start.saturating_mul(MIB) / SECTOR;
        let size = size.map_or(String::new(), |size| {
            format!(", size={}", size.saturating_mul(MIB) / SECTOR)
        });
        let _ = writeln!(
            script,
            "start={start}{size}, type={kind}, uuid={:08x}-0000-4000-8000-00000000000{}, name=\"{name}\"",
            index.saturating_add(1).saturating_mul(0x1000_0000),
            index.saturating_add(1)
        );
    }
    script
}

/// Which extents an install in `layout` must leave untouched. A prepared
/// install writes boot entries into the shared EFI partition it reuses, so
/// only the Linux filesystem is preserved there; a guided install adds its
/// own EFI partition, so the foreign one is preserved too.
fn preserved_names(layout: Layout) -> &'static [&'static str] {
    match layout {
        Layout::Erase => &[],
        Layout::PreparedAlongside => &["Linux sentinel"],
        Layout::GuidedAlongside => &["shared EFI", "Linux sentinel"],
    }
}

/// Build the alongside target as a sparse raw image at `raw`, returning the
/// extents to verify after the install. The caller creates a qcow2 child with
/// this immutable raw image as its backing file.
pub fn build(
    layout: Layout,
    raw: &Path,
    scratch: &Path,
    tools: &mut dyn Tools,
) -> Result<Vec<Preserved>, String> {
    std::fs::File::create(raw)
        .and_then(|file| file.set_len(TARGET_BYTES))
        .map_err(io("creating the target image"))?;
    tools.run(
        "sfdisk",
        &[&raw.display().to_string()],
        Some(&sfdisk_script(layout)),
    )?;
    for (name, start, size, _, filesystem) in partitions(layout) {
        let (Some(size), Some(filesystem)) = (size, filesystem) else {
            continue;
        };
        let image = scratch.join(format!("{}.img", name.replace(' ', "-")));
        std::fs::File::create(&image)
            .and_then(|file| file.set_len(size.saturating_mul(MIB)))
            .map_err(io("creating a partition image"))?;
        let path = image.display().to_string();
        match filesystem {
            "vfat" => tools.run("mkfs.vfat", &["-n", "SHARED-EFI", &path], None)?,
            _ => tools.run(
                "mkfs.ext4",
                &["-q", "-F", "-L", "LINUX-SENTINEL", &path],
                None,
            )?,
        }
        copy_into(&image, raw, start.saturating_mul(MIB)).map_err(io("placing a partition"))?;
        std::fs::remove_file(&image).map_err(io("removing a partition image"))?;
    }
    let mut preserved = Vec::new();
    for (name, start, size, _, _) in partitions(layout) {
        if !preserved_names(layout).contains(&name) {
            continue;
        }
        let offset = start.saturating_mul(MIB);
        let length = size.unwrap_or(0).saturating_mul(MIB);
        preserved.push(Preserved {
            name: name.to_owned(),
            offset,
            length,
            sha256: hash_range(raw, offset, length).map_err(io("hashing an extent"))?,
        });
    }
    Ok(preserved)
}

/// Re-hash every preserved extent of the installed raw image. Returns the
/// names of the extents that changed.
pub fn changed(raw: &Path, preserved: &[Preserved]) -> std::io::Result<Vec<String>> {
    let mut changed = Vec::new();
    for extent in preserved {
        if hash_range(raw, extent.offset, extent.length)? != extent.sha256 {
            changed.push(extent.name.clone());
        }
    }
    Ok(changed)
}

/// Re-hash only the preserved extents through the installed qcow2 chain.
/// Reading those ranges avoids converting the entire target back to raw.
pub fn changed_cow(
    qcow2: &Path,
    scratch: &Path,
    preserved: &[Preserved],
    tools: &mut dyn Tools,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(scratch).map_err(io("creating verification scratch"))?;
    let mut changed = Vec::new();
    for (index, extent) in preserved.iter().enumerate() {
        if extent.offset % MIB != 0 || extent.length % MIB != 0 {
            return Err(format!("{} is not MiB-aligned", extent.name));
        }
        let extracted = scratch.join(format!("preserved-{index}.raw"));
        let input = format!("if={}", qcow2.display());
        let output = format!("of={}", extracted.display());
        let skip = format!("skip={}", extent.offset / MIB);
        let count = format!("count={}", extent.length / MIB);
        tools.run(
            "qemu-img",
            &[
                "dd",
                "-f",
                "qcow2",
                "-O",
                "raw",
                &input,
                &output,
                "bs=1048576",
                &skip,
                &count,
            ],
            None,
        )?;
        let result = (|| {
            let actual_len = std::fs::metadata(&extracted)
                .map_err(io("checking extracted extent length"))?
                .len();
            if actual_len != extent.length {
                return Err(format!(
                    "{} extraction has {actual_len} bytes, expected {}",
                    extent.name, extent.length
                ));
            }
            hash_range(&extracted, 0, extent.length).map_err(io("hashing an extracted extent"))
        })();
        let _ = std::fs::remove_file(&extracted);
        if result? != extent.sha256 {
            changed.push(extent.name.clone());
        }
    }
    Ok(changed)
}

fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> String {
    move |error| format!("{what}: {error}")
}

fn copy_into(source: &Path, target: &Path, offset: u64) -> std::io::Result<()> {
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::OpenOptions::new().write(true).open(target)?;
    output.seek(SeekFrom::Start(offset))?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()
}

fn hash_range(path: &Path, offset: u64, length: u64) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut hasher = Sha256::new();
    let mut remaining = length;
    let mut buffer = vec![0_u8; 1024 * 1024];
    while remaining > 0 {
        let want = usize::try_from(remaining.min(1024 * 1024)).unwrap_or(1024 * 1024);
        let chunk = buffer.get_mut(..want).unwrap_or_default();
        let read = file.read(chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "preserved extent ends before its declared length",
            ));
        }
        hasher.update(chunk.get(..read).unwrap_or_default());
        remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Where the fixture's scratch images live, beside the target.
pub fn scratch_dir(cycle: &Path) -> PathBuf {
    cycle.join("fixture")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[derive(Default)]
    struct Recorder(Vec<String>);

    #[cfg(unix)]
    impl Tools for Recorder {
        fn run(&mut self, program: &str, args: &[&str], _: Option<&str>) -> Result<(), String> {
            self.0.push(format!("{program} {}", args.join(" ")));
            if program.starts_with("mkfs")
                && let Some(path) = args.last()
            {
                let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
                file.write_all(program.as_bytes()).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn layouts_leave_the_space_the_installer_needs() {
        let prepared = sfdisk_script(Layout::PreparedAlongside);
        assert!(prepared.starts_with("label: gpt\n"));
        assert_eq!(prepared.lines().count(), 4);
        assert!(prepared.contains("name=\"Tilo prepared root\""));
        let guided = sfdisk_script(Layout::GuidedAlongside);
        assert_eq!(guided.lines().count(), 3);
        // The guided layout ends after 1025 MiB of a 40 GiB disk, well past
        // the 34 GiB of unallocated space the installer requires.
        const { assert!(TARGET_BYTES - 1025 * MIB > 34 * 1024 * MIB) };
        assert_eq!(sfdisk_script(Layout::Erase), "label: gpt\n");
    }

    // build() creates a 40 GiB sparse raw fixture with Linux disk tools.
    // Windows does not make that file sparse with set_len, so never run it there.
    #[cfg(unix)]
    #[test]
    fn preserved_extents_are_hashed_and_a_change_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("target.raw");
        let mut tools = Recorder::default();
        let preserved = build(Layout::GuidedAlongside, &raw, dir.path(), &mut tools).unwrap();
        assert_eq!(
            preserved
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["shared EFI", "Linux sentinel"]
        );
        assert!(tools.0.iter().any(|call| call.starts_with("sfdisk ")));
        assert!(changed(&raw, &preserved).unwrap().is_empty());

        let mut file = std::fs::OpenOptions::new().write(true).open(&raw).unwrap();
        file.seek(SeekFrom::Start(513 * MIB + 4096)).unwrap();
        file.write_all(b"overwritten").unwrap();
        drop(file);
        assert_eq!(changed(&raw, &preserved).unwrap(), ["Linux sentinel"]);
    }
}

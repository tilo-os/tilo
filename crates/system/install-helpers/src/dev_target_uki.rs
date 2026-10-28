//! `tilo-installer-offline-target dev-target-uki validate-image|promote`:
//! project one exact CI development deployment into an unsigned target UKI.
//!
//! Only the disposable installer VM admits this route. The OCI image and
//! installed composefs tree remain unchanged; the ESP entry and origin boot
//! representation use installed root discovery, retaining bootc's
//! SHA256(kernel + initrd) identity.
//!
//! A split-kernel target arrives with a UKI the CI runner built from its exact
//! kernel, initramfs and pinned DTB (`os/dev/dev-target-uki.py`), carried
//! beside the OCI layout. The live medium has no UKI constructor: it checks
//! that UKI's payload against the selected image and writes only the installed
//! command line into the command-line room the runner reserved.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::Value;
use tilo_board_successor::profile::{Root, resolve_profile};
use tilo_esp_write::uki;

use crate::args::{self, Arg, Command};
use crate::ini::Ini;
use crate::py::{sha256_hex, split, strip, universal_newlines};
use crate::target_transport::is_hex;
use crate::{Error, Result, require};
use tilo_pycompat::splitlines;

/// The command line, after `tilo-installer-offline-target dev-target-uki`.
pub const COMMAND: Command = Command {
    prog: "tilo-installer-offline-target dev-target-uki",
    usage: "usage: tilo-installer-offline-target dev-target-uki [-h] --image-root IMAGE_ROOT
                                                   [--target-root TARGET_ROOT]
                                                   [--esp ESP]
                                                   {validate-image,promote}
",
    help: "
Project one exact CI development deployment into an unsigned target UKI.

positional arguments:
  {validate-image,promote}

options:
  -h, --help            show this help message and exit
  --image-root IMAGE_ROOT
  --target-root TARGET_ROOT
  --esp ESP
",
    args: &[
        Arg::Positional("command"),
        Arg::Option {
            flag: "--image-root",
            required: true,
        },
        Arg::Option {
            flag: "--target-root",
            required: false,
        },
        Arg::Option {
            flag: "--esp",
            required: false,
        },
    ],
};

/// The `/boot` mount every installed Tilo UKI carries.
pub const MOUNT: &str = "systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec";
/// `os/dev/dev-target-uki.py` reserves the command-line section with this one
/// token; the installer replaces it with the installed command line.
pub const CMDLINE_ROOM: &str = "tilo.dev-target-cmdline-room";
const KERNEL_CMDLINE_MAX: usize = 2047;
const TIMEOUT: Duration = Duration::from_secs(180);
const PAYLOAD: [&str; 4] = [".linux", ".initrd", ".cmdline", ".dtb"];

/// The installer VM the projection runs in. The provided methods are the
/// production behaviour; tests replace them.
pub trait Host {
    /// A tool's stripped standard output (180 s timeout, nonzero exit
    /// refused with its standard error).
    ///
    /// # Errors
    /// The tool failing, missing or timing out.
    fn command(&self, argv: &[&str]) -> Result<String> {
        let output = crate::process::run(argv, None, TIMEOUT)?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostic: String = strip(&stderr).chars().take(1024).collect();
        require(
            output.success(),
            &format!(
                "{} failed (exit {}): {}",
                crate::process::name(argv.first().copied().unwrap_or_default()),
                output.returncode(),
                if diagnostic.is_empty() {
                    "no diagnostic stderr"
                } else {
                    &diagnostic
                }
            ),
        )?;
        Ok(strip(&String::from_utf8_lossy(&output.stdout)).to_owned())
    }

    /// Whether this process runs as root.
    fn is_root(&self) -> bool {
        rustix::process::geteuid().is_root()
    }

    /// The marker only the VM-only development installer carries.
    fn marker(&self) -> PathBuf {
        PathBuf::from("/usr/share/tilo/unverified-vm-install")
    }

    /// The live medium's image flavour.
    fn flavour(&self) -> PathBuf {
        PathBuf::from("/usr/lib/tilo/image-flavour")
    }

    /// Whether the install proof's dedicated virtio port is present.
    fn proof_port(&self) -> bool {
        fs::metadata("/dev/virtio-ports/org.tilo.vm-install-proof")
            .is_ok_and(|info| info.file_type().is_char_device())
    }

    /// What `tilo-install` asks of the installed boot (`REQUIRE_TPM_PREDICTION`).
    fn require_tpm_prediction(&self) -> String {
        std::env::var("REQUIRE_TPM_PREDICTION").unwrap_or_else(|_| "0".into())
    }

    /// The selected development target's receipt.
    fn receipt(&self) -> PathBuf {
        PathBuf::from("/run/tilo-installer/offline-target.json")
    }

    /// Where the runner-built UKI arrives beside the target's OCI layout.
    fn transport_uki(&self) -> PathBuf {
        PathBuf::from("/var/lib/tilo-installer/target-oci/target-uki.efi")
    }

    /// Who must own the image's hardware profile inputs.
    fn image_owner(&self) -> u32 {
        0
    }

    fn has_ukify(&self) -> bool {
        std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|directory| {
                let candidate = directory.join("ukify");
                fs::metadata(&candidate)
                    .is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
            })
        })
    }
}

/// The installer VM as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |error| Error::new(format!("{error}: '{}'", path.display()))
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(io(path))
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = read(path)?;
    let text = String::from_utf8(bytes).map_err(|e| Error::new(e.to_string()))?;
    Ok(universal_newlines(&text))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A nonempty regular file at `relative` below `root`, reached without a
/// symlink or a `..`.
fn regular(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = fs::canonicalize(root).map_err(io(root))?;
    for part in Path::new(relative).components() {
        let Component::Normal(part) = part else {
            return Err(Error::new("unsafe target boot path"));
        };
        path.push(part);
        require(!is_symlink(&path), "target boot path contains a symlink")?;
    }
    require(
        fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0),
        &format!("missing target boot input: {relative}"),
    )?;
    Ok(path)
}

fn is_kernel_version(version: &str) -> bool {
    let mut chars = version.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

/// The payload sections of the ARM64 UKI at `path`, each its virtual bytes.
fn sections(path: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let data = read(path)?;
    let mut result = BTreeMap::new();
    for section in uki::section_table(&data).map_err(|e| Error::new(e.0))? {
        if !PAYLOAD.contains(&section.name.as_str()) {
            continue;
        }
        let offset = section.raw_offset as usize;
        let size = section.virtual_size as usize;
        let content = offset
            .checked_add(size)
            .filter(|_| {
                !result.contains_key(&section.name)
                    && size > 0
                    && section.virtual_size <= section.raw_size
            })
            .and_then(|end| data.get(offset..end))
            .ok_or_else(|| Error::new("invalid generated UKI section"))?;
        result.insert(section.name, content.to_vec());
    }
    require(
        uki::machine(&data).ok() == Some(0xaa64),
        "generated target UKI is not ARM64",
    )?;
    Ok(result)
}

fn trimmed(bytes: Option<&Vec<u8>>) -> &[u8] {
    let bytes = bytes.map_or(&[][..], Vec::as_slice);
    let end = bytes
        .iter()
        .rposition(|b| !b"\0\n".contains(b))
        .map_or(0, |at| at.saturating_add(1));
    bytes.get(..end).unwrap_or_default()
}

/// Whether the UKI carries exactly the pinned DTB, or none when there is none.
fn same_dtb(actual: &BTreeMap<String, Vec<u8>>, dtb: Option<&[u8]>) -> bool {
    actual.get(".dtb").map(Vec::as_slice) == dtb
}

/// What the selected image boots: its kernel version, kernel, initramfs and
/// pinned DTB.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub version: String,
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    pub dtb: Option<PathBuf>,
}

impl Inputs {
    fn dtb_bytes(&self) -> Result<Option<Vec<u8>>> {
        self.dtb.as_deref().map(read).transpose()
    }
}

/// `validate-image`: the selected image's boot inputs, checked before any
/// disk is touched.
///
/// # Errors
/// Not a development image, not exactly one kernel, an invalid hardware
/// profile, or a sealed or transported UKI that differs from the image.
pub fn image_inputs(host: &dyn Host, root: &Path) -> Result<Inputs> {
    require(
        read(&regular(root, "usr/lib/tilo/image-flavour")?)? == b"dev\n",
        "target is not development flavour",
    )?;
    let modules = root.join("usr/lib/modules");
    let mut versions = Vec::new();
    for entry in fs::read_dir(&modules).map_err(io(&modules))? {
        let entry = entry.map_err(io(&modules))?;
        let path = entry.path();
        if path.is_dir() && !is_symlink(&path) {
            versions.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    let [version] = versions.as_slice() else {
        return Err(Error::new("development target requires exactly one kernel"));
    };
    require(
        is_kernel_version(version),
        "development target requires exactly one kernel",
    )?;
    let mut kernel_directory = format!("usr/lib/modules/{version}");
    if !root.join(&kernel_directory).join("vmlinuz").exists()
        && !root.join(&kernel_directory).join("initramfs.img").exists()
    {
        kernel_directory = format!("kernel/{version}");
    }
    let kernel = regular(root, &format!("{kernel_directory}/vmlinuz"))?;
    let initrd = regular(root, &format!("{kernel_directory}/initramfs.img"))?;
    require(
        !root.join("boot/tilo-target").exists(),
        "development target already has a target boot bundle",
    )?;
    let image = Root {
        path: root.to_path_buf(),
        owner: host.image_owner(),
    };
    let dtb = resolve_profile(&image, version, "aarch64").map_err(|e| Error::new(e.to_string()))?;
    let inputs = Inputs {
        version: version.clone(),
        kernel,
        initrd,
        dtb,
    };
    let mut ukis = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("boot/EFI/Linux")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name
                .rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix == "efi")
            {
                ukis.push(regular(root, &format!("boot/EFI/Linux/{name}"))?);
            }
        }
    }
    require(ukis.len() <= 1, "development target has ambiguous UKIs")?;
    require(
        !kernel_directory.starts_with("kernel/") || ukis.len() == 1,
        "split development target requires one nonempty sealed UKI",
    )?;
    let transported = transported_uki(host, &inputs)?;
    require(
        ukis.is_empty() || transported.is_none(),
        "development target has both a sealed and a transported UKI",
    )?;
    if ukis.is_empty() && transported.is_none() {
        require(
            host.has_ukify(),
            "split-kernel target requires the runtime ukify constructor before installation",
        )?;
    }
    if let Some(sealed) = ukis.first() {
        let actual = sections(sealed)?;
        require(
            actual.get(".linux") == Some(&read(&inputs.kernel)?)
                && actual.get(".initrd") == Some(&read(&inputs.initrd)?),
            "CI development UKI differs from its split kernel payload",
        )?;
        require(
            same_dtb(&actual, inputs.dtb_bytes()?.as_deref()),
            "CI development UKI differs from its pinned hardware DTB",
        )?;
    }
    Ok(inputs)
}

/// The runner-built UKI beside the OCI layout, checked against the image.
fn transported_uki(host: &dyn Host, inputs: &Inputs) -> Result<Option<PathBuf>> {
    let path = host.transport_uki();
    if !lexists(&path) {
        return Ok(None);
    }
    require(
        !is_symlink(&path) && fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0),
        "transported development UKI is unsafe",
    )?;
    let actual = sections(&path)?;
    require(
        actual.get(".linux") == Some(&read(&inputs.kernel)?)
            && actual.get(".initrd") == Some(&read(&inputs.initrd)?),
        "transported development UKI differs from the selected split kernel payload",
    )?;
    require(
        same_dtb(&actual, inputs.dtb_bytes()?.as_deref()),
        "transported development UKI differs from its pinned hardware DTB",
    )?;
    let reserved: String = trimmed(actual.get(".cmdline"))
        .iter()
        .map(|&b| {
            if b.is_ascii() {
                char::from(b)
            } else {
                '\u{fffd}'
            }
        })
        .collect();
    let reserved = split(&reserved);
    require(
        matches!(reserved.as_slice(), [token] if token.starts_with(&format!("{CMDLINE_ROOM}="))),
        "transported development UKI has no reserved command-line room",
    )?;
    Ok(Some(path))
}

/// Replaces a reserved `.cmdline` in place; no other PE section moves.
fn write_cmdline(path: &Path, cmdline: &str) -> Result<()> {
    let mut data = read(path)?;
    let room = uki::find_cmdline_room(&data).map_err(|e| Error::new(e.0))?;
    let encoded = cmdline.as_bytes();
    require(
        !encoded.is_empty() && encoded.len() <= room.room.min(KERNEL_CMDLINE_MAX),
        &format!(
            "installed command line needs {} bytes; the transported UKI reserved {}",
            encoded.len(),
            room.room
        ),
    )?;
    uki::write_section(&mut data, &room, encoded).map_err(|e| Error::new(e.0))?;
    fs::write(path, data).map_err(io(path))
}

/// The keys of one split-kernel BLS entry.
fn fields(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for line in splitlines(raw) {
        let line = strip(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts = line.split_once(' ');
        let key = parts.map_or(line, |(key, _)| key);
        require(
            parts.is_some() && !values.contains_key(key),
            "ambiguous installed BLS entry",
        )?;
        let value = parts.map_or("", |(_, value)| value);
        values.insert(key.to_owned(), strip(value).to_owned());
    }
    require(
        ["linux", "initrd", "options"]
            .iter()
            .all(|key| values.contains_key(*key))
            && !values.contains_key("efi")
            && !values.contains_key("uki"),
        "expected one split-kernel BLS entry",
    )?;
    Ok(values)
}

/// Drops the words `bootc install` adds to name this disk's filesystems.
///
/// A split-kernel install leaves a BLS entry whose options start with
/// `root=UUID=<fs> rootflags=... rw boot=UUID=<boot>` and the matching
/// `systemd.mount-extra=UUID=<boot>:/boot:auto:ro`, ahead of the image's own
/// kargs. The installed UKI finds its root and /boot the way every Tilo UKI
/// does (root=tilo-auto and the `TILO_BOOT` mount), so only those words go;
/// any other root or /boot mount still fails the identity checks.
fn without_install_disk_identity(words: Vec<String>) -> Result<Vec<String>> {
    let roots: Vec<&String> = words
        .iter()
        .filter(|w| w.starts_with("root=UUID="))
        .collect();
    let boots: Vec<&String> = words
        .iter()
        .filter(|w| w.starts_with("boot=UUID="))
        .collect();
    require(
        roots.len() <= 1 && boots.len() <= 1,
        "installed boot entry has ambiguous bootc disk identities",
    )?;
    let mut dropped: Vec<String> = roots.iter().chain(&boots).map(|w| (*w).clone()).collect();
    if let Some(boot) = boots.first() {
        let uuid = boot.strip_prefix("boot=").unwrap_or(boot);
        dropped.push(format!("systemd.mount-extra={uuid}:/boot:auto:ro"));
    }
    let has_root = !roots.is_empty();
    Ok(words
        .into_iter()
        .filter(|w| {
            !dropped.contains(w) && !(has_root && (w == "rw" || w.starts_with("rootflags=")))
        })
        .collect())
}

/// The CI development image the receipt selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub target_digest: String,
    pub update_origin: Option<String>,
}

/// Refuses outside the development installer VM; returns the verified
/// development target's selection.
///
/// # Errors
/// Not root, not the VM-only development installer, a TPM prediction
/// requested, or no verified development target receipt.
pub fn gate(host: &dyn Host) -> Result<Selected> {
    let marker = host.marker();
    let flavour = host.flavour();
    require(
        host.is_root()
            && marker.is_file()
            && !is_symlink(&marker)
            && flavour.is_file()
            && !is_symlink(&flavour)
            && fs::read(&flavour).is_ok_and(|bytes| bytes == b"dev\n")
            && host.proof_port(),
        "unsigned target UKI requires the development installer VM",
    )?;
    host.command(&["systemd-detect-virt", "--vm", "--quiet"])?;
    require(
        host.require_tpm_prediction() == "0",
        "unsigned target UKI cannot provide TPM prediction",
    )?;
    let receipt = host.receipt();
    require(
        receipt.is_file()
            && !is_symlink(&receipt)
            && fs::metadata(&receipt).is_ok_and(|m| m.len() <= 8192),
        "development target receipt missing or unsafe",
    )?;
    let document: Value =
        serde_json::from_str(&read_text(&receipt)?).map_err(|e| Error::new(e.to_string()))?;
    let string = |key: &str| document.get(key).and_then(Value::as_str);
    let digest = string("target_digest").unwrap_or_default();
    require(
        string("schema") == Some("tilo.installer-dev-target.v1")
            && digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| is_hex(hex, 64))
            && string("target_ref") == Some(&format!("registry.tilo.so/tilo/tilo-vm@{digest}")),
        "unsigned UKI requires the verified development target",
    )?;
    Ok(Selected {
        target_digest: digest.to_owned(),
        update_origin: string("update_origin").map(str::to_owned),
    })
}

/// How the installed deployment boots, and what its target UKI changes.
struct Installed {
    /// `bls` (split kernel) or `uki`.
    boot_type: String,
    /// For a UKI install: whether its sealed command line has `tilo.install`.
    sealed_install: bool,
    /// Whether the `TILO_BOOT` mount is added (the install had no /boot mount).
    add_mount: bool,
}

fn installed_bls(entry: &Path, esp: &Path, kernel: &[u8], initrd: &[u8]) -> Result<Vec<String>> {
    let bls = fields(&read_text(entry)?)?;
    let field = |key: &str| bls.get(key).map_or("", String::as_str);
    let installed_kernel = regular(esp, field("linux").trim_start_matches('/'))?;
    let installed_initrd = regular(esp, field("initrd").trim_start_matches('/'))?;
    require(
        read(&installed_kernel)? == kernel && read(&installed_initrd)? == initrd,
        "installed boot payload differs from the selected CI development image",
    )?;
    without_install_disk_identity(
        split(field("options"))
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
}

fn installed_uki(
    entry: &Path,
    esp: &Path,
    relative: &str,
    kernel: &[u8],
    initrd: &[u8],
    dtb: Option<&[u8]>,
) -> Result<(Vec<String>, bool)> {
    let installed = sections(&regular(esp, relative)?)?;
    require(
        installed.get(".linux").map(Vec::as_slice) == Some(kernel)
            && installed.get(".initrd").map(Vec::as_slice) == Some(initrd),
        "installed UKI differs from the selected CI development image",
    )?;
    require(
        same_dtb(&installed, dtb),
        "installed UKI differs from immutable hardware DTB",
    )?;
    let sealed = std::str::from_utf8(trimmed(installed.get(".cmdline")))
        .map_err(|e| Error::new(e.to_string()))?;
    let words: Vec<String> = split(sealed).into_iter().map(str::to_owned).collect();
    let mut values = BTreeMap::new();
    for line in splitlines(&read_text(entry)?) {
        if let Some((key, value)) = line.split_once(' ') {
            require(
                !values.contains_key(key),
                "ambiguous installed UKI loader entry",
            )?;
            values.insert(key.to_owned(), strip(value).to_owned());
        }
    }
    let selected = values.get("efi").or_else(|| values.get("uki"));
    require(
        selected.map(String::as_str) == Some(&format!("/{relative}"))
            && !["linux", "initrd", "options"]
                .iter()
                .any(|key| values.contains_key(*key)),
        "installed loader entry selects a different UKI",
    )?;
    let sealed_install = words.iter().any(|w| w == "tilo.install");
    Ok((words, sealed_install))
}

/// Copies every extended attribute of `from` (its `SELinux` label among them)
/// onto `to`.
fn copy_xattrs(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut names = vec![0u8; 65536];
    let length = rustix::fs::listxattr(from, &mut names[..])?;
    for name in names.get(..length).unwrap_or_default().split(|&b| b == 0) {
        if name.is_empty() {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(name);
        let mut value = vec![0u8; 65536];
        let size = rustix::fs::getxattr(from, name, &mut value[..])?;
        rustix::fs::setxattr(
            to,
            name,
            value.get(..size).unwrap_or_default(),
            rustix::fs::XattrFlags::empty(),
        )?;
    }
    Ok(())
}

/// Rewrites the split-kernel BLS entry to select the new UKI, and the
/// origin's boot representation to `uki`: manifest, origin and digest stay.
fn transition_to_uki(entry: &Path, origin: &Path, ini: &mut Ini, relative: &str) -> Result<()> {
    let text = read_text(entry)?;
    let kept: Vec<&str> = splitlines(&text)
        .into_iter()
        .filter(|line| {
            let key = line.split(' ').next().unwrap_or_default();
            !["linux", "initrd", "options"].contains(&key)
        })
        .collect();
    let mut name = entry.as_os_str().to_owned();
    name.push(".tmp");
    let new_entry = PathBuf::from(name);
    fs::write(
        &new_entry,
        format!("{}\nefi /{relative}\n", kept.join("\n")),
    )
    .map_err(io(&new_entry))?;
    fs::set_permissions(&new_entry, fs::Permissions::from_mode(0o644)).map_err(io(&new_entry))?;
    fs::rename(&new_entry, entry).map_err(io(entry))?;
    // bootc 1.16.4 boot.rs hashes identical kernel+initrd bytes for bls and
    // uki. Transition only the representation; retain manifest, origin and
    // digest.
    ini.set("boot", "boot_type", "uki")?;
    let mut name = origin.as_os_str().to_owned();
    name.push(".tmp");
    let temporary = PathBuf::from(name);
    fs::write(&temporary, ini.write()).map_err(io(&temporary))?;
    let info = fs::metadata(origin).map_err(io(origin))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(info.mode() & 0o777))
        .map_err(io(&temporary))?;
    std::os::unix::fs::chown(&temporary, Some(info.uid()), Some(info.gid()))
        .map_err(io(&temporary))?;
    copy_xattrs(origin, &temporary)?;
    fs::rename(&temporary, origin).map_err(io(origin))
}

/// Writes the target UKI for `cmdline` to `destination`, through private
/// scratch on the ESP: from the installed UKI, the runner's UKI, or `ukify`.
fn project(
    host: &dyn Host,
    image_root: &Path,
    esp: &Path,
    inputs: &Inputs,
    transported: Option<&Path>,
    installed: &Installed,
    cmdline: &str,
    destination: &Path,
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(io(parent))?;
    }
    // Only private scratch and final ESP bytes are written. No image commit,
    // tag, or rootfs layer is created, and no target identity fields are
    // altered.
    let work = tempfile::Builder::new()
        .prefix("tilo-dev-target-")
        .tempdir_in(esp)
        .map_err(io(esp))?;
    let cmdline_file = work.path().join("cmdline");
    fs::write(&cmdline_file, format!("{cmdline}\n")).map_err(io(&cmdline_file))?;
    let output = work.path().join("target.efi");
    if installed.boot_type == "uki" {
        // The CI UKI already contains every verified payload. Change only its
        // bounded command-line section with the runtime editor that ships in
        // this installer; rebuilding needs absent build-time tools and would
        // discard unrelated CI PE sections.
        fs::copy(destination, &output).map_err(io(destination))?;
        if installed.sealed_install {
            uki::remove_cmdline(&output, b"tilo.install").map_err(|e| Error::new(e.0))?;
        }
        if installed.add_mount {
            uki::append_cmdline(&output, MOUNT.as_bytes()).map_err(|e| Error::new(e.0))?;
        }
    } else if let Some(transported) = transported {
        // The runner built this UKI from the same verified payload; only the
        // installed command line is written into its reserved room.
        fs::copy(transported, &output).map_err(io(transported))?;
        write_cmdline(&output, cmdline)?;
    } else {
        let stub = regular(image_root, "usr/lib/systemd/boot/efi/linuxaa64.efi.stub")?;
        let os_release = regular(image_root, "usr/lib/os-release")?;
        let mut argv = vec![
            "ukify".to_owned(),
            "build".into(),
            "--linux".into(),
            text(&inputs.kernel),
            "--initrd".into(),
            text(&inputs.initrd),
            "--stub".into(),
            text(&stub),
            "--uname".into(),
            inputs.version.clone(),
            "--os-release".into(),
            format!("@{}", text(&os_release)),
            "--cmdline".into(),
            format!("@{}", text(&cmdline_file)),
            "--output".into(),
            text(&output),
        ];
        if let Some(dtb) = &inputs.dtb {
            argv.extend(["--devicetree".into(), text(dtb)]);
        }
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        host.command(&argv)?;
    }
    let actual = sections(&output)?;
    require(
        actual.get(".linux") == Some(&read(&inputs.kernel)?)
            && actual.get(".initrd") == Some(&read(&inputs.initrd)?)
            && trimmed(actual.get(".cmdline")) == cmdline.as_bytes(),
        "generated target UKI changed kernel, initramfs or command line",
    )?;
    require(
        same_dtb(&actual, inputs.dtb_bytes()?.as_deref()),
        "generated target UKI differs from the immutable hardware DTB",
    )?;
    fs::set_permissions(&output, fs::Permissions::from_mode(0o644)).map_err(io(&output))?;
    fs::rename(&output, destination).map_err(io(destination))?;
    drop(work);
    Ok(())
}

/// The one installed composefs deployment's digest.
fn deployment(target_root: &Path) -> Result<String> {
    let deploy = target_root.join("state/deploy");
    let mut deployments = Vec::new();
    if let Ok(entries) = fs::read_dir(&deploy) {
        for entry in entries.flatten() {
            if lexists(&entry.path().join("etc")) {
                deployments.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    match deployments.as_slice() {
        [digest] if is_hex(digest, 128) => Ok(digest.clone()),
        _ => Err(Error::new(
            "development target requires one installed composefs deployment",
        )),
    }
}

/// The origin's boot type, once its manifest and origin are the selected
/// CI development image's.
fn installed_boot_type(ini: &Ini, selected: &Selected) -> Result<String> {
    let same_manifest = ini.get("image", "manifest_digest")? == selected.target_digest;
    let same_origin = same_manifest && {
        let update_origin = selected
            .update_origin
            .as_deref()
            .ok_or_else(|| Error::new("'update_origin'"))?;
        ini.get("origin", "container-image-reference")?
            == format!("ostree-unverified-image:docker://{update_origin}")
    };
    let boot_type = if same_origin {
        ini.get("boot", "boot_type")?
    } else {
        String::new()
    };
    require(
        boot_type == "bls" || boot_type == "uki",
        "installed origin differs from the selected CI development image",
    )?;
    Ok(boot_type)
}

fn loader_entry(esp: &Path) -> Result<PathBuf> {
    let entries_dir = esp.join("loader/entries");
    let mut entries = Vec::new();
    if let Ok(found) = fs::read_dir(&entries_dir) {
        for entry in found.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name
                .rsplit_once('.')
                .is_some_and(|(_, suffix)| suffix == "conf")
            {
                entries.push(name);
            }
        }
    }
    let [entry_name] = entries.as_slice() else {
        return Err(Error::new("expected exactly one installed BLS entry"));
    };
    regular(esp, &format!("loader/entries/{entry_name}"))
}

/// `promote`: writes the installed deployment's target UKI onto `esp` and
/// returns its composefs digest.
///
/// # Errors
/// Any boot input, origin or loader entry that differs from the selected CI
/// development image; nothing is changed then.
pub fn promote(
    host: &dyn Host,
    image_root: &Path,
    target_root: &Path,
    esp: &Path,
    selected: &Selected,
) -> Result<String> {
    let inputs = image_inputs(host, image_root)?;
    let transported = transported_uki(host, &inputs)?;
    let digest = deployment(target_root)?;
    let origin = regular(
        target_root,
        &format!("state/deploy/{digest}/{digest}.origin"),
    )?;
    let mut ini = Ini::parse(&read_text(&origin)?)?;
    let boot_type = installed_boot_type(&ini, selected)?;
    let entry = loader_entry(esp)?;
    let kernel = read(&inputs.kernel)?;
    let initrd = read(&inputs.initrd)?;
    let dtb = inputs.dtb_bytes()?;
    let relative = format!("EFI/Linux/bootc/bootc_composefs-{digest}.efi");
    let destination = esp.join(&relative);
    let (words, sealed_install) = if boot_type == "bls" {
        let words = installed_bls(&entry, esp, &kernel, &initrd)?;
        require(
            !destination.exists() && !is_symlink(&destination),
            "target UKI already exists",
        )?;
        (words, false)
    } else {
        installed_uki(&entry, esp, &relative, &kernel, &initrd, dtb.as_deref())?
    };
    require(
        ini.get("boot", "digest")? == sha256_hex(&[kernel.as_slice(), &initrd].concat()),
        "installed bootc payload digest differs",
    )?;
    let with = |prefix: &str| -> Vec<&str> {
        words
            .iter()
            .map(String::as_str)
            .filter(|w| w.starts_with(prefix))
            .collect()
    };
    require(
        with("composefs=") == [format!("composefs={digest}").as_str()]
            && with("root=") == ["root=tilo-auto"],
        "installed boot entry has a different root or composefs identity",
    )?;
    let boot_mounts: Vec<&str> = with("systemd.mount-extra=")
        .into_iter()
        .filter(|w| w.split(':').nth(1) == Some("/boot"))
        .collect();
    require(
        boot_mounts.len() <= 1 && boot_mounts.iter().all(|w| *w == MOUNT),
        "installed boot entry has a conflicting /boot mount",
    )?;
    let installed = Installed {
        boot_type,
        sealed_install,
        add_mount: boot_mounts.is_empty(),
    };
    let mut words: Vec<String> = words.into_iter().filter(|w| w != "tilo.install").collect();
    if installed.add_mount {
        words.push(MOUNT.to_owned());
    }
    let cmdline = words.join(" ");
    project(
        host,
        image_root,
        esp,
        &inputs,
        transported.as_deref(),
        &installed,
        &cmdline,
        &destination,
    )?;
    if installed.boot_type == "bls" {
        transition_to_uki(&entry, &origin, &mut ini, &relative)?;
    }
    Ok(digest)
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("{}{}: error: {message}", COMMAND.usage, COMMAND.prog);
    ExitCode::from(2)
}

/// The command line after `dev-target-uki`: a refusal exits 1, a usage error 2.
pub fn main(host: &dyn Host, args: &[OsString]) -> ExitCode {
    let values = match args::parse(&COMMAND, args) {
        Ok(args::Parsed::Help(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(usage) => {
            eprint!("{usage}");
            return ExitCode::from(2);
        }
        Ok(args::Parsed::Run(values)) => values,
    };
    let command = values
        .get("command")
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_default();
    if command != "validate-image" && command != "promote" {
        return usage_error(&format!(
            "argument command: invalid choice: '{command}' (choose from 'validate-image', 'promote')"
        ));
    }
    let path = |name: &str| values.get(name).map(PathBuf::from);
    let outcome = (|| {
        let selected = gate(host)?;
        let image_root = path("--image-root").unwrap_or_default();
        if command == "validate-image" {
            image_inputs(host, &image_root).map(|_| None)
        } else {
            let (Some(target_root), Some(esp)) = (path("--target-root"), path("--esp")) else {
                return Err(Error::new("target root and ESP required"));
            };
            promote(host, &image_root, &target_root, &esp, &selected).map(Some)
        }
    })();
    match outcome {
        Ok(digest) => {
            if let Some(digest) = digest {
                println!("{digest}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("development target UKI: {error}");
            ExitCode::FAILURE
        }
    }
}

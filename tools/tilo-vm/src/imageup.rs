//! Moving a running dev VM onto another image with bootc: this tree's dev
//! image, built by t's `os-dev-image-<arch>` check (`vm upgrade --from local`,
//! `image build`), a CI build (`--from ci:<tag>`) or any dev image from a
//! registry. A built image travels as an OCI layout mirrored into the VM, so
//! only new blobs cross; no registry and no daemon on the host.
//!
//! t is the one builder of dev images and disks: the checks that publish
//! main's build it for this tree too, on a build server of the image's
//! architecture, so no host compiles the OS or needs podman or loop devices.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::CliError;
use crate::hypervisor::{CommandLine, SshEndpoint};
use crate::vmbuild::sh_quote;

/// The OCI layout in the VM that bootc switches to; in /var, so it survives
/// the switch, and each upgrade only adds the blobs that changed.
pub const VM_LAYOUT: &str = "/var/lib/tilo-vm/image";
pub const LAYOUT_TAG: &str = "tilo-vm";
pub fn layout_imgref() -> String {
    format!("{VM_LAYOUT}:{LAYOUT_TAG}")
}
const LOCKS: &str = "/var/lib/tilo-vm/locks";

/// Where `image build` and `vm upgrade --from local` leave t's outputs,
/// relative to the worktree.
pub const BUILD_OUT: &str = "target/tilo-vm";

/// t's check that builds this tree's dev image for `arch`. The lean one
/// (the development loop's) skips the local model runtimes and names no
/// commit; its base carries the OS image's clock rather than the commit's.
pub fn image_check(arch: &str, lean: bool) -> String {
    format!("os-dev-image-{}{arch}", if lean { "lean-" } else { "" })
}

/// t's check that installs that image onto a dev disk.
pub fn disk_check(arch: &str, lean: bool) -> String {
    format!("os-dev-disk-{}{arch}", if lean { "lean-" } else { "" })
}

/// Run `check` on this tree's state on t and download its outputs under
/// `out`; t reuses its answer when nothing the check reads changed.
pub fn outputs_command(check: &str, root: &Path, out: &Path) -> CommandLine {
    CommandLine::new("t", &["outputs"])
        .arg(check)
        .arg("--dir")
        .arg(root.display().to_string())
        .arg("--to")
        .arg(out.display().to_string())
}

/// The dev image `t outputs` left under `out`: `--to` keeps the check's own
/// output path under the directory.
pub fn built_image(out: &Path, arch: &str) -> PathBuf {
    out.join("target/publish/os-dev").join(arch)
}

/// The dev disk `t outputs` left under `out`.
pub fn built_disk(out: &Path, arch: &str) -> PathBuf {
    out.join("target/publish/os-dev-disk")
        .join(arch)
        .join("disk")
        .join("disk.qcow2")
}

/// A dev image as t's checks leave it: an OCI layout in `oci/`, its manifest
/// digest in `digest` and the commit it is of (or `development-loop`) in
/// `revision`.
pub fn is_built_image(dir: &Path) -> bool {
    is_layout(&dir.join("oci")) && dir.join("digest").is_file()
}

/// The name a built image goes by in reports and receipts: never a
/// production name.
pub fn built_name(dir: &Path) -> String {
    let revision = std::fs::read_to_string(dir.join("revision")).unwrap_or_default();
    let revision = revision.trim();
    let tag = match revision.len() {
        0 => "built",
        40 if revision.bytes().all(|b| b.is_ascii_hexdigit()) => {
            revision.get(..12).unwrap_or(revision)
        }
        _ => revision,
    };
    format!("localhost/tilo-vm:{tag}")
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{} is not JSON: {e}", path.display()))
}

fn digest_of<'a>(descriptor: &'a Value, what: &str) -> Result<&'a str, String> {
    descriptor
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("its {what} names no digest"))
}

/// The one image an OCI layout holds: its manifest descriptor, read from
/// `index.json`.
fn layout_manifest(layout: &Path) -> Result<Value, String> {
    let index = read_json(&layout.join("index.json"))?;
    match index
        .get("manifests")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        Some([one]) => Ok(one.clone()),
        Some(many) => Err(format!("holds {} images, not one", many.len())),
        None => Err("has no manifests in its index.json".into()),
    }
}

/// The facts of the image in an OCI layout, from its own configuration's
/// labels: nothing runs and nothing is unpacked.
pub fn layout_facts(layout: &Path) -> Result<ImageFacts, String> {
    let manifest = layout_manifest(layout)?;
    let path = |digest: &str| {
        blob_path(layout, digest).ok_or_else(|| format!("names an unreadable digest {digest}"))
    };
    let manifest = read_json(&path(digest_of(&manifest, "index")?)?)?;
    let config = manifest
        .get("config")
        .ok_or("its manifest names no configuration")?;
    let config = read_json(&path(digest_of(config, "manifest's configuration")?)?)?;
    Ok(facts_from_labels(config.pointer("/config/Labels")))
}

/// An image's facts from its labels: `sh.tilo.image.flavour`, whether it is
/// sealed (`sh.tilo.image.sealed`), and the production base it was built
/// FROM (`sh.tilo.base.*`, in base.json's own terms).
pub fn facts_from_labels(labels: Option<&Value>) -> ImageFacts {
    let label = |key: &str| {
        labels
            .and_then(|l| l.get(key))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let base = [
        ("base_created", "sh.tilo.base.created"),
        ("image_digest", "sh.tilo.base.digest"),
        ("base_image", "sh.tilo.base.image"),
    ]
    .into_iter()
    .filter_map(|(key, name)| label(name).map(|v| (key.to_owned(), Value::String(v))))
    .collect::<serde_json::Map<_, _>>();
    ImageFacts {
        flavour: label("sh.tilo.image.flavour"),
        sealed: label("sh.tilo.image.sealed").as_deref() == Some("true"),
        base: (!base.is_empty()).then_some(Value::Object(base)),
    }
}

/// Put a built image into `layout`, the host's copy of what the VM holds,
/// under [`LAYOUT_TAG`]: only blobs it lacks are copied, and blobs no longer
/// referenced go. Returns the image's manifest digest.
pub fn stage_built(built: &Path, layout: &Path) -> Result<String, String> {
    let source = built.join("oci");
    let mut manifest = layout_manifest(&source)?;
    let digest = digest_of(&manifest, "index")?.to_owned();
    let recorded = std::fs::read_to_string(built.join("digest")).unwrap_or_default();
    if recorded.trim() != digest {
        return Err(format!(
            "its digest file names {}, its layout {digest}",
            recorded.trim()
        ));
    }
    let io = |what: &str, path: &Path, e: std::io::Error| format!("{what} {}: {e}", path.display());
    for blob in referenced_blobs(&source).map_err(|e| io("read", &source, e))? {
        let bad = || format!("names a bad digest {blob}");
        let from = blob_path(&source, &blob).ok_or_else(bad)?;
        let to = blob_path(layout, &blob).ok_or_else(bad)?;
        if to.is_file() {
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io("create", parent, e))?;
        }
        let partial = to.with_extension("partial");
        std::fs::copy(&from, &partial).map_err(|e| io("copy", &from, e))?;
        std::fs::rename(&partial, &to).map_err(|e| io("move", &partial, e))?;
    }
    if let Some(descriptor) = manifest.as_object_mut() {
        descriptor.insert(
            "annotations".into(),
            json!({ "org.opencontainers.image.ref.name": LAYOUT_TAG }),
        );
    }
    let index = json!({ "schemaVersion": 2, "manifests": [manifest] });
    std::fs::write(
        layout.join("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .map_err(|e| io("write", layout, e))?;
    std::fs::write(
        layout.join("index.json"),
        serde_json::to_vec(&index).map_err(|e| e.to_string())?,
    )
    .map_err(|e| io("write", layout, e))?;
    prune_layout(layout).map_err(|e| io("prune", layout, e))?;
    Ok(digest)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeFrom {
    /// This worktree's dev image, built by t.
    Local,
    Ci {
        tag: String,
    },
}

const FROM_FORMS: &str = "tilo-vm vm upgrade --from local (this worktree's dev image, built by t)   |   --from ci:main-<sha7> or --from sha:<commit> (a CI build)   |   --image target/tilo-vm/target/publish/os-dev/<arch> (one tilo-vm image build left)   |   --image registry.tilo.so/tilo/tilo-vm:<tag> (a published one)";

pub fn parse_from(
    from: &str,
    worktree: Option<&crate::context::Worktree>,
) -> Result<UpgradeFrom, CliError> {
    let unsupported = || CliError::Unsupported {
        what: format!("--from {from} for vm upgrade"),
        alternative: FROM_FORMS.into(),
    };
    match from.split_once(':') {
        None if from == "local" => Ok(UpgradeFrom::Local),
        Some(("ci" | "sha", _)) => match crate::ci::parse(from, worktree) {
            Ok(crate::ci::Source::Ci { tag, .. }) => Ok(UpgradeFrom::Ci { tag }),
            _ => Err(unsupported()),
        },
        _ => Err(unsupported()),
    }
}

/// The published dev image of a CI build for an architecture: what main
/// signs (publish-os-dev-<arch>); a dev image's policy takes no unsigned index.
pub fn ci_image(tag: &str, arch: &str) -> String {
    let tag = tag.strip_suffix(&format!("-{arch}")).unwrap_or(tag);
    let dev_tag = tag.strip_suffix("-ci").unwrap_or(tag);
    format!("registry.tilo.so/tilo/tilo-vm:{dev_tag}-ci-{arch}")
}

/// A board's dev image of a CI build.
pub fn board_image(tag: &str, board: &str) -> String {
    let tag = tag.strip_suffix(&format!("-{board}")).unwrap_or(tag);
    let dev_tag = tag.strip_suffix("-ci").unwrap_or(tag);
    format!("registry.tilo.so/tilo/tilo-vm:{dev_tag}-{board}")
}

pub fn exists_command(reference: &str) -> CommandLine {
    CommandLine::new("skopeo", &["inspect", "--raw"]).arg(format!("docker://{reference}"))
}

/// A dev image may only be named `localhost/tilo-vm` or
/// `registry.tilo.so/tilo/tilo-vm`, with a tag or digest.
pub fn is_dev_name(reference: &str) -> bool {
    ["localhost/tilo-vm", "registry.tilo.so/tilo/tilo-vm"]
        .iter()
        .any(|repo| {
            reference.strip_prefix(repo).is_some_and(|rest| {
                rest.len() > 1 && (rest.starts_with(':') || rest.starts_with("@sha256:"))
            })
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFacts {
    pub flavour: Option<String>,
    pub sealed: bool,
    pub base: Option<Value>,
}

/// Did bootc refuse a switch because the VM already boots that image? The
/// same image can carry another manifest digest (compressed or not), which
/// only bootc's composefs digest sees through.
pub fn is_already_booted(bootc_stderr: &str) -> bool {
    bootc_stderr.contains("same fs-verity digest as the existing Some(Booted) deployment")
}

/// Did the registry refuse bootc's pull for want of a login? A private
/// image is then copied through this host, whose credentials never reach
/// the VM.
pub fn is_auth_failure(bootc_stderr: &str) -> bool {
    let text = bootc_stderr.to_ascii_lowercase();
    ["unauthorized", "authentication required", "denied: "]
        .iter()
        .any(|needle| text.contains(needle))
}

pub fn registry_export_command(
    image: &str,
    arch: &str,
    layout: &Path,
    digest_file: &Path,
) -> CommandLine {
    CommandLine::new(
        "skopeo",
        &["copy", "--quiet", "--dest-oci-accept-uncompressed-layers"],
    )
    .arg("--override-arch")
    .arg(crate::ci::podman_arch(arch))
    .arg("--digestfile")
    .arg(digest_file.display().to_string())
    .arg(format!("docker://{image}"))
    .arg(format!("oci:{}:{LAYOUT_TAG}", layout.display()))
}

pub fn registry_inspect_command(image: &str, arch: &str) -> CommandLine {
    CommandLine::new("skopeo", &["inspect", "--override-arch"])
        .arg(crate::ci::podman_arch(arch))
        .arg(format!("docker://{image}"))
}

/// A registry image's facts, from `skopeo inspect`'s labels.
pub fn registry_facts(inspect: &str) -> ImageFacts {
    let value = serde_json::from_str::<Value>(inspect).unwrap_or_default();
    facts_from_labels(value.get("Labels"))
}

/// bootc's refusal for want of space, as (free, needed) in its own units:
/// `Insufficient free space for <image> (available: 2.3 GB required: 5.2 GB)`.
pub fn insufficient_space(bootc_stderr: &str) -> Option<(String, String)> {
    let (_, rest) = bootc_stderr.split_once("Insufficient free space for ")?;
    let (_, rest) = rest.split_once("(available: ")?;
    let (free, rest) = rest.split_once(" required: ")?;
    let (needed, _) = rest.split_once(')')?;
    Some((free.trim().to_owned(), needed.trim().to_owned()))
}

pub fn is_already_rollback(bootc_stderr: &str) -> bool {
    bootc_stderr.contains("same fs-verity digest as the existing Some(Rollback) deployment")
}

pub fn is_layout(path: &Path) -> bool {
    path.join("oci-layout").is_file() && path.join("index.json").is_file()
}

fn refused(image: &str, problem: impl Into<String>, fix: impl Into<String>) -> CliError {
    CliError::ImageRefused {
        image: image.to_owned(),
        problem: problem.into(),
        fix: fix.into(),
    }
}

/// Only an unsealed dev image with a dev name may go into a dev VM.
pub fn check_image(reference: &str, facts: &ImageFacts) -> Result<(), CliError> {
    let build = "move onto this worktree's dev image, which t builds FROM the production image: tilo-vm vm upgrade --from local";
    if !is_dev_name(reference) {
        return Err(refused(
            reference,
            "is not named as a dev image (localhost/tilo-vm:<tag> or registry.tilo.so/tilo/tilo-vm:<tag>); a production image is sealed and carries no sshd or tilo-vmd, and a VM must not be moved onto something that could be one",
            if reference.contains("/tilo:") || reference.contains("/tilo@") {
                build.to_owned()
            } else {
                "name a dev image: tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:<tag>   |   or build this worktree's: tilo-vm vm upgrade --from local".to_owned()
            },
        ));
    }
    if facts.flavour.as_deref() != Some("dev") {
        return Err(refused(
            reference,
            format!(
                "is not a dev image: its sh.tilo.image.flavour label is {}, and only os/Containerfile.dev sets it to dev",
                facts
                    .flavour
                    .as_deref()
                    .map_or_else(|| "missing".to_owned(), |f| format!("'{f}'"))
            ),
            build,
        ));
    }
    if facts.sealed {
        return Err(refused(
            reference,
            "is sealed: it carries a UKI under /boot/EFI/Linux, whose composefs digest no dev layer can match, so the VM could not merge accounts into /usr",
            build,
        ));
    }
    Ok(())
}

fn text<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value?.get(key)?.as_str().filter(|s| !s.is_empty())
}

/// Refuse an image whose production base is older than the one the VM runs;
/// `Ok(Some(note))` when the two cannot be compared.
pub fn check_base_age(
    image: &str,
    vm_base: Option<&Value>,
    image_base: Option<&Value>,
) -> Result<Option<String>, CliError> {
    let digest = |v| text(v, "image_digest");
    if digest(vm_base).is_some() && digest(vm_base) == digest(image_base) {
        return Ok(None);
    }
    let created = |v| text(v, "base_created").and_then(|t| t.parse::<jiff::Timestamp>().ok());
    match (created(vm_base), created(image_base)) {
        (Some(running), Some(new)) if new < running => Err(CliError::BaseOlder {
            image: image.to_owned(),
            base: text(image_base, "base_image")
                .unwrap_or("its base")
                .to_owned(),
            new: new.to_string(),
            running: running.to_string(),
        }),
        (Some(_), Some(_)) => Ok(None),
        (running, new) => Ok(Some(format!(
            "could not compare the production bases: {} records no base_created (dev images built before tilo-vm checked it)",
            match (running, new) {
                (None, None) => "neither the VM's image nor the new one",
                (None, _) => "the VM's image",
                _ => "the new image",
            }
        ))),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deployment {
    pub image: String,
    pub transport: String,
    pub digest: String,
    /// The composefs identity it boots (`composefs.verity`, without the
    /// `sha512:` prefix), which is how its boot loader entry names it.
    pub verity: Option<String>,
}

impl Deployment {
    fn from(value: &Value) -> Option<Self> {
        let image = value.get("image")?;
        Some(Self {
            image: image.pointer("/image/image")?.as_str()?.to_owned(),
            transport: image
                .pointer("/image/transport")
                .and_then(Value::as_str)
                .unwrap_or("registry")
                .to_owned(),
            digest: image.get("imageDigest")?.as_str()?.to_owned(),
            verity: value
                .pointer("/composefs/verity")
                .and_then(Value::as_str)
                .map(|v| v.strip_prefix("sha512:").unwrap_or(v))
                .filter(|v| v.len() == 128 && v.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_owned),
        })
    }

    /// `ref (sha256:0123456789ab)`.
    pub fn describe(&self) -> String {
        let reference = if self.transport == "registry" {
            self.image.clone()
        } else {
            format!("{}:{}", self.transport, self.image)
        };
        format!("{reference} ({})", short_digest(&self.digest))
    }
}

pub fn short_digest(digest: &str) -> &str {
    digest.get(..19).unwrap_or(digest)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootcStatus {
    pub spec: Option<(String, String)>,
    pub booted: Option<Deployment>,
    pub staged: Option<Deployment>,
    pub rollback: Option<Deployment>,
}

pub fn parse_bootc_status(json: &str) -> Result<BootcStatus, String> {
    let value: Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("bootc status --json: {e}"))?;
    let spec = value.pointer("/spec/image").and_then(|image| {
        Some((
            image.get("image")?.as_str()?.to_owned(),
            image
                .get("transport")
                .and_then(Value::as_str)
                .unwrap_or("registry")
                .to_owned(),
        ))
    });
    let deployment = |key: &str| {
        value
            .pointer(&format!("/status/{key}"))
            .filter(|v| !v.is_null())
            .and_then(Deployment::from)
    };
    Ok(BootcStatus {
        spec,
        booted: deployment("booted"),
        staged: deployment("staged"),
        rollback: deployment("rollback"),
    })
}

pub fn stage_command(status: &BootcStatus, transport: &str, imgref: &str) -> Vec<String> {
    let follows = status
        .spec
        .as_ref()
        .is_some_and(|(image, t)| image == imgref && t == transport);
    let mut words = vec!["bootc".to_owned()];
    if follows {
        words.push("upgrade".into());
    } else {
        words.push("switch".into());
        if transport != "registry" {
            words.extend(["--transport".to_owned(), transport.to_owned()]);
        }
        words.push(imgref.to_owned());
    }
    words
}

/// A subshell that runs `bootc <words>` with the ESP writable. On the
/// composefs backend bootc (1.16) mounts the ESP read-only at /boot through
/// `systemd.mount-extra`, and then fails its own status, switch and rollback
/// with "Mounting /dev/vda1: Device or resource busy": a second mount of a
/// vfat superblock that is already mounted read-only cannot be read-write.
/// So /boot is remounted read-write for the one command and read-only again
/// after it, which leaves the VM as it was.
pub fn bootc_line(words: &[&str]) -> String {
    let command = words
        .iter()
        .map(|w| sh_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    esp_writable_for(&format!("bootc {command}"))
}

fn esp_writable_for(command: &str) -> String {
    format!(
        "(ro=; if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then ro=1; mount -o remount,rw /boot || exit 1; fi; {command}; rc=$?; [ -z \"$ro\" ] || mount -o remount,ro /boot; exit $rc)"
    )
}

/// The device's update-channel helper: `update_command`'s first word.
pub const UPDATE_CHANNEL: &str = "/usr/libexec/tilo-update-channel";

pub fn update_command(pin: Option<&str>) -> Vec<String> {
    let mut words = vec![UPDATE_CHANNEL.to_owned()];
    words.extend(pin.map(str::to_owned));
    words
}

fn update_line(pin: Option<&str>) -> String {
    let (follow, direct) = match pin {
        Some(image) => (
            format!("{UPDATE_CHANNEL} pin {}", sh_quote(image)),
            vec!["switch", image],
        ),
        None => (format!("{UPDATE_CHANNEL} unpin"), vec!["upgrade"]),
    };
    format!(
        "if [ -x {UPDATE_CHANNEL} ]; then {follow} && {}; else {}; fi",
        esp_writable_for("/usr/libexec/tilo-verified-update"),
        bootc_line(&direct)
    )
}

/// Leave the ESP writable until the next boot. bootc applies a staged
/// composefs deployment at shutdown (`bootc-finalize-staged.service`), which
/// mounts the ESP too and fails the same way as [`bootc_line`] describes; the
/// VM then silently boots its old image again. The next boot mounts /boot
/// read-only as before.
pub const ESP_WRITABLE_UNTIL_BOOT: &str = "if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then mount -o remount,rw /boot; fi";

/// A remote shell line that runs `bootc <words>` under the VM lock, so no
/// swap, gc or other upgrade runs meanwhile; exit 75 means another command
/// holds it.
pub fn under_vm_lock(words: &[String]) -> String {
    let args: Vec<&str> = words.iter().map(String::as_str).skip(1).collect();
    let line = if words.first().map(String::as_str) == Some(UPDATE_CHANNEL) {
        update_line(args.first().copied())
    } else {
        bootc_line(&args)
    };
    format!(
        "mkdir -p {LOCKS} && flock --nonblock --conflict-exit-code 75 {LOCKS}/vm.lock sh -c {}",
        sh_quote(&line)
    )
}

pub const BOOT_ENTRIES: &str = "if [ -x /usr/libexec/tilo-deployment ]; then /usr/libexec/tilo-deployment entries 2>&1; echo \"@@entries $?\"; else echo '@@entries absent'; fi";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntriesOutcome {
    Done,
    /// The image's pass found no boot loader layout: an image from before
    /// tilo-esp-generator mounts nothing at /boot or /boot/efi, so its pass
    /// cannot see the ESP even where sd-boot started from one.
    NoLayout,
    Absent,
    Failed(String),
}

pub fn parse_entries(output: &str) -> EntriesOutcome {
    let status = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("@@entries "));
    match status {
        Some("absent") => EntriesOutcome::Absent,
        Some("0") if output.contains("nothing to maintain") => EntriesOutcome::NoLayout,
        Some("0") => EntriesOutcome::Done,
        _ => EntriesOutcome::Failed(
            output
                .lines()
                .filter(|line| !line.starts_with("@@entries"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

/// How many tries a staged deployment gets; tilo-deployment's own default.
pub const BOOT_TRIES: u32 = 3;

/// For an image whose own pass cannot see the ESP ([`EntriesOutcome::NoLayout`]
/// or [`EntriesOutcome::Absent`]): mount the ESP the boot loader started from
/// (`LoaderDevicePartUUID`) in a private mount namespace and give the entry
/// that boots `verity` its try counter, the same rename tilo-deployment does. The system
/// holds that ESP read-only at /boot, and a second mount of it shares that read-only
/// superblock: mount then falls back to read-only without failing, so the script makes it
/// writable (until the next boot, as staging an image does) before renaming.
/// The entry is found by the composefs identity it boots, never by name:
/// bootc renumbers every entry when it stages one. Prints `@@counted N`, or
/// `@@counter none` where no boot loader names an ESP (a VM started without
/// systemd-boot), or `@@counter failed: WHY`.
pub fn count_staged_entry_script(verity: &str) -> String {
    let inner = format!(
        r#"m=$(mktemp -d) || {{ echo '@@counter failed: no scratch directory'; exit 0; }}
if ! mount -t vfat -o rw "/dev/disk/by-partuuid/$1" "$m" 2>/dev/null; then echo "@@counter failed: could not mount the ESP (PARTUUID $1) read-write"; exit 0; fi
case ",$(findmnt -n -o OPTIONS "$m")," in *,ro,*) mount -o remount,rw "$m" 2>/dev/null || {{ umount "$m"; echo "@@counter failed: the ESP (PARTUUID $1) stays read-only"; exit 0; }} ;; esac
n=0
for f in "$m"/loader/entries.staged/bootc*.conf "$m"/loader/entries/bootc*.conf; do
  [ -f "$f" ] || continue
  case "${{f##*/}}" in *+*) continue ;; esac
  u=$(sed -n 's/^uki[[:space:]]\{{1,\}}//p' "$f" | head -1)
  if [ -n "$u" ]; then c=$(/usr/libexec/tilo-uki-cmdline show "$m/${{u#/}}" 2>/dev/null); else c=$(sed -n 's/^options[[:space:]]//p' "$f"); fi
  printf '%s\n' "$c" | tr ' \t' '\n\n' | sed 's/^composefs=sha512:/composefs=/' | grep -qx 'composefs={verity}' || continue
  mv "$f" "${{f%.conf}}+{BOOT_TRIES}.conf" || {{ umount "$m"; echo "@@counter failed: could not rename ${{f##*/}}"; exit 0; }}
  n=$((n + 1))
done
sync
umount "$m"; rmdir "$m"
echo "@@counted $n""#
    );
    format!(
        "p=$(tail -c +5 /sys/firmware/efi/efivars/LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f 2>/dev/null | tr -d '\\000' | tr 'A-F' 'a-f'); \
         case \"$p\" in ????????-????-????-????-????????????) ;; *) echo '@@counter none'; exit 0 ;; esac; \
         unshare -m sh -c {} counter \"$p\"",
        crate::vmbuild::sh_quote(&inner)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Counted {
    Entries(usize),
    NoBootLoader,
    Failed(String),
}

pub fn parse_counted(output: &str) -> Counted {
    for line in output.lines().rev() {
        let line = line.trim();
        if let Some(n) = line.strip_prefix("@@counted ") {
            return n
                .parse()
                .map_or_else(|_| Counted::Failed(line.to_owned()), Counted::Entries);
        }
        if line == "@@counter none" {
            return Counted::NoBootLoader;
        }
        if let Some(why) = line.strip_prefix("@@counter failed: ") {
            return Counted::Failed(why.to_owned());
        }
    }
    Counted::Failed(format!("no answer: {}", output.trim()))
}

/// Printed in place of bootc's status when the VM has no bootc.
const NO_BOOTC: &str = "@@nobootc";

pub const BOOTC_PROBE: &str =
    "if command -v bootc >/dev/null 2>&1; then echo bootc=yes; else echo bootc=no; fi";

pub fn parse_bootc_probe(output: &str) -> Option<bool> {
    output.lines().find_map(|line| match line.trim() {
        "bootc=yes" => Some(true),
        "bootc=no" => Some(false),
        _ => None,
    })
}

pub fn vm_facts_script() -> String {
    format!(
        "cat /proc/sys/kernel/random/boot_id; echo @@base; cat /usr/share/tilo-vm/base.json 2>/dev/null; echo; echo @@free; df -Pk /var | awk 'NR==2 {{print $4}}'; echo @@board; cat /usr/lib/tilo/hardware-profile.json 2>/dev/null; echo; echo @@bootc; if command -v bootc >/dev/null 2>&1; then {}; else echo {NO_BOOTC}; fi",
        bootc_line(&["status", "--json"])
    )
}

/// Why [`parse_vm_facts`] found no bootc status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsError {
    NoBootc,
    /// bootc ran but said nothing usable, or the output is cut short.
    Unreadable(String),
}

#[derive(Debug, Clone, Default)]
pub struct VmFacts {
    pub boot_id: String,
    pub base: Option<Value>,
    pub free_bytes: Option<u64>,
    /// The board the image was built for (`/usr/lib/tilo/hardware-profile.json`),
    /// on a device running a board dev image.
    pub board: Option<String>,
    pub bootc: BootcStatus,
}

impl VmFacts {
    pub fn arch(&self) -> Option<String> {
        let target = self.base.as_ref()?.get("target")?.as_str()?;
        target
            .split('-')
            .next()
            .filter(|a| !a.is_empty())
            .map(str::to_owned)
    }
}

pub fn parse_vm_facts(output: &str) -> Result<VmFacts, FactsError> {
    let cut = |what: &str| FactsError::Unreadable(what.to_owned());
    let (boot, rest) = output
        .split_once("@@base")
        .ok_or_else(|| cut("no boot id"))?;
    let (base, rest) = rest
        .split_once("@@free")
        .ok_or_else(|| cut("no base.json section"))?;
    let (free, rest) = rest
        .split_once("@@board")
        .ok_or_else(|| cut("no free space section"))?;
    let (board, bootc) = rest
        .split_once("@@bootc")
        .ok_or_else(|| cut("no hardware profile section"))?;
    if bootc.trim() == NO_BOOTC {
        return Err(FactsError::NoBootc);
    }
    Ok(VmFacts {
        boot_id: boot.trim().to_owned(),
        base: serde_json::from_str(base.trim()).ok(),
        free_bytes: free
            .trim()
            .parse::<u64>()
            .ok()
            .map(|kib| kib.saturating_mul(1024)),
        board: serde_json::from_str::<Value>(board.trim())
            .ok()
            .and_then(|profile| profile.get("id")?.as_str().map(str::to_owned))
            .filter(|id| !id.is_empty()),
        bootc: parse_bootc_status(bootc).map_err(FactsError::Unreadable)?,
    })
}

pub fn host_layout(vm: &str) -> PathBuf {
    tilo_paths::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-vm")
        .join("images")
        .join(vm)
}

pub fn rsync_command(
    layout: &Path,
    endpoint: &SshEndpoint,
    key: Option<&str>,
) -> Result<CommandLine, CliError> {
    let rsh = crate::vmbuild::rsync_rsh(endpoint, key)?;
    let mut source = crate::vmbuild::rsync_source(layout);
    if !source.ends_with('/') {
        source.push('/');
    }
    Ok(CommandLine::new(
        &crate::host::rsync_program().unwrap_or_else(|| "rsync".into()),
        &["--archive", "--delete", "--whole-file", "--stats"],
    )
    .arg("--rsh")
    .arg(rsh)
    .arg("--rsync-path")
    .arg(crate::remote::rsync_path_as_root(
        endpoint,
        &format!(
            "mkdir -p {LOCKS} {VM_LAYOUT} && flock --nonblock --conflict-exit-code 75 {LOCKS}/image.lock rsync"
        ),
    ))
    .arg(source)
    .arg(format!("{}@{}:{VM_LAYOUT}/", endpoint.user, endpoint.host))
    .env("LC_ALL", "C"))
}

pub fn rsync_sent_bytes(stats: &str) -> Option<u64> {
    let line = stats
        .lines()
        .find(|l| l.trim_start().starts_with("Total transferred file size:"))?;
    let digits: String = line
        .split(':')
        .nth(1)?
        .split_whitespace()
        .next()?
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn blob_path(layout: &Path, digest: &str) -> Option<PathBuf> {
    let (algorithm, hex) = digest.split_once(':')?;
    (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| layout.join("blobs").join(algorithm).join(hex))
}

pub fn referenced_blobs(layout: &Path) -> std::io::Result<BTreeSet<String>> {
    fn walk(layout: &Path, descriptor: &Value, seen: &mut BTreeSet<String>) -> std::io::Result<()> {
        let Some(digest) = descriptor.get("digest").and_then(Value::as_str) else {
            return Ok(());
        };
        if !seen.insert(digest.to_owned()) {
            return Ok(());
        }
        let media = descriptor
            .get("mediaType")
            .and_then(Value::as_str)
            .unwrap_or("");
        if media.contains("manifest") || media.contains("index") {
            let Some(path) = blob_path(layout, digest) else {
                return Ok(());
            };
            let value: Value =
                serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
            for child in value
                .get("manifests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(
                    value
                        .get("layers")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
                .chain(value.get("config"))
            {
                walk(layout, child, seen)?;
            }
        }
        Ok(())
    }
    let index: Value = serde_json::from_slice(&std::fs::read(layout.join("index.json"))?)
        .map_err(std::io::Error::other)?;
    let mut seen = BTreeSet::new();
    for manifest in index
        .get("manifests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk(layout, manifest, &mut seen)?;
    }
    Ok(seen)
}

pub fn prune_layout(layout: &Path) -> std::io::Result<u64> {
    let index_path = layout.join("index.json");
    let mut index: Value =
        serde_json::from_slice(&std::fs::read(&index_path)?).map_err(std::io::Error::other)?;
    if let Some(manifests) = index.get_mut("manifests").and_then(Value::as_array_mut) {
        let before = manifests.len();
        manifests.retain(|m| {
            m.pointer("/annotations/org.opencontainers.image.ref.name")
                .and_then(Value::as_str)
                == Some(LAYOUT_TAG)
        });
        if manifests.len() != before {
            std::fs::write(
                &index_path,
                serde_json::to_vec(&index).map_err(std::io::Error::other)?,
            )?;
        }
    }
    let keep = referenced_blobs(layout)?;
    let mut freed: u64 = 0;
    let blobs = layout.join("blobs");
    for algorithm in std::fs::read_dir(&blobs)? {
        let algorithm = algorithm?;
        let name = algorithm.file_name().to_string_lossy().into_owned();
        for blob in std::fs::read_dir(algorithm.path())? {
            let blob = blob?;
            let digest = format!("{name}:{}", blob.file_name().to_string_lossy());
            if !keep.contains(&digest) {
                freed = blob
                    .metadata()
                    .map_or(freed, |m| freed.saturating_add(m.len()));
                std::fs::remove_file(blob.path())?;
            }
        }
    }
    Ok(freed)
}

pub fn layout_bytes(layout: &Path) -> u64 {
    referenced_blobs(layout).map_or(0, |blobs| {
        blobs
            .iter()
            .filter_map(|d| blob_path(layout, d))
            .filter_map(|p| std::fs::metadata(p).ok())
            .fold(0_u64, |total, m| total.saturating_add(m.len()))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tilo_test_env::SpawnUnderEnvLock as _;

    use super::*;

    tilo_test_env::scenario! {
        fn upgrade_sources_resolve_to_labelled_dev_images;

        phase image_upgrades_accept_only_fresh_labelled_dev_images;
        {
            {
                assert_eq!(parse_from("local", None).unwrap(), UpgradeFrom::Local);
                for bad in [
                    "local:localhost/tilo-vm:mine",
                    "local:",
                    "ci:",
                    "ci:a b",
                    "sha:xyz",
                    "worktree",
                    "locally",
                ] {
                    let error = parse_from(bad, None).unwrap_err();
                    assert!(
                        error.hint().contains("--from local (this worktree's dev image, built by t)")
                            && error.hint().contains("--from ci:main-<sha7>"),
                        "{bad}: {error}"
                    );
                }
            }

            {
                for good in [
                    "localhost/tilo-vm:latest",
                    "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4",
                    "localhost/tilo-vm@sha256:0123",
                ] {
                    assert!(is_dev_name(good), "{good}");
                }
                for bad in [
                    "localhost/tilo:latest",
                    "registry.tilo.so/tilo/tilo:main",
                    "localhost/tilo-vm",
                    "localhost/tilo-vmx:latest",
                    "tilo-vm:latest",
                ] {
                    assert!(!is_dev_name(bad), "{bad}");
                }
            }

            {
                let error = check_image("localhost/tilo:latest", &dev_facts()).unwrap_err();
                assert!(
                    error.to_string().contains("not named as a dev image"),
                    "{error}"
                );
                assert!(error.hint().contains("--from local"), "{}", error.hint());
                assert_eq!(error.see().as_deref(), Some("tilo-vm guide images"));

                let mut facts = dev_facts();
                facts.flavour = None;
                let error = check_image("localhost/tilo-vm:x", &facts).unwrap_err();
                assert!(error.to_string().contains("label is missing"), "{error}");

                let mut facts = dev_facts();
                facts.sealed = true;
                let error = check_image("localhost/tilo-vm:x", &facts).unwrap_err();
                assert!(error.to_string().contains("sealed"), "{error}");

                check_image("localhost/tilo-vm:x", &dev_facts()).unwrap();
            }

            {
                let base = |digest: &str, created: &str| json!({ "image_digest": digest, "base_created": created, "base_image": "localhost/tilo:latest" });
                let running = base("sha256:aa", "2026-09-20T10:00:00Z");
                let older = base("sha256:bb", "2026-09-01T10:00:00Z");
                let newer = base("sha256:cc", "2026-09-21T10:00:00Z");
                let error = check_base_age("localhost/tilo-vm:latest", Some(&running), Some(&older))
                    .unwrap_err();
                assert!(matches!(error, CliError::BaseOlder { .. }), "{error}");
                assert!(
                    error.hint().contains("--allow-older-base"),
                    "{}",
                    error.hint()
                );
                assert_eq!(
                    check_base_age("i", Some(&running), Some(&newer)).unwrap(),
                    None
                );
                assert_eq!(
                    check_base_age("i", Some(&running), Some(&running)).unwrap(),
                    None
                );
                let note = check_base_age(
                    "i",
                    Some(&json!({"image_digest": "sha256:aa"})),
                    Some(&newer),
                )
                .unwrap()
                .unwrap();
                assert!(note.contains("the VM's image"), "{note}");
            }
        }

        phase a_built_image_is_read_from_its_own_labels_and_staged_once;
        {
            let out = tempfile::tempdir().unwrap();
            let built = built_image(out.path(), "x86_64");
            assert_eq!(
                built,
                out.path().join("target/publish/os-dev/x86_64"),
                "t outputs --to keeps the check's own output path"
            );
            assert!(!is_built_image(&built));
            let digest = write_built(
                &built,
                &json!({
                    "sh.tilo.image.flavour": "dev",
                    "sh.tilo.base.created": "2026-09-20T10:00:00Z",
                    "sh.tilo.base.digest": "sha256:aa",
                    "sh.tilo.base.image": "localhost/tilo:ci",
                }),
                b"layer one",
            );
            assert!(is_built_image(&built));
            std::fs::write(built.join("revision"), format!("{}\n", "1a2b3c4d".repeat(5))).unwrap();
            assert_eq!(built_name(&built), "localhost/tilo-vm:1a2b3c4d1a2b");
            std::fs::write(built.join("revision"), "development-loop\n").unwrap();
            assert_eq!(built_name(&built), "localhost/tilo-vm:development-loop");

            let facts = layout_facts(&built.join("oci")).unwrap();
            assert_eq!(facts.flavour.as_deref(), Some("dev"));
            assert!(!facts.sealed);
            assert_eq!(text(facts.base.as_ref(), "base_created"), Some("2026-09-20T10:00:00Z"));
            assert_eq!(text(facts.base.as_ref(), "image_digest"), Some("sha256:aa"));
            check_image(&built_name(&built), &facts).unwrap();

            // Into the host's copy under the VM's tag; a second build sends only its new layer
            // and the first one's blobs go.
            let layout = out.path().join("host");
            assert_eq!(stage_built(&built, &layout).unwrap(), digest);
            assert!(is_layout(&layout));
            let index: Value =
                serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap()).unwrap();
            assert_eq!(
                index.pointer("/manifests/0/annotations/org.opencontainers.image.ref.name"),
                Some(&json!(LAYOUT_TAG))
            );
            assert_eq!(layout_facts(&layout).unwrap(), facts);
            let first = referenced_blobs(&layout).unwrap();
            let second =
                write_built(&built, &json!({"sh.tilo.image.flavour": "dev"}), b"layer two");
            assert_eq!(stage_built(&built, &layout).unwrap(), second);
            let now = referenced_blobs(&layout).unwrap();
            assert!(now.contains(&second));
            for gone in first.difference(&now) {
                assert!(!blob_path(&layout, gone).unwrap().exists(), "{gone} was kept");
            }

            std::fs::write(built.join("digest"), "sha256:00\n").unwrap();
            let refused = stage_built(&built, &layout).unwrap_err();
            assert!(refused.contains("digest file names sha256:00"), "{refused}");

            // A production image's labels say it is no dev image, a board image's that it is
            // sealed; neither goes into a dev VM.
            let production = facts_from_labels(Some(&json!({"org.tilo.rust-lto": "thin"})));
            assert!(check_image("localhost/tilo-vm:x", &production)
                .unwrap_err()
                .to_string()
                .contains("label is missing"));
            let board = facts_from_labels(Some(&json!({
                "sh.tilo.image.flavour": "dev", "sh.tilo.image.sealed": "true"
            })));
            assert!(check_image("localhost/tilo-vm:x", &board)
                .unwrap_err()
                .to_string()
                .contains("sealed"));

            let dir = tempfile::tempdir().unwrap();
            assert!(!is_layout(dir.path()));
            std::fs::write(dir.path().join("oci-layout"), "{}").unwrap();
            std::fs::write(dir.path().join("index.json"), "{}").unwrap();
            assert!(is_layout(dir.path()));
            assert!(!is_layout(Path::new("registry.tilo.so/tilo/tilo-vm:main")));
            assert!(layout_facts(dir.path()).unwrap_err().contains("no manifests"));

            let root = Path::new("/w");
            let out = Path::new("/w/target/tilo-vm");
            assert_eq!(
                outputs_command(&image_check("aarch64", false), root, out).display(),
                "t outputs os-dev-image-aarch64 --dir /w --to /w/target/tilo-vm"
            );
            assert_eq!(disk_check("x86_64", true), "os-dev-disk-lean-x86_64");
            assert_eq!(image_check("x86_64", true), "os-dev-image-lean-x86_64");
            assert_eq!(
                built_disk(out, "x86_64"),
                Path::new("/w/target/tilo-vm/target/publish/os-dev-disk/x86_64/disk/disk.qcow2")
            );

            assert!(is_already_booted(
                "error: Switching: Composefs Switching: Target image has the same fs-verity digest as the existing Some(Booted) deployment."
            ));
            assert!(!is_already_booted(
                "error: Switching: pulling: connection refused"
            ));
        }

        phase ci_upgrade_sources_resolve_to_published_dev_images;
        {
            {
                assert_eq!(
                    parse_from("ci:main-1a2b3c4", None).unwrap(),
                    UpgradeFrom::Ci {
                        tag: "main-1a2b3c4".into()
                    }
                );
                assert_eq!(
                    parse_from("sha:1A2B3C4D", None).unwrap(),
                    UpgradeFrom::Ci {
                        tag: "main-1a2b3c4".into()
                    }
                );
            }

            {
                let expected = "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64";
                assert_eq!(ci_image("main-1a2b3c4", "x86_64"), expected);
                assert_eq!(ci_image("main-1a2b3c4-x86_64", "x86_64"), expected);
                assert_eq!(ci_image("main-1a2b3c4-ci", "x86_64"), expected);
                assert!(is_dev_name(expected));
                for tag in [
                    "main-1a2b3c4",
                    "main-1a2b3c4-ci",
                    "main-1a2b3c4-lenovo-83ed",
                ] {
                    assert_eq!(
                        board_image(tag, "lenovo-83ed"),
                        "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-lenovo-83ed",
                        "{tag}"
                    );
                }
                assert_eq!(
                    exists_command(expected).display(),
                    format!("skopeo inspect --raw docker://{expected}")
                );
            }

            {
                assert!(is_auth_failure(
                    "error: Switching: pulling: reading manifest main-1a2b3c4-lenovo-83ed: unauthorized: authentication required"
                ));
                assert!(!is_auth_failure(
                    "error: Switching: pulling: connection refused"
                ));
                let copy = registry_export_command(
                    "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-lenovo-83ed",
                    "aarch64",
                    Path::new("/c/images/yoga"),
                    Path::new("/c/images/yoga.digest"),
                );
                assert_eq!(
                    copy.display(),
                    "skopeo copy --quiet --dest-oci-accept-uncompressed-layers --override-arch arm64 \
                 --digestfile /c/images/yoga.digest docker://registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-lenovo-83ed \
                 oci:/c/images/yoga:tilo-vm"
                );
                assert_eq!(
                    registry_facts(r#"{"Labels":{"sh.tilo.image.flavour":"dev"}}"#)
                        .flavour
                        .as_deref(),
                    Some("dev")
                );
                assert_eq!(registry_facts(r#"{"Labels":null}"#), ImageFacts::default());
            }

        }
    }

    tilo_test_env::scenario! {
        fn the_guest_is_probed_and_the_image_staged_once;

        phase bootcs_space_refusal_reads_as_plain_language;
        {
            let stderr = "error: Switching: Composefs Switching: Performing Upgrade Operation: Insufficient free space for registry.tilo.so/tilo/tilo-vm:pr-4227-4ab5dcc-lenovo-83ed (available: 2.3 GB required: 5.2 GB)";
            let (free, needed) = insufficient_space(stderr).unwrap();
            assert_eq!((free.as_str(), needed.as_str()), ("2.3 GB", "5.2 GB"));
            let error = CliError::NoSpace {
                place: "VM yoga".into(),
                free,
                what: "the new OS image".into(),
                needed,
            };
            assert_eq!(
                error.to_string(),
                "not enough space on VM yoga: the new OS image needs 5.2 GB, 2.3 GB is free"
            );
            assert!(error.hint().contains("reused"), "{}", error.hint());
            assert_eq!(
                insufficient_space("error: Switching: pulling: connection refused"),
                None
            );
        }

        phase guest_facts_are_probed_in_one_round_trip;
        {
            {
                let status = parse_bootc_status(STATUS).unwrap();
                assert_eq!(status.spec, Some((layout_imgref(), "oci".to_owned())));
                assert_eq!(
                    status.booted.as_ref().unwrap().digest,
                    "sha256:1111111111111111111111"
                );
                assert_eq!(status.staged, None);
                let rollback = status.rollback.unwrap();
                assert_eq!(
                    rollback.describe(),
                    "localhost/tilo-vm:standin-a (sha256:222222222222)"
                );
                assert!(parse_bootc_status("not json").is_err());
            }

            {
                let output = format!(
                    "b00t\n@@base\n{{\"image_digest\":\"sha256:aa\",\"target\":\"aarch64-unknown-linux-gnu\"}}\n\n@@free\n1024\n@@board\n{{\"schema_version\":1,\"id\":\"lenovo-83ed\"}}\n@@bootc\n{STATUS}\n"
                );
                let facts = parse_vm_facts(&output).unwrap();
                assert_eq!(facts.boot_id, "b00t");
                assert_eq!(facts.board.as_deref(), Some("lenovo-83ed"));
                assert_eq!(facts.arch().as_deref(), Some("aarch64"));
                assert_eq!(facts.free_bytes, Some(1 << 20));
                assert_eq!(text(facts.base.as_ref(), "image_digest"), Some("sha256:aa"));
                assert!(facts.bootc.booted.is_some());
                assert!(matches!(
                    parse_vm_facts("b00t\n@@base\n\n@@free\n\n@@board\n\n@@bootc\n"),
                    Err(FactsError::Unreadable(_))
                ));
                assert_eq!(
                    parse_vm_facts("b00t\n@@base\n\n@@free\n1\n@@board\n\n@@bootc\n@@nobootc\n")
                        .unwrap_err(),
                    FactsError::NoBootc
                );
            }

            {
                assert_eq!(parse_bootc_probe("bootc=yes\n"), Some(true));
                assert_eq!(parse_bootc_probe("motd\nbootc=no\n"), Some(false));
                assert_eq!(parse_bootc_probe(""), None);
                assert!(BOOTC_PROBE.contains("command -v bootc"));
            }
        }

        phase image_transfer_stages_once_and_prunes_only_unreachable_blobs;
        {
            {
                let fresh = BootcStatus {
                    spec: Some(("localhost/tilo-vm:standin-a".into(), "registry".into())),
                    ..BootcStatus::default()
                };
                assert_eq!(
                    stage_command(&fresh, "oci", &layout_imgref()).join(" "),
                    "bootc switch --transport oci /var/lib/tilo-vm/image:tilo-vm"
                );
                let following = parse_bootc_status(STATUS).unwrap();
                assert_eq!(
                    stage_command(&following, "oci", &layout_imgref()).join(" "),
                    "bootc upgrade"
                );
                assert_eq!(
                    stage_command(&following, "registry", "registry.tilo.so/tilo/tilo-vm:x").join(" "),
                    "bootc switch registry.tilo.so/tilo/tilo-vm:x"
                );
                let locked = under_vm_lock(&["bootc".into(), "upgrade".into()]);
                assert!(
                    locked.contains(
                        "flock --nonblock --conflict-exit-code 75 /var/lib/tilo-vm/locks/vm.lock sh -c"
                    ),
                    "{locked}"
                );
                assert!(locked.contains("bootc upgrade; rc=$?"), "{locked}");
                let pinned = under_vm_lock(&update_command(Some("registry.tilo.so/tilo/tilo-vm:x")));
                assert!(
                    pinned.contains("/usr/libexec/tilo-update-channel pin")
                        && pinned.contains("/usr/libexec/tilo-verified-update; rc=$?")
                        && pinned.contains("bootc switch"),
                    "{pinned}"
                );
                let channel = under_vm_lock(&update_command(None));
                assert!(
                    channel.contains("/usr/libexec/tilo-update-channel unpin")
                        && channel.contains("bootc upgrade"),
                    "{channel}"
                );
                // bootc gets a writable ESP for the one command, and it is read-only again after.
                let line = bootc_line(&["status", "--json"]);
                assert!(line.contains("mount -o remount,rw /boot"), "{line}");
                assert!(
                    line.contains("mount -o remount,ro /boot; exit $rc)"),
                    "{line}"
                );
                assert!(vm_facts_script().contains(&format!("then {line}; else echo @@nobootc; fi")));
            }

            {
                let endpoint = SshEndpoint {
                    user: "root".into(),
                    host: "127.0.0.1".into(),
                    port: 2223,
                };
                let rsync = rsync_command(Path::new("/c/dev"), &endpoint, Some("/k/id")).unwrap();
                let text = rsync.display();
                assert!(text.contains("--delete"), "{text}");
                assert!(
                    text.contains("/var/lib/tilo-vm/locks/image.lock rsync"),
                    "{text}"
                );
                assert!(
                    text.ends_with("/c/dev/ root@127.0.0.1:/var/lib/tilo-vm/image/"),
                    "{text}"
                );
                assert_eq!(
                    rsync_sent_bytes(
                        "Number of files: 9\nTotal transferred file size: 12,345,678 bytes\n"
                    ),
                    Some(12_345_678)
                );
            }

            #[cfg(windows)]
            {
                let endpoint = SshEndpoint {
                    user: "root".into(),
                    host: "127.0.0.1".into(),
                    port: 2223,
                };
                let line =
                    rsync_command(Path::new(r"C:\tilo-build\image-layout"), &endpoint, None).unwrap();
                assert_eq!(
                    line.args[line.args.len() - 2],
                    "/c/tilo-build/image-layout/"
                );
            }

            {
                let dir = tempfile::tempdir().unwrap();
                let layout = dir.path();
                let blobs = layout.join("blobs/sha256");
                std::fs::create_dir_all(&blobs).unwrap();
                let write = |hex: &str, body: &str| std::fs::write(blobs.join(hex), body).unwrap();
                write("aa", "layer");
                write("bb", "config");
                write("cc", "stale layer");
                write(
                    "dd",
                    &json!({"config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": "sha256:bb"},
                            "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": "sha256:aa"}]})
                    .to_string(),
                );
                let stale = json!({"layers": [{"mediaType": "x", "digest": "sha256:cc"}]}).to_string();
                write("ee", &stale);
                let manifest = |hex: &str, tag: &str| json!({"mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": format!("sha256:{hex}"), "annotations": {"org.opencontainers.image.ref.name": tag}});
                std::fs::write(
                    layout.join("index.json"),
                    json!({"schemaVersion": 2, "manifests": [manifest("dd", LAYOUT_TAG), manifest("ee", "old")]}).to_string(),
                )
                .unwrap();
                let freed = prune_layout(layout).unwrap();
                assert_eq!(freed, ("stale layer".len() + stale.len()) as u64);
                let left: BTreeSet<String> = std::fs::read_dir(&blobs)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                assert_eq!(left, ["aa", "bb", "dd"].map(str::to_owned).into());
                assert_eq!(
                    layout_bytes(layout),
                    (5 + 6) as u64 + std::fs::metadata(blobs.join("dd")).unwrap().len()
                );
            }
        }
    }

    /// A dev image as t's check leaves it: one layer, a configuration with `labels`. Returns
    /// its manifest digest.
    fn write_built(dir: &Path, labels: &Value, layer: &[u8]) -> String {
        use sha2::Digest as _;
        let oci = dir.join("oci");
        let blobs = oci.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        let put = |bytes: &[u8]| {
            let hex = hex::encode(sha2::Sha256::digest(bytes));
            std::fs::write(blobs.join(&hex), bytes).unwrap();
            (format!("sha256:{hex}"), bytes.len())
        };
        let (layer, layer_size) = put(layer);
        let (config, config_size) =
            put(&serde_json::to_vec(&json!({"config": {"Labels": labels}})).unwrap());
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": config, "size": config_size},
            "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": layer, "size": layer_size}],
        });
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let (digest, size) = put(&bytes);
        let index = json!({"schemaVersion": 2, "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": digest, "size": size,
            "annotations": {"org.opencontainers.image.ref.name": "tilo"}
        }]});
        std::fs::write(oci.join("index.json"), serde_json::to_vec(&index).unwrap()).unwrap();
        std::fs::write(oci.join("oci-layout"), r#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
        std::fs::write(dir.join("digest"), format!("{digest}\n")).unwrap();
        digest
    }

    fn dev_facts() -> ImageFacts {
        ImageFacts {
            flavour: Some("dev".into()),
            sealed: false,
            base: None,
        }
    }

    const STATUS: &str = r#"{"apiVersion":"org.containers.bootc/v1","kind":"BootcHost",
      "spec":{"image":{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"},"bootOrder":"default"},
      "status":{"staged":null,
        "booted":{"image":{"image":{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"},"imageDigest":"sha256:1111111111111111111111"},"pinned":false},
        "rollback":{"image":{"image":{"image":"localhost/tilo-vm:standin-a","transport":"registry"},"imageDigest":"sha256:2222222222222222222222"}}}}"#;

    tilo_test_env::scenario! {
        fn the_staged_boot_entry_is_counted;

        phase a_staged_deployment_names_the_composefs_identity_its_entry_boots;
        {
            let verity = "ab".repeat(64);
            let json = format!(
                r#"{{"status":{{"staged":{{"image":{{"image":{{"image":"registry.tilo.so/tilo/tilo-vm:x","transport":"registry"}},"imageDigest":"sha256:1"}},"composefs":{{"verity":"sha512:{verity}"}}}},"booted":{{"image":{{"image":{{"image":"a"}},"imageDigest":"sha256:0"}},"composefs":{{"verity":"short"}}}}}}}}"#
            );
            let status = parse_bootc_status(&json).unwrap();
            assert_eq!(
                status.staged.unwrap().verity.as_deref(),
                Some(verity.as_str())
            );
            assert_eq!(
                status.booted.unwrap().verity,
                None,
                "a malformed identity is no identity"
            );
        }

        phase the_boot_entry_pass_says_whether_the_next_boot_is_counted;
        {
            assert_eq!(
                parse_entries(
                    "tilo-deployment: boot counter: bootc_x-1.conf gets 3 tries\n@@entries 0\n"
                ),
                EntriesOutcome::Done
            );
            assert_eq!(
                parse_entries(
                    "tilo-deployment: no systemd-boot layout — nothing to maintain\n@@entries 0\n"
                ),
                EntriesOutcome::NoLayout
            );
            assert_eq!(
                parse_entries(
                    "tilo-deployment: the ESP this machine started from (PARTUUID x) is not mounted and could not be mounted read-only; nothing to maintain\n@@entries 0\n"
                ),
                EntriesOutcome::NoLayout
            );
            assert_eq!(parse_entries("@@entries absent\n"), EntriesOutcome::Absent);
            assert_eq!(
                parse_entries("tilo-deployment: not a managed UKI entry: x\n@@entries 1\n"),
                EntriesOutcome::Failed("tilo-deployment: not a managed UKI entry: x".into())
            );
            assert!(matches!(parse_entries(""), EntriesOutcome::Failed(_)));
        }

        phase the_staged_entry_is_counted_by_the_identity_it_boots_in_a_private_mount;
        {
            let verity = "cd".repeat(64);
            let script = count_staged_entry_script(&verity);
            assert!(script.contains("LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"));
            assert!(script.contains("unshare -m sh -c"));
            assert!(script.contains(&format!("composefs={verity}")));
            assert!(script.contains("+3.conf"));
            assert!(script.contains("entries.staged"));
            assert!(
                script.contains("remount,rw"),
                "a read-only fallback mount is made writable: {script}"
            );
            assert_eq!(parse_counted("@@counted 1\n"), Counted::Entries(1));
            assert_eq!(parse_counted("@@counted 0"), Counted::Entries(0));
            assert_eq!(parse_counted("@@counter none\n"), Counted::NoBootLoader);
            assert_eq!(
                parse_counted("@@counter failed: could not mount the ESP (PARTUUID x) read-write\n"),
                Counted::Failed("could not mount the ESP (PARTUUID x) read-write".into())
            );
            assert!(matches!(
                parse_counted("sh: unshare: not found"),
                Counted::Failed(_)
            ));
        }

        phase the_counting_script_runs_in_a_shell;
        {
            let script = count_staged_entry_script(&"ef".repeat(64));
            let status = std::process::Command::new("sh")
                .args(["-n", "-c", &script])
                .status_locked()
                .unwrap();
            assert!(status.success(), "{script}");
        }
    }
}

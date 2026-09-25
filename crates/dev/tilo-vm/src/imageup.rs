//! Moving a running dev VM onto another image with bootc: a dev image built on
//! this host (`vm upgrade --from local`) or one from a registry.
//!
//! A local image travels as an OCI layout. `skopeo` writes it from podman's
//! storage into a per-VM directory on the host, reusing every blob already
//! there, so only new layers are written; `rsync` mirrors that directory to
//! [`VM_LAYOUT`] in the VM, so only new blobs cross; and
//! `bootc switch --transport oci` imports only the layers the VM's store lacks.
//! No registry, no daemon on the host, and nothing hypervisor-specific: it
//! needs only the SSH the rest of tilo-vm uses. The image the VM boots is the
//! `podman build` output byte for byte, never a patched rootfs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::CliError;
use crate::hypervisor::{CommandLine, SshEndpoint};
use crate::remote;
use crate::vmbuild::sh_quote;

/// What `--from local` builds and moves the VM onto.
pub const LOCAL_DEV_IMAGE: &str = "localhost/tilo-vm:latest";
/// The production image the local dev image is built FROM.
pub const LOCAL_BASE: &str = "localhost/tilo:latest";
/// The script `--from local` runs (relative to the worktree).
pub const BUILD_SCRIPT: &str = "os/dev/build-dev-image.sh";
/// The OCI layout in the VM that bootc switches to; in /var, so it survives
/// the switch, and each upgrade only adds the blobs that changed.
pub const VM_LAYOUT: &str = "/var/lib/tilo-vm/image";
/// The tag inside that layout.
pub const LAYOUT_TAG: &str = "tilo-vm";
/// The bootc image reference of [`VM_LAYOUT`].
pub fn layout_imgref() -> String {
    format!("{VM_LAYOUT}:{LAYOUT_TAG}")
}
const LOCKS: &str = "/var/lib/tilo-vm/locks";

/// `--from local` or `--from local:<image>`: the image, and whether to build it.
///
/// # Errors
/// Anything else.
pub fn parse_from(from: &str) -> Result<(String, bool), CliError> {
    match from.split_once(':') {
        None if from == "local" => Ok((LOCAL_DEV_IMAGE.to_owned(), true)),
        Some(("local", image)) if !image.is_empty() => Ok((image.to_owned(), false)),
        _ => Err(CliError::Unsupported {
            what: format!("--from {from} for vm upgrade"),
            alternative: "tilo-vm vm upgrade --from local (build this worktree's dev image)   |   --from local:localhost/tilo-vm:<tag> (one already built)   |   --image code.podesta.ai/qos/tilo-vm:<tag> (a published one)".into(),
        }),
    }
}

/// A dev image may only be named `localhost/tilo-vm` or
/// `code.podesta.ai/qos/tilo-vm`, with a tag or digest: the same rule
/// `os/dev/build-dev-image.sh` enforces when it tags one.
pub fn is_dev_name(reference: &str) -> bool {
    ["localhost/tilo-vm", "code.podesta.ai/qos/tilo-vm"]
        .iter()
        .any(|repo| {
            reference.strip_prefix(repo).is_some_and(|rest| {
                rest.len() > 1 && (rest.starts_with(':') || rest.starts_with("@sha256:"))
            })
        })
}

/// What `podman image inspect` and a look inside say about a local image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFacts {
    pub id: String,
    pub flavour: Option<String>,
    /// A UKI under /boot/EFI/Linux: a sealed production image.
    pub sealed: bool,
    /// The image's /usr/share/tilo-vm/base.json.
    pub base: Option<Value>,
}

pub fn inspect_command(image: &str) -> CommandLine {
    CommandLine::new("podman", &["image", "inspect", image])
}

/// Read base.json and look for a UKI, in one throwaway container.
pub fn probe_command(image_id: &str) -> CommandLine {
    CommandLine::new(
        "podman",
        &[
            "run",
            "--rm",
            "--network=none",
            "--entrypoint",
            "/bin/sh",
            image_id,
            "-c",
            "cat /usr/share/tilo-vm/base.json 2>/dev/null; echo; echo @@uki; ls /boot/EFI/Linux/ 2>/dev/null | grep -i '[.]efi$' || true",
        ],
    )
}

/// # Errors
/// Output that is not `podman image inspect` JSON.
pub fn parse_inspect(json: &str) -> Result<(String, Option<String>), String> {
    let value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let image = value.get(0).unwrap_or(&value);
    let id = image
        .get("Id")
        .and_then(Value::as_str)
        .ok_or("no Id in podman image inspect output")?
        .to_owned();
    let flavour = image
        .pointer("/Labels/sh.tilo.image.flavour")
        .or_else(|| image.pointer("/Config/Labels/sh.tilo.image.flavour"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((id, flavour))
}

/// Apply [`probe_command`]'s output to `facts`.
pub fn parse_probe(output: &str, facts: &mut ImageFacts) {
    let (base, uki) = output.split_once("@@uki").unwrap_or((output, ""));
    facts.base = serde_json::from_str(base.trim()).ok();
    facts.sealed = uki.lines().any(|l| !l.trim().is_empty());
}

fn refused(image: &str, problem: impl Into<String>, fix: impl Into<String>) -> CliError {
    CliError::ImageRefused {
        image: image.to_owned(),
        problem: problem.into(),
        fix: fix.into(),
    }
}

/// Only an unsealed dev image with a dev name may go into a dev VM.
///
/// # Errors
/// A production name, a missing dev flavour, a sealed image.
pub fn check_image(reference: &str, facts: &ImageFacts) -> Result<(), CliError> {
    let build = "build the dev image FROM it: tilo-vm vm upgrade --from local   (or TILO_DEV_DISK=0 ./os/build.sh --dev)";
    if !is_dev_name(reference) {
        return Err(refused(
            reference,
            "is not named as a dev image (localhost/tilo-vm:<tag> or code.podesta.ai/qos/tilo-vm:<tag>); a production image is sealed and carries no sshd or tilo-vmd, and a VM must not be moved onto something that could be one",
            if reference.contains("/tilo:") || reference.contains("/tilo@") {
                build.to_owned()
            } else {
                "tag the dev image as localhost/tilo-vm:<tag>, then: tilo-vm vm upgrade --from local:localhost/tilo-vm:<tag>".to_owned()
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
            "is sealed: it carries a UKI under /boot/EFI/Linux, whose composefs digest no dev layer can match, so the VM could not merge slots into /usr",
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
///
/// # Errors
/// A base created before the VM's.
pub fn check_base_age(
    image: &str,
    vm_base: Option<&Value>,
    image_base: Option<&Value>,
) -> Result<Option<String>, CliError> {
    let digest = |v| text(v, "image_digest");
    if digest(vm_base).is_some() && digest(vm_base) == digest(image_base) {
        return Ok(None);
    }
    let created =
        |v| text(v, "base_created").and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
    match (created(vm_base), created(image_base)) {
        (Some(running), Some(new)) if new < running => Err(CliError::BaseOlder {
            image: image.to_owned(),
            base: text(image_base, "base_image")
                .unwrap_or("its base")
                .to_owned(),
            new: new.to_rfc3339(),
            running: running.to_rfc3339(),
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

/// One bootc deployment, as `bootc status --json` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deployment {
    pub image: String,
    pub transport: String,
    pub digest: String,
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

/// `bootc status --json`, reduced to what upgrade and rollback need.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootcStatus {
    /// The image the next boot follows: (image, transport).
    pub spec: Option<(String, String)>,
    pub booted: Option<Deployment>,
    pub staged: Option<Deployment>,
    pub rollback: Option<Deployment>,
}

/// # Errors
/// Output that is not bootc's status JSON.
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

/// The bootc command that stages `imgref`: `switch`, or `upgrade` when the VM
/// already follows it (bootc treats a switch to the same spec as a no-op).
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
    format!(
        "(ro=; if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then ro=1; mount -o remount,rw /boot || exit 1; fi; bootc {command}; rc=$?; [ -z \"$ro\" ] || mount -o remount,ro /boot; exit $rc)"
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
    format!(
        "mkdir -p {LOCKS} && flock --nonblock --conflict-exit-code 75 {LOCKS}/vm.lock sh -c {}",
        sh_quote(&bootc_line(&args))
    )
}

/// Printed in place of bootc's status when the VM has no bootc.
const NO_BOOTC: &str = "@@nobootc";

/// A remote line that says whether the VM has bootc.
pub const BOOTC_PROBE: &str =
    "if command -v bootc >/dev/null 2>&1; then echo bootc=yes; else echo bootc=no; fi";

/// [`BOOTC_PROBE`]'s answer; `None` when it did not run.
pub fn parse_bootc_probe(output: &str) -> Option<bool> {
    output.lines().find_map(|line| match line.trim() {
        "bootc=yes" => Some(true),
        "bootc=no" => Some(false),
        _ => None,
    })
}

/// What `upgrade` reads from the VM before it changes anything.
pub fn vm_facts_script() -> String {
    format!(
        "cat /proc/sys/kernel/random/boot_id; echo @@base; cat /usr/share/tilo-vm/base.json 2>/dev/null; echo; echo @@free; df -Pk /var | awk 'NR==2 {{print $4}}'; echo @@bootc; if command -v bootc >/dev/null 2>&1; then {}; else echo {NO_BOOTC}; fi",
        bootc_line(&["status", "--json"])
    )
}

/// Why [`parse_vm_facts`] found no bootc status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsError {
    /// The VM has no bootc at all: not a bootc dev image.
    NoBootc,
    /// bootc ran but said nothing usable, or the output is cut short.
    Unreadable(String),
}

#[derive(Debug, Clone, Default)]
pub struct VmFacts {
    pub boot_id: String,
    pub base: Option<Value>,
    pub free_bytes: Option<u64>,
    pub bootc: BootcStatus,
}

/// # Errors
/// No bootc on the VM, or no bootc status in the output.
pub fn parse_vm_facts(output: &str) -> Result<VmFacts, FactsError> {
    let cut = |what: &str| FactsError::Unreadable(what.to_owned());
    let (boot, rest) = output
        .split_once("@@base")
        .ok_or_else(|| cut("no boot id"))?;
    let (base, rest) = rest
        .split_once("@@free")
        .ok_or_else(|| cut("no base.json section"))?;
    let (free, bootc) = rest
        .split_once("@@bootc")
        .ok_or_else(|| cut("no free space section"))?;
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
        bootc: parse_bootc_status(bootc).map_err(FactsError::Unreadable)?,
    })
}

/// The host-side OCI layout for one VM.
pub fn host_layout(vm: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-vm")
        .join("images")
        .join(vm)
}

/// Write `image` from podman's storage into the layout. Blobs the layout
/// already has are reused, and layers stay uncompressed: compressing costs far
/// more than it saves on the way into a local VM.
pub fn export_command(image: &str, layout: &Path, digest_file: &Path) -> CommandLine {
    CommandLine::new(
        "skopeo",
        &["copy", "--quiet", "--dest-oci-accept-uncompressed-layers"],
    )
    .arg("--digestfile")
    .arg(digest_file.display().to_string())
    .arg(format!("containers-storage:{image}"))
    .arg(format!("oci:{}:{LAYOUT_TAG}", layout.display()))
}

/// Mirror the layout into [`VM_LAYOUT`]: only new blobs cross, and blobs no
/// longer referenced go, so the VM keeps one image's worth.
pub fn rsync_command(layout: &Path, endpoint: &SshEndpoint, key: Option<&str>) -> CommandLine {
    let ssh = remote::ssh_command(endpoint, key, &[], false);
    let mut words = vec![sh_quote(&ssh.program)];
    let options = ssh.args.len().saturating_sub(1);
    words.extend(ssh.args.iter().take(options).map(|a| sh_quote(a)));
    let mut source = crate::vmbuild::rsync_source(layout);
    if !source.ends_with('/') {
        source.push('/');
    }
    CommandLine::new(
        &crate::host::rsync_program().unwrap_or_else(|| "rsync".into()),
        &["--archive", "--delete", "--whole-file", "--stats"],
    )
    .arg("--rsh")
    .arg(words.join(" "))
    .arg("--rsync-path")
    .arg(format!(
        "mkdir -p {LOCKS} {VM_LAYOUT} && flock --nonblock --conflict-exit-code 75 {LOCKS}/image.lock rsync"
    ))
    .arg(source)
    .arg(format!("{}@{}:{VM_LAYOUT}/", endpoint.user, endpoint.host))
    .env("LC_ALL", "C")
}

/// Bytes rsync sent, from its `--stats`.
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

/// Every blob the layout's index reaches: manifests, configs, layers.
///
/// # Errors
/// An unreadable index or manifest.
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

/// Keep only the tag this tool writes, then delete the blobs nothing
/// references any more. Returns the bytes freed.
///
/// # Errors
/// An unreadable layout.
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

/// Total size of the blobs the layout references.
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

    use super::*;

    #[test]
    fn from_local_builds_and_from_local_image_does_not() {
        assert_eq!(
            parse_from("local").unwrap(),
            (LOCAL_DEV_IMAGE.to_owned(), true)
        );
        assert_eq!(
            parse_from("local:localhost/tilo-vm:mine").unwrap(),
            ("localhost/tilo-vm:mine".to_owned(), false)
        );
        for bad in ["local:", "ci:main", "worktree", "locally"] {
            let error = parse_from(bad).unwrap_err();
            assert!(
                error
                    .hint()
                    .contains("--from local:localhost/tilo-vm:<tag>"),
                "{bad}: {error}"
            );
        }
    }

    #[test]
    fn only_dev_names_pass() {
        for good in [
            "localhost/tilo-vm:latest",
            "code.podesta.ai/qos/tilo-vm:main-1a2b3c4",
            "localhost/tilo-vm@sha256:0123",
        ] {
            assert!(is_dev_name(good), "{good}");
        }
        for bad in [
            "localhost/tilo:latest",
            "code.podesta.ai/qos/tilo:main",
            "localhost/tilo-vm",
            "localhost/tilo-vmx:latest",
            "tilo-vm:latest",
        ] {
            assert!(!is_dev_name(bad), "{bad}");
        }
    }

    fn dev_facts() -> ImageFacts {
        ImageFacts {
            id: "abc".into(),
            flavour: Some("dev".into()),
            sealed: false,
            base: None,
        }
    }

    #[test]
    fn production_named_unlabelled_and_sealed_images_are_refused_with_the_fix() {
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

    #[test]
    fn inspect_and_probe_output_become_facts() {
        let (id, flavour) =
            parse_inspect(r#"[{"Id":"4bd2","Labels":{"sh.tilo.image.flavour":"dev","x":"y"}}]"#)
                .unwrap();
        assert_eq!((id.as_str(), flavour.as_deref()), ("4bd2", Some("dev")));
        let (_, none) = parse_inspect(r#"[{"Id":"4bd2","Labels":null}]"#).unwrap();
        assert_eq!(none, None);

        let mut facts = ImageFacts::default();
        parse_probe(
            "{\"base_created\":\"2026-09-01T00:00:00Z\"}\n\n@@uki\n",
            &mut facts,
        );
        assert!(!facts.sealed);
        assert_eq!(
            text(facts.base.as_ref(), "base_created"),
            Some("2026-09-01T00:00:00Z")
        );
        parse_probe("\n@@uki\ntilo-1.efi\n", &mut facts);
        assert!(facts.sealed);
        assert!(facts.base.is_none());
    }

    #[test]
    fn an_older_base_is_refused_and_an_unknown_age_is_a_note() {
        let base = |digest: &str, created: &str| json!({ "image_digest": digest, "base_created": created, "base_image": "localhost/tilo:latest" });
        let running = base("sha256:aa", "2026-09-20T10:00:00Z");
        let older = base("sha256:bb", "2026-09-01T10:00:00Z");
        let newer = base("sha256:cc", "2026-09-21T10:00:00Z");
        let error =
            check_base_age("localhost/tilo-vm:latest", Some(&running), Some(&older)).unwrap_err();
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

    const STATUS: &str = r#"{"apiVersion":"org.containers.bootc/v1","kind":"BootcHost",
      "spec":{"image":{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"},"bootOrder":"default"},
      "status":{"staged":null,
        "booted":{"image":{"image":{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"},"imageDigest":"sha256:1111111111111111111111"},"pinned":false},
        "rollback":{"image":{"image":{"image":"localhost/tilo-vm:standin-a","transport":"registry"},"imageDigest":"sha256:2222222222222222222222"}}}}"#;

    #[test]
    fn bootc_status_reduces_to_spec_and_deployments() {
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

    #[test]
    fn staging_switches_once_then_upgrades() {
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
            stage_command(&following, "registry", "code.podesta.ai/qos/tilo-vm:x").join(" "),
            "bootc switch code.podesta.ai/qos/tilo-vm:x"
        );
        let locked = under_vm_lock(&["bootc".into(), "upgrade".into()]);
        assert!(
            locked.contains(
                "flock --nonblock --conflict-exit-code 75 /var/lib/tilo-vm/locks/vm.lock sh -c"
            ),
            "{locked}"
        );
        assert!(locked.contains("bootc upgrade; rc=$?"), "{locked}");
        // bootc gets a writable ESP for the one command, and it is read-only again after.
        let line = bootc_line(&["status", "--json"]);
        assert!(line.contains("mount -o remount,rw /boot"), "{line}");
        assert!(
            line.contains("mount -o remount,ro /boot; exit $rc)"),
            "{line}"
        );
        assert!(vm_facts_script().contains(&format!("then {line}; else echo @@nobootc; fi")));
    }

    #[test]
    fn vm_facts_parse_in_one_round_trip() {
        let output = format!(
            "b00t\n@@base\n{{\"image_digest\":\"sha256:aa\"}}\n\n@@free\n1024\n@@bootc\n{STATUS}\n"
        );
        let facts = parse_vm_facts(&output).unwrap();
        assert_eq!(facts.boot_id, "b00t");
        assert_eq!(facts.free_bytes, Some(1 << 20));
        assert_eq!(text(facts.base.as_ref(), "image_digest"), Some("sha256:aa"));
        assert!(facts.bootc.booted.is_some());
        assert!(matches!(
            parse_vm_facts("b00t\n@@base\n\n@@free\n\n@@bootc\n"),
            Err(FactsError::Unreadable(_))
        ));
        // No bootc at all is its own answer, not a JSON parse error.
        assert_eq!(
            parse_vm_facts("b00t\n@@base\n\n@@free\n1\n@@bootc\n@@nobootc\n").unwrap_err(),
            FactsError::NoBootc
        );
    }

    #[test]
    fn the_bootc_probe_answers_yes_no_or_nothing() {
        assert_eq!(parse_bootc_probe("bootc=yes\n"), Some(true));
        assert_eq!(parse_bootc_probe("motd\nbootc=no\n"), Some(false));
        assert_eq!(parse_bootc_probe(""), None);
        assert!(BOOTC_PROBE.contains("command -v bootc"));
    }

    #[test]
    fn transfer_commands_reuse_blobs_and_hold_the_image_lock() {
        let export = export_command(
            "localhost/tilo-vm:latest",
            Path::new("/c/dev"),
            Path::new("/c/dev.digest"),
        );
        assert_eq!(
            export.display(),
            "skopeo copy --quiet --dest-oci-accept-uncompressed-layers --digestfile /c/dev.digest containers-storage:localhost/tilo-vm:latest oci:/c/dev:tilo-vm"
        );
        let endpoint = SshEndpoint {
            user: "root".into(),
            host: "127.0.0.1".into(),
            port: 2223,
        };
        let rsync = rsync_command(Path::new("/c/dev"), &endpoint, Some("/k/id"));
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
            rsync_sent_bytes("Number of files: 9\nTotal transferred file size: 12,345,678 bytes\n"),
            Some(12_345_678)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_image_layout_is_a_local_msys2_rsync_operand() {
        let endpoint = SshEndpoint {
            user: "root".into(),
            host: "127.0.0.1".into(),
            port: 2223,
        };
        let line = rsync_command(Path::new(r"C:\tilo-build\image-layout"), &endpoint, None);
        assert_eq!(
            line.args[line.args.len() - 2],
            "/c/tilo-build/image-layout/"
        );
    }

    #[test]
    fn pruning_keeps_what_the_tag_reaches() {
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

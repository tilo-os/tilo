//! Moving a running dev VM onto another image with bootc: a dev image built on
//! this host (`vm upgrade --from local`), a CI build (`--from ci:<tag>`: its
//! published dev image, or one built here FROM its integration image) or any
//! dev image from a registry.
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
use crate::vmbuild::sh_quote;

/// The tag of the locally built images: `TILO_LOCAL_TAG`, `latest` when unset,
/// so builds from different checkouts on one host can keep their images apart.
///
/// # Errors
/// A value that is not an image tag.
pub fn local_tag() -> Result<String, CliError> {
    parse_local_tag(std::env::var("TILO_LOCAL_TAG").ok().as_deref())
}

fn parse_local_tag(value: Option<&str>) -> Result<String, CliError> {
    let tag = match value {
        None | Some("") => return Ok("latest".to_owned()),
        Some(tag) => tag,
    };
    let mut chars = tag.chars();
    let first = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    if first && tag.len() <= 128 && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
        Ok(tag.to_owned())
    } else {
        Err(CliError::InvalidInput(format!(
            "TILO_LOCAL_TAG={tag} is not an image tag ([A-Za-z0-9_][A-Za-z0-9_.-]*, at most 128 characters)"
        )))
    }
}

/// What `--from local` builds and moves the VM onto.
///
/// # Errors
/// An invalid `TILO_LOCAL_TAG`.
pub fn local_dev_image() -> Result<String, CliError> {
    Ok(format!("localhost/tilo-vm:{}", local_tag()?))
}

/// The production image the local dev image is built FROM.
///
/// # Errors
/// An invalid `TILO_LOCAL_TAG`.
pub fn local_base() -> Result<String, CliError> {
    Ok(format!("localhost/tilo:{}", local_tag()?))
}
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

/// What `vm upgrade --from` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeFrom {
    /// `local`: build this worktree's dev image; `local:<image>`: one already
    /// in podman's storage.
    Local { image: String, build: bool },
    /// `ci:<tag>` or `sha:<commit>`: a CI build of the OS, by its `main-<sha7>`
    /// (or release) tag.
    Ci { tag: String },
}

const FROM_FORMS: &str = "tilo-vm vm upgrade --from local (build this worktree's dev image)   |   --from ci:main-<sha7> or --from sha:<commit> (a CI build)   |   --from local:localhost/tilo-vm:<tag> (one already built)   |   --image registry.tilo.so/tilo/tilo-vm:<tag> (a published one)";

/// Parse `--from`; a `sha:` is resolved through the worktree's git.
///
/// # Errors
/// Anything else.
pub fn parse_from(
    from: &str,
    worktree: Option<&crate::context::Worktree>,
) -> Result<UpgradeFrom, CliError> {
    let unsupported = || CliError::Unsupported {
        what: format!("--from {from} for vm upgrade"),
        alternative: FROM_FORMS.into(),
    };
    match from.split_once(':') {
        None if from == "local" => Ok(UpgradeFrom::Local {
            image: local_dev_image()?,
            build: true,
        }),
        Some(("local", image)) if !image.is_empty() => Ok(UpgradeFrom::Local {
            image: image.to_owned(),
            build: false,
        }),
        Some(("ci" | "sha", _)) => match crate::ci::parse(from, worktree) {
            Ok(crate::ci::Source::Ci { tag, .. }) => Ok(UpgradeFrom::Ci { tag }),
            _ => Err(unsupported()),
        },
        _ => Err(unsupported()),
    }
}

/// Where a CI build of `tag` can come from, in the order `vm upgrade` tries
/// them: the dev images dev-disk.yml publishes (from the unsigned integration
/// image, then from a signed release), which the VM pulls itself; then the
/// production images a dev image can be built FROM here: the integration image
/// build-image.yml pushes for every main build, then the signed release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiCandidates {
    pub dev_images: Vec<String>,
    pub bases: Vec<String>,
}

pub fn ci_candidates(tag: &str, arch: &str) -> CiCandidates {
    // `ci:main-1a2b3c4-x86_64` names the integration image's own tag.
    let tag = tag.strip_suffix(&format!("-{arch}")).unwrap_or(tag);
    let dev_tag = tag.strip_suffix("-ci").unwrap_or(tag);
    CiCandidates {
        dev_images: vec![
            format!("registry.tilo.so/tilo/tilo-vm:{dev_tag}-ci"),
            format!("registry.tilo.so/tilo/tilo-vm:{dev_tag}"),
        ],
        bases: vec![
            format!("registry.tilo.so/tilo/tilo-ci:{dev_tag}-{arch}"),
            format!("registry.tilo.so/tilo/tilo:{dev_tag}"),
        ],
    }
}

/// The seven-hex commit a `sha:` tag (`main-<sha7>`) names.
pub fn ci_commit(tag: &str) -> Option<&str> {
    tag.strip_prefix("main-")
        .filter(|sha| sha.len() == 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A dev build of a branch or pull request is tagged `<ref>-<sha7>`, so a
/// commit that is not on main is found by listing the repositories' tags:
/// `tilo-vm:<ref>-<sha7>-ci` is the published dev image,
/// `tilo-ci:<ref>-<sha7>-<arch>` the image a dev image can be built FROM.
/// `skopeo list-tags` output in, candidates out; several refs sort by name so
/// the choice is stable.
pub fn ci_ref_candidates(sha: &str, arch: &str, vm_tags: &str, ci_tags: &str) -> CiCandidates {
    fn tags(listing: &str) -> Vec<String> {
        serde_json::from_str::<serde_json::Value>(listing)
            .ok()
            .and_then(|value| value.get("Tags").cloned())
            .and_then(|tags| serde_json::from_value::<Vec<String>>(tags).ok())
            .unwrap_or_default()
    }
    fn named_ref(tag: &str, suffix: &str) -> bool {
        tag.strip_suffix(suffix).is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
    }
    let mut dev_images: Vec<String> = tags(vm_tags)
        .into_iter()
        .filter(|tag| named_ref(tag, &format!("-{sha}-ci")))
        .map(|tag| format!("registry.tilo.so/tilo/tilo-vm:{tag}"))
        .collect();
    let mut bases: Vec<String> = tags(ci_tags)
        .into_iter()
        .filter(|tag| named_ref(tag, &format!("-{sha}-{arch}")))
        .map(|tag| format!("registry.tilo.so/tilo/tilo-ci:{tag}"))
        .collect();
    dev_images.sort();
    bases.sort();
    CiCandidates { dev_images, bases }
}

/// `skopeo list-tags` for one repository.
pub fn list_tags_command(repository: &str) -> CommandLine {
    CommandLine::new("skopeo", &["list-tags"]).arg(format!("docker://{repository}"))
}

/// The local tag of a dev image built FROM a CI base.
pub fn ci_dev_image(base: &str) -> String {
    let tag = base.rsplit_once(':').map_or(base, |(_, tag)| tag);
    format!("localhost/tilo-vm:ci-{tag}")
}

/// Does `reference` exist in its registry? (`skopeo inspect --raw` reads only
/// the manifest.)
pub fn exists_command(reference: &str) -> CommandLine {
    CommandLine::new("skopeo", &["inspect", "--raw"]).arg(format!("docker://{reference}"))
}

/// A dev image may only be named `localhost/tilo-vm` or
/// `registry.tilo.so/tilo/tilo-vm`, with a tag or digest: the same rule
/// `os/dev/build-dev-image.sh` enforces when it tags one.
pub fn is_dev_name(reference: &str) -> bool {
    ["localhost/tilo-vm", "registry.tilo.so/tilo/tilo-vm"]
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

/// Where `tilo-vm image build` writes, under its output directory.
pub const BUILT_LAYOUT: &str = "image";
/// The dev disk `tilo-vm image build --disk` writes there.
pub const BUILT_DISK: &str = "disk.qcow2";

/// The script `image disk` runs (relative to the worktree): the build's disk
/// step, with `TILO_SKIP_BUILD=1`.
pub const DISK_SCRIPT: &str = "os/dev/build-image.sh";

/// Copy a layout [`BUILT_LAYOUT`]-style (tag [`LAYOUT_TAG`]) into podman's
/// storage as `image`.
pub fn import_layout_command(layout: &Path, image: &str) -> CommandLine {
    CommandLine::new("skopeo", &["copy", "--quiet"])
        .arg(format!("oci:{}:{LAYOUT_TAG}", layout.display()))
        .arg(format!("containers-storage:{image}"))
}

/// Did bootc refuse a switch because the VM already boots that image? The
/// same image can carry another manifest digest (compressed or not), which
/// only bootc's composefs digest sees through.
pub fn is_already_booted(bootc_stderr: &str) -> bool {
    bootc_stderr.contains("same fs-verity digest as the existing Some(Booted) deployment")
}

pub fn is_already_rollback(bootc_stderr: &str) -> bool {
    bootc_stderr.contains("same fs-verity digest as the existing Some(Rollback) deployment")
}

/// Is `path` an OCI image layout directory (what `--image <dir>` takes)?
pub fn is_layout(path: &Path) -> bool {
    path.join("oci-layout").is_file() && path.join("index.json").is_file()
}

/// What `vm upgrade --image <layout>` checks, saved beside the layout as
/// `<layout>.facts`: [`inspect_command`]'s output, a `@@probe` line and
/// [`probe_command`]'s, as they were where the image was built.
pub fn layout_facts_text(inspect: &str, probe: &str) -> String {
    format!("{}\n@@probe\n{probe}", inspect.trim_end())
}

/// The image's name and facts from [`layout_facts_text`].
///
/// # Errors
/// Text without the marker, or inspect output that does not parse.
pub fn parse_layout_facts(text: &str) -> Result<(String, ImageFacts), String> {
    let (inspect, probe) = text
        .split_once("\n@@probe\n")
        .ok_or("no @@probe section in the layout's facts")?;
    let (id, flavour) = parse_inspect(inspect)?;
    let name = serde_json::from_str::<Value>(inspect)
        .ok()
        .and_then(|v| {
            v.pointer("/0/RepoTags/0")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .map_or_else(|| local_dev_image().map_err(|e| e.to_string()), Ok)?;
    let mut facts = ImageFacts {
        id,
        flavour,
        ..ImageFacts::default()
    };
    parse_probe(probe, &mut facts);
    Ok((name, facts))
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
            "is not named as a dev image (localhost/tilo-vm:<tag> or registry.tilo.so/tilo/tilo-vm:<tag>); a production image is sealed and carries no sshd or tilo-vmd, and a VM must not be moved onto something that could be one",
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
    xdg_paths::cache_dir()
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
///
/// # Errors
/// Returns an error when the rsync SSH transport is unavailable.
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
    .arg(format!(
        "mkdir -p {LOCKS} {VM_LAYOUT} && flock --nonblock --conflict-exit-code 75 {LOCKS}/image.lock rsync"
    ))
    .arg(source)
    .arg(format!("{}@{}:{VM_LAYOUT}/", endpoint.user, endpoint.host))
    .env("LC_ALL", "C"))
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

    /// `TILO_LOCAL_TAG` names the local images' tag, `latest` when unset; anything else is refused.
    #[test]
    fn local_tag_is_latest_unless_named() {
        assert_eq!(parse_local_tag(None).unwrap(), "latest");
        assert_eq!(parse_local_tag(Some("")).unwrap(), "latest");
        assert_eq!(parse_local_tag(Some("wt-2_a.b")).unwrap(), "wt-2_a.b");
        assert_eq!(
            parse_local_tag(Some(&"a".repeat(128))).unwrap(),
            "a".repeat(128)
        );
        for bad in [".x", "-x", "a/b", "a:b", "a b", &"a".repeat(129)] {
            assert!(parse_local_tag(Some(bad)).is_err(), "{bad}");
        }
    }

    /// from-local builds and from-local-image does not; only dev names pass; production-named, unlabelled
    /// and sealed images are refused with the fix; an older base is refused and an unknown age is a note.
    #[test]
    fn image_upgrades_accept_only_fresh_labelled_dev_images() {
        // Phase: from_local_builds_and_from_local_image_does_not.
        {
            assert_eq!(
                parse_from("local", None).unwrap(),
                UpgradeFrom::Local {
                    image: local_dev_image().unwrap(),
                    build: true
                }
            );
            assert_eq!(
                parse_from("local:localhost/tilo-vm:mine", None).unwrap(),
                UpgradeFrom::Local {
                    image: "localhost/tilo-vm:mine".to_owned(),
                    build: false
                }
            );
            for bad in ["local:", "ci:", "ci:a b", "sha:xyz", "worktree", "locally"] {
                let error = parse_from(bad, None).unwrap_err();
                assert!(
                    error
                        .hint()
                        .contains("--from local:localhost/tilo-vm:<tag>")
                        && error.hint().contains("--from ci:main-<sha7>"),
                    "{bad}: {error}"
                );
            }
        }

        // Phase: only_dev_names_pass.
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

        // Phase: production_named_unlabelled_and_sealed_images_are_refused_with_the_fix.
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

        // Phase: an_older_base_is_refused_and_an_unknown_age_is_a_note.
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

    /// A built layout's facts read back as the image's own would, and a layout directory is told
    /// apart from a registry reference.
    #[test]
    fn built_layout_facts_read_as_the_images_own() {
        let inspect = "[{\"Id\": \"abc\", \"RepoTags\": [\"localhost/tilo-vm:latest\"], \"Labels\": {\"sh.tilo.image.flavour\": \"dev\"}}]\n";
        let written = layout_facts_text(
            inspect,
            "{\"base_created\": \"2026-09-20T10:00:00Z\"}\n@@uki\n",
        );
        let (name, facts) = parse_layout_facts(&written).unwrap();
        assert_eq!(name, "localhost/tilo-vm:latest");
        assert_eq!(facts.id, "abc");
        assert_eq!(facts.flavour.as_deref(), Some("dev"));
        assert!(!facts.sealed);
        assert_eq!(
            text(facts.base.as_ref(), "base_created"),
            Some("2026-09-20T10:00:00Z")
        );
        check_image(&name, &facts).unwrap();
        let (_, sealed) =
            parse_layout_facts(&layout_facts_text(inspect, "{}\n@@uki\ntilo.efi\n")).unwrap();
        assert!(sealed.sealed);
        assert!(parse_layout_facts(inspect).is_err());

        let dir = tempfile::tempdir().unwrap();
        assert!(!is_layout(dir.path()));
        std::fs::write(dir.path().join("oci-layout"), "{}").unwrap();
        std::fs::write(dir.path().join("index.json"), "{}").unwrap();
        assert!(is_layout(dir.path()));
        assert!(!is_layout(Path::new("registry.tilo.so/tilo/tilo-vm:main")));

        // The VM already boots this image under another manifest digest.
        assert!(is_already_booted(
            "error: Switching: Composefs Switching: Target image has the same fs-verity digest as the existing Some(Booted) deployment."
        ));
        assert!(!is_already_booted(
            "error: Switching: pulling: connection refused"
        ));
    }

    /// A commit off main is found by its `<ref>-<sha7>` tags.
    #[test]
    fn a_pull_request_commit_resolves_through_its_ref_tags() {
        assert_eq!(ci_commit("main-1a2b3c4"), Some("1a2b3c4"));
        assert_eq!(ci_commit("main-1a2b3c4-x86_64"), None);
        assert_eq!(ci_commit("v1.2.3"), None);
        let vm = r#"{"Repository":"registry.tilo.so/tilo/tilo-vm","Tags":["main-0000000-ci","pr-12-1a2b3c4-ci","pr-12-1a2b3c4-ci-x86_64","fix-x-1a2b3c4-ci","1a2b3c4-ci"]}"#;
        let ci =
            r#"{"Tags":["pr-12-1a2b3c4-x86_64","pr-12-1a2b3c4-aarch64","main-1a2b3c4x-x86_64"]}"#;
        let found = ci_ref_candidates("1a2b3c4", "x86_64", vm, ci);
        assert_eq!(
            found.dev_images,
            vec![
                "registry.tilo.so/tilo/tilo-vm:fix-x-1a2b3c4-ci".to_owned(),
                "registry.tilo.so/tilo/tilo-vm:pr-12-1a2b3c4-ci".to_owned(),
            ]
        );
        assert_eq!(
            found.bases,
            vec!["registry.tilo.so/tilo/tilo-ci:pr-12-1a2b3c4-x86_64".to_owned()]
        );
        let none = ci_ref_candidates("1a2b3c4", "aarch64", "not json", "{}");
        assert!(none.dev_images.is_empty() && none.bases.is_empty());
        assert_eq!(
            list_tags_command("registry.tilo.so/tilo/tilo-vm").display(),
            "skopeo list-tags docker://registry.tilo.so/tilo/tilo-vm"
        );
    }

    /// `--from ci:` and `--from sha:` name a CI tag, which resolves to published dev images, then
    /// bases; a dev image built here from a CI base is a local dev name.
    #[test]
    fn ci_upgrade_sources_resolve_to_published_or_locally_built_dev_images() {
        // Phase: from_ci_and_sha_name_a_ci_tag.
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

        // Phase: ci_candidates_try_published_dev_images_then_bases.
        {
            let expected = CiCandidates {
                dev_images: vec![
                    "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci".into(),
                    "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4".into(),
                ],
                bases: vec![
                    "registry.tilo.so/tilo/tilo-ci:main-1a2b3c4-x86_64".into(),
                    "registry.tilo.so/tilo/tilo:main-1a2b3c4".into(),
                ],
            };
            assert_eq!(ci_candidates("main-1a2b3c4", "x86_64"), expected);
            // The integration image's own tag and the dev disk's -ci tag name the same build.
            assert_eq!(ci_candidates("main-1a2b3c4-x86_64", "x86_64"), expected);
            assert_eq!(ci_candidates("main-1a2b3c4-ci", "x86_64"), expected);
            for image in expected.dev_images {
                assert!(is_dev_name(&image), "{image}");
            }
        }

        // Phase: a_dev_image_built_from_a_ci_base_is_a_local_dev_name.
        {
            let image = ci_dev_image("registry.tilo.so/tilo/tilo-ci:main-1a2b3c4-x86_64");
            assert_eq!(image, "localhost/tilo-vm:ci-main-1a2b3c4-x86_64");
            assert!(is_dev_name(&image));
            assert_eq!(
                exists_command("registry.tilo.so/tilo/tilo-ci:main-1a2b3c4-x86_64").display(),
                "skopeo inspect --raw docker://registry.tilo.so/tilo/tilo-ci:main-1a2b3c4-x86_64"
            );
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

    /// Inspect and probe output become facts; bootc status reduces to spec and deployments; VM facts parse
    /// in one round trip; the bootc probe answers yes, no or nothing.
    #[test]
    fn guest_facts_are_probed_in_one_round_trip() {
        // Phase: inspect_and_probe_output_become_facts.
        {
            let (id, flavour) = parse_inspect(
                r#"[{"Id":"4bd2","Labels":{"sh.tilo.image.flavour":"dev","x":"y"}}]"#,
            )
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

        // Phase: bootc_status_reduces_to_spec_and_deployments.
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

        // Phase: vm_facts_parse_in_one_round_trip.
        {
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

        // Phase: the_bootc_probe_answers_yes_no_or_nothing.
        {
            assert_eq!(parse_bootc_probe("bootc=yes\n"), Some(true));
            assert_eq!(parse_bootc_probe("motd\nbootc=no\n"), Some(false));
            assert_eq!(parse_bootc_probe(""), None);
            assert!(BOOTC_PROBE.contains("command -v bootc"));
        }
    }

    const STATUS: &str = r#"{"apiVersion":"org.containers.bootc/v1","kind":"BootcHost",
      "spec":{"image":{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"},"bootOrder":"default"},
      "status":{"staged":null,
        "booted":{"image":{"image":{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"},"imageDigest":"sha256:1111111111111111111111"},"pinned":false},
        "rollback":{"image":{"image":{"image":"localhost/tilo-vm:standin-a","transport":"registry"},"imageDigest":"sha256:2222222222222222222222"}}}}"#;

    /// Staging switches once then upgrades; transfer commands reuse blobs and hold the image lock; the
    /// Windows image layout is a local MSYS2 rsync operand; pruning keeps what the tag reaches.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn image_transfer_stages_once_and_prunes_only_unreachable_blobs() {
        // Phase: staging_switches_once_then_upgrades.
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
            // bootc gets a writable ESP for the one command, and it is read-only again after.
            let line = bootc_line(&["status", "--json"]);
            assert!(line.contains("mount -o remount,rw /boot"), "{line}");
            assert!(
                line.contains("mount -o remount,ro /boot; exit $rc)"),
                "{line}"
            );
            assert!(vm_facts_script().contains(&format!("then {line}; else echo @@nobootc; fi")));
        }

        // Phase: transfer_commands_reuse_blobs_and_hold_the_image_lock.
        {
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

        // Phase: windows_image_layout_is_a_local_msys2_rsync_operand.
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

        // Phase: pruning_keeps_what_the_tag_reaches.
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

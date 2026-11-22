//! Moving a running dev VM onto another image with bootc: this tree's dev
//! image, built by t's `os-dev-image-<arch>` check (`vm upgrade --from local`,
//! `image build`), a CI build (`--from ci:<tag>`) or any dev image from a
//! registry. A built image travels as an OCI layout whose blobs go to the
//! device as content it does not have yet, so only new blobs cross; no
//! registry and no daemon on the host.
//!
//! t is the one builder of dev images and disks: the checks that publish
//! main's build it for this tree too, on a build server of the image's
//! architecture, so no host compiles the OS or needs podman or loop devices.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::CliError;
use crate::hypervisor::CommandLine;

pub const LAYOUT_TAG: &str = "tilo-vm";

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
            "is not named as a dev image (localhost/tilo-vm:<tag> or registry.tilo.so/tilo/tilo-vm:<tag>); a production image is sealed and carries no sshd or device steps, and a VM must not be moved onto something that could be one",
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

pub fn host_layout(vm: &str) -> PathBuf {
    tilo_paths::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-vm")
        .join("images")
        .join(vm)
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
        fn built_images_are_read_and_their_layout_pruned;

        phase a_layout_keeps_only_the_blobs_its_index_reaches;
        {
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
}

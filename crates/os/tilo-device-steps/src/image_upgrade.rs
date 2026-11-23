//! The device's OS image: what it boots ([`boot_facts`]), and the `upgrade` step, which stages
//! another image with bootc under the VM lock and gives the staged deployment its boot loader try
//! counter. Rebooting into it is the `reboot` step's ([`crate::power`]).

use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use serde_json::Value;
use tilo_device_protocol::TreeManifest;
use tilo_device_protocol::image::{
    BootFacts, BootcStatus, Deployment, ImageSource, LAYOUT_DIR, LAYOUT_TAG, Upgrade, UpgradeResult,
};

use crate::content;
use crate::daemon::Daemon;
use crate::error::DaemonError;

/// The device's update-channel helper, on images that follow a channel.
const UPDATE_CHANNEL: &str = "/usr/libexec/tilo-update-channel";

/// How many tries a staged deployment gets; tilo-deployment's own default.
const BOOT_TRIES: u32 = 3;

/// One shell line's outcome.
struct Ran {
    success: bool,
    stdout: String,
    stderr: String,
}

fn sh(line: &str) -> Result<Ran, DaemonError> {
    let output = Command::new("sh")
        .args(["-c", line])
        .output()
        .map_err(DaemonError::io("run sh"))?;
    Ok(Ran {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// `command` with the ESP writable. On the composefs backend bootc mounts the ESP read-only at
/// /boot, and then fails its own status, switch and rollback with "Device or resource busy": a
/// second mount of a vfat superblock already mounted read-only cannot be read-write. So /boot is
/// remounted read-write for the one command and read-only again after it.
fn esp_writable_for(command: &str) -> String {
    format!(
        "(ro=; if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then ro=1; mount -o remount,rw /boot || exit 1; fi; {command}; rc=$?; [ -z \"$ro\" ] || mount -o remount,ro /boot; exit $rc)"
    )
}

fn bootc(words: &[&str]) -> String {
    let words: Vec<String> = words.iter().map(|word| quote(word)).collect();
    esp_writable_for(&format!("bootc {}", words.join(" ")))
}

/// Leave the ESP writable until the next boot: bootc applies a staged composefs deployment at
/// shutdown, which mounts the ESP too and fails as [`esp_writable_for`] describes, and the device
/// then silently boots its old image again.
pub(crate) const ESP_WRITABLE_UNTIL_BOOT: &str = "if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then mount -o remount,rw /boot; fi";

fn deployment(value: &Value) -> Option<Deployment> {
    let image = value.get("image")?;
    Some(Deployment {
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

fn parse_bootc_status(json: &str) -> Result<BootcStatus, String> {
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
    let at = |key: &str| {
        value
            .pointer(&format!("/status/{key}"))
            .filter(|v| !v.is_null())
            .and_then(deployment)
    };
    Ok(BootcStatus {
        spec,
        booted: at("booted"),
        staged: at("staged"),
        rollback: at("rollback"),
    })
}

fn has_bootc() -> bool {
    crate::daemon::on_path("bootc")
}

fn bootc_status() -> Result<BootcStatus, DaemonError> {
    let ran = sh(&bootc(&["status", "--json"]))?;
    parse_bootc_status(&ran.stdout).map_err(|detail| DaemonError::Apply {
        step: "bootc status --json".into(),
        detail: format!("{detail}; {}", ran.stderr.trim()),
    })
}

/// The device's boot, for `status`.
pub fn boot_facts() -> BootFacts {
    let read = |path: &str| fs::read_to_string(path).unwrap_or_default();
    let free_bytes = sh("df -Pk /var | awk 'NR==2 {print $4}'")
        .ok()
        .and_then(|ran| ran.stdout.trim().parse::<u64>().ok())
        .map(|kib| kib.saturating_mul(1024));
    BootFacts {
        boot_id: read("/proc/sys/kernel/random/boot_id").trim().to_owned(),
        base: serde_json::from_str(&read("/usr/share/tilo-vm/base.json")).ok(),
        free_bytes,
        board: serde_json::from_str::<Value>(&read("/usr/lib/tilo/hardware-profile.json"))
            .ok()
            .and_then(|profile| profile.get("id")?.as_str().map(str::to_owned))
            .filter(|id| !id.is_empty()),
        bootc: has_bootc().then(bootc_status).and_then(Result::ok),
        booted_image: tilo_device_protocol::booted_image(&read("/proc/cmdline")),
    }
}

/// Lay the OCI layout `manifest` names out at [`LAYOUT_DIR`], deleting the blobs it no longer
/// names, so bootc stages from a layout that holds exactly the image.
fn lay_out(store: &Path, manifest: &str) -> Result<(), DaemonError> {
    let manifest: TreeManifest = serde_json::from_slice(
        &fs::read(content::blob(store, manifest)?).map_err(DaemonError::io("read the layout"))?,
    )
    .map_err(|e| DaemonError::InvalidParams(format!("the image layout is not one: {e}")))?;
    let root = Path::new(LAYOUT_DIR);
    let mut wanted = std::collections::BTreeSet::new();
    for file in &manifest.entries {
        let path = Path::new(&file.path);
        if !path.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(DaemonError::InvalidParams(format!(
                "'{}' is not a path inside the layout",
                file.path
            )));
        }
        let dest = root.join(path);
        let source = content::blob(store, &file.hash)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(DaemonError::io(format!("create {}", parent.display())))?;
        }
        fs::copy(&source, &dest).map_err(DaemonError::io(format!("write {}", dest.display())))?;
        wanted.insert(dest);
    }
    if let Ok(blobs) = fs::read_dir(root.join("blobs/sha256")) {
        for blob in blobs.flatten() {
            if !wanted.contains(&blob.path()) {
                fs::remove_file(blob.path())
                    .map_err(DaemonError::io(format!("remove {}", blob.path().display())))?;
            }
        }
    }
    Ok(())
}

/// The bootc line that stages `source`.
fn stage_line(status: &BootcStatus, source: &ImageSource) -> String {
    let (transport, imgref) = match source {
        ImageSource::Rollback => return bootc(&["rollback"]),
        ImageSource::Registry { image } => {
            let (follow, direct) = match image {
                Some(image) => (
                    format!("{UPDATE_CHANNEL} pin {}", quote(image)),
                    bootc(&["switch", image]),
                ),
                None => (format!("{UPDATE_CHANNEL} unpin"), bootc(&["upgrade"])),
            };
            return format!(
                "if [ -x {UPDATE_CHANNEL} ]; then {follow} && {}; else {direct}; fi",
                esp_writable_for("/usr/libexec/tilo-verified-update")
            );
        }
        ImageSource::Layout { .. } => ("oci", format!("{LAYOUT_DIR}:{LAYOUT_TAG}")),
    };
    let follows = status
        .spec
        .as_ref()
        .is_some_and(|(image, t)| *image == imgref && t == transport);
    if follows {
        bootc(&["upgrade"])
    } else {
        bootc(&["switch", "--transport", transport, &imgref])
    }
}

/// bootc's refusal for want of space.
fn insufficient_space(stderr: &str) -> Option<(String, String)> {
    let (_, rest) = stderr.split_once("Insufficient free space for ")?;
    let (_, rest) = rest.split_once("(available: ")?;
    let (free, rest) = rest.split_once(" required: ")?;
    let (needed, _) = rest.split_once(')')?;
    Some((free.trim().to_owned(), needed.trim().to_owned()))
}

fn same_digest_as(stderr: &str, which: &str) -> bool {
    stderr.contains(&format!(
        "same fs-verity digest as the existing Some({which}) deployment"
    ))
}

fn is_auth_failure(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    ["unauthorized", "authentication required", "denied: "]
        .iter()
        .any(|needle| text.contains(needle))
}

/// Run an [`Upgrade`].
///
/// # Errors
/// [`DaemonError::Busy`] while other VM-wide work runs; [`DaemonError::RegistryAuth`] for an image
/// the device may not pull itself; [`DaemonError::Apply`] when bootc refuses or stages another
/// image than expected.
pub fn upgrade(daemon: &Daemon, request: &Upgrade) -> Result<UpgradeResult, DaemonError> {
    if !has_bootc() {
        return Err(DaemonError::ToolMissing {
            tool: "bootc".into(),
            purpose: "move it onto another image".into(),
        });
    }
    let _vm = daemon.locks.vm_exclusive()?;
    let before = bootc_status()?;
    let mut source = request.source.clone();
    if let ImageSource::Layout { manifest } = &source {
        lay_out(&daemon.content, manifest)?;
        // The image it rolls back to is the one asked for: rolling back is instant.
        if before.staged.is_none()
            && request.expected.is_some()
            && before.rollback.as_ref().map(|r| &r.digest) == request.expected.as_ref()
        {
            source = ImageSource::Rollback;
        }
    }
    let mut ran = sh(&stage_line(&before, &source))?;
    if !ran.success && same_digest_as(&ran.stderr, "Rollback") {
        source = ImageSource::Rollback;
        ran = sh(&stage_line(&before, &source))?;
    }
    if !ran.success {
        if same_digest_as(&ran.stderr, "Booted") {
            return Ok(UpgradeResult::AlreadyBooted);
        }
        if let ImageSource::Registry { image: Some(image) } = &source
            && is_auth_failure(&ran.stderr)
        {
            return Err(DaemonError::RegistryAuth {
                image: image.clone(),
            });
        }
        let detail = match insufficient_space(&ran.stderr) {
            Some((free, needed)) => {
                format!("the new OS image needs {needed}, {free} is free on the device")
            }
            None => ran.stderr.trim().to_owned(),
        };
        return Err(DaemonError::Apply {
            step: "staging the image".into(),
            detail,
        });
    }
    let after = bootc_status()?;
    let queued = matches!(source, ImageSource::Rollback)
        .then(|| before.rollback.clone())
        .flatten();
    let Some(next) = after.staged.clone().or(queued) else {
        let booted = after.booted;
        if request.expected.is_none()
            || booted.as_ref().map(|b| &b.digest) == request.expected.as_ref()
        {
            return Ok(UpgradeResult::Current { booted });
        }
        return Err(DaemonError::Apply {
            step: "staging the image".into(),
            detail: "bootc reported no staged deployment".into(),
        });
    };
    if let Some(want) = &request.expected
        && &next.digest != want
    {
        return Err(DaemonError::Apply {
            step: "staging the image".into(),
            detail: format!("expected {want}, bootc staged {}", next.digest),
        });
    }
    let mut warnings = put_on_trial(&next)?;
    if !sh(ESP_WRITABLE_UNTIL_BOOT)?.success {
        warnings.push(
            "/boot could not be made writable; bootc may fail to apply the new image at shutdown"
                .into(),
        );
    }
    Ok(UpgradeResult::Staged {
        previous: after.booted,
        next,
        warnings,
    })
}

const BOOT_ENTRIES: &str = "if [ -x /usr/libexec/tilo-deployment ]; then /usr/libexec/tilo-deployment entries 2>&1; echo \"@@entries $?\"; else echo '@@entries absent'; fi";

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntriesOutcome {
    Done,
    /// The image's pass found no boot loader layout: an older image mounts nothing at /boot.
    NoLayout,
    Absent,
    Failed(String),
}

fn parse_entries(output: &str) -> EntriesOutcome {
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

/// For an image whose own pass cannot see the ESP: mount the ESP the boot loader started from
/// (`LoaderDevicePartUUID`) in a private mount namespace and give the entry that boots `verity`
/// its try counter, the same rename tilo-deployment does. The entry is found by the composefs
/// identity it boots, never by name: bootc renumbers every entry when it stages one. Prints
/// `@@counted N`, `@@counter none` where no boot loader names an ESP, or `@@counter failed: WHY`.
fn count_staged_entry_script(verity: &str) -> String {
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
        quote(&inner)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Counted {
    Entries(usize),
    NoBootLoader,
    Failed(String),
}

fn parse_counted(output: &str) -> Counted {
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

/// Give the staged deployment a try counter, so a deployment that fails to boot falls back to the
/// previous one: the image's own pass when it can, else the rename it would do. Answers what the
/// person should know about the boot that follows.
fn put_on_trial(staged: &Deployment) -> Result<Vec<String>, DaemonError> {
    let entries = sh(BOOT_ENTRIES)?;
    let broken = match parse_entries(&entries.stdout) {
        EntriesOutcome::Done => return Ok(Vec::new()),
        EntriesOutcome::Failed(detail) => Some(detail),
        EntriesOutcome::NoLayout | EntriesOutcome::Absent => None,
    };
    let unprotected = |detail: String| DaemonError::BootLoader {
        detail: format!(
            "{detail}; the next boot would have no try counter, so a deployment that fails to boot would be chosen again instead of the previous one"
        ),
    };
    let Some(verity) = staged.verity.as_deref() else {
        return broken.map_or(Ok(Vec::new()), |detail| Err(unprotected(detail)));
    };
    let counted = sh(&count_staged_entry_script(verity))?;
    match parse_counted(&counted.stdout) {
        Counted::Entries(count) if count > 0 => Ok(broken
            .map(|detail| {
                format!(
                    "its image cannot maintain its own boot entries ({detail}); the staged deployment got its try counter, and the image it boots maintains them from then on"
                )
            })
            .into_iter()
            .collect()),
        Counted::Entries(_) => match broken {
            Some(detail) => Err(unprotected(detail)),
            None => Ok(vec![format!(
                "no boot loader entry boots the staged deployment (composefs {}), so it has no try counter; if it fails to boot, choose the previous entry in the boot menu",
                verity.get(..16).unwrap_or(verity)
            )]),
        },
        Counted::NoBootLoader => broken.map_or(Ok(Vec::new()), |detail| Err(unprotected(detail))),
        Counted::Failed(why) => Err(unprotected(format!("{why}; {}", counted.stderr.trim()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_test_env::SpawnUnderEnvLock as _;

    #[test]
    fn bootc_answers_read_as_what_they_mean() {
        let refused = "error: Switching: Insufficient free space for registry.tilo.so/tilo/tilo-vm:x (available: 2.3 GB required: 5.2 GB)";
        assert_eq!(
            insufficient_space(refused),
            Some(("2.3 GB".into(), "5.2 GB".into()))
        );
        assert_eq!(
            insufficient_space("error: pulling: connection refused"),
            None
        );
        assert!(same_digest_as(
            "same fs-verity digest as the existing Some(Booted) deployment",
            "Booted"
        ));
        assert!(is_auth_failure(
            "Error: reading manifest: unauthorized: access"
        ));

        let verity = "ab".repeat(64);
        let json = format!(
            r#"{{"spec":{{"image":{{"image":"/var/lib/tilo-vm/image:tilo-vm","transport":"oci"}}}},"status":{{"staged":{{"image":{{"image":{{"image":"registry.tilo.so/tilo/tilo-vm:x","transport":"registry"}},"imageDigest":"sha256:1"}},"composefs":{{"verity":"sha512:{verity}"}}}},"booted":{{"image":{{"image":{{"image":"a"}},"imageDigest":"sha256:0"}},"composefs":{{"verity":"short"}}}},"rollback":null}}}}"#
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
        assert_eq!(status.rollback, None);
        assert!(parse_bootc_status("not json").is_err());
        let following = BootcStatus {
            spec: Some((format!("{LAYOUT_DIR}:{LAYOUT_TAG}"), "oci".into())),
            ..BootcStatus::default()
        };
        let layout = ImageSource::Layout {
            manifest: String::new(),
        };
        assert!(stage_line(&following, &layout).contains("bootc 'upgrade'"));
        assert!(
            stage_line(&BootcStatus::default(), &layout)
                .contains("bootc 'switch' '--transport' 'oci' '/var/lib/tilo-vm/image:tilo-vm'")
        );
    }

    #[test]
    fn the_staged_entry_is_given_its_try_counter() {
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
        assert_eq!(parse_entries("@@entries absent\n"), EntriesOutcome::Absent);
        assert_eq!(
            parse_entries("tilo-deployment: not a managed UKI entry: x\n@@entries 1\n"),
            EntriesOutcome::Failed("tilo-deployment: not a managed UKI entry: x".into())
        );
        assert_eq!(parse_counted("@@counted 1\n"), Counted::Entries(1));
        assert_eq!(parse_counted("@@counter none\n"), Counted::NoBootLoader);
        assert_eq!(
            parse_counted("@@counter failed: could not mount the ESP (PARTUUID x) read-write\n"),
            Counted::Failed("could not mount the ESP (PARTUUID x) read-write".into())
        );
        assert!(matches!(
            parse_counted("sh: unshare: not found"),
            Counted::Failed(_)
        ));
        let script = count_staged_entry_script(&"ef".repeat(64));
        assert!(script.contains(&format!("composefs={}", "ef".repeat(64))));
        let parsed = Command::new("sh")
            .args(["-n", "-c", &script])
            .status_locked()
            .unwrap();
        assert!(
            parsed.success(),
            "the counting script is one shell line: {script}"
        );
    }
}

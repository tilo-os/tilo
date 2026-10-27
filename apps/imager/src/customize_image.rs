//! `imager --customize-image`: the write step's Wi-Fi, hostname and owner
//! SSH customization, applied to a raw disk image file instead of a freshly
//! written drive, plus files for the home of the image's account.
//!
//! A board or VM disk is customized once, checked (booted in a VM, or
//! mounted), and then written to as many cards as needed. The files are
//! staged and copied onto the installed root by the same code a write uses
//! ([`crate::handoff::stage_access_handoff`] and the write path's root
//! probe), so a customized image and a customized card cannot drift apart.
//!
//! ```text
//! sudo imager --customize-image disk.raw \
//!     --hostname tilo-board \
//!     --wifi-ssid studio --wifi-psk-file psk.txt \
//!     --ssh-keys-file keys.pub \
//!     --home-file SETUP.md
//! ```
//!
//! The Wi-Fi passphrase is read from a file, never from the command line,
//! where every process on the machine could read it. `--ssh-keys-file` holds
//! one OpenSSH public key per line; they become key-only SSH login for the
//! image's one login account, as an installer gives the account it creates.
//! `--home-file` (repeatable) copies a file into that account's home under
//! its own name. The target must be a regular raw image file (convert a
//! qcow2 with `qemu-img convert -O raw` first); a drive is written through
//! the wizard, which asks for typed confirmation first. Mounting the image
//! needs root.

use std::path::{Path, PathBuf};

use crate::model::InstallProfile;

pub const CUSTOMIZE_IMAGE_ARG: &str = "--customize-image";

/// What one `--customize-image` invocation asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Request {
    image: PathBuf,
    hostname: Option<String>,
    wifi_ssid: Option<String>,
    wifi_psk_file: Option<PathBuf>,
    ssh_keys_file: Option<PathBuf>,
    home_files: Vec<PathBuf>,
}

const USAGE: &str = "usage: imager --customize-image DISK.raw [--hostname NAME] \
[--wifi-ssid SSID --wifi-psk-file FILE] [--ssh-keys-file FILE] [--home-file FILE]...";

fn parse(args: &[String]) -> Result<Option<Request>, String> {
    let Some(at) = args.iter().position(|arg| arg == CUSTOMIZE_IMAGE_ARG) else {
        return Ok(None);
    };
    let mut rest = args.iter().skip(at + 1);
    let image = rest
        .next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| format!("the disk image is missing\n{USAGE}"))?;
    let mut request = Request {
        image: PathBuf::from(image),
        ..Request::default()
    };
    while let Some(flag) = rest.next() {
        if flag == "--wifi-psk" {
            return Err(
                "pass the Wi-Fi password in a file (--wifi-psk-file), not on the command line"
                    .into(),
            );
        }
        let value = rest
            .next()
            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?
            .clone();
        match flag.as_str() {
            "--hostname" => request.hostname = Some(value),
            "--wifi-ssid" => request.wifi_ssid = Some(value),
            "--wifi-psk-file" => request.wifi_psk_file = Some(PathBuf::from(value)),
            "--ssh-keys-file" => request.ssh_keys_file = Some(PathBuf::from(value)),
            "--home-file" => request.home_files.push(PathBuf::from(value)),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    if request.wifi_ssid.is_some() != request.wifi_psk_file.is_some() {
        return Err("--wifi-ssid and --wifi-psk-file go together".into());
    }
    if request.hostname.is_none()
        && request.wifi_ssid.is_none()
        && request.ssh_keys_file.is_none()
        && request.home_files.is_empty()
    {
        return Err(format!("nothing to customize\n{USAGE}"));
    }
    Ok(Some(request))
}

/// The passphrase file's first line; the newline an editor adds is not part
/// of it.
fn read_psk(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    Ok(text.lines().next().unwrap_or_default().to_string())
}

/// A home file's name on the image: the source file's own name.
fn home_file_name(path: &Path) -> Result<&str, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))
}

/// Refuse anything but a regular raw disk image file.
fn check_target(image: &Path) -> Result<(), String> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(image)
        .map_err(|error| format!("{}: {error}", image.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "{} is not a regular file. Write a drive with the imager's write step, which confirms the target first.",
            image.display()
        ));
    }
    let mut magic = [0_u8; 4];
    let read = std::fs::File::open(image)
        .and_then(|mut file| file.read(&mut magic))
        .map_err(|error| format!("{}: {error}", image.display()))?;
    if read == 4 && &magic == b"QFI\xfb" {
        return Err(format!(
            "{} is a qcow2 image. Convert it first: qemu-img convert -O raw {} DISK.raw",
            image.display(),
            image.display()
        ));
    }
    Ok(())
}

/// Stage everything `request` asks for. The caller removes the tree.
fn stage(
    request: &Request,
    profile: &InstallProfile,
) -> Result<crate::handoff::StagedHandoff, String> {
    let psk = match &request.wifi_psk_file {
        Some(path) => read_psk(path)?,
        None => String::new(),
    };
    let keys = match &request.ssh_keys_file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?,
        None => String::new(),
    };
    if request.ssh_keys_file.is_some() && keys.trim().is_empty() {
        return Err("the SSH keys file has no keys".into());
    }
    let mut home_files = Vec::new();
    for path in &request.home_files {
        let name = home_file_name(path)?;
        let contents = std::fs::read(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        home_files.push((name, contents));
    }
    let staged = match crate::handoff::stage_access_handoff(profile, &psk, &keys, true)? {
        Some(staged) => staged,
        None => crate::handoff::StagedHandoff {
            root: tempfile::Builder::new()
                .prefix("tilo-imager-handoff-")
                .tempdir()
                .map_err(|error| error.to_string())?
                .keep(),
        },
    };
    for (name, contents) in &home_files {
        if let Err(error) = crate::handoff::stage_account_home_file(staged.path(), name, contents) {
            let _ = std::fs::remove_dir_all(staged.path());
            return Err(error);
        }
    }
    Ok(staged)
}

/// Run `--customize-image` when it is on the command line.
pub fn maybe_run(args: &[String]) -> Result<Option<i32>, Box<dyn std::error::Error>> {
    let Some(request) = parse(args)? else {
        return Ok(None);
    };
    check_target(&request.image)?;
    let mut profile = InstallProfile::default();
    if let Some(hostname) = &request.hostname {
        if !install_profile_model::valid_hostname(hostname) {
            return Err(
                "Hostname must be 1–63 letters, numbers or hyphens, without a hyphen at either end."
                    .into(),
            );
        }
        hostname.clone_into(&mut profile.hostname);
    }
    if let Some(ssid) = &request.wifi_ssid {
        ssid.clone_into(&mut profile.wifi_ssid);
    }
    let staged = stage(&request, &profile)?;
    let result = crate::write::apply_handoff_stage_to_image_file(&request.image, staged.path());
    let _ = std::fs::remove_dir_all(staged.path());
    result?;
    let mut applied = Vec::new();
    if let Some(hostname) = crate::handoff::chosen_hostname(&profile) {
        applied.push(format!("hostname {hostname}"));
    }
    if !profile.wifi_ssid.is_empty() {
        applied.push(format!("Wi-Fi {}", profile.wifi_ssid));
    }
    if request.ssh_keys_file.is_some() {
        applied.push("owner SSH keys".into());
    }
    for path in &request.home_files {
        applied.push(format!("home file {}", home_file_name(path)?));
    }
    println!(
        "customized {}: {}",
        request.image.display(),
        applied.join(", ")
    );
    Ok(Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("imager")
            .chain(list.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn other_command_lines_are_left_alone() {
        assert_eq!(parse(&args(&["--demo"])).unwrap(), None);
    }

    #[test]
    fn parses_every_customization() {
        let request = parse(&args(&[
            "--customize-image",
            "disk.raw",
            "--hostname",
            "tilo-board",
            "--wifi-ssid",
            "studio",
            "--wifi-psk-file",
            "psk.txt",
            "--ssh-keys-file",
            "keys.pub",
            "--home-file",
            "SETUP.md",
            "--home-file",
            "notes.txt",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            request,
            Request {
                image: "disk.raw".into(),
                hostname: Some("tilo-board".into()),
                wifi_ssid: Some("studio".into()),
                wifi_psk_file: Some("psk.txt".into()),
                ssh_keys_file: Some("keys.pub".into()),
                home_files: vec!["SETUP.md".into(), "notes.txt".into()],
            }
        );
    }

    #[test]
    fn refuses_a_passphrase_on_the_command_line() {
        let error = parse(&args(&[
            "--customize-image",
            "disk.raw",
            "--wifi-ssid",
            "studio",
            "--wifi-psk",
            "correct-horse",
        ]))
        .unwrap_err();
        assert!(error.contains("--wifi-psk-file"), "{error}");
    }

    #[test]
    fn refuses_a_network_without_its_passphrase_and_an_empty_request() {
        let error = parse(&args(&[
            "--customize-image",
            "disk.raw",
            "--wifi-ssid",
            "studio",
        ]))
        .unwrap_err();
        assert!(error.contains("go together"), "{error}");
        let error = parse(&args(&["--customize-image", "disk.raw"])).unwrap_err();
        assert!(error.contains("nothing to customize"), "{error}");
        let error = parse(&args(&["--customize-image", "--hostname", "x"])).unwrap_err();
        assert!(error.contains("disk image is missing"), "{error}");
    }

    #[test]
    fn only_a_regular_raw_image_is_a_target() {
        let dir = tempfile::tempdir().unwrap();
        let error = check_target(dir.path()).unwrap_err();
        assert!(error.contains("not a regular file"), "{error}");
        let qcow2 = dir.path().join("disk.qcow2");
        std::fs::write(&qcow2, b"QFI\xfb\0\0\0\x03").unwrap();
        let error = check_target(&qcow2).unwrap_err();
        assert!(error.contains("qemu-img convert"), "{error}");
        let raw = dir.path().join("disk.raw");
        std::fs::write(&raw, [0_u8; 1024]).unwrap();
        check_target(&raw).unwrap();
    }

    #[test]
    fn the_passphrase_file_loses_its_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("psk");
        std::fs::write(&path, "correct-horse\n").unwrap();
        assert_eq!(read_psk(&path).unwrap(), "correct-horse");
    }

    #[test]
    fn stages_keys_and_home_files_by_their_names() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys.pub");
        std::fs::write(
            &keys,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA a\n\
             ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB b\n",
        )
        .unwrap();
        let readme = dir.path().join("SETUP.md");
        std::fs::write(&readme, "# Setup\n").unwrap();
        let request = Request {
            image: "disk.raw".into(),
            ssh_keys_file: Some(keys),
            home_files: vec![readme],
            ..Request::default()
        };
        let staged = stage(&request, &InstallProfile::default()).unwrap();
        let root = staged.path();
        assert_eq!(
            std::fs::read_to_string(root.join(crate::handoff::BRINGUP_SSH_KEY))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_eq!(
            std::fs::read(
                root.join(crate::handoff::ACCOUNT_HOME_FILES)
                    .join("SETUP.md")
            )
            .unwrap(),
            b"# Setup\n"
        );
        assert!(
            !root.join("etc/hostname").exists(),
            "the default hostname is kept"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

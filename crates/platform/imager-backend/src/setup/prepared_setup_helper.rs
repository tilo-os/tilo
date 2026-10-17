//! Headless target-user consumer. Its only secret input is bounded raw UTF-8
//! stdin; stdout contains a receipt only after all requested custody is saved.
pub mod enrolled;
use super::{
    destination_security, migration::PinnedDirectory, prepared_setup::PreparedSetupReview,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

pub mod authenticate;
pub mod completed;
pub mod original_reattest;

/// Fixed private consuming helper entry, called before controller initialization.
/// Its sole argument is one inherited decimal descriptor, parsed by the driver.
/// The driver isolates this child before any worker or runtime starts, then
/// authenticates the actual owned parent and ordinary Shell launch context.
/// The original source actor remains in its original namespace.
///
/// # Errors
/// Refuses malformed routing, an uninstalled helper, unrelated kernel peers or
/// incomplete owned consumer and namespace retirement.
pub fn retained_consumer_bootstrap(args: impl Iterator<Item = OsString>) -> io::Result<()> {
    super::retained_consumer_driver::run_consuming_bootstrap(args)
}

/// Fixed installed ordinary-app retained Home consumer.
/// # Errors
/// Refuses uninstalled helpers, unrelated callers, stale certified selection,
/// invalid source authority and incomplete native namespace retirement.
pub fn retained_home_consumer(args: impl Iterator<Item = OsString>) -> io::Result<()> {
    super::retained_consumer_driver::run(args)
}

/// Fixed receiving namespace child. This entry accepts no source paths or
/// caller-produced proof; its own UID/PID1/root-owned scenario and actual root
/// supervisor route are checked before every target operation.
/// # Errors
/// Refuses unrelated callers, mutable artifacts and failed native restore/retry.
pub fn applications_consumer_qualification_child(
    args: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    super::home_capture::applications_qualification::run_child(args, output)
}

/// Dispatch the existing authenticated selected-medium controller before any
/// prepared-consumer parsing. No alternate helper, channel or source admission.
/// # Errors
/// Returns the canonical read-media helper's closed failure on malformed or
/// unauthenticated registered invocations; unrelated modes remain untouched.
pub fn maybe_run_original_controller(args: &[String]) -> Result<Option<i32>, String> {
    super::read_media::maybe_run(args)
}

#[cfg(test)]
mod original_controller_tests {
    use super::*;
    #[test]
    fn registered_original_controller_refuses_malformed_arguments_without_consumer_fallback() {
        for args in [
            vec!["--tilo-read-selected-media".into()],
            vec![
                "--tilo-read-selected-media".into(),
                "request.json".into(),
                "digest".into(),
                "extra".into(),
            ],
        ] {
            assert_eq!(
                maybe_run_original_controller(&args),
                Err("Selected-medium read permission failed".into())
            );
        }
        for mode in [
            "--home-consumer-capability",
            "--selected-install-generation",
            "--authenticate-install",
            "--portable-home",
            "--package",
        ] {
            assert_eq!(maybe_run_original_controller(&[mode.into()]).unwrap(), None);
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub identity_public_key: [u8; 32],
    pub account_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_identity: Option<tilo_identity_host::enrolled_transport::CurrentDeviceDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_sessions: Option<super::service_sessions::Declaration>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_home: Option<PortableHomeDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_flatpak: Option<super::flatpak_transfer::portable::Expectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<super::prepared_setup::BrowserHandoff>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableHomeDeclaration {
    pub schema: u32,
    pub setup_binding: String,
    pub selection: super::prepared_setup::PortableHomeSelection,
    #[serde(default)]
    pub applications: bool,
}
fn failed() -> io::Error {
    io::Error::other("Prepared setup could not be imported")
}
fn arguments(args: impl IntoIterator<Item = OsString>) -> io::Result<BTreeMap<String, PathBuf>> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(name) = args.next() {
        let name = name.to_str().ok_or_else(failed)?;
        if ![
            "--package",
            "--quarantine",
            "--identity-dir",
            "--config-root",
            "--host-device-root",
        ]
        .contains(&name)
            || values.contains_key(name)
        {
            return Err(failed());
        }
        let path = PathBuf::from(args.next().ok_or_else(failed)?);
        if !path.is_absolute() {
            return Err(failed());
        }
        values.insert(name.to_owned(), path);
    }
    if values.len() != 4 + usize::from(values.contains_key("--host-device-root"))
        || [
            "--package",
            "--quarantine",
            "--identity-dir",
            "--config-root",
        ]
        .iter()
        .any(|name| !values.contains_key(*name))
    {
        return Err(failed());
    }
    Ok(values)
}
fn private_parent(path: &Path) -> io::Result<()> {
    let parent = PinnedDirectory::open_absolute(path.parent().ok_or_else(failed)?)?;
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)
}
/// Production entry point, independent of any app/widget/runtime event loop.
/// The caller closes stdin after raw password bytes and uses a fresh quarantine
/// for each retry. Existing canonical target custody is verified idempotently.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    run_with_home_admission(args, input, output, || {
        super::home_admission::installed().map(|_| ())
    })
}

pub fn run_with_home_admission(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
    admission: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let args = arguments(args)?;
    let mut password = Zeroizing::new(Vec::new());
    input.take(4097).read_to_end(&mut password)?;
    if password.is_empty() || password.len() > 4096 || std::str::from_utf8(&password).is_err() {
        return Err(failed());
    }
    let package = &args["--package"];
    let quarantine = &args["--quarantine"];
    let identity = &args["--identity-dir"];
    let accounts = &args["--config-root"];
    // The caller must first copy delivered ciphertext into its private grant.
    // The OS package on public media is never treated as target-user custody.
    private_parent(package)?;
    private_parent(quarantine)?;
    let review =
        PreparedSetupReview::open(package, &password, quarantine, &AtomicBool::new(false))?;
    review.require_completed_export()?;
    if review.manifest().account_custody_protocol.is_some() {
        super::account_admission::installed()?;
    }
    let mut candidate_service = None;
    if let Some(sessions) = &review.manifest().service_sessions {
        let installed = super::account_admission::installed()?;
        if !installed.supports_service_session_protocol(sessions.consumer_protocol) {
            if !super::account_admission::receiving_candidate(
                sessions.consumer_protocol,
                &installed.runtime_root_hash,
                &review.manifest().identity_public_key,
                &review.manifest().setup_binding,
            )? {
                return Err(failed());
            }
            candidate_service = Some((sessions.consumer_protocol, installed.runtime_root_hash));
        }
    }
    if review.manifest().portable_home.is_some() || review.manifest().portable_flatpak.is_some() {
        admission()?;
    }
    if review
        .manifest()
        .portable_flatpak
        .as_ref()
        .is_some_and(|apps| apps.consumer_protocol == 2)
    {
        super::applications_admission::installed()?;
    }
    review.validate_destinations(identity, accounts)?;
    let current = if matches!(
        review.manifest().identity_kind,
        super::prepared_setup::IdentityKind::CurrentDevice { .. }
    ) {
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        let system = paths
            .device_cert
            .parent()
            .and_then(Path::parent)
            .ok_or_else(failed)?;
        Some(review.resolve_current_identity(system, identity, &password)?)
    } else {
        if let Some(host) = args.get("--host-device-root") {
            let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
            let configured = paths
                .device_cert
                .parent()
                .and_then(Path::parent)
                .ok_or_else(failed)?;
            if std::env::var_os("TILO_IDENTITY_ROOT").as_deref() != Some(host.as_os_str())
                || host != configured
            {
                return Err(failed());
            }
            if matches!(
                review.manifest().identity_kind,
                super::prepared_setup::IdentityKind::EnrolledDevice { .. }
            ) {
                review.import_host_identity(host, identity, &password)?;
            } else {
                review.import_identity(identity, &password)?;
            }
        } else {
            review.import_identity(identity, &password)?;
        }
        None
    };
    let current_identity = current
        .as_ref()
        .map(|identity| identity.declaration())
        .transpose()?;
    let imported = review.import_accounts(accounts, &password)?;
    review.import_service_sessions(accounts, &password)?;
    let receipt = Receipt {
        identity_public_key: review.manifest().identity_public_key,
        account_count: imported.len(),
        current_identity,
        service_sessions: review.manifest().service_sessions.clone(),
        device_label: review
            .manifest()
            .install_profile
            .as_ref()
            .map(|profile| profile.device_label.clone())
            .unwrap_or_default(),
        portable_home: review.manifest().portable_home.as_ref().map(|plan| {
            PortableHomeDeclaration {
                schema: plan.schema,
                setup_binding: review.manifest().setup_binding.clone(),
                selection: plan.selection.clone(),
                applications: plan.applications,
            }
        }),
        portable_flatpak: review.manifest().portable_flatpak.clone(),
        browser: review.manifest().browser.clone(),
    };
    if let Some(current) = &current {
        current.validate()?;
    }
    review.validate()?;
    if let Some((protocol, sdk)) = &candidate_service
        && !super::account_admission::receiving_candidate(
            *protocol,
            sdk,
            &review.manifest().identity_public_key,
            &review.manifest().setup_binding,
        )?
    {
        return Err(failed());
    }
    let bytes = serde_json::to_vec(&receipt).map_err(|_| failed())?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()?;
    if let Some(current) = &current {
        current.validate()?;
    }
    review.validate()
}

/// Read-only image-build probe of this packaged executable and GNU tar.
/// Password input, destination custody, and application startup are unused.
/// # Errors
/// Refuses extra arguments, mutable namespace edges, non-ELF helpers, and old tar.
pub fn home_consumer_capability(
    args: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::process::{Command, Stdio};
    if args.into_iter().next().is_some() {
        return Err(failed());
    }
    let helper = std::env::current_exe()?;
    // Only the packaged target executable may attest the installed consumer.
    if helper != Path::new("/usr/libexec/tilo-prepared-setup") {
        return Err(failed());
    }
    let digest = |path: &Path, maximum: u64, elf: bool| -> io::Result<String> {
        let mut edge = PathBuf::from("/");
        for component in path.components().skip(1) {
            edge.push(component.as_os_str());
            let info = std::fs::symlink_metadata(&edge)?;
            if info.uid() != 0 || info.mode() & 0o022 != 0 || info.file_type().is_symlink() {
                return Err(failed());
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.len() == 0
            || before.len() > maximum
            || before.mode() & 0o111 == 0
        {
            return Err(failed());
        }
        let mut header = [0u8; 4];
        file.read_exact(&mut header)?;
        if elf && header != *b"\x7fELF" {
            return Err(failed());
        }
        let mut hash = Sha256::new();
        hash.update(header);
        let mut remaining = maximum - 4;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            remaining = remaining.checked_sub(count as u64).ok_or_else(failed)?;
            hash.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        let observed = std::fs::symlink_metadata(path)?;
        if before.dev() != observed.dev()
            || before.ino() != observed.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err(failed());
        }
        Ok(hex::encode(hash.finalize()))
    };
    let helper_digest = digest(&helper, 1024 * 1024 * 1024, true)?;
    let firstboot = Path::new("/usr/libexec/tilo-firstboot-user");
    let seed_helper = Path::new("/usr/libexec/tilo-firstboot-home-seeds");
    let firstboot_digest = digest(firstboot, 1024 * 1024, false)?;
    let seed_digest = digest(seed_helper, 64 * 1024 * 1024, false)?;
    let tar = Path::new("/usr/bin/tar");
    let tar_digest = digest(tar, 64 * 1024 * 1024, true)?;
    let mut child = Command::new(tar)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().ok_or_else(failed)?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let _reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(4097).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let version = match receive.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(bytes)) if bytes.len() <= 4096 => bytes,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed());
        }
    };
    // A bounded stdout alone is not successful exit; never wait without a deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(failed());
            }
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    validate_tar_version(&version)?;
    if digest(&helper, 1024 * 1024 * 1024, true)? != helper_digest
        || digest(tar, 64 * 1024 * 1024, true)? != tar_digest
        || digest(firstboot, 1024 * 1024, false)? != firstboot_digest
        || digest(seed_helper, 64 * 1024 * 1024, false)? != seed_digest
    {
        return Err(failed());
    }
    let proof = super::home_admission::ConsumerCapability {
        schema: 1,
        archive_protocol: super::home_admission::ARCHIVE_PROTOCOL,
        seed_protocol: super::home_admission::SEED_PROTOCOL,
        firstboot_user_sha256: firstboot_digest,
        seed_helper_sha256: seed_digest,
        prepared_helper_sha256: helper_digest,
        tar_sha256: tar_digest,
    };
    proof.validate()?;
    serde_json::to_writer(&mut *output, &proof).map_err(|_| failed())?;
    output.write_all(b"\n")?;
    output.flush()
}
fn validate_tar_version(bytes: &[u8]) -> io::Result<()> {
    let first = std::str::from_utf8(bytes)
        .map_err(|_| failed())?
        .lines()
        .next()
        .ok_or_else(failed)?;
    let version = first.strip_prefix("tar (GNU tar) ").ok_or_else(failed)?;
    let mut parts = version.split('.');
    let major: u32 = parts
        .next()
        .ok_or_else(failed)?
        .parse()
        .map_err(|_| failed())?;
    let minor: u32 = parts
        .next()
        .ok_or_else(failed)?
        .parse()
        .map_err(|_| failed())?;
    if (major, minor) < (1, 30)
        || parts.any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(failed());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Fixed installed Copy controller on the existing authenticated helper channel.
/// # Errors
/// Refuses stale source or person custody and unobserved worker/importer retirement.
pub fn installed_original_copy_controller(args: impl Iterator<Item = OsString>) -> io::Result<()> {
    super::retained_consumer_driver::run_original_copy(args)
}

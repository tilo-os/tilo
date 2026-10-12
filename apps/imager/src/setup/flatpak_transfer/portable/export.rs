//! Read-only source subset export. Flatpak create-usb updates its source summary;
//! OSTree create-usb instead copies exact reviewed commits into private staging.
use super::{OriginalInstallation, commit};
use crate::setup::migration::PinnedDirectory;
use std::{
    ffi::OsString,
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Opaque original authority supplied only by a captured portable source review.
pub struct SourceExportGrant<'a> {
    pub(super) original: &'a OriginalInstallation,
}
/// Opaque destination authority supplied only by the private staging operation.
pub struct SubsetExportGrant<'a> {
    pub(super) source: SourceExportGrant<'a>,
    pub(super) destination: &'a PinnedDirectory,
    pub(super) path: &'a Path,
}
/// Metadata reviewed against an immutable source inventory; no ambient lookup
/// or configuration is used to select refs during staging.
pub struct ReviewedSubset {
    deployments: Vec<Ref>,
    extra_storage: u64,
}
impl ReviewedSubset {
    pub(super) fn extra_storage(&self) -> u64 {
        self.extra_storage
    }
}
struct Ref {
    collection: String,
    reference: String,
    commit: String,
    origin: String,
}
impl SourceExportGrant<'_> {
    fn validate(&self) -> Result<(), String> {
        self.original.pin.validate().map_err(|e| e.to_string())?;
        self.original
            .inventory
            .validate(&self.original.pin)
            .map_err(|e| e.to_string())
    }
}
fn source_command(
    grant: &SourceExportGrant<'_>,
    args: Vec<OsString>,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    grant.validate()?;
    let repository = grant.original.path.join("repo");
    let pin = PinnedDirectory::open_absolute(&repository).map_err(|e| e.to_string())?;
    pin.validate().map_err(|e| e.to_string())?;
    let output = command(&pin, &repository, args, cancel)?;
    grant.validate()?;
    Ok(output)
}
fn command(
    pin: &PinnedDirectory,
    repository: &Path,
    args: Vec<OsString>,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    pin.validate().map_err(|e| e.to_string())?;
    let mut repo = OsString::from("--repo=");
    repo.push(repository.as_os_str());
    let mut command = Command::new("/usr/bin/ostree");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .arg(repo)
        .args(args);
    super::super::offline_original::configure_ostree(&mut command)
        .map_err(|error| error.to_string())?;
    let mut worker =
        bounded_command::spawn_prepared(command, None, 8 * 1024 * 1024, Duration::from_secs(3600))
            .map_err(|_| "Read-only Flatpak subset operation could not start")?;
    let result = loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("Offline application export cancelled".into());
        }
        if let Some(result) = worker.try_wait() {
            break result.map_err(|_| "Read-only Flatpak subset operation exceeded bounds")?;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    pin.validate().map_err(|e| e.to_string())?;
    if !result.status.success() {
        #[cfg(test)]
        if std::env::var("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS").as_deref() == Ok("1") {
            eprintln!(
                "Synthetic read-only subset command failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        return Err("Read-only Flatpak subset operation failed".into());
    }
    String::from_utf8(result.stdout).map_err(|_| "Invalid native subset metadata".into())
}
fn collection(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 255
        || !value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                && part.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        })
        || !value.contains('.')
    {
        return Err("Offline applications require an exact approved collection ID".into());
    }
    Ok(value.to_owned())
}
pub(in crate::setup::flatpak_transfer) fn review(
    grant: &SourceExportGrant<'_>,
    cancel: Option<&AtomicBool>,
) -> Result<ReviewedSubset, String> {
    grant.validate()?;
    let mut deployments = Vec::new();
    let mut extra_storage = 0u64;
    for reference in &grant.original.required {
        let (item, hash) = grant
            .original
            .deployments
            .iter()
            .find(|(item, _)| &item.reference == reference)
            .ok_or("Required dependency has no reviewed commit")?;
        if !commit(hash)
            || !grant
                .original
                .remotes
                .iter()
                .any(|(remote, _)| remote.name == item.origin)
        {
            return Err("Required dependency has no approved signing authority".into());
        }
        let group = format!("--group=remote \"{}\"", item.origin);
        let id = collection(&source_command(
            grant,
            vec![
                "config".into(),
                "get".into(),
                group.into(),
                "collection-id".into(),
            ],
            cancel,
        )?)?;
        let binding = source_command(
            grant,
            vec![
                "show".into(),
                "--print-metadata-key=ostree.collection-binding".into(),
                hash.into(),
            ],
            cancel,
        )?;
        if binding
            .trim()
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            != Some(id.as_str())
        {
            return Err(
                "Original signed commit collection differs from its approved repository".into(),
            );
        }
        let logical = source_command(
            grant,
            vec![
                "show".into(),
                "--print-metadata-key=ostree.ref-binding".into(),
                hash.into(),
            ],
            cancel,
        )?;
        let bindings = super::super::native::bindings::string_array(&logical)?;
        if !bindings.contains(reference) {
            return Err("Original commit is not bound to its reviewed logical ref".into());
        }
        extra_storage = extra_storage
            .checked_add(
                super::extra_data::verify(&grant.original.path.join("repo"), hash, cancel)
                    .and_then(|storage| storage.staging_bytes())
                    .map_err(|error| error.to_string())?,
            )
            .ok_or("Extra-data storage overflows")?;
        deployments.push(Ref {
            collection: id,
            reference: reference.clone(),
            commit: hash.clone(),
            origin: item.origin.clone(),
        });
    }
    Ok(ReviewedSubset {
        deployments,
        extra_storage,
    })
}
pub(in crate::setup::flatpak_transfer) fn export(
    grant: &SubsetExportGrant<'_>,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    grant.source.validate()?;
    grant
        .destination
        .validate_destination()
        .map_err(|e| e.to_string())?;
    crate::setup::destination_security::check_private_file(&grant.destination.file)
        .map_err(|e| e.to_string())?;
    let actual = PinnedDirectory::open_absolute(grant.path).map_err(|e| e.to_string())?;
    if !crate::setup::nofollow::same_directory_identity(
        &actual.identity,
        &grant.destination.identity,
    ) || actual.contains_identity(&grant.source.original.pin.identity)
        || grant
            .source
            .original
            .pin
            .contains_identity(&actual.identity)
    {
        return Err("Offline export destination overlaps or differs from its private grant".into());
    }
    let reviewed = grant
        .source
        .original
        .native_export
        .as_ref()
        .ok_or("Native export has no independently reviewed source metadata")?;
    for item in &reviewed.deployments {
        super::extra_data::verify(
            &grant.source.original.path.join("repo"),
            &item.commit,
            cancel,
        )
        .map_err(|error| error.to_string())?;
        grant
            .destination
            .validate_destination()
            .map_err(|e| e.to_string())?;
        source_command(
            &grant.source,
            vec![
                "create-usb".into(),
                format!("--commit={}", item.commit).into(),
                grant.path.as_os_str().to_owned(),
                item.collection.clone().into(),
                item.reference.clone().into(),
            ],
            cancel,
        )?;
        grant
            .destination
            .validate_destination()
            .map_err(|e| e.to_string())?;
    }
    verify_signatures(grant, reviewed, cancel)?;
    grant.source.validate()
}
fn verify_signatures(
    grant: &SubsetExportGrant<'_>,
    reviewed: &ReviewedSubset,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    verify_carrier(
        grant.source.original,
        grant.path,
        grant.path,
        reviewed,
        cancel,
    )?;
    grant
        .destination
        .validate_destination()
        .map_err(|error| error.to_string())
}
fn verify_carrier_refs(
    carrier: &Path,
    reviewed: &ReviewedSubset,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let carrier_repo = carrier.join(".ostree/repo");
    let carrier_pin =
        PinnedDirectory::open_absolute(&carrier_repo).map_err(|error| error.to_string())?;
    let refs = command(
        &carrier_pin,
        &carrier_repo,
        vec!["refs".into(), "--collections".into(), "--revision".into()],
        cancel,
    )?;
    let mut found = std::collections::BTreeSet::new();
    for line in refs.lines() {
        let (name, commit) = line
            .split_once('\t')
            .ok_or("Invalid carrier collection listing")?;
        let (collection, reference) = name
            .strip_prefix('(')
            .and_then(|name| name.strip_suffix(')'))
            .and_then(|name| name.split_once(", "))
            .ok_or("Invalid carrier collection ref")?;
        if reference == "ostree-metadata" || reference.starts_with("appstream") {
            continue;
        }
        if !reviewed.deployments.iter().any(|item| {
            item.collection == collection && item.reference == reference && item.commit == commit
        }) || !found.insert(reference.to_owned())
        {
            return Err("Carrier contains unreviewed or changed collection deployment".into());
        }
    }
    if found.len() != reviewed.deployments.len() {
        return Err("Carrier omits reviewed dependency".into());
    }
    command(
        &carrier_pin,
        &carrier_repo,
        vec!["fsck".into(), "--quiet".into(), "--verify-bindings".into()],
        cancel,
    )?;
    Ok(())
}
pub(super) fn verify_carrier(
    original: &OriginalInstallation,
    carrier: &Path,
    private_scratch: &Path,
    reviewed: &ReviewedSubset,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let scratch =
        PinnedDirectory::open_absolute(private_scratch).map_err(|error| error.to_string())?;
    scratch
        .validate_destination()
        .map_err(|error| error.to_string())?;
    crate::setup::destination_security::check_private_file(&scratch.file)
        .map_err(|error| error.to_string())?;
    verify_carrier_refs(carrier, reviewed, cancel)?;
    let temporary = tempfile::Builder::new()
        .prefix(".verify-signed-")
        .tempdir_in(private_scratch)
        .map_err(|e| e.to_string())?;
    let directory = PinnedDirectory::open_absolute(temporary.path()).map_err(|e| e.to_string())?;
    directory
        .validate_destination()
        .map_err(|e| e.to_string())?;
    let repo = temporary.path().join("repo");
    directory
        .create_child(std::ffi::OsStr::new("repo"))
        .map_err(|e| e.to_string())?;
    let pin = PinnedDirectory::open_absolute(&repo).map_err(|e| e.to_string())?;
    command(
        &pin,
        &repo,
        vec!["init".into(), "--mode=archive".into()],
        cancel,
    )?;
    let mut origins: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for item in &reviewed.deployments {
        let name = if let Some(name) = origins.get(&item.origin) {
            name.clone()
        } else {
            let key_path = temporary.path().join(format!("key-{}", origins.len()));
            write_approved_key(&SourceExportGrant { original }, &item.origin, &key_path)?;
            directory
                .validate_destination()
                .map_err(|e| e.to_string())?;
            let name = format!("verified-{}", origins.len());
            command(
                &pin,
                &repo,
                vec![
                    "remote".into(),
                    "add".into(),
                    format!("--gpg-import={}", key_path.display()).into(),
                    "--set=gpg-verify=true".into(),
                    name.clone().into(),
                    format!("file://{}", carrier.join(".ostree/repo").display()).into(),
                ],
                cancel,
            )?;
            origins.insert(item.origin.clone(), name.clone());
            name
        };
        command(
            &pin,
            &repo,
            vec![
                "pull-local".into(),
                "--untrusted".into(),
                "--gpg-verify".into(),
                format!("--remote={name}").into(),
                carrier.join(".ostree/repo").as_os_str().to_owned(),
                item.commit.clone().into(),
            ],
            cancel,
        )?;
    }
    pin.validate_destination().map_err(|e| e.to_string())?;
    directory
        .validate_destination()
        .map_err(|e| e.to_string())?;
    scratch
        .validate_destination()
        .map_err(|error| error.to_string())?;
    temporary.close().map_err(|e| e.to_string())
}

fn write_approved_key(
    grant: &SourceExportGrant<'_>,
    origin: &str,
    path: &Path,
) -> Result<(), String> {
    use base64::Engine as _;
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
    let (_, definition) = grant
        .original
        .remotes
        .iter()
        .find(|(remote, _)| remote.name == origin)
        .ok_or("Exported ref lost its approved origin")?;
    let text =
        std::str::from_utf8(definition).map_err(|_| "Invalid approved repository definition")?;
    let encoded = text
        .lines()
        .find_map(|line| line.strip_prefix("GPGKey="))
        .ok_or("Missing approved signing key")?;
    let key = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid approved signing key")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&key)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}

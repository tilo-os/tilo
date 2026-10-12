//! Rootless application continuation; secret input is bounded raw stdin.
use super::{
    destination_security,
    flatpak_transfer::portable::{
        ApplicationImport, Expectation, NativeOfflineRunner, OfflineRunner,
    },
    migration::PinnedDirectory,
    migration_durability as durable,
    portable_home_import::{HomeImportGrants, PortableHomeReview},
    prepared_setup::PreparedSetupReview,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

fn failure() -> io::Error {
    io::Error::other("Portable applications could not be authenticated or restored")
}
fn arguments(args: impl IntoIterator<Item = OsString>) -> io::Result<BTreeMap<String, OsString>> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| failure())?;
        if ![
            "--setup-package",
            "--setup-quarantine",
            "--package",
            "--receipt",
            "--home",
            "--workspace",
            "--action",
        ]
        .contains(&key.as_str())
            || values.contains_key(&key)
        {
            return Err(failure());
        }
        values.insert(key, args.next().ok_or_else(failure)?);
    }
    if values.len() != 7 {
        return Err(failure());
    }
    Ok(values)
}
fn path(values: &BTreeMap<String, OsString>, key: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(values.get(key).ok_or_else(failure)?);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(failure());
    }
    Ok(path)
}
fn private(path: &Path) -> io::Result<std::sync::Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(path)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    Ok(pin)
}
fn read(path: &Path) -> io::Result<Vec<u8>> {
    let pin = private(path.parent().ok_or_else(failure)?)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(
        &pin.file,
        Path::new(path.file_name().ok_or_else(failure)?),
        &options,
    )?;
    destination_security::check_private_file(&file)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 16 * 1024 {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(failure());
    }
    pin.validate_destination()?;
    Ok(bytes)
}
fn private_package(path: &Path) -> io::Result<()> {
    let pin = private(path.parent().ok_or_else(failure)?)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(
        &pin.file,
        Path::new(path.file_name().ok_or_else(failure)?),
        &options,
    )?;
    destination_security::check_private_file(&file)?;
    if !file.metadata()?.is_file() {
        return Err(failure());
    }
    pin.validate_destination()
}
fn emit(output: &mut impl Write, value: serde_json::Value) -> io::Result<()> {
    output.write_all(&serde_json::to_vec(&value)?)?;
    output.write_all(b"\n")?;
    output.flush()
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    expectation: Expectation,
    home: durable::Identity,
    workspace: durable::Identity,
    restored: Option<usize>,
}
// Receipts record facts, never authorize skipping archive or target checks.
fn persist(workspace: &Path, name: &str, receipt: &Receipt) -> io::Result<()> {
    let pin = private(workspace)?;
    let bytes = serde_json::to_vec(receipt)?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    match cap_primitives::fs::open(&pin.file, Path::new(name), &options) {
        Ok(mut file) => {
            file.write_all(&bytes)?;
            durable::sync_file(&file)?;
            durable::sync_directory(&pin.file)?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let prior: Receipt = serde_json::from_slice(&read(&workspace.join(name))?)?;
            if &prior != receipt {
                return Err(failure());
            }
        }
        Err(error) => return Err(error),
    }
    pin.validate_destination()
}
fn verify_home(
    setup: &PreparedSetupReview,
    home: &Path,
    password: &[u8],
    cancel: &AtomicBool,
) -> io::Result<Option<PortableHomeReview>> {
    if setup.manifest().portable_home.is_none() {
        return Ok(None);
    }
    let delivered = home.join(".local/state/tilo/prepared-home");
    let expected: super::portable_home_media::Expectation =
        serde_json::from_slice(&read(&delivered.join("expectation.json"))?)?;
    expected.validate()?;
    let workspace = home
        .join(".local/state/tilo/portable-home")
        .join(&expected.operation);
    let review = PortableHomeReview::review(
        HomeImportGrants {
            package: delivered.join(&expected.filename),
            home: home.into(),
            workspace,
        },
        setup,
        &expected,
        password,
        cancel,
    )?;
    Ok(Some(review))
}
/// Review or restore the exact application package declared by authenticated setup.
/// No apps are launched and identity custody is never activated.
///
/// # Errors
/// Refuses unsafe grants, changed declarations, incomplete Home adoption and
/// failed native verification. Partial operations retain honest retry receipts.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    super::home_admission::installed()?;
    run_with_runner(args, input, output, &NativeOfflineRunner)
}
fn run_with_runner(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
    runner: &impl OfflineRunner,
) -> io::Result<()> {
    run_with_admission(args, input, output, runner, || {
        super::applications_admission::installed().map(|_| ())
    })
}
fn run_with_admission(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
    runner: &impl OfflineRunner,
    admission: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let values = arguments(args)?;
    let action = values["--action"].to_str().ok_or_else(failure)?;
    if !["review", "restore"].contains(&action) {
        return Err(failure());
    }
    let mut password = Zeroizing::new(Vec::new());
    input.take(4097).read_to_end(&mut password)?;
    if password.is_empty() || password.len() > 4096 || std::str::from_utf8(&password).is_err() {
        return Err(failure());
    }
    let home = path(&values, "--home")?;
    let workspace = path(&values, "--workspace")?;
    let expected: Expectation = serde_json::from_slice(&read(&path(&values, "--receipt")?)?)?;
    expected.validate()?;
    if workspace
        != home
            .join(".local/state/tilo/portable-applications")
            .join(&expected.operation)
    {
        return Err(failure());
    }
    let home_pin = private(&home)?;
    let workspace_pin = private(&workspace)?;
    let cancel = AtomicBool::new(false);
    let setup_package = path(&values, "--setup-package")?;
    let setup_quarantine = path(&values, "--setup-quarantine")?;
    private_package(&setup_package)?;
    private(setup_quarantine.parent().ok_or_else(failure)?)?;
    let setup = PreparedSetupReview::open(&setup_package, &password, &setup_quarantine, &cancel)?;
    if setup.manifest().portable_flatpak.as_ref() != Some(&expected) {
        return Err(failure());
    }
    if expected.consumer_protocol == 2 {
        admission()?;
    }
    let mut home_review = verify_home(&setup, &home, &password, &cancel)?;
    let package = path(&values, "--package")?;
    private_package(&package)?;
    if package.starts_with(&workspace)
        || setup_package.starts_with(&workspace)
        || setup_quarantine.starts_with(workspace.join("imported"))
    {
        return Err(failure());
    }
    let mut import = ApplicationImport::review(
        &setup,
        &package,
        &password,
        &workspace.join("imported"),
        &cancel,
    )?;
    if let Some(review) = &mut home_review {
        import.verify_home_for_applications(review, runner, &home, &workspace, &cancel)?;
    }
    emit(
        output,
        serde_json::json!({"event":"ready-to-restore", "applications":import.manifest().selected.len(), "deployments":import.manifest().deployments.len(), "user_state":import.manifest().user_state}),
    )?;
    if action == "review" {
        return Ok(());
    }
    let mut receipt = Receipt {
        schema: 1,
        expectation: expected,
        home: durable::identity(&home_pin.file)?,
        workspace: durable::identity(&workspace_pin.file)?,
        restored: None,
    };
    persist(&workspace, "restore-started.json", &receipt)?;
    emit(
        output,
        serde_json::json!({"event":"restoring-applications"}),
    )?;
    let restored = if let Some(review) = &mut home_review {
        import.restore_after_home(review, runner, &home, &workspace, &cancel)?
    } else {
        import.adopt_user_state(runner, &home, &workspace, &cancel)?;
        import.restore(runner, &home, &cancel)?
    };
    home_pin.validate_destination()?;
    workspace_pin.validate_destination()?;
    receipt.restored = Some(import.manifest().deployments.len());
    persist(&workspace, "restore-complete.json", &receipt)?;
    emit(
        output,
        serde_json::json!({"event":"complete", "restored":restored, "verified":true}),
    )
}
#[cfg(test)]
mod tests;

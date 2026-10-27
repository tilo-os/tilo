//! Target-user browser continuation. Passwords enter only through bounded stdin.
use super::{
    browser_transfer::{self, Expectation},
    destination_security,
    migration::PinnedDirectory,
    migration_durability as durable,
    prepared_setup::{BrowserHandoff, PreparedSetupReview},
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use zeroize::Zeroizing;

pub(crate) mod completion;

const RECEIPT: &str = "activation.json";
const MAX_RECEIPT: usize = 32 * 1024;
fn failure() -> io::Error {
    io::Error::other("Browser data could not be authenticated or activated")
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
fn private(path: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(path)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    Ok(pin)
}
fn controlled(path: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(path)?;
    pin.validate_destination()?;
    tilo_files::native::atomic::StorageDirectory::from_file(durable::readable_directory(
        &pin.file,
    )?)?
    .validate_namespace_control()?;
    Ok(pin)
}
fn open_private(path: &Path) -> io::Result<std::fs::File> {
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
    pin.validate_destination()?;
    Ok(file)
}
fn read(path: &Path) -> io::Result<Vec<u8>> {
    let file = open_private(path)?;
    if file.metadata()?.len() > MAX_RECEIPT as u64 {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take((MAX_RECEIPT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RECEIPT {
        return Err(failure());
    }
    Ok(bytes)
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Activation {
    schema: u32,
    handoff: BrowserHandoff,
    home: durable::Identity,
    workspace: durable::Identity,
    projection: PathBuf,
    projection_identity: durable::Identity,
}
struct Grants {
    home: Arc<PinnedDirectory>,
    workspace: Arc<PinnedDirectory>,
    workspace_path: PathBuf,
    package: PathBuf,
}
impl Grants {
    fn validate(&self) -> io::Result<()> {
        self.home.validate_destination()?;
        self.workspace.validate_destination()
    }
}
fn grants(values: &BTreeMap<String, OsString>, expected: &Expectation) -> io::Result<Grants> {
    let home = path(values, "--home")?;
    let workspace = path(values, "--workspace")?;
    if workspace
        != home
            .join(".local/state/tilo/browser-continuity")
            .join(&expected.operation)
    {
        return Err(failure());
    }
    let package = path(values, "--package")?;
    let delivered = home.join(".local/state/tilo/prepared-browser");
    if package != delivered.join(&expected.filename)
        || path(values, "--receipt")? != delivered.join("expectation.json")
    {
        return Err(failure());
    }
    open_private(&package)?;
    Ok(Grants {
        home: controlled(&home)?,
        workspace: private(&workspace)?,
        workspace_path: workspace,
        package,
    })
}
fn persist(grants: &Grants, activation: &Activation) -> io::Result<()> {
    let bytes = serde_json::to_vec(activation)?;
    if bytes.len() > MAX_RECEIPT {
        return Err(failure());
    }
    let directory = tilo_files::native::atomic::StorageDirectory::from_file(
        durable::readable_directory(&grants.workspace.file)?,
    )?;
    directory
        .publish(
            std::ffi::OsStr::new(RECEIPT),
            &bytes,
            tilo_files::native::atomic::PublishMode::IfAbsent,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )
        .map_err(io::Error::other)?;
    grants.validate()
}
fn verify_activation(
    grants: &Grants,
    activation: &Activation,
    handoff: &BrowserHandoff,
    review: &mut super::portable_transfer::ImportReview,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let imported = activation.projection.parent().ok_or_else(failure)?;
    let container = imported.parent().ok_or_else(failure)?;
    if activation.schema != 1
        || &activation.handoff != handoff
        || activation.home != durable::identity(&grants.home.file)?
        || activation.workspace != durable::identity(&grants.workspace.file)?
        || container.parent() != Some(grants.workspace_path.as_path())
        || !container
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("browser-import-"))
        || imported.file_name() != Some(std::ffi::OsStr::new("import"))
        || activation.projection.file_name() != Some(std::ffi::OsStr::new("home"))
    {
        return Err(failure());
    }
    private(container)?;
    private(imported)?;
    let projection = private(&activation.projection)?;
    if activation.projection_identity != durable::identity(&projection.file)? {
        return Err(failure());
    }
    review.verify_existing_tree(&activation.projection, cancel)?;
    browser_transfer::verify_tree(&activation.projection, &handoff.plan, cancel)?;
    projection.validate_destination()?;
    grants.validate()
}
fn activate(
    grants: &Grants,
    handoff: &BrowserHandoff,
    password: &[u8],
    review: &mut super::portable_transfer::ImportReview,
    cancel: &AtomicBool,
) -> io::Result<Activation> {
    match read(&grants.workspace_path.join(RECEIPT)) {
        Ok(bytes) => {
            let activation: Activation = serde_json::from_slice(&bytes)?;
            verify_activation(grants, &activation, handoff, review, cancel)?;
            return Ok(activation);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let projection = browser_transfer::extract(
        &handoff.payload,
        &handoff.plan,
        &grants.package,
        Zeroizing::new(password.to_vec()),
        &grants.workspace_path,
        cancel,
    )?;
    let root = private(&projection.tree.root)?;
    let activation = Activation {
        schema: 1,
        handoff: handoff.clone(),
        home: durable::identity(&grants.home.file)?,
        workspace: durable::identity(&grants.workspace.file)?,
        projection: projection.tree.root.clone(),
        projection_identity: durable::identity(&root.file)?,
    };
    verify_activation(grants, &activation, handoff, review, cancel)?;
    // Retain before the durable receipt: a crash may leave an orphan, never a
    // successful declaration pointing to plaintext that was already removed.
    projection.retain();
    persist(grants, &activation)?;
    verify_activation(grants, &activation, handoff, review, cancel)?;
    Ok(activation)
}
fn emit(output: &mut impl Write, value: serde_json::Value) -> io::Result<()> {
    output.write_all(&serde_json::to_vec(&value)?)?;
    output.write_all(b"\n")?;
    output.flush()
}
/// Activate the separately consented browser subset without granting a whole home.
///
/// # Errors
/// Refuses changed ciphertext, declarations, grants, output bytes or profile layout.
/// Existing activation JSON never replaces authentication of the actual carrier.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    let values = arguments(args)?;
    let action = values["--action"].to_str().ok_or_else(failure)?;
    if !["review", "activate"].contains(&action) {
        return Err(failure());
    }
    let mut password = Zeroizing::new(Vec::new());
    input.take(4097).read_to_end(&mut password)?;
    if password.is_empty() || password.len() > 4096 || std::str::from_utf8(&password).is_err() {
        return Err(failure());
    }
    let expected: Expectation = serde_json::from_slice(&read(&path(&values, "--receipt")?)?)?;
    expected.validate()?;
    let grants = grants(&values, &expected)?;
    let setup_package = path(&values, "--setup-package")?;
    let quarantine = path(&values, "--setup-quarantine")?;
    if setup_package.starts_with(&grants.workspace_path)
        || quarantine.starts_with(&grants.workspace_path)
    {
        return Err(failure());
    }
    open_private(&setup_package)?;
    private(quarantine.parent().ok_or_else(failure)?)?;
    let cancel = AtomicBool::new(false);
    let mut setup_carrier = super::portable_transfer::ImportReview::review(
        &setup_package,
        &password,
        super::portable_transfer::Purpose::PreparedSetup,
        super::prepared_setup::LIMITS,
        &cancel,
    )?;
    let setup = PreparedSetupReview::open(&setup_package, &password, &quarantine, &cancel)?;
    let handoff = setup.manifest().browser.as_ref().ok_or_else(failure)?;
    if handoff.payload != expected {
        return Err(failure());
    }
    let mut review = browser_transfer::review(
        &expected,
        &handoff.plan,
        &grants.package,
        &password,
        &cancel,
    )?;
    grants.validate()?;
    if action == "review" {
        return emit(
            output,
            serde_json::json!({"event":"ready-to-activate", "profiles":handoff.plan.profiles.len()}),
        );
    }
    let activation = activate(&grants, handoff, &password, &mut review, &cancel)?;
    completion::publish(
        &grants,
        &activation,
        handoff,
        &mut review,
        &mut setup_carrier,
        &cancel,
    )?;
    emit(
        output,
        serde_json::json!({"event":"complete", "verified":true,
        "projection":activation.projection, "identity_public_key":handoff.plan.identity_public_key,
        "operation":expected.operation, "profiles":handoff.plan.profiles.len()}),
    )
}
#[cfg(test)]
mod tests;

//! Rootless, bounded first-use Home review/adoption protocol; passwords only stdin.
use super::{
    destination_security,
    migration::PinnedDirectory,
    portable_home_import::{HomeImportGrants, PortableHomeReview},
    portable_home_media::Expectation,
    prepared_setup::PreparedSetupReview,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;
fn failure() -> io::Error {
    io::Error::other("Portable home could not be authenticated or adopted")
}
fn arguments(args: impl IntoIterator<Item = OsString>) -> io::Result<BTreeMap<String, OsString>> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| failure())?;
        if ![
            "--package",
            "--receipt",
            "--setup-package",
            "--setup-quarantine",
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
    if !path.is_absolute() {
        return Err(failure());
    }
    Ok(path)
}
fn receipt(path: &Path) -> io::Result<Expectation> {
    let pin = PinnedDirectory::open_absolute(path.parent().ok_or_else(failure)?)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
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
    let result: Expectation = serde_json::from_slice(&bytes)?;
    result.validate()?;
    pin.validate_destination()?;
    Ok(result)
}
fn emit(output: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}
/// Serve the same authenticated controller for separate review/adopt actions.
/// The caller supplies a fresh setup quarantine and closes bounded secret stdin.
///
/// # Errors
/// Refuses missing/changed declarations or unsafe grants, preserves partial data
/// and collisions, and never creates or activates an identity.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    run_with_admission(args, input, output, || {
        super::home_admission::installed().map(|_| ())
    })
}

pub fn run_with_admission(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
    admission: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    admission()?;
    let values = arguments(args)?;
    let action = values
        .get("--action")
        .and_then(|value| value.to_str())
        .ok_or_else(failure)?;
    if !["review", "adopt"].contains(&action) {
        return Err(failure());
    }
    let mut password = Zeroizing::new(Vec::new());
    input.take(4097).read_to_end(&mut password)?;
    if password.is_empty() || password.len() > 4096 || std::str::from_utf8(&password).is_err() {
        return Err(failure());
    }
    let cancel = AtomicBool::new(false);
    let expectation = receipt(&path(&values, "--receipt")?)?;
    let setup = PreparedSetupReview::open(
        &path(&values, "--setup-package")?,
        &password,
        &path(&values, "--setup-quarantine")?,
        &cancel,
    )?;
    let mut review = PortableHomeReview::review(
        HomeImportGrants {
            package: path(&values, "--package")?,
            home: path(&values, "--home")?,
            workspace: path(&values, "--workspace")?,
        },
        &setup,
        &expectation,
        &password,
        &cancel,
    )?;
    let preview = review.prepare(password, &cancel)?;
    emit(
        output,
        &serde_json::json!({"event":"ready-to-adopt","preview":preview}),
    )?;
    if action == "review" {
        return Ok(());
    }
    let mut worker = review.start_adoption(&cancel)?;
    emit(output, &serde_json::json!({"event":"adopting"}))?;
    loop {
        if let Some(result) = worker.try_next() {
            let result = result.map_err(|_| failure())?;
            let event = if result.complete {
                "complete"
            } else if result.cancelled {
                "cancelled"
            } else {
                "incomplete"
            };
            return emit(output, &serde_json::json!({"event":event,"result":result}));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

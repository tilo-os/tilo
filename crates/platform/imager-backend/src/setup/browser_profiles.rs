//! Browser profile authority lives in the native backend.
//! Service-session discovery retains its independent, consented helpers.
#[cfg(any(test, feature = "test-support"))]
pub use crate::browser_profiles::discover;

#[cfg(target_os = "linux")]
pub mod service_profiles;

#[cfg(target_os = "linux")]
use super::{
    account_transfer::{Candidate, Origin, Provider as AccountProvider},
    migration::PinnedDirectory,
    nofollow::metadata_nofollow,
};
#[cfg(target_os = "linux")]
use browser_continuity::Provider;
#[cfg(target_os = "linux")]
use std::{
    io::{self, Read},
    path::{Component, Path},
    sync::Arc,
};
#[cfg(target_os = "linux")]
use tilo_files::native::atomic::{Privacy, StorageDirectory};

#[cfg(target_os = "linux")]
fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
#[cfg(target_os = "linux")]
fn storage(pin: &PinnedDirectory) -> io::Result<StorageDirectory> {
    StorageDirectory::from_file(super::migration_durability::readable_directory(&pin.file)?)
}
#[cfg(target_os = "linux")]
fn components(path: &Path) -> io::Result<Vec<String>> {
    path.components()
        .map(|item| match item {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("Unsupported profile name")),
            _ => Err(invalid(
                "Browser profile must remain inside its granted source",
            )),
        })
        .collect()
}
#[cfg(target_os = "linux")]
fn walk(root: &Arc<PinnedDirectory>, names: &[String]) -> io::Result<Arc<PinnedDirectory>> {
    let mut directory = Arc::clone(root);
    for name in names {
        let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
        directory = directory.open_child(name.as_ref(), &metadata)?;
        storage(&directory)?.validate_namespace_control()?;
    }
    Ok(directory)
}
#[cfg(target_os = "linux")]
fn regular(directory: &PinnedDirectory, name: &str) -> io::Result<bool> {
    match metadata_nofollow(&directory.file, Path::new(name)) {
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => Ok(true),
        Ok(_) => Err(invalid("Browser data must be a regular source file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
const MAX_PROFILE_DIRECTORIES: usize = 128;
#[cfg(target_os = "linux")]
const MAX_INI: u64 = 64 * 1024;

#[cfg(target_os = "linux")]
fn firefox_sections(content: &str) -> io::Result<Vec<(String, String)>> {
    let mut output = Vec::new();
    let mut active = false;
    let mut name = None;
    let mut path = None;
    let mut relative = None;
    let finish = |active,
                  name: &mut Option<String>,
                  path: &mut Option<String>,
                  relative: &mut Option<bool>,
                  output: &mut Vec<(String, String)>|
     -> io::Result<()> {
        if active {
            if relative.take() != Some(true) {
                return Err(invalid("Firefox profile lies outside its granted source"));
            }
            output.push((
                name.take().unwrap_or_default(),
                path.take()
                    .ok_or_else(|| invalid("Firefox profile has no path"))?,
            ));
            if output.len() > browser_continuity::MAX_PROFILES {
                return Err(invalid("Too many Firefox profiles"));
            }
        }
        name.take();
        path.take();
        relative.take();
        Ok(())
    };
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            finish(active, &mut name, &mut path, &mut relative, &mut output)?;
            active = line.starts_with("[Profile");
        } else if active && let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "Name" => name = Some(value.into()),
                "Path" => path = Some(value.into()),
                "IsRelative" => relative = Some(value == "1"),
                _ => {}
            }
        }
    }
    finish(active, &mut name, &mut path, &mut relative, &mut output)?;
    Ok(output)
}

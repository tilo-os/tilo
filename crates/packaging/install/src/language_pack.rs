//! Installing a language pack: one locale's catalogues, verified like any
//! package, put where the runtime reads the person's language packs.

use std::fs;

use crate::{BlobDir, BlobFetcher, Candidate, Checked, Error, PackageStore};

/// Where the installed versions of the packs are kept, under the packs'
/// root; each locale's entry there is a link to its current version.
const VERSIONS: &str = ".versions";

/// Install a verified language pack: it is recorded in the package store
/// like any package (so `update` brings its newer versions, as for apps and
/// runtimes), and its locale's entry under [`tilo_paths::language_packs_dir`]
/// is switched to the new catalogues in one rename, so a reader sees the
/// old catalogues or the new ones and never a mix.
pub(crate) fn install_language_pack(
    store: &PackageStore,
    candidate: &Candidate,
    checked: &Checked,
    dry_run: bool,
    payload: Option<&dyn BlobFetcher>,
) -> Result<(), Error> {
    let locale = tilo_app_package::language_pack_locale(checked.id())
        .ok_or_else(|| Error::Policy(format!("{} names no locale", checked.id())))?;
    let root = tilo_paths::language_packs_dir().ok_or(Error::NoDataDir)?;
    if dry_run {
        println!("plan: install language pack {locale}");
        println!("  package   : {} {}", checked.id(), checked.version());
        println!("  into      : {}", root.display());
        println!("(nothing was written)");
        return Ok(());
    }
    fs::create_dir_all(&root)?;
    let staging = root.join(format!(".staging-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| -> Result<(), Error> {
        let blobs = staging.join("blobs");
        fs::create_dir_all(&blobs)?;
        let mut sink = BlobDir(blobs);
        let tapp = tilo_app_package::tapp::read(candidate.reader()?, &mut sink)
            .map_err(|error| Error::Parse(error.to_string()))?;
        let missing = tapp.missing();
        if !missing.is_empty() {
            let fetcher = payload.ok_or_else(|| {
                Error::Policy(format!(
                    "the language pack does not carry {} catalogue(s); name the registry that \
                     offers it with --payload-from",
                    missing.len()
                ))
            })?;
            fetcher.fetch(&missing, &mut sink).map_err(|error| {
                Error::Store(format!("fetching the language pack's catalogues: {error}"))
            })?;
        }
        let tree = staging.join("tree");
        tilo_app_package::unpack(&tapp.meta, &sink, &tree)
            .map_err(|error| Error::Store(format!("unpacking the language pack: {error}")))?;
        // The signed payload is one directory, named by the locale.
        let mut directories = fs::read_dir(&tree)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        let directory = match (directories.pop(), directories.is_empty()) {
            (Some(directory), true)
                if directory
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(locale)) =>
            {
                directory
            }
            _ => {
                return Err(Error::Policy(format!(
                    "{} carries more than its {locale} catalogues",
                    checked.id()
                )));
            }
        };
        let unpacked = tree.join(&directory);
        // Recorded like every package, so updates find it.
        if !crate::record_package(store, candidate, checked, false, false, &sink)? {
            return Ok(());
        }
        let versions = root.join(VERSIONS);
        fs::create_dir_all(&versions)?;
        let version = format!(
            "{}-{}-{}",
            directory.to_string_lossy(),
            checked.version(),
            std::process::id()
        );
        fs::rename(&unpacked, versions.join(&version))?;
        let dest = root.join(&directory);
        let previous = fs::read_link(&dest).ok();
        if previous.is_none() && dest.is_dir() {
            // A locale installed before the packs were versioned.
            fs::rename(&dest, staging.join("retired"))?;
        }
        let link = root.join(format!(".{}.next", directory.to_string_lossy()));
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(std::path::Path::new(VERSIONS).join(&version), &link)?;
        fs::rename(&link, &dest)?;
        if let Some(previous) = previous {
            let _ = fs::remove_dir_all(root.join(previous));
        }
        println!(
            "installed language pack {} {}",
            checked.id(),
            checked.version()
        );
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

//! Installing a language pack: one locale's catalogues, verified like any
//! package, put where the runtime reads the person's language packs.

use std::fs;

use crate::{BlobDir, BlobFetcher, Candidate, Checked, Error};

/// The file in an installed language pack's directory naming the package
/// and version it came from.
const LANGUAGE_PACK_MARKER: &str = ".package";

/// Install a verified language pack: its one locale directory of
/// catalogues replaces the one installed for that locale under
/// [`tilo_paths::language_packs_dir`], whole, so a reader sees the old
/// catalogues or the new ones and never a mix.
pub(crate) fn install_language_pack(
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
        fs::write(
            unpacked.join(LANGUAGE_PACK_MARKER),
            format!("{} {}\n", checked.id(), checked.version()),
        )?;
        let dest = root.join(&directory);
        let retired = staging.join("retired");
        if dest.exists() {
            fs::rename(&dest, &retired)?;
        }
        fs::rename(&unpacked, &dest)?;
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

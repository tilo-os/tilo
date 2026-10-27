use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use compat_catalogue::{
    CATALOGUE_SCHEMA_VERSION, Catalogue, CatalogueError, Provenance, Runner, RunnerSettings,
    SecurityProfileRef, proton_records_from_umu_csv, wine_records_from_bottles_recipe,
};
use thiserror::Error;

const MAX_BOTTLES_FILES: usize = 16_384;
const MAX_BUILTIN_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ShippedCataloguePaths {
    pub builtins: PathBuf,
    pub umu_csv: PathBuf,
    pub bottles_root: PathBuf,
}

impl Default for ShippedCataloguePaths {
    fn default() -> Self {
        let root = Path::new("/usr/share/tilo/windows/catalogues");
        Self {
            builtins: root.join("tilo-builtins.json"),
            umu_csv: root.join("umu-database.csv"),
            bottles_root: root.join("bottles-programs"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CatalogueLoadReport {
    pub catalogue: Catalogue,
    pub built_in_records: usize,
    pub umu_records: usize,
    pub bottles_records: usize,
    /// Immutable Bottles files whose metadata-only subset was unsupported.
    /// They are diagnostics, never partially interpreted launch recipes.
    pub skipped_bottles_files: Vec<PathBuf>,
}

/// Load only image-owned, checksum-pinned catalogue sources. Paths are daemon
/// configuration and never request input. Imported records are marked signed
/// here because the final image copied them from checksum-pinned build stages;
/// a mutable/user catalogue must use a different authenticated loader.
pub fn load_shipped_catalogue(
    paths: &ShippedCataloguePaths,
) -> Result<CatalogueLoadReport, CatalogueLoadError> {
    let builtin_meta = fs::symlink_metadata(&paths.builtins)?;
    require_regular(&paths.builtins, &builtin_meta)?;
    let builtins = tilo_files::native::bounded::read_or(
        &paths.builtins,
        MAX_BUILTIN_BYTES,
        CatalogueLoadError::OversizedBuiltins,
    )?
    .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    let builtins = String::from_utf8(builtins)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let builtins = Catalogue::from_json(&builtins)?;
    if builtins
        .records
        .iter()
        .any(|record| record.provenance != Provenance::BuiltIn || !record.signed)
    {
        return Err(CatalogueLoadError::UntrustedBuiltinRecord);
    }

    require_regular(&paths.umu_csv, &fs::symlink_metadata(&paths.umu_csv)?)?;
    let profile = SecurityProfileRef::new("windows-app")?;
    let proton = Runner::new("umu-proton", "UMU-Proton-10.0-4")?;
    let wine = Runner::new("wine", "system")?;
    let mut umu = proton_records_from_umu_csv(
        &fs::read_to_string(&paths.umu_csv)?,
        &proton,
        &profile,
        &RunnerSettings::default(),
    )?;
    for record in &mut umu {
        record.signed = true;
    }

    let mut bottles_paths = Vec::new();
    collect_bottles_files(&paths.bottles_root, &mut bottles_paths)?;
    bottles_paths.sort();
    let mut bottles = Vec::new();
    let mut skipped = Vec::new();
    for path in bottles_paths {
        match wine_records_from_bottles_recipe(&fs::read_to_string(&path)?, &wine, &profile) {
            Ok(mut records) => {
                for record in &mut records {
                    record.signed = true;
                }
                bottles.extend(records);
            }
            Err(CatalogueError::InvalidBottlesRecipe(_)) => skipped.push(path),
            Err(error) => return Err(error.into()),
        }
    }

    // Stable key ordering makes source traversal irrelevant. Image-owned
    // built-ins win an ID collision; imported duplicates are discarded.
    let built_in_records = builtins.records.len();
    let umu_records = umu.len();
    let bottles_records = bottles.len();
    let mut records = BTreeMap::new();
    for record in builtins.records {
        records.insert(record.id.clone(), record);
    }
    for record in umu.into_iter().chain(bottles) {
        records.entry(record.id.clone()).or_insert(record);
    }
    let catalogue = Catalogue {
        schema_version: CATALOGUE_SCHEMA_VERSION,
        records: records.into_values().collect(),
    };
    catalogue.validate()?;
    Ok(CatalogueLoadReport {
        catalogue,
        built_in_records,
        umu_records,
        bottles_records,
        skipped_bottles_files: skipped,
    })
}

fn require_regular(path: &Path, metadata: &fs::Metadata) -> Result<(), CatalogueLoadError> {
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(CatalogueLoadError::NotRegular(path.to_path_buf()));
    }
    Ok(())
}

fn collect_bottles_files(
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), CatalogueLoadError> {
    if output.len() >= MAX_BOTTLES_FILES {
        return Err(CatalogueLoadError::TooManyBottlesFiles);
    }
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CatalogueLoadError::UnsafeBottlesEntry(
            directory.to_path_buf(),
        ));
    }
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(CatalogueLoadError::UnsafeBottlesEntry(path));
        }
        if metadata.is_dir() {
            collect_bottles_files(&path, output)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(CatalogueLoadError::UnsafeBottlesEntry(path));
        }
        let extension = path.extension().and_then(|value| value.to_str());
        if matches!(extension, Some("yml" | "yaml")) {
            if output.len() >= MAX_BOTTLES_FILES {
                return Err(CatalogueLoadError::TooManyBottlesFiles);
            }
            output.push(path);
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CatalogueLoadError {
    #[error("could not read the immutable Windows catalogue: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error("catalogue source is not a regular immutable file: {0}")]
    NotRegular(PathBuf),
    #[error("built-in Windows catalogue exceeds 2 MiB")]
    OversizedBuiltins,
    #[error("built-in Windows catalogue contains a record not marked built-in and signed")]
    UntrustedBuiltinRecord,
    #[error("Bottles catalogue contains a symlink or non-file entry: {0}")]
    UnsafeBottlesEntry(PathBuf),
    #[error("Bottles catalogue contains too many files")]
    TooManyBottlesFiles,
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    fn fixture() -> (TempDir, ShippedCataloguePaths) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        fs::write(
            root.join("tilo-builtins.json"),
            r#"{"schema_version":1,"records":[{"id":"visual-studio-vm","version":1,"provenance":"built_in","signed":true,"matches":[{"publisher":"Microsoft","product":"Visual Studio"}],"recipe":{"layer":"vm","runner":{"family":"windows-vm","version":"managed"},"settings":{},"security_profile":"windows-app"}}]}"#,
        )
        .unwrap();
        fs::write(
            root.join("umu.csv"),
            "TITLE,STORE,CODENAME,UMU_ID\nGame,steam,Game,42\n",
        )
        .unwrap();
        fs::create_dir(root.join("bottles")).unwrap();
        fs::write(
            root.join("bottles/reader.yml"),
            "Name: Reader\nExecutable: reader.exe\nGrade: Gold\n",
        )
        .unwrap();
        (
            temp,
            ShippedCataloguePaths {
                builtins: root.join("tilo-builtins.json"),
                umu_csv: root.join("umu.csv"),
                bottles_root: root.join("bottles"),
            },
        )
    }

    #[test]
    fn fixed_catalogue_sources_build_deterministically_and_reject_symlinks() {
        {
            let (_temp, paths) = fixture();
            let report = load_shipped_catalogue(&paths).unwrap();
            assert_eq!(report.built_in_records, 1);
            assert_eq!(report.umu_records, 1);
            assert_eq!(report.bottles_records, 1);
            assert_eq!(report.catalogue.records.len(), 3);
            assert!(report.catalogue.records.iter().all(|record| record.signed));
        }
        {
            let (temp, paths) = fixture();
            let outside = temp.path().join("outside.yml");
            fs::write(&outside, "Name: Bad\nExecutable: bad.exe\n").unwrap();
            symlink(outside, paths.bottles_root.join("link.yml")).unwrap();
            assert!(matches!(
                load_shipped_catalogue(&paths),
                Err(CatalogueLoadError::UnsafeBottlesEntry(_))
            ));
        }
    }
}

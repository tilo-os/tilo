//! Home data moving on the same computer travels as the same sealed Home unit
//! as every other route: the reviewed source is sealed into a private
//! workspace with a throwaway password and admitted again before anything
//! consumes it. A retry reopens the sealed unit instead of reading the source
//! again.
use super::{Expectation, HomeSource, MediaGrant};
use crate::setup::{
    home_transfer::promotion::AdoptionWitness,
    migration::{PinnedDirectory, changed},
    portable_transfer::{ExportSummary, ImportReview, Observer, Purpose},
};
use crate::user_transfer::{
    clear_workspace, private_workspace, publish_private_artifact, read_private_artifact,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use zeroize::Zeroizing;

const UNIT: &str = "unit.json";

/// Owner-only record of the sealed unit in the private workspace. The
/// password protects the package in transit; here it sits beside the admitted
/// plaintext, which the same owner only already holds.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    expectation: Expectation,
    password: String,
    summary: ExportSummary,
}

/// An admitted Home unit: the authenticated package and the tree extracted
/// from it at `root`.
#[derive(Clone)]
pub struct SealedHome {
    root: PathBuf,
    summary: ExportSummary,
    review: Arc<Mutex<ImportReview>>,
}

impl SealedHome {
    /// Seal `source` into `workspace` and admit it at `root`, a new private
    /// directory, or reopen the unit an earlier attempt sealed there.
    ///
    /// # Errors
    /// Refuses a nonprivate workspace, an existing `root` without a sealed
    /// unit, a changed source, insufficient space or a changed sealed unit.
    pub fn seal(
        source: &HomeSource,
        workspace: &Path,
        root: &Path,
        cancel: &AtomicBool,
        observer: &mut Observer<'_>,
    ) -> io::Result<Self> {
        if let Some(unit) = Self::reopen(workspace, root, false, cancel)? {
            return Ok(unit);
        }
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(changed(
                    "Home destination already exists; it was not replaced",
                ));
            }
        }
        let pin = private_workspace(workspace)?;
        clear_workspace(&pin, workspace)?;
        let summary = source.summary()?;
        let operation = uuid::Uuid::new_v4().to_string();
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(io::Error::other)?;
        let password = Zeroizing::new(hex::encode(secret));
        let expectation = source.export_to_reporting(
            &MediaGrant::open(workspace)?,
            &operation,
            &operation,
            Zeroizing::new(password.as_bytes().to_vec()),
            cancel,
            observer,
        )?;
        let record = Record {
            schema: 1,
            expectation,
            password: password.to_string(),
            summary,
        };
        let review = authenticate(workspace, &record, cancel)?;
        // The record precedes admission, so an interrupted admission resumes
        // from this unit and `root` never exists without one.
        publish_private_artifact(&pin, UNIT, &serde_json::to_vec(&record)?)?;
        Self::admit(review, &record, root, false, cancel)
    }

    /// Reopen the unit sealed in `workspace`: authenticate the package again,
    /// then extract it at `root` if an earlier admission was interrupted, or
    /// verify the admitted tree. Once applications were deployed into the
    /// tree, only their user installation may differ. Returns `None` when
    /// nothing was sealed there yet.
    ///
    /// # Errors
    /// Refuses a changed package, record or admitted tree.
    pub fn reopen(
        workspace: &Path,
        root: &Path,
        applications: bool,
        cancel: &AtomicBool,
    ) -> io::Result<Option<Self>> {
        let Some(record) = record(workspace)? else {
            return Ok(None);
        };
        let review = authenticate(workspace, &record, cancel)?;
        Self::admit(review, &record, root, applications, cancel).map(Some)
    }

    /// Reopen the unit after adoption moved its tree from `root`, verifying
    /// every authenticated entry where the adoption journal placed it.
    ///
    /// # Errors
    /// Refuses a missing or changed unit and moved data that differs.
    pub fn reopen_adopted(
        workspace: &Path,
        root: &Path,
        witness: &AdoptionWitness,
        applications: bool,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let record = record(workspace)?.ok_or_else(|| changed("Sealed Home unit is missing"))?;
        let mut review = authenticate(workspace, &record, cancel)?;
        if applications {
            review.verify_adoption_after_applications(witness, cancel)?;
        } else {
            review.verify_adoption(witness, cancel)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            summary: record.summary,
            review: Arc::new(Mutex::new(review)),
        })
    }

    fn admit(
        mut review: ImportReview,
        record: &Record,
        root: &Path,
        applications: bool,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                review.extract_root(
                    Zeroizing::new(record.password.as_bytes().to_vec()),
                    root,
                    cancel,
                )?;
            }
            Err(error) => return Err(error),
            Ok(_) if applications => {
                review.verify_existing_tree_after_applications(root, cancel)?
            }
            Ok(_) => review.verify_existing_tree(root, cancel)?,
        }
        Ok(Self {
            root: root.to_path_buf(),
            summary: record.summary.clone(),
            review: Arc::new(Mutex::new(review)),
        })
    }

    /// Where the admitted tree lives until adoption moves it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What the reviewed source held when it was sealed.
    pub fn summary(&self) -> &ExportSummary {
        &self.summary
    }

    /// Verify the admitted tree against the authenticated package; with
    /// `applications`, deployments into its user installation are allowed.
    ///
    /// # Errors
    /// Refuses changed ciphertext and changed, missing or added entries.
    pub fn verify(&self, applications: bool, cancel: &AtomicBool) -> io::Result<()> {
        let mut review = self
            .review
            .lock()
            .map_err(|_| changed("Sealed Home review became unavailable"))?;
        if applications {
            review.verify_existing_tree_after_applications(&self.root, cancel)
        } else {
            review.verify_existing_tree(&self.root, cancel)
        }
    }
}

fn record(workspace: &Path) -> io::Result<Option<Record>> {
    let pin = private_workspace(workspace)?;
    let bytes = match read_private_artifact(&pin, UNIT) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let record: Record = serde_json::from_slice(&bytes)?;
    if record.schema != 1 {
        return Err(changed("Sealed Home record is unsupported"));
    }
    Ok(Some(record))
}

/// Admit the package only if it is exactly the unit its record expects.
fn authenticate(
    workspace: &Path,
    record: &Record,
    cancel: &AtomicBool,
) -> io::Result<ImportReview> {
    let expected = &record.expectation;
    expected.validate()?;
    let review = ImportReview::review(
        &workspace.join(&expected.filename),
        record.password.as_bytes(),
        Purpose::Home,
        expected.limits,
        cancel,
    )?;
    let info = review.info();
    if review.ciphertext_digest() != expected.ciphertext_sha256
        || info.setup_binding.as_deref() != Some(expected.setup_binding.as_str())
        || info.sealed_bytes != expected.sealed_bytes
        || info.plaintext_bytes != expected.plaintext_bytes
        || info.entries != expected.entries
        || info.runtime_sockets != expected.kind.runtime_sockets
    {
        return Err(changed("Sealed Home unit differs from its expectation"));
    }
    Ok(review)
}

/// Exclusions that narrow a Home review to the user's Flatpak installation
/// and app data, the subtrees an applications-only transfer carries.
///
/// # Errors
/// Refuses links or non-folders on the way to those subtrees.
pub fn application_data_exclusions(home: &Path) -> io::Result<Vec<PathBuf>> {
    let mut excluded = Vec::new();
    for (parent, kept) in [
        ("", &[".local", ".var"][..]),
        (".local", &["share"][..]),
        (".local/share", &["flatpak"][..]),
        (".var", &["app"][..]),
    ] {
        let path = home.join(parent);
        if !parent.is_empty() {
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
                Ok(_) => {}
            }
        }
        let pin = PinnedDirectory::open_absolute(&path)?;
        let listing = cap_std::fs::Dir::from_std_file(
            crate::setup::migration_durability::readable_directory(&pin.file)?,
        );
        for entry in listing.entries()? {
            let entry = entry?;
            let name = entry.file_name();
            if kept.iter().any(|kept| name == *kept) {
                let metadata =
                    crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(&name))?;
                if !metadata.is_dir() || metadata.is_symlink() {
                    return Err(changed(
                        "Flatpak data traverses a non-directory or symbolic link",
                    ));
                }
            } else {
                excluded.push(Path::new(parent).join(name));
            }
        }
        pin.validate()?;
    }
    Ok(excluded)
}

//! System applications moving on the same computer travel as the same sealed
//! applications unit as every other route: the reviewed original deployments
//! are staged, sealed, and admitted again before the applier sees them. No
//! repository is contacted; nothing is installed from a network remote.
use super::{
    ApplicationImport, ApplicationsManifest, Deployment, Expectation, OfflineRunner,
    PortableFlatpakSource, StageGrant,
};
use crate::setup::{
    migration::{PinnedDirectory, changed},
    portable_transfer::ImportReview,
    user_transfer::{publish_private_artifact, read_private_artifact},
};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use zeroize::Zeroizing;

const UNIT: &str = "unit.json";
const STAGE: &str = "stage";
const STAGE_JOURNALS: &str = "stage-journals";
const QUARANTINE: &str = "admitted";

/// Owner-only record of the sealed unit in the private workspace. The
/// password protects the package in transit; here it sits beside the
/// admitted plaintext, which the same owner-only directory already holds.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    expectation: Expectation,
    password: String,
}

/// An admitted applications unit as the applier sees it: the authenticated
/// deployments and signing definitions, extracted from the sealed package.
#[derive(Clone)]
pub struct SealedApplications {
    root: PathBuf,
    manifest: ApplicationsManifest,
    review: Option<Arc<Mutex<ImportReview>>>,
}

impl SealedApplications {
    /// A unit the caller admitted and re-attests itself around the apply.
    pub(super) fn admitted(import: &ApplicationImport) -> Self {
        Self {
            root: import.root.clone(),
            manifest: import.manifest.clone(),
            review: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(root: PathBuf, manifest: ApplicationsManifest) -> Self {
        Self {
            root,
            manifest,
            review: None,
        }
    }

    /// Seal the reviewed original deployments into `workspace`, or reopen the
    /// unit an earlier attempt sealed there.
    ///
    /// # Errors
    /// Refuses a nonprivate workspace, changed originals, missing signing
    /// authority, insufficient space or a changed sealed unit.
    pub fn seal(
        source: &Arc<PortableFlatpakSource>,
        runner: &impl OfflineRunner,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        if let Some(unit) = Self::reopen(workspace, cancel)? {
            return Ok(unit);
        }
        let pin = private(workspace)?;
        clear(&pin, workspace)?;
        pin.create_child(OsStr::new(STAGE))?;
        pin.create_child(OsStr::new(STAGE_JOURNALS))?;
        let limits = crate::setup::portable_home_media::reviewed_directory_limits(pin.clone())?;
        let stage = workspace.join(STAGE);
        let journals = workspace.join(STAGE_JOURNALS);
        let staged = source.stage(
            runner,
            StageGrant {
                root: &stage,
                journals: &journals,
                allocation_bytes: limits.plaintext_bytes / 2,
                include_user_state: false,
            },
            cancel,
        )?;
        let operation = uuid::Uuid::new_v4().to_string();
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(io::Error::other)?;
        let password = Zeroizing::new(hex::encode(secret));
        let package = workspace.join(format!("applications-{operation}.sealed"));
        let expectation = staged.seal(
            &package,
            &operation,
            &operation,
            Zeroizing::new(password.as_bytes().to_vec()),
            limits,
            cancel,
        )?;
        staged.validate()?;
        std::fs::remove_dir_all(&stage)?;
        std::fs::remove_dir_all(&journals)?;
        let record = Record {
            schema: 1,
            expectation,
            password: password.to_string(),
        };
        let unit = Self::admit(workspace, &record, cancel)?;
        publish_private_artifact(&pin, UNIT, &serde_json::to_vec(&record)?)?;
        Ok(unit)
    }

    /// Reopen the unit sealed in `workspace`, re-attesting the package and its
    /// admitted tree. Returns `None` when nothing was sealed there yet.
    ///
    /// # Errors
    /// Refuses a changed package, admitted tree or record.
    pub fn reopen(workspace: &Path, cancel: &AtomicBool) -> io::Result<Option<Self>> {
        let pin = private(workspace)?;
        let bytes = match read_private_artifact(&pin, UNIT) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let record: Record = serde_json::from_slice(&bytes)?;
        if record.schema != 1 {
            return Err(changed("Sealed applications record is unsupported"));
        }
        Self::admit(workspace, &record, cancel).map(Some)
    }

    fn admit(workspace: &Path, record: &Record, cancel: &AtomicBool) -> io::Result<Self> {
        let import = ApplicationImport::review_expected(
            &record.expectation,
            &workspace.join(&record.expectation.filename),
            record.password.as_bytes(),
            &workspace.join(QUARANTINE),
            cancel,
        )?;
        let ApplicationImport {
            review,
            root,
            manifest,
            ..
        } = import;
        Ok(Self {
            root,
            manifest,
            review: Some(Arc::new(Mutex::new(review))),
        })
    }

    /// Every sealed deployment, including the runtime closure of the apps.
    pub fn deployments(&self) -> &[Deployment] {
        &self.manifest.deployments
    }

    /// Deploy the unit into the target user installation.
    ///
    /// # Errors
    /// Refuses a changed unit, a conflicting user version or a failed deployment.
    pub fn apply(
        &self,
        runner: &impl OfflineRunner,
        home: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        let mut check = || match &self.review {
            Some(review) => review
                .lock()
                .map_err(|_| changed("Sealed applications review became unavailable"))?
                .verify_existing_tree(&self.root, cancel),
            None => Ok(()),
        };
        check()?;
        super::restore::apply(runner, &self.root, &self.manifest, home, cancel, &mut check)
    }
}

fn private(workspace: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(workspace)?;
    pin.validate_destination()?;
    crate::setup::destination_security::check_private_file(&pin.file)?;
    Ok(pin)
}

/// Remove what an interrupted seal left behind; no record means no unit.
fn clear(pin: &Arc<PinnedDirectory>, workspace: &Path) -> io::Result<()> {
    let directory = cap_std::fs::Dir::from_std_file(
        crate::setup::migration_durability::readable_directory(&pin.file)?,
    );
    for entry in directory.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let path = workspace.join(&name);
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    pin.validate_destination()
}

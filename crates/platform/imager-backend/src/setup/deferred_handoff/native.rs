//! The Linux-hosted route: the journal, and the reads that consume it.

use super::super::{
    account_transfer::{self, ThunderbirdSelection},
    browser_transfer,
    migration_handoff::{DeferredHandoff, Plan},
    migration_store::NativePath,
};
use super::{Prepare, Prepared};
use crate::{
    account_sources::{self, ReviewedThunderbird, SelectedAccount, SourceRoots},
    browser_profiles::{self, ReviewedProfile},
    browser_selection::SelectedSources,
};
use browser_continuity::Profile as BrowserProfile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The same-computer route only migrates a normal first installed account.
/// Arbitrary source/target UID mapping requires an authenticated identity
/// transport and is refused here instead of inferring it from a plan field.
pub const ORIGINAL_UID: u32 = super::super::migration_handoff::DEFERRED_ORIGINAL_UID;
pub const SCHEMA: u32 = 1;
const PURPOSE: &str = "same-computer-retained-selection";
const JOURNAL_SUFFIX: &str = ".deferred.json";
const MAX_JOURNAL_BYTES: usize = 512 * 1024;
const DIRECTORY: [&str; 4] = [".local", "state", "tilo", "migration-handoff"];

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn refused(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}

fn source_uid_matches(uid: u32) -> bool {
    // Test fixtures run as the build account. Production only admits the
    // reviewed UID 1000 source Home.
    cfg!(test) || uid == ORIGINAL_UID
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "migration cancelled",
        ))
    } else {
        Ok(())
    }
}

fn operation_path(home: &Path, operation: &str) -> PathBuf {
    let mut directory = home.to_path_buf();
    for segment in DIRECTORY {
        directory.push(segment);
    }
    directory.join(format!("{operation}{JOURNAL_SUFFIX}"))
}

fn open_or_create(
    directory: &Arc<crate::migration_directory::PinnedDirectory>,
    name: &str,
) -> io::Result<Arc<crate::migration_directory::PinnedDirectory>> {
    match crate::setup::nofollow::metadata_nofollow(&directory.file, Path::new(name)) {
        Ok(metadata) => {
            if metadata.is_symlink() || !metadata.is_dir() {
                return Err(refused("private journal path is not a directory"));
            }
            directory.open_child(name.as_ref(), &metadata)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use cap_primitives::fs::DirBuilderExt as _;
            let mut options = cap_primitives::fs::DirOptions::new();
            options.mode(0o700);
            cap_primitives::fs::create_dir(&directory.file, Path::new(name), &options)?;
            let metadata =
                crate::setup::nofollow::metadata_nofollow(&directory.file, Path::new(name))?;
            directory.open_child(name.as_ref(), &metadata)
        }
        Err(error) => Err(error),
    }
}

fn journal_directory(home: &Path) -> io::Result<Arc<crate::migration_directory::PinnedDirectory>> {
    let mut directory = crate::migration_directory::PinnedDirectory::open_absolute(home)?;
    for segment in DIRECTORY {
        directory = open_or_create(&directory, segment)?;
    }
    private_journal_directory(directory)
}

fn existing_journal_directory(
    home: &Path,
) -> io::Result<Arc<crate::migration_directory::PinnedDirectory>> {
    let mut directory = crate::migration_directory::PinnedDirectory::open_absolute(home)?;
    for segment in DIRECTORY {
        let metadata =
            crate::setup::nofollow::metadata_nofollow(&directory.file, Path::new(segment))?;
        if metadata.is_symlink() || !metadata.is_dir() {
            return Err(refused("private journal path is not a directory"));
        }
        directory = directory.open_child(segment.as_ref(), &metadata)?;
    }
    private_journal_directory(directory)
}

fn private_journal_directory(
    directory: Arc<crate::migration_directory::PinnedDirectory>,
) -> io::Result<Arc<crate::migration_directory::PinnedDirectory>> {
    directory.validate()?;
    let metadata = directory.file.metadata()?;
    if metadata.mode() & 0o077 != 0 {
        return Err(refused("private journal directory is not private"));
    }
    Ok(directory)
}

fn journal_bytes(
    directory: &Arc<crate::migration_directory::PinnedDirectory>,
    name: &str,
) -> io::Result<Vec<u8>> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt as _};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    use std::io::Read;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_JOURNAL_BYTES as u64
    {
        return Err(refused("private journal is not a bounded private file"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_JOURNAL_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(invalid("Deferred selection journal exceeds its bound"));
    }
    Ok(bytes)
}

fn publish_journal(
    directory: &Arc<crate::migration_directory::PinnedDirectory>,
    name: &str,
    bytes: &[u8],
) -> io::Result<()> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt as _};
    use std::io::Write;
    match crate::setup::nofollow::metadata_nofollow(&directory.file, Path::new(name)) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.is_symlink() {
                return Err(refused("private journal was replaced"));
            }
            if journal_bytes(directory, name)? != bytes {
                return Err(invalid("Deferred selection journal changed after review"));
            }
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .follow(FollowSymlinks::No);
    let mut file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    crate::setup::migration_durability::sync_directory(&directory.file)?;
    if journal_bytes(directory, name)? != bytes {
        return Err(invalid("Deferred selection journal did not persist"));
    }
    Ok(())
}

/// One selected file in a mail profile, with both its opaque discovery ID and
/// the source-owned name that survives a fresh discovery epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MailFile {
    id: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MailFolder {
    id: String,
    hierarchy: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MailAccount {
    id: String,
    source_account: String,
    server_key: String,
    folders: Vec<MailFolder>,
    address_books: Vec<MailFile>,
    calendars: Vec<MailFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MailProfile {
    id: String,
    label: String,
    accounts: Vec<MailAccount>,
}

/// The private selection journal. It is private source data, never media
/// metadata, and every field is checked against a freshly resolved source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    schema: u32,
    purpose: String,
    operation_id: String,
    plan_sha256: [u8; 32],
    original_uid: u32,
    applications: bool,
    accounts: bool,
    exclusions: Vec<NativePath>,
    mail: Vec<MailProfile>,
    browser: Vec<BrowserProfile>,
    /// Exact selected GOA identities and services. This is private source
    /// metadata: only the journal digest in the reviewed plan leaves the Home.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    selected_accounts: Vec<SelectedAccount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reconnect: Vec<String>,
}

impl Journal {
    fn new(
        plan: &Plan,
        exclusions: &[NativePath],
        applications: bool,
        accounts: bool,
    ) -> io::Result<Self> {
        plan.validate()?;
        if plan.home_uid() != ORIGINAL_UID || plan.home_inode() == 0 {
            return Err(refused(
                "Same-computer migration requires the reviewed original UID 1000 Home",
            ));
        }
        if plan.home_exclusions != exclusions {
            return Err(invalid(
                "Selected Home exclusions differ from the reviewed migration plan",
            ));
        }
        if applications && !plan.categories.flatpak {
            return Err(invalid("Application selection lacks reviewed consent"));
        }
        if accounts != plan.categories.accounts {
            return Err(invalid(
                "Account selection differs from the reviewed migration plan",
            ));
        }
        Ok(Self {
            schema: SCHEMA,
            purpose: PURPOSE.into(),
            operation_id: plan.operation_id.clone(),
            plan_sha256: plan.base_digest()?,
            original_uid: ORIGINAL_UID,
            applications,
            accounts,
            exclusions: exclusions.to_vec(),
            mail: Vec::new(),
            browser: Vec::new(),
            selected_accounts: Vec::new(),
            reconnect: Vec::new(),
        })
    }

    /// Read a previously written journal from the retained Home. The caller
    /// must already hold that Home through a revalidated retained-source mount.
    ///
    /// # Errors
    /// Refuses a missing or changed plan, a journal from another operation,
    /// a substituted source inode or UID, and any journal edited since review.
    pub fn open(home: &Path, plan: &Plan) -> io::Result<Self> {
        let binding = plan
            .deferred_handoff
            .as_ref()
            .ok_or_else(|| invalid("This plan has no deferred selected-source journal"))?;
        let metadata = std::fs::symlink_metadata(home)?;
        if !metadata.is_dir()
            || metadata.is_symlink()
            || !source_uid_matches(metadata.uid())
            || metadata.ino() != plan.home_inode()
            || plan.home_uid() != ORIGINAL_UID
            || binding.original_uid != ORIGINAL_UID
            || binding.schema != SCHEMA
        {
            return Err(refused(
                "Retained source Home is not the reviewed UID 1000 source",
            ));
        }
        let directory = existing_journal_directory(home)?;
        let name = format!("{}{JOURNAL_SUFFIX}", plan.operation_id);
        let bytes = journal_bytes(&directory, &name)?;
        let journal: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("Malformed deferred selection journal"))?;
        if journal.schema != SCHEMA
            || journal.purpose != PURPOSE
            || journal.operation_id != plan.operation_id
            || journal.original_uid != ORIGINAL_UID
            || journal.plan_sha256 != plan.base_digest()?
            || journal.exclusions != plan.home_exclusions
            || journal.applications != plan.categories.flatpak
            || journal.accounts != plan.categories.accounts
            || (!journal.accounts && !journal.selected_accounts.is_empty())
            || journal
                .selected_accounts
                .iter()
                .any(|account| account.services.is_empty())
            || binding.journal_sha256 != <[u8; 32]>::from(Sha256::digest(&bytes))
        {
            return Err(invalid(
                "Deferred selection journal is not bound to this retained source",
            ));
        }
        directory.validate()?;
        Ok(journal)
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn exclusions(&self) -> &[NativePath] {
        &self.exclusions
    }
    pub fn applications(&self) -> bool {
        self.applications
    }
    pub fn selected_accounts(&self) -> &[SelectedAccount] {
        &self.selected_accounts
    }
    pub fn reconnect(&self) -> &[String] {
        &self.reconnect
    }
}

fn add_selected_accounts(
    journal: &mut Journal,
    accounts: bool,
    selected: &[SelectedAccount],
) -> io::Result<()> {
    if !accounts && !selected.is_empty() {
        return Err(invalid(
            "Selected online accounts lack reviewed account consent",
        ));
    }
    for selected in selected {
        if selected.source_id.is_empty()
            || selected.source_id.len() > 256
            || selected.identity.is_empty()
            || selected.identity.len() > 256
            || selected.identity.chars().any(char::is_control)
            || selected.label.len() > 256
            || selected.label.chars().any(char::is_control)
            || selected.services.is_empty()
            || selected.services.len() > 4
            || !matches!(
                selected.provider,
                account_sources::Provider::Google | account_sources::Provider::Microsoft
            )
        {
            return Err(invalid("Selected online account metadata is invalid"));
        }
        match journal
            .selected_accounts
            .iter()
            .find(|existing| existing.source_id == selected.source_id)
        {
            Some(existing) if existing != selected => {
                return Err(invalid(
                    "One selected online account was reviewed with two different services",
                ));
            }
            Some(_) => {}
            None => journal.selected_accounts.push(selected.clone()),
        }
    }
    journal
        .selected_accounts
        .sort_by(|left, right| left.source_id.cmp(&right.source_id));
    Ok(())
}

/// Write the person's sealed choices into the source Home and return the
/// opaque locator the reviewed plan carries across installation.
///
/// # Errors
/// Refuses a replaced source Home, selections that differ from the plan, or
/// a journal that cannot be kept private.
pub fn prepare(input: &Prepare<'_>) -> io::Result<Prepared> {
    let metadata = std::fs::symlink_metadata(input.source_home)?;
    if !metadata.is_dir()
        || metadata.is_symlink()
        || !source_uid_matches(metadata.uid())
        || metadata.ino() != input.plan.home_inode()
    {
        return Err(refused(
            "Selected source Home differs from the reviewed UID 1000 source",
        ));
    }
    let mut journal = Journal::new(
        input.plan,
        input.exclusions,
        input.applications,
        input.accounts,
    )?;
    for (reviewed, selection) in input.mail {
        journal.mail.push(mail_profile(reviewed, selection)?);
        if reviewed
            .accounts
            .iter()
            .any(|account| account.inventory.needs_reconnect)
        {
            journal
                .reconnect
                .push(format!("mail:{}", reviewed.profile.source_id));
        }
    }
    if input.accounts_in_package && !input.account_sources.is_empty() {
        return Err(invalid(
            "Selected accounts cannot be both in the package and in the journal",
        ));
    }
    if input.browser_in_package && !input.browser.profiles().is_empty() {
        return Err(invalid(
            "Selected browser profiles cannot be both in the package and in the journal",
        ));
    }
    add_selected_accounts(&mut journal, input.accounts, input.account_sources)?;
    for (source, selected) in input.browser.profiles() {
        source.validate()?;
        if selected.id != source.profile.id {
            return Err(invalid(
                "Browser selection differs from its reviewed source",
            ));
        }
        journal.browser.push(selected.clone());
    }
    if !journal.browser.is_empty() && !input.browser_in_package {
        // Website session stores and saved passwords remain in their source
        // vault; the copied profile data does not claim they were transferred.
        journal.reconnect.push("browser:sessions".into());
        journal.reconnect.push("browser:credentials".into());
    }
    if journal.accounts && !input.accounts_in_package {
        journal.reconnect.push("accounts:credentials".into());
    }
    journal
        .browser
        .sort_by(|left, right| left.id.cmp(&right.id));
    journal.mail.sort_by(|left, right| left.id.cmp(&right.id));
    journal.reconnect.sort();
    journal.reconnect.dedup();
    let directory = journal_directory(input.source_home)?;
    let bytes = serde_json::to_vec(&journal).map_err(io::Error::other)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(invalid("Deferred selection journal exceeds its bound"));
    }
    let name = format!("{}{JOURNAL_SUFFIX}", input.plan.operation_id);
    publish_journal(&directory, &name, &bytes)?;
    directory.validate()?;
    let binding = DeferredHandoff {
        schema: SCHEMA,
        original_uid: ORIGINAL_UID,
        journal_sha256: Sha256::digest(&bytes).into(),
    };
    Ok(Prepared {
        binding,
        journal: operation_path(input.source_home, &input.plan.operation_id),
    })
}

fn mail_profile(
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
) -> io::Result<MailProfile> {
    selection
        .validate(reviewed)
        .map_err(|_| invalid("Selected mail differs from its reviewed source"))?;
    let mut accounts = Vec::with_capacity(selection.accounts.len());
    for selected in &selection.accounts {
        let account = reviewed
            .accounts
            .iter()
            .find(|account| account.inventory.source_id == selected.source_id)
            .ok_or_else(|| invalid("Selected mail account disappeared"))?;
        let folders = account
            .folders
            .iter()
            .filter(|folder| selected.folders.contains(&folder.inventory.source_id))
            .map(|folder| MailFolder {
                id: folder.inventory.source_id.clone(),
                hierarchy: folder.inventory.hierarchy.clone(),
            })
            .collect();
        let address_books = reviewed
            .address_books
            .iter()
            .filter(|book| selected.address_books.contains(&book.source_id))
            .map(|book| MailFile {
                id: book.source_id.clone(),
                name: book.name.clone(),
            })
            .collect();
        let calendars = reviewed
            .calendars
            .iter()
            .filter(|calendar| selected.calendars.contains(&calendar.source_id))
            .map(|calendar| MailFile {
                id: calendar.source_id.clone(),
                name: calendar.name.clone(),
            })
            .collect();
        accounts.push(MailAccount {
            id: account.inventory.source_id.clone(),
            source_account: account.inventory.source_account.clone(),
            server_key: account.metadata.server_key.clone(),
            folders,
            address_books,
            calendars,
        });
    }
    Ok(MailProfile {
        id: reviewed.profile.source_id.clone(),
        label: reviewed.profile.label.clone(),
        accounts,
    })
}

fn current_mail_selection(
    saved: &MailProfile,
    reviewed: &ReviewedThunderbird,
) -> io::Result<ThunderbirdSelection> {
    let same_profile = saved.id == reviewed.profile.source_id;
    let mut accounts = Vec::with_capacity(saved.accounts.len());
    for selected in &saved.accounts {
        let account = reviewed
            .accounts
            .iter()
            .find(|account| {
                (same_profile && account.inventory.source_id == selected.id)
                    || (account.inventory.source_account == selected.source_account
                        && account.metadata.server_key == selected.server_key)
            })
            .ok_or_else(|| invalid("A selected mail account no longer exists at the source"))?;
        let mut folders = Vec::with_capacity(selected.folders.len());
        for folder in &selected.folders {
            let current = account
                .folders
                .iter()
                .find(|current| {
                    current.inventory.source_id == folder.id
                        || current.inventory.hierarchy == folder.hierarchy
                })
                .ok_or_else(|| invalid("A selected mail folder no longer exists at the source"))?;
            folders.push(current.inventory.source_id.clone());
        }
        let mut address_books = Vec::with_capacity(selected.address_books.len());
        for book in &selected.address_books {
            let saved_id = &book.id;
            let saved_name = &book.name;
            let current = reviewed
                .address_books
                .iter()
                .find(|current| &current.source_id == saved_id || &current.name == saved_name)
                .ok_or_else(|| invalid("A selected address book no longer exists at the source"))?;
            address_books.push(current.source_id.clone());
        }
        let mut calendars = Vec::with_capacity(selected.calendars.len());
        for calendar in &selected.calendars {
            let saved_id = &calendar.id;
            let saved_name = &calendar.name;
            let current = reviewed
                .calendars
                .iter()
                .find(|current| &current.source_id == saved_id || &current.name == saved_name)
                .ok_or_else(|| invalid("A selected calendar no longer exists at the source"))?;
            calendars.push(current.source_id.clone());
        }
        accounts.push(account_transfer::ThunderbirdAccountSelection {
            source_id: account.inventory.source_id.clone(),
            folders,
            address_books,
            calendars,
        });
    }
    let selection = ThunderbirdSelection { accounts };
    selection
        .validate(reviewed)
        .map_err(|_| invalid("Deferred mail selection is no longer valid"))?;
    Ok(selection)
}

fn current_browser_sources(
    saved: &[BrowserProfile],
    home: &Path,
    candidates: &[account_sources::Candidate],
) -> io::Result<SelectedSources> {
    let mut current = browser_profiles::discover(home, candidates)?;
    let mut selected = Vec::with_capacity(saved.len());
    for saved in saved {
        let index = current
            .iter()
            .position(|source| {
                source.profile.provider == saved.provider
                    && source.profile.relative_path == saved.relative_path
            })
            .ok_or_else(|| invalid("A selected browser profile no longer exists at the source"))?;
        let source: ReviewedProfile = current.remove(index);
        if !source.exportable
            || (saved.bookmarks && !source.profile.bookmarks)
            || (saved.history && !source.profile.history)
        {
            return Err(invalid(
                "A selected browser data class is no longer available",
            ));
        }
        let mut profile = source.profile.clone();
        profile.bookmarks &= saved.bookmarks;
        profile.history &= saved.history;
        profile
            .validate()
            .map_err(|_| invalid("Selected browser profile changed"))?;
        selected.push((Arc::new(source), profile));
    }
    SelectedSources::from_reviewed(selected)
}

fn browser_operation_directory_path(home: &Path, operation: &str) -> io::Result<PathBuf> {
    let root = browser_continuity::completion::deferred_root(home, operation);
    if root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| uuid::Uuid::parse_str(name).ok())
        .is_none()
    {
        return Err(refused("Browser projection operation is not canonical"));
    }
    Ok(root)
}

fn browser_operation_directory(
    home: &Path,
    operation: &str,
) -> io::Result<Arc<crate::migration_directory::PinnedDirectory>> {
    let mut directory = crate::migration_directory::PinnedDirectory::open_absolute(home)?;
    for segment in [".local", "state", "tilo"] {
        directory = open_or_create(&directory, segment)?;
    }
    directory = open_or_create(&directory, "browser-continuity")?;
    directory = private_journal_directory(directory)?;
    let root = browser_operation_directory_path(home, operation)?;
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refused("Browser projection operation is not canonical"))?;
    directory = open_or_create(&directory, name)?;
    private_journal_directory(directory)
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserReceipt {
    pub profiles: Vec<String>,
    /// Private target path for the verified projection. Never serialized into
    /// a public receipt because it names the destination account's Home.
    #[serde(skip)]
    pub projection: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub operation_id: String,
    pub mail: Vec<account_transfer::ThunderbirdImportReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<account_transfer::AccountImportReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconnect: Vec<String>,
}

/// Reopen the retained source's private journal and import exactly the saved
/// mail and browser selections into the current target Home.
///
/// The caller supplies the already resolved retained source directory and the
/// active target person; neither is read from the journal. Files and original
/// applications remain owned by the existing installed migration helper, which
/// receives the same plan and therefore the same exclusions and app inventory.
///
/// # Errors
/// Refuses a changed source, a journal edited after review, a vanished selected
/// child, or an unavailable target. It never widens the saved selection.
pub fn import_selected_from_source(
    plan: &Plan,
    source_home: &Path,
    target_home: &Path,
    _workspace: &Path,
    target_person: [u8; 32],
    cancel: &AtomicBool,
) -> io::Result<Receipt> {
    check_cancel(cancel)?;
    if plan.deferred_handoff.is_none() {
        return Ok(Receipt {
            operation_id: plan.operation_id.clone(),
            ..Receipt::default()
        });
    }
    let journal = Journal::open(source_home, plan)?;
    check_cancel(cancel)?;
    let needs_source = !journal.mail.is_empty()
        || !journal.browser.is_empty()
        || !journal.selected_accounts.is_empty();
    let inventory = needs_source.then(|| {
        account_sources::discover(&SourceRoots {
            home: source_home.to_path_buf(),
            config: source_home.join(".config"),
        })
    });
    let mut receipt = Receipt {
        operation_id: plan.operation_id.clone(),
        reconnect: journal.reconnect.clone(),
        ..Receipt::default()
    };
    if let Some(inventory) = &inventory {
        import_selected_accounts(&journal, inventory, target_home, &mut receipt)?;
        import_mail(&journal, inventory, plan, target_home, cancel, &mut receipt)?;
        receipt.browser = import_browser(
            &journal,
            inventory,
            plan,
            source_home,
            target_home,
            target_person,
            cancel,
        )?;
    }
    receipt.reconnect.sort();
    receipt.reconnect.dedup();
    check_cancel(cancel)?;
    Ok(receipt)
}

fn import_selected_accounts(
    journal: &Journal,
    inventory: &account_sources::Inventory,
    target_home: &Path,
    receipt: &mut Receipt,
) -> io::Result<()> {
    if journal.selected_accounts.is_empty() {
        return Ok(());
    }
    let target_config = target_home.join(".config/tilo");
    let accounts = account_transfer::import_selected_goa(
        &target_config,
        inventory,
        &journal.selected_accounts,
    )
    .map_err(|error| {
        io::Error::other(format!(
            "Selected online accounts could not be prepared: {error:?}"
        ))
    })?;
    for account in &accounts {
        if account.pending {
            receipt
                .reconnect
                .push(format!("accounts:{}", account.source_id));
        }
    }
    receipt.accounts = accounts;
    Ok(())
}

fn import_mail(
    journal: &Journal,
    inventory: &account_sources::Inventory,
    plan: &Plan,
    target_home: &Path,
    cancel: &AtomicBool,
    receipt: &mut Receipt,
) -> io::Result<()> {
    let binding = plan.operation_id.as_bytes();
    for saved in &journal.mail {
        check_cancel(cancel)?;
        let reviewed = inventory
            .thunderbird_reviewed
            .iter()
            .find(|reviewed| reviewed.profile.source_id == saved.id)
            .or_else(|| {
                inventory.thunderbird_reviewed.iter().find(|reviewed| {
                    reviewed.profile.label == saved.label
                        && saved.accounts.iter().all(|selected| {
                            reviewed.accounts.iter().any(|account| {
                                account.inventory.source_account == selected.source_account
                                    && account.metadata.server_key == selected.server_key
                            })
                        })
                })
            })
            .ok_or_else(|| invalid("A selected mail profile no longer exists at the source"))?;
        let selection = current_mail_selection(saved, reviewed)?;
        // The retained source Home is already granted and pinned; the streamed
        // import reads the folders straight from it instead of building a
        // bundle of every message in memory first.
        let imported =
            account_transfer::import_reviewed(target_home, reviewed, &selection, binding, cancel)
                .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Selected mail could not be imported: {error}"),
                )
            })?;
        if imported
            .accounts
            .iter()
            .any(|account| account.needs_reconnect)
        {
            receipt.reconnect.push(format!("mail:{}", saved.id));
        }
        receipt.mail.push(imported);
        check_cancel(cancel)?;
    }
    Ok(())
}

fn import_browser(
    journal: &Journal,
    inventory: &account_sources::Inventory,
    plan: &Plan,
    source_home: &Path,
    target_home: &Path,
    target_person: [u8; 32],
    cancel: &AtomicBool,
) -> io::Result<Option<BrowserReceipt>> {
    if journal.browser.is_empty() {
        return Ok(None);
    }
    let selected = current_browser_sources(&journal.browser, source_home, &inventory.candidates)
        .map_err(|error| io::Error::other(format!("Selected browser source changed: {error}")))?;
    let browser_plan = selected
        .plan(format!("deferred:{}", plan.operation_id), target_person)
        .map_err(|error| io::Error::other(format!("Selected browser plan failed: {error}")))?;
    // The Browser app consumes a launcher-granted projection at its canonical
    // location. Building it here is the same capture the authenticated
    // installation route uses; only the binding signature differs, and it is
    // issued by the active device for this reviewed migration operation.
    let operation_root = browser_operation_directory_path(target_home, &plan.operation_id)?;
    let operation_directory = browser_operation_directory(target_home, &plan.operation_id)?;
    let stable = browser_continuity::completion::deferred_projection(&operation_root);
    let container_path = operation_root.join(browser_continuity::completion::DEFERRED_CONTAINER);
    match std::fs::symlink_metadata(&container_path) {
        Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => {
            std::fs::remove_dir_all(&container_path)?;
        }
        Ok(_) => return Err(refused("Browser projection path was redirected")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let completion_path = operation_root.join(browser_continuity::completion::FILE_NAME);
    match std::fs::symlink_metadata(&completion_path) {
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => {
            std::fs::remove_file(&completion_path)?;
        }
        Ok(_) => return Err(refused("Browser completion path was redirected")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let container = open_or_create(
        &operation_directory,
        browser_continuity::completion::DEFERRED_CONTAINER,
    )?;
    let container = private_journal_directory(container)?;
    let import = open_or_create(&container, "import")?;
    let import = private_journal_directory(import)?;
    let home_directory = open_or_create(&import, "home")?;
    private_journal_directory(home_directory)?;
    let plan_bytes = browser_plan.encode().map_err(|error| {
        io::Error::other(format!("Selected browser plan encoding failed: {error}"))
    })?;
    write_private_file(&stable.join(browser_continuity::PLAN_FILE), &plan_bytes)?;
    let profiles = stable.join(browser_continuity::PROFILES_DIRECTORY);
    create_private_dir(&profiles)?;
    for (source, profile) in selected.profiles() {
        check_cancel(cancel)?;
        source.validate()?;
        let snapshot = source.capture_files_cancelled(profile, cancel)?;
        let profile_directory = profiles.join(&profile.id);
        create_private_dir(&profile_directory)?;
        for file in snapshot.files() {
            write_private_file(&profile_directory.join(file.name()), file.bytes())?;
        }
        source.validate()?;
    }
    browser_transfer::verify_tree(&stable, &browser_plan, cancel).map_err(|error| {
        io::Error::other(format!("Selected browser projection failed: {error}"))
    })?;
    let setup_digest = browser_continuity::completion::deferred_setup_digest(&plan.operation_id);
    let browser_digest: [u8; 32] = Sha256::digest(&plan_bytes).into();
    let unsigned = browser_continuity::completion::capture(
        browser_plan.clone(),
        setup_digest,
        browser_digest,
        plan.operation_id.clone(),
        stable.clone(),
    )
    .map_err(|error| io::Error::other(format!("Selected browser capture failed: {error}")))?;
    let (signer, certificate) =
        super::super::browser_transfer_helper::completion::certified_signer(target_person)?;
    let signed = unsigned
        .sign(&signer, certificate)
        .map_err(|error| io::Error::other(format!("Selected browser proof failed: {error}")))?;
    signed
        .verify_binding(
            &browser_plan,
            setup_digest,
            browser_digest,
            &plan.operation_id,
        )
        .map_err(|error| io::Error::other(format!("Selected browser proof changed: {error}")))?;
    signed
        .verify_projection(signer.verifying_key().to_bytes())
        .map_err(|error| {
            io::Error::other(format!("Selected browser projection changed: {error}"))
        })?;
    let completion_bytes = signed.encode().map_err(|error| {
        io::Error::other(format!("Selected browser proof encoding failed: {error}"))
    })?;
    write_private_file(&completion_path, &completion_bytes)?;
    operation_directory.validate()?;
    Ok(Some(BrowserReceipt {
        profiles: browser_plan
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect(),
        projection: stable,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{
        migration_handoff::{self, Categories, HomePolicy},
        migration_store::NativePath,
        prepared_identity::PreparedIdentity,
    };
    use std::os::unix::fs::PermissionsExt;
    use tilo_identity_host::provisioning::IdentityStorage;

    fn write(path: &Path, bytes: impl AsRef<[u8]>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn source_dir(path: &Path) {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The active device that signs a deferred first-launch completion. The key
    /// and certificate are the real identity-host custody, not a fixture grant.
    fn provision_device(
        root: &Path,
        identity: &PreparedIdentity,
        environment: &mut tilo_test_env::ScopedEnv,
    ) -> [u8; 32] {
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
        environment.set("XDG_CONFIG_HOME", &config);
        environment.set("TILO_IDENTITY_ROOT", root.join("system"));
        let record = identity.custody_directory().join("primary.json");
        let mut registry = tilo_identity_host::registry::IdentityRegistry::default();
        let id = registry.register_record(&record).unwrap().id.clone();
        tilo_identity_host::registry::IdentityRegistryStore::from_env()
            .unwrap()
            .save(&registry)
            .unwrap();
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        let cert_path =
            tilo_identity_host::provisioning::ensure_device_certificate_for_record(&record, &paths)
                .unwrap();
        let cert = tilo_user_identity::persist::read_device_cert(&cert_path).unwrap();
        tilo_identity_host::device_fleet::DeviceFleetStore::from_env()
            .unwrap()
            .remember_certificate(&id, cert)
            .unwrap();
        identity.public_key()
    }

    fn mail_fixture(home: &Path) {
        let root = home.join(".thunderbird/abc.default");
        write(
            &home.join(".thunderbird/profiles.ini"),
            "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\n",
        );
        write(
            &root.join("prefs.js"),
            r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.identity.id1.useremail", "person@example.test");
user_pref("mail.identity.id1.fullName", "Personal");
"#,
        );
        write(
            &root.join("ImapMail/imap.example.test/Inbox"),
            "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nSubject: Kept\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <kept@example.test>\r\n\r\nBody.\r\n",
        );
    }

    fn browser_fixture(home: &Path) {
        let profile = home.join(".config/chromium/Default");
        write(
            &profile.join("Bookmarks"),
            "{\"roots\":{\"bookmark_bar\":{\"type\":\"folder\",\"name\":\"Bar\",\"children\":[{\"type\":\"url\",\"name\":\"Bookmark\",\"url\":\"https://example.test/\"}]}}}",
        );
        write(
            &profile.join("History"),
            b"not a database, but the reviewed fixture only copies exact bytes",
        );
    }

    fn plan(home: &Path) -> Plan {
        let mut plan = migration_handoff::capture_fixture(
            home,
            &uuid::Uuid::new_v4().to_string(),
            Categories {
                home: true,
                flatpak: false,
                accounts: false,
            },
        )
        .unwrap();
        plan.home_policy = HomePolicy::Copy;
        plan.home_exclusions = vec![NativePath::encode(Path::new("Documents/Private"))];
        plan.set_fixture_original_uid(ORIGINAL_UID);
        plan.validate().unwrap();
        plan
    }

    fn prepared(home: &Path) -> (Plan, Prepared, ReviewedThunderbird) {
        mail_fixture(home);
        browser_fixture(home);
        let mut plan = plan(home);
        let inventory = account_sources::discover(&SourceRoots {
            home: home.to_path_buf(),
            config: home.join(".config"),
        });
        let reviewed = inventory.thunderbird_reviewed[0].clone();
        let selection = ThunderbirdSelection::all(&reviewed);
        let browser = crate::browser_selection::Selection::discover(home, &inventory.candidates)
            .unwrap()
            .selected();
        let prepared = prepare(&Prepare {
            source_home: home,
            plan: &plan,
            exclusions: &plan.home_exclusions,
            mail: &[(reviewed.clone(), selection)],
            browser: &browser,
            applications: false,
            accounts: false,
            account_sources: &[],
            accounts_in_package: false,
            browser_in_package: false,
        })
        .unwrap();
        plan.attach_deferred_handoff(prepared.binding.clone())
            .unwrap();
        (plan, prepared, reviewed)
    }

    #[test]
    fn journal_is_private_bound_and_reimports_only_the_selected_mail_and_browser() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("root");
        source_dir(&root);
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [1, 2, 3],
            IdentityStorage::Files,
        )
        .unwrap();
        let person = provision_device(&root.join("device"), &identity, &mut environment);
        let home = fixture.path().join("source");
        source_dir(&home);
        let (plan, prepared, _reviewed) = prepared(&home);
        assert_eq!(
            std::fs::metadata(&prepared.journal)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let opened = Journal::open(&home, &plan).unwrap();
        assert_eq!(opened.operation_id(), plan.operation_id);
        assert_eq!(opened.exclusions(), plan.home_exclusions.as_slice());
        let target = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let receipt = import_selected_from_source(
            &plan,
            &home,
            target.path(),
            workspace.path(),
            person,
            &AtomicBool::new(false),
        )
        .unwrap_or_else(|error| panic!("deferred import failed: {error}"));
        assert_eq!(receipt.mail.len(), 1);
        assert_eq!(receipt.mail[0].accounts.len(), 1);
        assert!(
            receipt
                .reconnect
                .iter()
                .any(|reason| reason == "browser:sessions"),
            "copied browser profiles still require a specific sign-in"
        );
        assert!(
            receipt
                .reconnect
                .iter()
                .any(|reason| reason == "browser:credentials"),
            "saved passwords stay in the source and require a new sign-in"
        );
        assert!(target.path().join(".config/tilo").is_dir());
        assert!(target.path().join(".cache/so.tilo.mail/mail").is_dir());
        let browser = receipt.browser.expect("selected browser projection");
        assert_eq!(browser.profiles.len(), 1);
        assert!(browser.projection.join("plan.json").is_file());
        assert!(browser.projection.join("profiles").is_dir());
        let deferred_root =
            browser_continuity::completion::deferred_root(target.path(), &plan.operation_id);
        assert_eq!(
            browser.projection,
            browser_continuity::completion::deferred_projection(&deferred_root)
        );
        let completion = browser_continuity::completion::Completion::read(
            &deferred_root.join(browser_continuity::completion::FILE_NAME),
        )
        .unwrap();
        assert_eq!(completion.operation(), plan.operation_id);
        assert_eq!(
            completion.setup_digest(),
            browser_continuity::completion::deferred_setup_digest(&plan.operation_id)
        );
        assert_eq!(completion.plan().identity_public_key, person);
        // Nothing private is written into the installation plan metadata.
        let plan_bytes = plan.bytes().unwrap();
        assert!(!String::from_utf8_lossy(&plan_bytes).contains("person@example.test"));
        assert!(!String::from_utf8_lossy(&plan_bytes).contains("ImapMail"));
        assert!(!String::from_utf8_lossy(&plan_bytes).contains("profile.id"));
    }

    #[test]
    fn browser_already_carried_by_the_package_is_not_duplicated_in_the_journal() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("source");
        source_dir(&home);
        browser_fixture(&home);
        let inventory = account_sources::discover(&SourceRoots {
            home: home.clone(),
            config: home.join(".config"),
        });
        let browser = crate::browser_selection::Selection::discover(&home, &inventory.candidates)
            .unwrap()
            .selected();
        let mut package_plan = plan(&home);
        assert!(!browser.profiles().is_empty());
        let prepared = prepare(&Prepare {
            source_home: &home,
            plan: &package_plan,
            exclusions: &package_plan.home_exclusions,
            mail: &[],
            browser: &SelectedSources::default(),
            applications: false,
            accounts: false,
            account_sources: &[],
            accounts_in_package: false,
            browser_in_package: true,
        })
        .unwrap();
        package_plan
            .attach_deferred_handoff(prepared.binding)
            .unwrap();
        let journal = Journal::open(&home, &package_plan).unwrap();
        assert!(journal.browser.is_empty());
        assert!(
            !journal
                .reconnect()
                .iter()
                .any(|reason| reason.starts_with("browser:") || reason.starts_with("accounts:")),
            "the package already carries the browser and any selected sign-ins"
        );

        let duplicate = prepare(&Prepare {
            source_home: &home,
            plan: &plan(&home),
            exclusions: &package_plan.home_exclusions,
            mail: &[],
            browser: &browser,
            applications: false,
            accounts: false,
            account_sources: &[],
            accounts_in_package: false,
            browser_in_package: true,
        })
        .expect_err("a profile cannot be both in the package and the journal");
        assert!(duplicate.to_string().contains("both in the package"));
    }

    #[test]
    fn only_the_selected_goa_account_and_its_services_cross_the_private_journal() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("source");
        source_dir(&home);
        write(
            &home.join(".config/goa-1.0/accounts.conf"),
            "[Account account_1]\nProvider=google\nIdentity=person@example.test\nPresentationIdentity=person@example.test\nFilesEnabled=true\nMailEnabled=true\nCalendarEnabled=false\nContactsEnabled=false\n[Account account_2]\nProvider=google\nIdentity=other@example.test\nPresentationIdentity=other@example.test\nFilesEnabled=true\nMailEnabled=true\nCalendarEnabled=false\nContactsEnabled=false\n",
        );
        let mut plan = migration_handoff::capture_fixture(
            &home,
            &uuid::Uuid::new_v4().to_string(),
            Categories {
                home: true,
                flatpak: false,
                accounts: true,
            },
        )
        .unwrap();
        plan.home_policy = HomePolicy::Copy;
        plan.home_exclusions = Vec::new();
        plan.set_fixture_original_uid(ORIGINAL_UID);
        plan.validate().unwrap();
        let inventory = account_sources::discover(&SourceRoots {
            home: home.clone(),
            config: home.join(".config"),
        });
        let selected: Vec<_> = inventory
            .candidates
            .iter()
            .filter(|candidate| candidate.identity.as_deref() == Some("person@example.test"))
            .filter_map(SelectedAccount::from_candidate)
            .collect();
        assert_eq!(selected.len(), 1);
        let prepared = prepare(&Prepare {
            source_home: &home,
            plan: &plan,
            exclusions: &plan.home_exclusions,
            mail: &[],
            browser: &SelectedSources::default(),
            applications: false,
            accounts: true,
            account_sources: &selected,
            accounts_in_package: false,
            browser_in_package: false,
        })
        .unwrap();
        plan.attach_deferred_handoff(prepared.binding).unwrap();
        let journal = Journal::open(&home, &plan).unwrap();
        assert_eq!(journal.selected_accounts(), selected.as_slice());
        let target = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let receipt = import_selected_from_source(
            &plan,
            &home,
            target.path(),
            workspace.path(),
            [7; 32],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(receipt.accounts.len(), 1);
        assert_eq!(
            receipt.accounts[0].services,
            vec![
                account_sources::Service::Files,
                account_sources::Service::Mail
            ]
        );
        assert!(receipt.accounts[0].pending);
        let serialized = serde_json::to_string(&receipt).unwrap();
        assert!(!serialized.contains("person@example.test"));
        assert!(!serialized.contains("other@example.test"));
        assert!(!serialized.contains(&home.to_string_lossy().to_string()));
        let store = accounts::AccountStore::load_in(&target.path().join(".config/tilo"));
        assert!(store.all().iter().any(|account| {
            account.type_id.as_str() == accounts::thunderbird::PENDING_TYPE
                && account.config["email"] == "person@example.test"
        }));
        assert!(store.all().iter().all(|account| {
            account
                .config
                .get("email")
                .and_then(serde_json::Value::as_str)
                != Some("other@example.test")
        }));
    }

    #[test]
    fn changed_plan_journal_or_source_root_is_refused() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("source");
        source_dir(&home);
        let (plan, _prepared, _reviewed) = prepared(&home);
        let journal = journal_bytes(
            &journal_directory(&home).unwrap(),
            &format!("{}{JOURNAL_SUFFIX}", plan.operation_id),
        )
        .unwrap();

        // A plan edited after review no longer matches the journal digest.
        let mut changed = plan.clone();
        changed.home_exclusions = Vec::new();
        assert!(Journal::open(&home, &changed).is_err());

        // A substituted journal no longer matches the plan's journal digest.
        let mut altered = journal.clone();
        altered.push(b' ');
        let journal_path = operation_path(&home, &plan.operation_id);
        std::fs::write(&journal_path, &altered).unwrap();
        assert!(Journal::open(&home, &plan).is_err());
        std::fs::write(&journal_path, &journal).unwrap();

        // Replacing the source root changes the reviewed inode.
        let replacement = fixture.path().join("replacement");
        source_dir(&replacement);
        std::fs::rename(&home, fixture.path().join("old-source")).unwrap();
        std::fs::rename(&replacement, &home).unwrap();
        assert!(Journal::open(&home, &plan).is_err());
        let target = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        assert!(
            import_selected_from_source(
                &plan,
                &home,
                target.path(),
                workspace.path(),
                [7; 32],
                &AtomicBool::new(false),
            )
            .is_err()
        );
    }
}

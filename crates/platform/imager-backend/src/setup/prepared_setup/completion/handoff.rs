//! Immutable native-issued installer generation; the pending generation stays intact.
use super::boot_review::{CompletedBootReview, Selection, child};
use super::*;
use crate::setup::migration::{changed, check_cancel};
use crate::setup::prepared_setup::{OfflineApplicationsPlan, OriginalSelectionPlan};
use cap_primitives::fs::{DirBuilderExt as _, MetadataExt as _};
use std::{ffi::OsStr, fs::File, os::unix::fs::MetadataExt as _};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
const ROOT: &str = "/run/tilo-installer";
const SELECTOR: &str = ".completed-generation.json";
const BOUND: u64 = 256 * 1024;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) enum Payload {
    Home(crate::setup::portable_home_media::Expectation),
    Applications(crate::setup::flatpak_transfer::portable::Expectation),
    Browser(crate::setup::browser_transfer::Expectation),
}
impl Payload {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::Home(_) => "home",
            Self::Applications(_) => "applications",
            Self::Browser(_) => "browser",
        }
    }
    pub(super) fn filename(&self) -> &str {
        match self {
            Self::Home(e) => &e.filename,
            Self::Applications(e) => &e.filename,
            Self::Browser(e) => &e.filename,
        }
    }
    pub(super) fn length(&self) -> u64 {
        match self {
            Self::Home(e) => e.sealed_bytes,
            Self::Applications(e) => e.sealed_bytes,
            Self::Browser(e) => e.sealed_bytes,
        }
    }
    pub(super) fn digest(&self) -> [u8; 32] {
        match self {
            Self::Home(e) => e.ciphertext_sha256,
            Self::Applications(e) => e.ciphertext_sha256,
            Self::Browser(e) => e.ciphertext_sha256,
        }
    }
    fn validate(&self) -> io::Result<()> {
        match self {
            Self::Home(e) => e.validate(),
            Self::Applications(e) => e.validate(),
            Self::Browser(e) => e.validate(),
        }
    }
    fn expectation_json(&self) -> io::Result<serde_json::Value> {
        Ok(match self {
            Self::Home(e) => serde_json::to_value(e)?,
            Self::Applications(e) => serde_json::to_value(e)?,
            Self::Browser(e) => serde_json::to_value(e)?,
        })
    }
    fn locator_name(&self) -> &'static str {
        match self {
            Self::Home(_) => "portable-home.json",
            Self::Applications(_) => "portable-applications.json",
            Self::Browser(_) => "portable-browser.json",
        }
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Member {
    pub(super) payload: Payload,
    pub(super) snapshot: [u64; 5],
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    schema: u32,
    generation: String,
    inode: u64,
    pub(super) selection: Selection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    declaration: Option<OfflineHomePlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applications_declaration: Option<OfflineApplicationsPlan>,
    original_present: bool,
    pub(super) members: Vec<Member>,
    profile_sha256: [u8; 32],
}
impl Record {
    pub(super) fn declaration(&self) -> io::Result<OriginalSelectionPlan> {
        self.selection.validate_shape()?;
        match (
            self.schema,
            &self.declaration,
            &self.applications_declaration,
        ) {
            (1, Some(home), None) => {
                home.validate()?;
                Ok(OriginalSelectionPlan::Home(home.clone()))
            }
            (2, None, Some(apps)) => {
                apps.validate()?;
                Ok(OriginalSelectionPlan::Applications(apps.clone()))
            }
            _ => Err(invalid()),
        }
    }
}

fn root() -> io::Result<Arc<PinnedDirectory>> {
    // SAFETY: geteuid only observes the current native helper's credentials.
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(changed(
            "Completed installer generation requires root custody",
        ));
    }
    let pin = PinnedDirectory::open_absolute(Path::new(ROOT))?;
    pin.validate_destination()?;
    let m = pin.file.metadata()?;
    if m.uid() != 0 || m.gid() != 0 || m.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    Ok(pin)
}
fn storage(pin: &Arc<PinnedDirectory>) -> io::Result<StorageDirectory> {
    StorageDirectory::from_file(migration_durability::readable_directory(&pin.file)?)
}
fn publish(pin: &Arc<PinnedDirectory>, name: &str, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() as u64 > BOUND {
        return Err(invalid());
    }
    let s = storage(pin)?;
    match bounded_file(pin, name, BOUND) {
        Ok(actual) if actual == bytes => {}
        Ok(_) => {
            return Err(changed(
                "Installer generation conflicts with existing custody",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            s.publish(
                OsStr::new(name),
                bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(|e| e.into_io_error())?;
        }
        Err(e) => return Err(e),
    }
    if bounded_file(pin, name, BOUND)? != bytes {
        return Err(invalid());
    }
    pin.validate_destination()
}
fn original(
    root: &Arc<PinnedDirectory>,
    expected: &crate::imager_model::PreparedSetupExpectation,
) -> io::Result<bool> {
    let receipt = bounded_file(root, "receipt.json", 1024);
    let file = crate::setup::nofollow::metadata_nofollow(
        &root.file,
        Path::new(crate::setup::media_setup::FILE),
    );
    match (receipt, file) {
        (Err(a), Err(b))
            if a.kind() == io::ErrorKind::NotFound && b.kind() == io::ErrorKind::NotFound =>
        {
            Ok(false)
        }
        (Ok(bytes), Ok(_)) => {
            if serde_json::from_slice::<crate::imager_model::PreparedSetupExpectation>(&bytes)?
                != *expected
            {
                return Err(invalid());
            }
            check_setup(root, expected)?;
            Ok(true)
        }
        _ => Err(changed(
            "Original pending installer custody is incomplete or changed",
        )),
    }
}
fn check_setup(
    pin: &Arc<PinnedDirectory>,
    expected: &crate::imager_model::PreparedSetupExpectation,
) -> io::Result<()> {
    if !expected.valid() {
        return Err(invalid());
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut f = cap_primitives::fs::open(
        &pin.file,
        Path::new(crate::setup::media_setup::FILE),
        &options,
    )?;
    let before = cap_primitives::fs::Metadata::from_file(&f)?;
    if !before.is_file()
        || before.uid() != 0
        || before.mode() & 0o077 != 0
        || before.nlink() != 1
        || before.len() != expected.sealed_bytes
    {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    let mut n = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let size = f.read(&mut buf)?;
        if size == 0 {
            break;
        }
        n = n
            .checked_add(size as u64)
            .filter(|n| *n <= expected.sealed_bytes)
            .ok_or_else(invalid)?;
        hash.update(&buf[..size]);
    }
    if n != expected.sealed_bytes
        || hex::encode(hash.finalize()) != expected.ciphertext_sha256
        || !crate::setup::migration::same_snapshot(
            &before,
            &cap_primitives::fs::Metadata::from_file(&f)?,
        )
        || !crate::setup::migration::same_snapshot(
            &before,
            &crate::setup::nofollow::metadata_nofollow(
                &pin.file,
                Path::new(crate::setup::media_setup::FILE),
            )?,
        )
    {
        return Err(invalid());
    }
    pin.validate_destination()
}
fn ensure_generation(root: &Arc<PinnedDirectory>, name: &str) -> io::Result<Arc<PinnedDirectory>> {
    let mut options = cap_primitives::fs::DirOptions::new();
    options.mode(0o700);
    match cap_primitives::fs::create_dir(&root.file, Path::new(name), &options) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    child(root, name)
}
pub(super) fn snapshot(file: &File) -> io::Result<[u64; 5]> {
    let m = file.metadata()?;
    let ns = |sec: i64, nsec: i64| -> io::Result<u64> {
        u64::try_from((sec as i128) * 1_000_000_000 + nsec as i128).map_err(|_| invalid())
    };
    Ok([
        m.dev(),
        m.ino(),
        m.len(),
        ns(m.mtime(), m.mtime_nsec())?,
        ns(m.ctime(), m.ctime_nsec())?,
    ])
}
pub(super) fn open_payload(parent: &File, payload: &Payload) -> io::Result<File> {
    payload.validate()?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = cap_primitives::fs::open(parent, Path::new(payload.filename()), &options)?;
    let m = file.metadata()?;
    if !m.is_file() || m.mode() & 0o077 != 0 || m.nlink() != 1 || m.len() != payload.length() {
        return Err(invalid());
    }
    Ok(file)
}
fn locator(member: &Member, record: &Record) -> io::Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &serde_json::json!({"schema":1,"binding":{"kind":"completed_generation","attempt":record.selection.attempt},"expectation":member.payload.expectation_json()?,"source_identity":member.snapshot,"prepared_setup":record.selection.completed_setup}),
    )?)
}
pub(super) fn adopt(review: &CompletedBootReview<'_>, cancel: &AtomicBool) -> io::Result<()> {
    review.validate(cancel)?;
    let root = root()?;
    let declaration = review
        .pending
        .authenticated_setup()
        .pending_original_declaration()?;
    let original_present = original(&root, &review.selected.pending_setup)?;
    let generation = format!(".completed-setup-{}", review.selected.attempt);
    let pin = ensure_generation(&root, &generation)?;
    let mut members = Vec::new();
    review.with_payloads(cancel, |home, selection| {
        if let Some(expected) = &selection.home {
            let payload = Payload::Home(expected.clone());
            let file = open_payload(&home.file, &payload)?;
            let member = Member {
                snapshot: snapshot(&file)?,
                payload,
            };
            super::stream::stream(&home.file, &member, &mut io::sink(), cancel)?;
            members.push(member);
        }
        if let Some(expected) = &review.selected.applications {
            let payload = Payload::Applications(expected.clone());
            let file = open_payload(&home.file, &payload)?;
            let member = Member {
                snapshot: snapshot(&file)?,
                payload,
            };
            super::stream::stream(&home.file, &member, &mut io::sink(), cancel)?;
            members.push(member);
        }
        Ok(())
    })?;
    if let Some(browser) = &review.capsule.authenticated_setup().manifest().browser {
        review.pending.with_payload(|inbox| {
            let payload = Payload::Browser(browser.payload.clone());
            let file = open_payload(inbox, &payload)?;
            let member = Member {
                snapshot: snapshot(&file)?,
                payload,
            };
            super::stream::stream(inbox, &member, &mut io::sink(), cancel)?;
            members.push(member);
            Ok(())
        })?;
    }
    let mut profile = serde_json::to_value(
        review
            .capsule
            .authenticated_setup()
            .manifest()
            .install_profile
            .as_ref()
            .ok_or_else(invalid)?,
    )?;
    profile.as_object_mut().ok_or_else(invalid)?.insert(
        "prepared_setup".into(),
        serde_json::to_value(&review.selected.completed_setup)?,
    );
    let profile = serde_json::to_vec(&profile)?;
    let record = Record {
        schema: review.selected.schema,
        generation,
        inode: pin.file.metadata()?.ino(),
        selection: review.selected.clone(),
        declaration: match &declaration {
            OriginalSelectionPlan::Home(p) => Some(p.clone()),
            _ => None,
        },
        applications_declaration: match &declaration {
            OriginalSelectionPlan::Applications(p) => Some(p.clone()),
            _ => None,
        },
        original_present,
        members,
        profile_sha256: Sha256::digest(&profile).into(),
    };
    let record_bytes = serde_json::to_vec(&record)?;
    if record_bytes.len() as u64 > BOUND {
        return Err(invalid());
    }
    let receipt_bytes = serde_json::to_vec(&record.selection.completed_setup)?;
    let mut pending_bytes = 0u64;
    let mut account = |parent: &Arc<PinnedDirectory>, name: &str, bytes: &[u8]| -> io::Result<()> {
        match bounded_file(parent, name, BOUND) {
            Ok(actual) if actual == bytes => Ok(()),
            Ok(_) => Err(changed(
                "Installer generation conflicts with existing custody",
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                pending_bytes = pending_bytes
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(invalid)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    };
    account(&pin, "receipt.json", &receipt_bytes)?;
    account(&pin, "profile.json", &profile)?;
    for member in &record.members {
        account(
            &pin,
            member.payload.locator_name(),
            &locator(member, &record)?,
        )?;
    }
    account(&root, SELECTOR, &record_bytes)?;
    let ciphertext_absent = match crate::setup::nofollow::metadata_nofollow(
        &pin.file,
        Path::new(crate::setup::media_setup::FILE),
    ) {
        Ok(_) => {
            check_setup(&pin, &record.selection.completed_setup)?;
            false
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };
    if ciphertext_absent {
        pending_bytes = pending_bytes
            .checked_add(record.selection.completed_setup.sealed_bytes)
            .ok_or_else(invalid)?;
    }
    if pending_bytes > 0
        && super::super::completed_setup_free_bytes(&pin.file)?
            < pending_bytes.checked_add(32 * 1024).ok_or_else(invalid)?
    {
        return Err(io::ErrorKind::StorageFull.into());
    }
    let destination = Path::new(ROOT)
        .join(&record.generation)
        .join(crate::setup::media_setup::FILE);
    match crate::setup::nofollow::metadata_nofollow(
        &pin.file,
        Path::new(crate::setup::media_setup::FILE),
    ) {
        Ok(_) => check_setup(&pin, &record.selection.completed_setup)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut input = std::fs::File::open(review.capsule.package_path())?;
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .mode(0o600)
                .follow(FollowSymlinks::No);
            let mut output = cap_primitives::fs::open(
                &pin.file,
                Path::new(crate::setup::media_setup::FILE),
                &options,
            )?;
            let result = (|| {
                let mut total = 0u64;
                let mut buffer = [0u8; 65536];
                loop {
                    check_cancel(cancel)?;
                    let size = input.read(&mut buffer)?;
                    if size == 0 {
                        break;
                    }
                    total = total
                        .checked_add(size as u64)
                        .filter(|n| *n <= record.selection.completed_setup.sealed_bytes)
                        .ok_or_else(invalid)?;
                    output.write_all(&buffer[..size])?;
                }
                output.sync_all()?;
                check_setup(&pin, &record.selection.completed_setup)
            })();
            if result.is_err() {
                drop(output);
                let _ = std::fs::remove_file(&destination);
            }
            result?;
        }
        Err(e) => return Err(e),
    }
    publish(
        &pin,
        "receipt.json",
        &serde_json::to_vec(&record.selection.completed_setup)?,
    )?;
    publish(&pin, "profile.json", &profile)?;
    for member in &record.members {
        publish(
            &pin,
            member.payload.locator_name(),
            &locator(member, &record)?,
        )?;
    }
    review.validate(cancel)?;
    if original(&root, &record.selection.pending_setup)? != record.original_present {
        return Err(invalid());
    }
    check_cancel(cancel)?;
    publish(&root, SELECTOR, &record_bytes)?;
    RunCustody::open()?.ok_or_else(invalid)?.validate()?;
    review.validate(cancel)
}
/// Constructor reads only the fixed root-owned namespace; it grants no access
/// from a caller-supplied receipt, locator, generation name or filesystem path.
pub(super) struct RunCustody {
    root: Arc<PinnedDirectory>,
    generation: Arc<PinnedDirectory>,
    pub(super) record: Record,
    bytes: Vec<u8>,
}
impl RunCustody {
    pub(super) fn open() -> io::Result<Option<Self>> {
        let root = root()?;
        let bytes = match bounded_file(&root, SELECTOR, BOUND) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let record: Record = serde_json::from_slice(&bytes)?;
        if !matches!(record.schema, 1 | 2)
            || record.generation != format!(".completed-setup-{}", record.selection.attempt)
            || record.inode == 0
            || record.members.is_empty()
            || record.members.len() > 3
        {
            return Err(invalid());
        }
        let generation = child(&root, &record.generation)?;
        let result = Self {
            root,
            generation,
            record,
            bytes,
        };
        result.validate()?;
        Ok(Some(result))
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.root.validate_destination()?;
        self.generation.validate_destination()?;
        if self.generation.file.metadata()?.ino() != self.record.inode
            || bounded_file(&self.root, SELECTOR, BOUND)? != self.bytes
            || original(&self.root, &self.record.selection.pending_setup)?
                != self.record.original_present
        {
            return Err(invalid());
        }
        let actual: crate::imager_model::PreparedSetupExpectation =
            serde_json::from_slice(&bounded_file(&self.generation, "receipt.json", 1024)?)?;
        if actual != self.record.selection.completed_setup
            || <[u8; 32]>::from(Sha256::digest(bounded_file(
                &self.generation,
                "profile.json",
                BOUND,
            )?)) != self.record.profile_sha256
        {
            return Err(invalid());
        }
        check_setup(&self.generation, &actual)?;
        let declaration = self.record.declaration()?;
        if self.record.selection.operation != declaration.source().operation_id
            || self.record.selection.medium != *declaration.media()
            || self.record.selection.declaration_sha256 != declaration.canonical_digest()?
            || self.record.selection.schema != self.record.schema
            || self
                .record
                .selection
                .home
                .as_ref()
                .is_some_and(|home| home.setup_binding != declaration.setup_binding())
            || self
                .record
                .selection
                .applications
                .as_ref()
                .is_some_and(|apps| apps.setup_binding != declaration.setup_binding())
            || !uuid::Uuid::parse_str(&self.record.selection.attempt)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.record.selection.attempt)
        {
            return Err(invalid());
        }
        let mut seen = std::collections::BTreeSet::new();
        for member in &self.record.members {
            member.payload.validate()?;
            match &member.payload {
                Payload::Home(e) if Some(e) != self.record.selection.home.as_ref() => {
                    return Err(invalid());
                }
                Payload::Applications(e)
                    if self.record.selection.applications.as_ref() != Some(e) =>
                {
                    return Err(invalid());
                }
                Payload::Browser(e) if e.setup_binding != declaration.setup_binding() => {
                    return Err(invalid());
                }
                _ => {}
            }
            if !seen.insert(member.payload.kind())
                || bounded_file(&self.generation, member.payload.locator_name(), BOUND)?
                    != locator(member, &self.record)?
            {
                return Err(invalid());
            }
        }
        if seen.contains("home") != self.record.selection.home.is_some()
            || seen.contains("applications") != self.record.selection.applications.is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn path(&self) -> PathBuf {
        Path::new(ROOT).join(&self.record.generation)
    }
}
pub fn selected_generation() -> io::Result<PathBuf> {
    match RunCustody::open()? {
        Some(c) => {
            c.validate()?;
            Ok(c.path())
        }
        None => Ok(PathBuf::from(ROOT)),
    }
}

//! Retained-source credential purpose. Serialized declarations are requests;
//! only root-owned immutable source records and an owned RW lease confer authority.
use super::super::OriginalCallerFacts;
use super::*;
#[cfg(all(test, feature = "native-capture-qualification"))]
use crate::setup::prepared_setup::PreparedSetupReview;
use crate::setup::{
    home_capture::transport::{HostChannel, RetainedHomeInput},
    migration_handoff::{HomePolicy, Plan, RetainedSource},
    prepared_setup::{AuthenticatedInstalledRetained, Manifest},
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
};

// Fits the existing manifest bound without truncating kernel group facts. A
// larger caller fails registration explicitly rather than silently losing groups.
const DECLARATION_LIMIT: usize = 12 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedCallerDeclaration {
    schema: u32,
    operation: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
    plan_sha256: [u8; 32],
    namespace: NativePath,
    original: OriginalCallerFacts,
    record_sha256: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_home_alias: Option<launcher::retained_view::OriginalHomeAlias>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedRecord {
    schema: u32,
    purpose: String,
    plan: Plan,
    setup_binding: String,
    identity_public_key: [u8; 32],
    namespace: NativePath,
    original: OriginalCallerFacts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_home_alias: Option<launcher::retained_view::OriginalHomeAlias>,
}
fn alias_schema(
    schema: u32,
    alias: Option<&launcher::retained_view::OriginalHomeAlias>,
    plan: &Plan,
) -> io::Result<()> {
    match (schema, alias) {
        (1, None) => Ok(()),
        (2, Some(alias)) => {
            alias.validate()?;
            if alias.source_ino() != plan.home_inode() {
                return Err(failure("Retained original Home alias inode differs"));
            }
            Ok(())
        }
        _ => Err(failure("Retained original Home alias schema differs")),
    }
}
fn selected(plan: &Plan) -> io::Result<()> {
    plan.bytes()?;
    if !plan.categories.home || plan.home_policy != HomePolicy::AccessReadWrite {
        return Err(failure(
            "Source groups require explicitly retained writable Home",
        ));
    }
    operation(&plan.operation_id)
}
fn binding(setup: &str, person: &[u8; 32]) -> io::Result<()> {
    if setup.is_empty()
        || setup.len() > 256
        || setup.chars().any(char::is_control)
        || *person == [0; 32]
    {
        return Err(failure("Retained setup/person binding is invalid"));
    }
    Ok(())
}
fn namespace(value: &NativePath) -> io::Result<PathBuf> {
    let path = value.decode().map_err(io::Error::other)?;
    crate::setup::migration_handoff::relative(&path)?;
    if !path.ends_with("tilo-imager/home-capture/records") {
        return Err(failure("Retained record namespace is not fixed"));
    }
    Ok(path)
}
impl RetainedRecord {
    fn bytes(&self) -> io::Result<Vec<u8>> {
        selected(&self.plan)?;
        binding(&self.setup_binding, &self.identity_public_key)?;
        namespace(&self.namespace)?;
        self.original.validate(self.plan.home_uid())?;
        alias_schema(self.schema, self.original_home_alias.as_ref(), &self.plan)?;
        if !matches!(self.schema, 1 | 2) || self.purpose != "retained-source-groups" {
            return Err(failure("Retained record purpose differs"));
        }
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(failure("Retained record exceeds protected bound"));
        }
        Ok(bytes)
    }
    fn declaration(&self) -> io::Result<RetainedCallerDeclaration> {
        let value = RetainedCallerDeclaration {
            schema: self.schema,
            original_home_alias: self.original_home_alias.clone(),
            operation: self.plan.operation_id.clone(),
            setup_binding: self.setup_binding.clone(),
            identity_public_key: self.identity_public_key,
            plan_sha256: Sha256::digest(self.plan.bytes()?).into(),
            namespace: self.namespace.clone(),
            original: self.original.clone(),
            record_sha256: Sha256::digest(self.bytes()?).into(),
        };
        value.validate_binding(&self.plan, &self.setup_binding, &self.identity_public_key)?;
        Ok(value)
    }
}
impl RetainedCallerDeclaration {
    /// Authenticated producer facts for declaration equality only. This accessor
    /// neither registers groups nor grants source access.
    pub fn requested_caller(&self) -> &OriginalCallerFacts {
        &self.original
    }
    pub fn operation(&self) -> &str {
        &self.operation
    }
    /// Shape and encrypted-envelope binding only, never grants group authority.
    pub fn validate_binding(&self, plan: &Plan, setup: &str, person: &[u8; 32]) -> io::Result<()> {
        selected(plan)?;
        binding(setup, person)?;
        namespace(&self.namespace)?;
        self.original.validate(plan.home_uid())?;
        alias_schema(self.schema, self.original_home_alias.as_ref(), plan)?;
        if !matches!(self.schema, 1 | 2)
            || self.operation != plan.operation_id
            || self.setup_binding != setup
            || self.identity_public_key != *person
            || self.plan_sha256 != <[u8; 32]>::from(Sha256::digest(plan.bytes()?))
            || self.record_sha256 == [0; 32]
            || serde_json::to_vec(self)?.len() > DECLARATION_LIMIT
        {
            return Err(failure(
                "Retained declaration differs from authenticated setup",
            ));
        }
        Ok(())
    }
}
impl Store {
    /// Finite original-OS root producer. The only source of caller groups is
    /// the existing mutually authenticated kernel peer, never request fields.
    pub(in crate::setup::home_capture) fn register_retained_caller(
        &self,
        selection: (&Path, &RetainedHomeInput),
        plan: &Plan,
        peer: &HostChannel,
        setup: &str,
        person: &[u8; 32],
    ) -> io::Result<RetainedCallerDeclaration> {
        if rustix::process::geteuid().as_raw() != 0 || self.operation != plan.operation_id {
            return Err(failure(
                "Retained registration requires root operation custody",
            ));
        }
        selected(plan)?;
        binding(setup, person)?;
        let (home, input) = selection;
        let pin = PinnedDirectory::open_absolute(home)?;
        input.validate(peer, &pin.file)?;
        let original = peer.original_caller_facts().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("original session credentials: {error}"),
            )
        })?;
        original.validate(plan.home_uid())?;
        let records = self
            .records_path
            .as_ref()
            .ok_or_else(|| failure("Retained registration lacks system record namespace"))?;
        let relative = crate::setup::migration_handoff::original_record_namespace(
            plan,
            home,
            records,
            &self.records,
        )?;
        let record = RetainedRecord {
            schema: 2,
            original_home_alias: Some(
                launcher::retained_view::OriginalHomeAlias::capture(home, &pin.file).map_err(
                    |error| io::Error::new(error.kind(), format!("original Home alias: {error}")),
                )?,
            ),
            purpose: "retained-source-groups".into(),
            plan: plan.clone(),
            setup_binding: setup.into(),
            identity_public_key: *person,
            namespace: NativePath::encode(&relative),
            original,
        };
        let declaration = record.declaration()?;
        let bytes = record.bytes()?;
        let held = storage(&self.records)?;
        held.validate_privacy(Privacy::OwnerOnly)?;
        if peer.original_caller_facts()? != record.original {
            return Err(failure("Retained caller changed before publication"));
        }
        let name = format!("{}.retained-caller.json", self.operation);
        match held.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
            Ok(file) => {
                read_record(file, &bytes)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                held.publish(
                    OsStr::new(&name),
                    &bytes,
                    PublishMode::IfAbsent,
                    Privacy::OwnerOnly,
                )
                .map_err(io::Error::other)?;
            }
            Err(error) => return Err(error),
        }
        // Reopen the exact named inode and reattest source/kernel facts before ACK.
        read_record(
            held.open_read(OsStr::new(&name), Privacy::OwnerOnly)?,
            &bytes,
        )?;
        if crate::setup::migration_handoff::original_record_namespace(
            plan,
            home,
            records,
            &self.records,
        )? != relative
            || peer.original_caller_facts()? != record.original
        {
            return Err(failure(
                "Retained source or original peer changed before ACK",
            ));
        }
        pin.validate()?;
        input.validate(peer, &pin.file)?;
        if record.original_home_alias.as_ref()
            != Some(&launcher::retained_view::OriginalHomeAlias::capture(
                home, &pin.file,
            )?)
        {
            return Err(failure(
                "Retained original absolute Home changed before ACK",
            ));
        }
        Ok(declaration)
    }
}
fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.uid(),
        meta.mode(),
        meta.nlink(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn read_record(mut file: File, expected: &[u8]) -> io::Result<File> {
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != 0
        || before.nlink() != 1
        || before.mode() & 0o077 != 0
        || before.len() > MAX_RECORD
    {
        return Err(failure(
            "Retained record is not bounded private root custody",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)?;
    if bytes != expected || stamp(&before) != stamp(&file.metadata()?) {
        return Err(failure("Retained protected record changed or conflicts"));
    }
    let parsed: RetainedRecord = serde_json::from_slice(&bytes)?;
    if parsed.bytes()? != bytes {
        return Err(failure("Retained record is not canonical"));
    }
    Ok(file)
}
/// Not deserializable and no public constructor. Minted only after the exact
/// protected source record matches the authenticated prepared declaration.
/// It permits the root controller to hold a RW mount; it NEVER changes IDs.
pub struct RootRetainedPermit {
    plan: Plan,
}
impl RootRetainedPermit {
    pub(in crate::setup) fn validate_plan(&self, plan: &Plan) -> io::Result<()> {
        if rustix::process::geteuid().as_raw() != 0 || self.plan != *plan {
            return Err(failure(
                "Root retained permit differs from exact admitted Plan",
            ));
        }
        selected(plan)
    }
}
/// Root-only nonserializable source authority. No argv/JSON/CRDT can construct
/// this witness. Increment B must retain this Arc until actual app quiescence;
/// a cloned directory descriptor alone is explicitly insufficient.
static UNKNOWN_SOURCE_RETIREMENT: AtomicBool = AtomicBool::new(false);
fn retire_physical_source(source: RetainedSource) -> io::Result<()> {
    let result = source.retire();
    if result.is_err() {
        UNKNOWN_SOURCE_RETIREMENT.store(true, std::sync::atomic::Ordering::Release);
    }
    result
}

struct RetainedMountedEpoch {
    source: Option<RetainedSource>,
    retirement: Arc<std::sync::Mutex<bool>>,
    manifest: Manifest,
    declaration: RetainedCallerDeclaration,
    record: File,
    bytes: Vec<u8>,
}
impl RetainedMountedEpoch {
    fn source(&self) -> io::Result<&RetainedSource> {
        self.source
            .as_ref()
            .ok_or_else(|| failure("Retained physical epoch retired"))
    }
    fn revalidate(&self) -> io::Result<()> {
        let source = self.source()?;
        source.validate()?;
        let named = source.open_retained_caller(&namespace(&self.declaration.namespace)?)?;
        if stamp(&named.metadata()?) != stamp(&self.record.metadata()?) {
            return Err(failure("Retained protected record inode was replaced"));
        }
        read_record(named, &self.bytes)?;
        source.validate()
    }
    /// Retirement releases this actor's epoch reference. Only the last actor
    /// obtains physical ownership and can positively observe the owned unmount.
    fn retire(self: Arc<Self>) -> io::Result<()> {
        // Keep the state lock independent of physical ownership so last Arc
        // consumption and abandoned actor Drop serialize without a race.
        let state = Arc::clone(&self.retirement);
        let orphaned = state.lock().unwrap_or_else(|error| error.into_inner());
        if *orphaned {
            retain_orphaned_epoch(&self);
            return Err(failure("Retained source has abandoned actor custody"));
        }
        let Some(mut final_owner) = Arc::into_inner(self) else {
            return Ok(());
        };
        let source = final_owner
            .source
            .take()
            .ok_or_else(|| failure("Retained epoch retired"))?;
        drop(final_owner); // Close protected record custody before unmount.
        drop(orphaned);
        retire_physical_source(source)
    }
}
fn retain_orphaned_epoch(epoch: &Arc<RetainedMountedEpoch>) {
    UNKNOWN_SOURCE_RETIREMENT.store(true, std::sync::atomic::Ordering::Release);
    static UNKNOWN: std::sync::Mutex<Vec<Arc<RetainedMountedEpoch>>> =
        std::sync::Mutex::new(Vec::new());
    let held_epoch = Arc::clone(epoch);
    match UNKNOWN.lock() {
        Ok(mut held) if held.len() < 64 => held.push(held_epoch),
        _ => std::mem::forget(held_epoch),
    }
}
impl Drop for RetainedMountedEpoch {
    fn drop(&mut self) {
        let Some(source) = self.source.take() else {
            return;
        };
        UNKNOWN_SOURCE_RETIREMENT.store(true, std::sync::atomic::Ordering::Release);
        // Unexpected last-owner loss is not an observed retirement. Preserve
        // exact mount/service/Home custody instead of implicit unmount in Drop.
        static UNKNOWN: std::sync::Mutex<Vec<RetainedSource>> = std::sync::Mutex::new(Vec::new());
        match UNKNOWN.lock() {
            Ok(mut held) if held.len() < 64 => held.push(source),
            _ => std::mem::forget(source),
        }
    }
}

pub struct RetainedSourceAuthority {
    epoch: Arc<RetainedMountedEpoch>,
    retired: bool,
    cancelled: AtomicBool,
    installed: Option<Arc<AuthenticatedInstalledRetained>>,
}
impl Drop for RetainedSourceAuthority {
    fn drop(&mut self) {
        if !self.retired {
            let mut orphaned = self
                .epoch
                .retirement
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *orphaned = true;
            retain_orphaned_epoch(&self.epoch);
        }
    }
}
impl RetainedSourceAuthority {
    /// Terminal process-owned custody observation, never access authority.
    /// Constructors can fail before returning an actor; the root supervisor
    /// must still remain alive while exact quarantined source custody is unknown.
    pub fn unknown_retirement() -> bool {
        UNKNOWN_SOURCE_RETIREMENT.load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup) fn review(
        setup: &PreparedSetupReview,
        active_person: &[u8; 32],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_authenticated(
            setup.manifest(),
            &|| setup.validate(),
            None,
            active_person,
            cancel,
        )
    }
    /// Only the installed root actor may use this continuation. Its active
    /// person still comes from the separately authenticated native session.
    pub(in crate::setup) fn review_installed(
        installed: Arc<AuthenticatedInstalledRetained>,
        active_person: &[u8; 32],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_authenticated(
            installed.manifest(),
            &|| installed.revalidate(),
            Some(Arc::clone(&installed)),
            active_person,
            cancel,
        )
    }
    fn review_authenticated(
        manifest: &Manifest,
        validate: &dyn Fn() -> io::Result<()>,
        installed: Option<Arc<AuthenticatedInstalledRetained>>,
        active_person: &[u8; 32],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        crate::setup::migration::check_cancel(cancel)?;
        validate()?;
        if manifest.identity_public_key != *active_person {
            return Err(failure(
                "Retained person differs from active native identity",
            ));
        }
        let original = manifest
            .retained_home
            .as_ref()
            .ok_or_else(|| failure("Authenticated retained Home is absent"))?;
        let declaration = manifest
            .retained_caller
            .as_ref()
            .ok_or_else(|| failure("Legacy retained Home has no source-group witness"))?;
        declaration.validate_binding(original.as_plan(), &manifest.setup_binding, active_person)?;
        let effective = manifest
            .planned_btrfs_resize
            .as_ref()
            .map(|resize| resize.effective_plan(original))
            .transpose()?
            .unwrap_or_else(|| original.clone());
        // Read authenticated control data through an actual RO lease first;
        // no root writable permit exists until this proof has been checked.
        let readonly =
            crate::setup::migration_handoff::mount_authenticated_read_only(effective.as_plan())?;
        if let Some(resize) = &manifest.planned_btrfs_resize {
            let verified = readonly.verify_completed_btrfs_resize(resize.filesystem_bytes());
            if let Err(error) = verified {
                readonly.retire()?;
                return Err(error);
            }
        }
        let record = readonly.open_retained_record_readonly(&namespace(&declaration.namespace)?)?;
        let mut bytes = Vec::new();
        (&record).take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
        let parsed: RetainedRecord = serde_json::from_slice(&bytes)?;
        if parsed.plan != *original.as_plan() || parsed.declaration()? != *declaration {
            return Err(failure(
                "Encrypted retained groups differ from protected source record",
            ));
        }
        let record = read_record(record, &bytes)?;
        let record_stamp = stamp(&record.metadata()?);
        drop(record);
        readonly.retire()?;
        validate()?;
        let permit = Arc::new(RootRetainedPermit {
            plan: effective.as_plan().clone(),
        });
        let source =
            crate::setup::migration_handoff::retain_scoped_source(effective.as_plan(), permit)?;
        let record = match (|| {
            let record = source.open_retained_caller(&namespace(&declaration.namespace)?)?;
            if stamp(&record.metadata()?) != record_stamp {
                return Err(failure(
                    "Retained record changed between RO proof and RW lease",
                ));
            }
            let record = read_record(record, &bytes)?;
            validate()?;
            crate::setup::migration::check_cancel(cancel)?;
            Ok(record)
        })() {
            Ok(record) => record,
            Err(error) => {
                retire_physical_source(source)?;
                return Err(error);
            }
        };
        let value = Arc::new(Self {
            epoch: Arc::new(RetainedMountedEpoch {
                source: Some(source),
                retirement: Arc::new(std::sync::Mutex::new(false)),
                manifest: manifest.clone(),
                declaration: declaration.clone(),
                record,
                bytes,
            }),
            retired: false,
            cancelled: AtomicBool::new(false),
            installed,
        });
        if let Err(error) = value.revalidate(cancel) {
            value.retire()?;
            return Err(error);
        }
        Ok(value)
    }
    pub fn retire(self: Arc<Self>) -> io::Result<()> {
        let mut owned = Arc::try_unwrap(self)
            .map_err(|_| failure("Retained source still has native projection custody"))?;
        let epoch = Arc::clone(&owned.epoch);
        owned.retired = true;
        drop(owned); // Release only this actor's installed record/latch.
        epoch.retire()
    }
    pub fn revoke(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        let result = self.revalidate_current(cancel);
        if result.is_err() {
            self.revoke();
        }
        result
    }
    fn revalidate_current(&self, cancel: &AtomicBool) -> io::Result<()> {
        crate::setup::migration::check_cancel(cancel)?;
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if let Some(installed) = &self.installed {
            installed.revalidate()?;
        }
        self.epoch.revalidate()?;
        if let Some(installed) = &self.installed {
            installed.revalidate()?;
        }
        crate::setup::migration::check_cancel(cancel)
    }
    /// Narrow handoff for the upcoming canonical source-only native launcher.
    /// The callback MUST attach this exact Arc to process custody; this seam
    /// deliberately does not launch, mint an app grant or advertise support.
    pub fn projection(
        self: &Arc<Self>,
        cancel: &AtomicBool,
    ) -> io::Result<RetainedSourceProjection> {
        self.revalidate(cancel)?;
        let file = self.epoch.source()?.retained_directory()?;
        self.revalidate(cancel)?;
        Ok(RetainedSourceProjection {
            directory: file,
            custody: self.clone(),
        })
    }
    pub fn native_owner_sha256(&self, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
        self.revalidate(cancel)?;
        // These canonical bytes came from the independently opened private root
        // record, not the serialized caller declaration or transient launch PID.
        let owner = Sha256::digest(&self.epoch.bytes).into();
        self.revalidate(cancel)?;
        Ok(owner)
    }
    pub fn removal_namespace(&self, cancel: &AtomicBool) -> io::Result<File> {
        self.revalidate(cancel)?;
        let directory = self
            .epoch
            .source()?
            .retained_control_directory(&namespace(&self.epoch.declaration.namespace)?)?;
        self.revalidate(cancel)?;
        Ok(directory)
    }
    pub fn original(&self) -> &OriginalCallerFacts {
        &self.epoch.declaration.original
    }
    pub fn identity_public_key(&self) -> &[u8; 32] {
        &self.epoch.declaration.identity_public_key
    }
}

/// Native-only inseparable source descriptor/custody handoff for increment B.
/// The launcher must retain this object until its namespace is actually stopped.
pub struct RetainedSourceProjection {
    directory: File,
    custody: Arc<RetainedSourceAuthority>,
}
impl RetainedSourceProjection {
    /// Stable authenticated source owner, never an editor/process identity.
    pub fn native_owner_sha256(&self, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
        self.revalidate(cancel)?;
        let owner = self.custody.native_owner_sha256(cancel)?;
        self.revalidate(cancel)?;
        Ok(owner)
    }
    pub fn directory(&self) -> &File {
        &self.directory
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.custody.revalidate(cancel)
    }
    /// Source custody only; caller must separately authenticate the new actor.
    /// No pathname, manifest DTO or directory FD can construct this derivative.
    pub fn derive_installed(
        &self,
        installed: Arc<AuthenticatedInstalledRetained>,
        active_person: &[u8; 32],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<RetainedSourceAuthority>> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        crate::setup::migration::check_cancel(cancel)?;
        self.revalidate(&AtomicBool::new(false))?;
        crate::setup::migration::check_cancel(cancel)?;
        installed.revalidate()?;
        let previous = self
            .custody
            .installed
            .as_ref()
            .ok_or_else(|| failure("Editor requires actual installed source custody"))?;
        previous.revalidate()?;
        let epoch = &self.custody.epoch;
        if *epoch
            .retirement
            .lock()
            .map_err(|_| failure("Retained epoch custody poisoned"))?
            || installed.manifest() != previous.manifest()
            || installed.manifest() != &epoch.manifest
            || active_person != self.identity_public_key()
        {
            return Err(failure("Fresh editor installed source binding differs"));
        }
        let observed = self.directory.metadata()?;
        let fresh = epoch.source()?.retained_directory()?;
        let fresh = fresh.metadata()?;
        if (observed.dev(), observed.ino()) != (fresh.dev(), fresh.ino()) {
            return Err(failure("Derived source projection identity differs"));
        }
        let derived = Arc::new(RetainedSourceAuthority {
            epoch: Arc::clone(epoch),
            retired: false,
            cancelled: AtomicBool::new(false),
            installed: Some(installed),
        });
        if let Err(error) = derived
            .revalidate(cancel)
            .and_then(|()| self.revalidate(&AtomicBool::new(false)))
            .and_then(|()| crate::setup::migration::check_cancel(cancel))
            .and_then(|()| {
                if *epoch
                    .retirement
                    .lock()
                    .map_err(|_| failure("Retained epoch custody poisoned"))?
                {
                    Err(failure("Retained source has abandoned actor custody"))
                } else {
                    Ok(())
                }
            })
        {
            derived.retire()?;
            return Err(error);
        }
        Ok(derived)
    }
    pub fn original(&self) -> &OriginalCallerFacts {
        self.custody.original()
    }
    pub fn identity_public_key(&self) -> &[u8; 32] {
        self.custody.identity_public_key()
    }
    pub fn original_home_alias(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<&launcher::retained_view::OriginalHomeAlias> {
        self.revalidate(cancel)?;
        self.custody
            .epoch
            .declaration
            .original_home_alias
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Legacy retained Home lacks original absolute path authority",
                )
            })
    }
}

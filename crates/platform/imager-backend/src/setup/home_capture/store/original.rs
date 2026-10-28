//! Original-volume-only intent variant in the existing protected Store.
//! No snapshot, receiver identity or portable group authority is synthesized.
use super::super::{original_caller::OriginalCallerFacts, transport::HostChannel};
use super::*;
use crate::setup::migration_handoff::{HomePolicy, Plan, ResolvedSource};
mod completed_geometry;
mod inventory;
pub use completed_geometry::CompletedOriginalGeometry;
pub(in crate::setup::home_capture) mod mounted;
use inventory::InventoryCustody;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIntent {
    schema: u32,
    kind: String,
    home: Plan,
    namespace: NativePath,
    original: OriginalCallerFacts,
    omissions: Vec<crate::setup::migration_review::OriginalOmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inventory: Option<InventoryCustody>,
}
/// Returned only over the existing mutually admitted private helper channel.
/// A serialized witness alone grants no source access; the receiver reopens
/// its protected same-source record and matches this exact digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWitness {
    schema: u32,
    pub operation: String,
    pub namespace: NativePath,
    record_sha256: [u8; 32],
    plan_sha256: [u8; 32],
}
impl SourceWitness {
    pub fn validate_plan(&self, plan: &Plan) -> io::Result<()> {
        use sha2::Digest;
        let digest: [u8; 32] = sha2::Sha256::digest(plan.bytes()?).into();
        let namespace = self.namespace.decode().map_err(io::Error::other)?;
        crate::setup::migration_handoff::relative(&namespace)?;
        if self.schema != 1
            || self.operation != plan.operation_id
            || self.plan_sha256 != digest
            || !namespace.ends_with("tilo-imager/home-capture/records")
        {
            return Err(failure(
                "Original registration witness differs from selected Plan",
            ));
        }
        Ok(())
    }
}
impl SourceIntent {
    pub(in crate::setup::home_capture) fn original_caller(&self) -> &OriginalCallerFacts {
        &self.original
    }
    pub(in crate::setup::home_capture) fn witness(&self) -> io::Result<SourceWitness> {
        use sha2::Digest;
        self.validate()?;
        Ok(SourceWitness {
            schema: 1,
            operation: self.home.operation_id.clone(),
            namespace: self.namespace.clone(),
            record_sha256: sha2::Sha256::digest(serde_json::to_vec(self)?).into(),
            plan_sha256: sha2::Sha256::digest(self.home.bytes()?).into(),
        })
    }
}
impl SourceIntent {
    fn validate(&self) -> io::Result<()> {
        self.home.validate()?;
        let home = self.kind == "offline-original-source" && self.home.categories.home;
        let applications = self.kind == "offline-original-applications"
            && !self.home.categories.home
            && self.home.categories.flatpak
            && self.home.flatpak_inventory.is_some();
        if self.schema != 3 || !(home || applications) || self.home.home_policy != HomePolicy::Copy
        {
            return Err(failure("Invalid original-source intent variant"));
        }
        let namespace = self.namespace.decode().map_err(io::Error::other)?;
        crate::setup::migration_handoff::relative(&namespace)?;
        if !namespace.ends_with("tilo-imager/home-capture/records") {
            return Err(failure("Original source record namespace is not fixed"));
        }
        crate::setup::migration_review::OriginalOmission::validate_shapes(&self.omissions)?;
        InventoryCustody::validate_selection(&self.home, self.inventory.as_ref())?;
        self.original.validate(self.home.home_uid())
    }
}

impl Store {
    /// Root service only: facts are freshly observed from its authenticated
    /// channel, never deserialized request facts. Unsupported same-volume
    /// topology refuses registration; existing online capture is unaffected.
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup::home_capture) fn register_original_selection(
        &self,
        home: &Path,
        plan: &Plan,
        peer: &HostChannel,
    ) -> io::Result<SourceIntent> {
        self.register_original_selection_with_preparation(home, plan, peer, None)
    }
    pub(in crate::setup::home_capture) fn register_original_selection_with_preparation(
        &self,
        home: &Path,
        plan: &Plan,
        peer: &HostChannel,
        preparation: Option<&crate::setup::migration_review::OriginalPreparation>,
    ) -> io::Result<SourceIntent> {
        self.register_original_selection_with_inventory(home, plan, peer, preparation, None)
    }
    pub(in crate::setup::home_capture) fn register_original_selection_with_inventory(
        &self,
        home: &Path,
        plan: &Plan,
        peer: &HostChannel,
        preparation: Option<&crate::setup::migration_review::OriginalPreparation>,
        inventory: Option<&File>,
    ) -> io::Result<SourceIntent> {
        if plan.operation_id != self.operation {
            return Err(failure("Original operation does not hold this Store lease"));
        }
        let original = peer.original_caller_facts()?;
        original.validate(plan.home_uid())?;
        let records = self
            .records_path
            .as_ref()
            .ok_or_else(|| failure("System record namespace was not admitted"))?;
        let namespace = crate::setup::migration_handoff::original_record_namespace(
            plan,
            home,
            records,
            &self.records,
        )?;
        let mut omissions = Vec::new();
        if let Some(preparation) = preparation {
            omissions.push(preparation.validate_original(home, original.uid())?);
        }
        if let Some(control) = crate::setup::migration_review::OriginalOmission::source_control(
            home,
            records,
            &self.records.file,
        )? {
            omissions.push(control);
        }
        let source = crate::setup::migration::PinnedDirectory::open_absolute(home)?;
        crate::setup::migration_review::OriginalOmission::validate_set(
            &omissions,
            &source.file,
            original.uid(),
        )?;
        let inventory = InventoryCustody::register(&storage(&self.records)?, plan, inventory)?;
        let intent = SourceIntent {
            schema: 3,
            kind: if plan.categories.home {
                "offline-original-source"
            } else {
                "offline-original-applications"
            }
            .into(),
            home: plan.clone(),
            namespace: NativePath::encode(&namespace),
            original,
            omissions,
            inventory,
        };
        intent.validate()?;
        // Bracket the filesystem review with a second actual peer proof.
        if peer.original_caller_facts()? != intent.original {
            return Err(failure("Original peer changed during source registration"));
        }
        let bytes = serde_json::to_vec(&intent)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(failure(
                "Original source intent exceeds protected record bound",
            ));
        }
        let held = storage(&self.records)?;
        held.validate_privacy(Privacy::OwnerOnly)?;
        let name = format!("{}.json", self.operation);
        if !matching_original_record(&held, OsStr::new(&name), &bytes)? {
            held.publish(
                OsStr::new(&name),
                &bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(io::Error::other)?;
        }
        self.records.validate_destination()?;
        let renewed = crate::setup::migration_handoff::original_record_namespace(
            plan,
            home,
            records,
            &self.records,
        )?;
        if NativePath::encode(&renewed) != intent.namespace
            || peer.original_caller_facts()? != intent.original
        {
            return Err(failure(
                "Original selection or peer changed before acknowledgement",
            ));
        }
        crate::setup::migration_review::OriginalOmission::validate_set(
            &intent.omissions,
            &source.file,
            intent.original.uid(),
        )?;
        source.validate()?;
        Ok(intent)
    }
}

/// Existing operation lease excludes another registrar for this operation.
/// Lost acknowledgements may reuse only the exact original variant, bytes and
/// private root-owned named inode; incompatible records are never repaired.
fn matching_original_record(
    directory: &StorageDirectory,
    name: &OsStr,
    expected: &[u8],
) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let mut existing = match directory.open_read(name, Privacy::OwnerOnly) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let stamp = |metadata: &std::fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.uid(),
            metadata.mode(),
            metadata.nlink(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    let before = existing.metadata()?;
    if before.uid() != 0 || before.nlink() != 1 || before.len() > MAX_RECORD {
        return Err(failure(
            "Existing original record is not bounded private root custody",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut existing)
        .take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)?;
    let named = directory.open_read(name, Privacy::OwnerOnly)?;
    if stamp(&before) != stamp(&existing.metadata()?)
        || stamp(&before) != stamp(&named.metadata()?)
        || bytes != expected
    {
        return Err(failure("Existing original record conflicts or changed"));
    }
    match serde_json::from_slice::<DurableRecord>(&bytes)? {
        DurableRecord::Original(intent) => intent.validate()?,
        DurableRecord::Capture(_) => {
            return Err(failure(
                "Snapshot record cannot authorize original registration",
            ));
        }
    }
    Ok(true)
}

/// Keeps the exact original read-only mount lease. This is not a certificate
/// and cannot grant receiver-wide UID/groups or access to another filesystem.
pub struct SourceReadContext {
    intent: SourceIntent,
    geometry: Option<Arc<CompletedOriginalGeometry>>,
    record: File,
    inventory: Option<File>,
    // Close protected descriptors before the source's legacy teardown.
    source: ResolvedSource,
}
/// Shared consuming teardown primitive. Descriptor arguments are closed only;
/// they confer no source authority. The typed resolver retains owned mount rights.
pub fn retire_original_source(
    source: ResolvedSource,
    record: File,
    inventory: Option<File>,
) -> io::Result<()> {
    drop((record, inventory));
    source.retire()
}
impl SourceReadContext {
    /// Consume protected source descriptors before a root-controller combined
    /// Home/application mount retirement batch. No new mount rights are minted.
    pub fn into_retirement_source(self) -> ResolvedSource {
        let Self {
            source,
            record,
            inventory,
            intent,
            geometry,
        } = self;
        drop((record, inventory, intent, geometry));
        source
    }
    /// Consumes this exact source custody; protected descriptors close first.
    pub fn retire(self) -> io::Result<()> {
        let Self {
            source,
            record,
            inventory,
            intent,
            geometry,
        } = self;
        drop((intent, geometry));
        retire_original_source(source, record, inventory)
    }
    /// The installed owner supplies the only completed-geometry constructor.
    pub(in crate::setup) fn review_installed_copy(
        installed: &Arc<crate::setup::prepared_setup::AuthenticatedInstalledOriginalCopy>,
    ) -> io::Result<Self> {
        installed.revalidate()?;
        let declaration = installed
            .manifest()
            .installed_original_copy_declaration()?
            .ok_or_else(|| failure("Installed original copy is absent"))?;
        let geometry = CompletedOriginalGeometry::admit(installed)?;
        let physical = geometry
            .as_ref()
            .map_or(declaration.source(), |g| g.effective());
        let source = crate::setup::migration_handoff::mount_authenticated_read_only(physical)?;
        let reviewed = (|| {
            if let Some(geometry) = &geometry {
                geometry.check(&source)?;
            }
            let context = Self::review_with_geometry(
                source.clone(),
                &declaration
                    .source_witness()
                    .namespace
                    .decode()
                    .map_err(io::Error::other)?,
                geometry,
            )?;
            if context.intent.witness()? != *declaration.source_witness() {
                return Err(failure(
                    "Installed original witness differs from protected source",
                ));
            }
            installed.revalidate()?;
            context.revalidate()?;
            Ok(context)
        })();
        match reviewed {
            Ok(context) => {
                drop(source);
                Ok(context)
            }
            Err(error) => {
                source.retire()?;
                Err(error)
            }
        }
    }
    pub fn reopen_witness(&self, witness: &SourceWitness) -> io::Result<Self> {
        self.revalidate()?;
        let context = Self::review_with_geometry(
            self.source.clone(),
            &witness.namespace.decode().map_err(io::Error::other)?,
            self.geometry.clone(),
        )?;
        if witness.schema != 1 || context.intent.witness()? != *witness {
            return Err(failure("Reopened original witness differs"));
        }
        self.revalidate()?;
        Ok(context)
    }
    pub(in crate::setup) fn review(source: ResolvedSource, namespace: &Path) -> io::Result<Self> {
        Self::review_with_geometry(source, namespace, None)
    }
    fn review_with_geometry(
        source: ResolvedSource,
        namespace: &Path,
        geometry: Option<Arc<CompletedOriginalGeometry>>,
    ) -> io::Result<Self> {
        // SAFETY: reads only this process's current effective user ID.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut record = source.open_original_record(namespace)?;
        if record.metadata()?.len() > MAX_RECORD {
            return Err(failure("Original source record exceeds bound"));
        }
        let before = migration_durability::identity(&record)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut record)
            .take(MAX_RECORD + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(failure("Original source record exceeds bound"));
        }
        let intent = match serde_json::from_slice::<DurableRecord>(&bytes)? {
            DurableRecord::Original(intent) => intent,
            DurableRecord::Capture(_) => {
                return Err(failure(
                    "Legacy snapshot record contains no original-peer source facts",
                ));
            }
        };
        intent.validate()?;
        if let Some(geometry) = &geometry {
            geometry.check(&source)?;
        }
        let requested = geometry
            .as_ref()
            .map_or(source.original_plan(), |g| g.original());
        if intent.home != *requested
            || intent.namespace.decode().map_err(io::Error::other)? != namespace
            || migration_durability::identity(&record)? != before
        {
            return Err(failure(
                "Protected original record does not bind this Home operation",
            ));
        }
        let inventory = if intent.inventory.is_some() {
            Some(source.open_original_inventory(namespace)?)
        } else {
            None
        };
        let result = Self {
            intent,
            geometry,
            record,
            inventory,
            source,
        };
        result.revalidate()?;
        Ok(result)
    }
    pub(in crate::setup) fn review_witness(
        source: ResolvedSource,
        witness: &SourceWitness,
    ) -> io::Result<Self> {
        let context = Self::review(
            source,
            &witness.namespace.decode().map_err(io::Error::other)?,
        )?;
        if witness.schema != 1 || context.intent.witness()? != *witness {
            return Err(failure(
                "Original source witness does not match protected record",
            ));
        }
        Ok(context)
    }
    /// Request facts never acquire ambient receiver authority. They must match
    /// this exact source record before a future confined reader can use them.
    pub fn verify_requested(
        &self,
        plan: &Plan,
        witness: &SourceWitness,
        requested: &OriginalCallerFacts,
    ) -> io::Result<()> {
        self.revalidate()?;
        witness.validate_plan(plan)?;
        requested.validate(plan.home_uid())?;
        if self.plan() != plan || self.intent.witness()? != *witness || self.original() != requested
        {
            return Err(failure(
                "Pending original source request differs from protected record",
            ));
        }
        Ok(())
    }
    pub(in crate::setup::home_capture) fn qualification_owned_root(&self) -> io::Result<()> {
        self.revalidate()?;
        self.source.qualification_owned_root()?;
        self.revalidate()
    }
    pub fn revalidate(&self) -> io::Result<()> {
        if let Some(geometry) = &self.geometry {
            geometry.check(&self.source)?;
        }
        let current = self
            .source
            .open_original_record(&self.intent.namespace.decode().map_err(io::Error::other)?)?;
        if migration_durability::identity(&current)?
            != migration_durability::identity(&self.record)?
        {
            return Err(failure("Protected original source record was replaced"));
        }
        // Record and source are read-only without a writable alias; compare
        // exact bytes as well as inode to make changed protected contents fail.
        let mut left = self.record.try_clone()?;
        let mut right = current;
        use std::io::{Seek, SeekFrom};
        left.seek(SeekFrom::Start(0))?;
        right.seek(SeekFrom::Start(0))?;
        let mut a = Vec::new();
        let mut b = Vec::new();
        left.take(MAX_RECORD + 1).read_to_end(&mut a)?;
        right.take(MAX_RECORD + 1).read_to_end(&mut b)?;
        if a != serde_json::to_vec(&self.intent)? || a != b {
            return Err(failure("Protected original source facts changed"));
        }
        self.original_omissions()?;
        self.revalidate_original_inventory()?;
        if let Some(geometry) = &self.geometry {
            geometry.check(&self.source)?;
        }
        Ok(())
    }
    fn revalidate_original_inventory(&self) -> io::Result<()> {
        match (&self.intent.inventory, &self.inventory) {
            (Some(proof), Some(held)) => {
                let current = self.source.open_original_inventory(
                    &self.intent.namespace.decode().map_err(io::Error::other)?,
                )?;
                if migration_durability::identity(&current)?
                    != migration_durability::identity(held)?
                {
                    return Err(failure("Protected original inventory was replaced"));
                }
                proof.verify(&self.intent.home, held, self.record.metadata()?.dev())?;
                proof.verify(&self.intent.home, &current, self.record.metadata()?.dev())
            }
            (None, None) => Ok(()),
            _ => Err(failure("Protected inventory custody missing")),
        }
    }
    /// Reviewed nested Home exclusions from the protected plan. Shape is checked
    /// against the plan; existence is checked under the held source by the claim.
    ///
    /// # Errors
    /// Refuses changed protected facts and an unrepresentable plan.
    pub fn original_exclusions(
        &self,
    ) -> io::Result<Vec<crate::setup::migration_store::NativePath>> {
        self.revalidate()?;
        let paths = self.intent.home.home_exclusion_paths()?;
        self.revalidate()?;
        Ok(paths
            .iter()
            .map(|path| crate::setup::migration_store::NativePath::encode(path.as_path()))
            .collect())
    }
    pub fn original_flatpak_inventory(&self) -> io::Result<Option<(File, u64, [u8; 32])>> {
        self.revalidate()?;
        let value = match (&self.intent.inventory, &self.inventory) {
            (Some(proof), Some(file)) => Some((file.try_clone()?, proof.len, proof.sha256)),
            (None, None) => None,
            _ => return Err(failure("Protected inventory custody missing")),
        };
        self.revalidate()?;
        Ok(value)
    }
    pub fn original_omissions(
        &self,
    ) -> io::Result<Vec<crate::setup::migration_review::OriginalOmission>> {
        crate::setup::migration_review::OriginalOmission::read_projection(
            &self.intent.omissions,
            &self.source.clone_original_home()?,
        )
    }
    pub fn original_flatpak_runner(
        &self,
        inventory: &crate::setup::flatpak_transfer::offline_original::OriginalInventoryGrant,
    ) -> io::Result<
        crate::setup::flatpak_transfer::SourceScopedRunner<
            crate::setup::flatpak_transfer::portable::NativeOfflineRunner,
        >,
    > {
        self.revalidate()?;
        if self.plan().categories.home {
            inventory.matches_plan(self.plan())?;
        } else {
            inventory.matches_applications_plan(self.plan())?;
        }
        let runner = match &self.geometry {
            Some(geometry) => self
                .source
                .original_flatpak_runner_completed(inventory, geometry),
            None => self.source.original_flatpak_runner(inventory),
        }
        .map_err(io::Error::other)?;
        self.revalidate()?;
        Ok(runner)
    }
    pub fn clone_original_home(&self) -> io::Result<File> {
        self.revalidate()?;
        let home = self.source.clone_original_home()?;
        self.revalidate()?;
        Ok(home)
    }
    pub fn original(&self) -> &OriginalCallerFacts {
        &self.intent.original
    }
    pub fn plan(&self) -> &Plan {
        &self.intent.home
    }
}

#[cfg(all(test, feature = "native-capture-qualification"))]
pub mod native_tests;

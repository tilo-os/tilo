//! Relocation witnesses. A journal cannot authorize unverified home
//! data: the caller additionally checks every original completed fidelity entry.
use super::*;
use crate::migration_review::storage::{IndexValue, SortedIndex};
#[derive(Serialize, Deserialize)]
pub(super) struct Marker;
impl IndexValue for Marker {
    const RECORD_BYTES: usize = 1024 * 1024;
}
#[derive(Serialize, Deserialize)]
struct ParentEvent {
    parent: NativePath,
}
impl IndexValue for ParentEvent {
    const RECORD_BYTES: usize = 1024 * 1024;
}
fn moved_under(index: &SortedIndex<Marker>, path: &Path) -> io::Result<bool> {
    if path.components().count() > 128 {
        return Err(changed("Adoption lookup exceeds path depth"));
    }
    for ancestor in path.ancestors() {
        if index.get(ancestor)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}
fn parent_key(path: &Path) -> String {
    hex::encode(Sha256::digest(path.as_os_str().as_bytes()))
}
fn source_index(plan: &Plan) -> io::Result<SortedIndex<Marker>> {
    let mut index = SortedIndex::with_scratch(plan.entries.store().scratch());
    let cancel = AtomicBool::new(false);
    for path in plan.source_items.iter() {
        index.insert(decoded(&path?)?, Marker, &cancel)?;
    }
    index.finish(&cancel)?;
    Ok(index)
}
pub(super) fn verify_remaining(
    plan: &Plan,
    quarantine: &Arc<PinnedDirectory>,
    moved: &SortedIndex<Marker>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let store = Store::new(
        plan.entries.store().scratch(),
        Arc::new(AtomicBool::new(false)),
    )?;
    let actual = tree::items(quarantine, Path::new(""), &store, cancel)?;
    let mut actual = actual.iter();
    for expected in plan.source_items.iter() {
        super::super::super::migration::check_cancel(cancel)?;
        let expected = decoded(&expected?)?;
        if moved_under(moved, &expected)? {
            continue;
        }
        if actual
            .next()
            .transpose()?
            .map(|path| decoded(&path))
            .transpose()?
            .as_ref()
            != Some(&expected)
        {
            return Err(changed("Unreceipted quarantine additions or missing items"));
        }
    }
    if let Some(extra) = actual.next() {
        extra?;
        return Err(changed("Unreceipted quarantine additions or missing items"));
    }
    Ok(())
}
pub struct AdoptionWitness {
    pub quarantine: Arc<PinnedDirectory>,
    home: Arc<PinnedDirectory>,
    moved: Arc<SortedIndex<Marker>>,
    changed_parents: Arc<SortedIndex<ParentEvent>>,
    items: Arc<SortedIndex<Marker>>,
    scope: PathBuf,
    quarantine_path: PathBuf,
    home_path: PathBuf,
}
impl AdoptionWitness {
    fn selected_items(&self) -> impl Iterator<Item = io::Result<PathBuf>> + '_ {
        self.items.iter().filter_map(|item| match item {
            Err(error) => Some(Err(error)),
            Ok((path, _)) => path
                .strip_prefix(&self.scope)
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| Ok(path.to_owned())),
        })
    }
    pub fn mapped_path(&self, path: &Path) -> io::Result<PathBuf> {
        if Arc::ptr_eq(&self.target(path)?, &self.home) {
            Ok(self.home_path.join(path))
        } else {
            Ok(self.quarantine_path.join(path))
        }
    }
    pub fn scoped(&self, prefix: &Path) -> Result<Self, String> {
        if !prefix.as_os_str().is_empty() {
            decoded(&NativePath::encode(prefix)).map_err(|e| e.to_string())?;
        }
        let root = self.mapped_path(prefix).map_err(|e| e.to_string())?;
        let quarantine = PinnedDirectory::open_absolute(&root).map_err(|e| e.to_string())?;
        let target_path = self.home_path.join(prefix);
        let home = match PinnedDirectory::open_absolute(&target_path) {
            Ok(pin) => pin,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Arc::clone(&quarantine),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            quarantine,
            home,
            moved: self.moved.clone(),
            changed_parents: self.changed_parents.clone(),
            items: self.items.clone(),
            scope: self.scope.join(prefix),
            quarantine_path: root,
            home_path: target_path,
        })
    }
    pub fn validate_application_namespace(&self) -> Result<(), String> {
        for path in self.selected_items() {
            let path = path.map_err(|e| e.to_string())?;
            if path.starts_with(".local/share/flatpak")
                || path.starts_with(".var/app")
                || [".local", ".local/share", ".var"]
                    .iter()
                    .any(|allowed| path == Path::new(allowed))
            {
                continue;
            }
            return Err("Application adoption contains unreviewed home data".into());
        }
        Ok(())
    }
    pub fn verify_archive_items(
        &self,
        expected: impl IntoIterator<Item = io::Result<PathBuf>>,
    ) -> io::Result<()> {
        let mut actual = self.selected_items();
        for expected in expected {
            let expected = expected?;
            if actual.next().transpose()?.as_ref() != Some(&expected) {
                return Err(changed(
                    "Adoption items differ from the authenticated portable archive",
                ));
            }
        }
        if let Some(extra) = actual.next() {
            extra?;
            return Err(changed(
                "Adoption items differ from the authenticated portable archive",
            ));
        }
        Ok(())
    }
    pub fn verify_inventory(
        &self,
        expected: &BTreeSet<PathBuf>,
        installation: Option<&Path>,
    ) -> Result<(), String> {
        for path in expected {
            if self
                .items
                .get(&self.scope.join(path))
                .map_err(|e| e.to_string())?
                .is_none()
            {
                return Err("Adoption plan omitted original completed user data".into());
            }
        }
        for path in self.selected_items() {
            let path = path.map_err(|e| e.to_string())?;
            if expected.contains(&path) || installation.is_some_and(|root| path.starts_with(root)) {
                continue;
            }
            if installation.is_some_and(|root| root.starts_with(&path)) {
                let root = self.target(&path).map_err(|e| e.to_string())?;
                let container = path.parent().ok_or("Missing installation ancestor")?;
                let name = path
                    .file_name()
                    .ok_or("Missing installation ancestor name")?;
                let directory = parent(&root, container).map_err(|e| e.to_string())?;
                if metadata_nofollow(&directory.file, Path::new(name))
                    .map_err(|e| e.to_string())?
                    .is_dir()
                {
                    continue;
                }
            }
            return Err(
                "Unreviewed data was added outside the approved application installation".into(),
            );
        }
        Ok(())
    }
    pub fn target(&self, path: &Path) -> io::Result<Arc<PinnedDirectory>> {
        if !path.as_os_str().is_empty() {
            decoded(&NativePath::encode(path))?;
        }
        Ok(if moved_under(&self.moved, &self.scope.join(path))? {
            self.home.clone()
        } else {
            self.quarantine.clone()
        })
    }
    pub fn application_home(&self) -> Result<&Path, String> {
        let selected = self.uniform_target(Path::new(".local/share/flatpak"))?;
        Ok(if Arc::ptr_eq(&selected, &self.home) {
            &self.home_path
        } else {
            &self.quarantine_path
        })
    }
    pub fn uniform_target(&self, prefix: &Path) -> Result<Arc<PinnedDirectory>, String> {
        let selected = self.target(prefix).map_err(|e| e.to_string())?;
        for path in self.selected_items() {
            let path = path.map_err(|e| e.to_string())?;
            if path.starts_with(prefix)
                && !Arc::ptr_eq(&self.target(&path).map_err(|e| e.to_string())?, &selected)
            {
                return Err("Application installation has retained adoption collisions; resolve them before retrying.".into());
            }
        }
        Ok(selected)
    }
    pub fn changed_parent(&self, path: &Path) -> io::Result<bool> {
        if !path.as_os_str().is_empty() {
            decoded(&NativePath::encode(path))?;
        }
        let path = self.scope.join(path);
        let prefix = parent_key(&path) + ":";
        let Some((key, event)) = self.changed_parents.first_at_or_after(Path::new(&prefix))? else {
            return Ok(false);
        };
        if !key.as_os_str().as_bytes().starts_with(prefix.as_bytes()) {
            return Ok(false);
        }
        let actual = event.parent.decode().map_err(io::Error::other)?;
        if actual != path {
            return Err(changed("Adoption parent index binding changed"));
        }
        Ok(true)
    }
}

impl PromotionReview {
    /// Verify every approved move actually happened without executing pending moves.
    fn require_root_acl_checkpoint(&self) -> Result<(), String> {
        if self.plan.root_acl.is_none() {
            return Ok(());
        }
        let (_, _, j) =
            roots(&self.quarantine, &self.home, &self.journal).map_err(|e| e.to_string())?;
        let log = Log::open(
            &j,
            self.journal.file_name().ok_or("Missing adoption journal")?,
            self.plan.entries.store().scratch(),
        )
        .map_err(|e| e.to_string())?;
        if log.root_receipt.is_none() {
            return Err("Home root ACL adoption is not checkpointed".into());
        }
        Ok(())
    }
    pub fn verify_completed(&self) -> Result<(), String> {
        self.require_root_acl_checkpoint()?;
        let witness = self.witness()?;
        for entry in self.plan.entries.iter() {
            let entry = entry.map_err(|e| e.to_string())?;
            match entry.kind {
                Kind::Move
                    if !witness
                        .moved
                        .get(&decoded(&entry.path).map_err(|error| error.to_string())?)
                        .map_err(|e| e.to_string())?
                        .is_some() =>
                {
                    return Err("Home adoption has pending moves".into());
                }
                Kind::Collision { .. } => {
                    return Err("Home adoption has retained collisions".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
    /// Verify completed relocation while deferring only the approved Flatpak
    /// repository and its directory ancestors to an authenticated archive and
    /// independent signed-output verifier. This witness alone is not data proof.
    pub fn completed_application_witness(&self) -> Result<AdoptionWitness, String> {
        self.require_root_acl_checkpoint()?;
        let witness = self
            .witness_inner(Some(Path::new(".local/share/flatpak")))
            .map_err(|error| error.to_string())?;
        for entry in self.plan.entries.iter() {
            let entry = entry.map_err(|e| e.to_string())?;
            match entry.kind {
                Kind::Move
                    if !witness
                        .moved
                        .get(&decoded(&entry.path).map_err(|error| error.to_string())?)
                        .map_err(|e| e.to_string())?
                        .is_some() =>
                {
                    return Err("Home adoption has pending moves".into());
                }
                Kind::Collision { .. } => {
                    return Err("Home adoption has retained collisions".into());
                }
                _ => {}
            }
        }
        Ok(witness)
    }
    pub fn witness(&self) -> Result<AdoptionWitness, String> {
        self.witness_inner(None).map_err(|error| error.to_string())
    }
    fn witness_inner(&self, application: Option<&Path>) -> io::Result<AdoptionWitness> {
        let (q, h, j) = roots(&self.quarantine, &self.home, &self.journal)?;
        let log = Log::open(
            &j,
            self.journal
                .file_name()
                .ok_or_else(|| changed("Missing adoption journal"))?,
            self.plan.entries.store().scratch(),
        )?;
        if plan_digest(&log.plan)? != plan_digest(&self.plan)? {
            return Err(changed("Adoption consent changed"));
        }
        if let Some(intent) = &self.plan.root_acl {
            intent.verify(&q, &h, log.root_receipt.is_some())?;
        }
        // Only restore the exact temporary parent-mode grant after its inode
        // and retained or moved child match the durable plan. No item is moved.
        repair_parent_modes(&self.plan, &q, &h)?;
        let mut moved = SortedIndex::with_scratch(self.plan.entries.store().scratch());
        let mut changed_parents = SortedIndex::with_scratch(self.plan.entries.store().scratch());
        let cancel = AtomicBool::new(false);
        for (index, entry) in self.plan.entries.iter().enumerate() {
            let entry = entry?;
            let path = decoded(&entry.path)?;
            let container = path
                .parent()
                .ok_or_else(|| changed("Missing adoption parent"))?;
            let name = path
                .file_name()
                .ok_or_else(|| changed("Missing adoption name"))?;
            let source = parent(&q, container)?;
            let target = parent(&h, container)?;
            match &entry.kind {
                Kind::Move => match metadata_nofollow(&source.file, Path::new(name)) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if let Some(seed) = &entry.seed {
                            seeds::verify_retained(&h, seed)?;
                        }
                        if application.is_some() && !log.done.contains(index)? {
                            return Err(changed(
                                "Application adoption move lacks a durable checkpoint",
                            ));
                        }
                        if application.is_some_and(|root| root.starts_with(&path)) {
                            let items = entry.structure.as_ref().ok_or_else(|| changed("Retained adoption lacks original structural metadata; application deferral is unavailable"))?;
                            structure::verify(items, &entry, &h)?;
                        }
                        verify_moved(
                            &target,
                            name,
                            &path,
                            &entry.fact,
                            application,
                            &entry.items.store(),
                        )?;
                        moved.insert(path.clone(), Marker, &cancel)?;
                        let key = PathBuf::from(format!("{}:{index:016x}", parent_key(container)));
                        changed_parents.insert(
                            key,
                            ParentEvent {
                                parent: NativePath::encode(container),
                            },
                            &cancel,
                        )?;
                    }
                    Ok(_) => {
                        if log.done.contains(index)?
                            || tree::fact_in(
                                &source,
                                name,
                                &path,
                                &entry.items.store(),
                                &AtomicBool::new(false),
                            )? != entry.fact
                        {
                            return Err(changed("Retained adoption item changed"));
                        }
                        if let Some(seed) = &entry.seed {
                            seeds::verify_pending(&h, &path, seed)?;
                        } else {
                            match metadata_nofollow(&target.file, Path::new(name)) {
                                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                                _ => return Err(changed("Adoption target appeared after review")),
                            }
                        }
                    }
                    Err(error) => return Err(error),
                },
                Kind::MergedDirectory { target: expected } => {
                    if tree::shallow_directory(&source, name)?.0 != entry.fact
                        || tree::shallow_directory(&target, name)?.0 != *expected
                    {
                        return Err(changed("Merged adoption directory changed"));
                    }
                }
                Kind::Collision { target: expected } => {
                    if tree::fact_in(
                        &source,
                        name,
                        &path,
                        &entry.items.store(),
                        &AtomicBool::new(false),
                    )? != entry.fact
                        || tree::target_fact(&target, name, &path)?.0 != *expected
                    {
                        return Err(changed("Adoption collision changed"));
                    }
                }
                Kind::InactiveCustody => {
                    if tree::fact_in(
                        &source,
                        name,
                        &path,
                        &entry.items.store(),
                        &AtomicBool::new(false),
                    )? != entry.fact
                    {
                        return Err(changed("Retained identity custody changed"));
                    }
                }
            }
        }
        moved.finish(&cancel)?;
        changed_parents.finish(&cancel)?;
        verify_remaining(&self.plan, &q, &moved, &cancel)?;
        let items = source_index(&self.plan)?;
        q.validate_destination()?;
        h.validate_destination()?;
        Ok(AdoptionWitness {
            quarantine: q,
            home: h,
            moved: Arc::new(moved),
            changed_parents: Arc::new(changed_parents),
            items: Arc::new(items),
            scope: PathBuf::new(),
            quarantine_path: self.quarantine.clone(),
            home_path: self.home.clone(),
        })
    }
}

fn verify_moved(
    target: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
    expected: &Fact,
    application: Option<&Path>,
    store: &Arc<Store>,
) -> io::Result<()> {
    if application.is_some_and(|root| path.starts_with(root) || root.starts_with(path)) {
        let metadata = metadata_nofollow(&target.file, Path::new(name))?;
        // SAFETY: geteuid only reads the current account identity.
        if !expected.directory
            || !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o6000 != 0
            || metadata.mode() & 0o777 != expected.mode
        {
            return Err(changed(
                "Moved application directory ownership or mode changed",
            ));
        }
        let directory = target.open_child(name, &metadata)?;
        if tree::directory_identity(&directory)? != expected.identity {
            return Err(changed("Moved application directory identity changed"));
        }
        directory.validate_destination()?;
    } else if tree::fact_in(target, name, path, store, &AtomicBool::new(false))? != *expected {
        return Err(changed("Moved adoption item changed"));
    }
    Ok(())
}

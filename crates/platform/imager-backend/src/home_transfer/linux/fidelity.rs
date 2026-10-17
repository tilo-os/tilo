//! A durable finalization log. The byte journal remains unchanged: once this
//! log is published, retries revalidate this manifest rather than its private
//! file assumptions. Every path is relative and opened one no-follow edge at a
//! time. Literal links are never traversed, including during recovery.
mod adoption;
use super::super::super::migration_store::NativePath;
use super::*;
pub(super) use adoption::{
    verify as verify_adoption, verify_applications as verify_adoption_applications,
};
use advisory_lock::FileLock;
use cap_primitives::fs::MetadataExt as UnixMetadataExt;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{Seek, SeekFrom},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: NativePath,
    source: Snapshot,
    device: u64,
    inode: u64,
    mode: u32,
    kind: Kind,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Kind {
    Directory {
        destination: durability::Identity,
    },
    File {
        digest: [u8; 32],
        destination: durability::Identity,
        hardlink: Option<usize>,
    },
    Link {
        target: NativePath,
    },
    Special,
    RuntimeSocket,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    revision: [u8; 32],
    source: durability::Identity,
    root: durability::Identity,
    entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Done {
    index: usize,
    destination: Option<durability::Identity>,
}

struct Log {
    file: FileLock,
    parent: Arc<PinnedDirectory>,
    name: std::ffi::OsString,
    identity: durability::Identity,
    manifest: Manifest,
    completed: BTreeMap<usize, Option<durability::Identity>>,
}

pub(super) fn begin(
    review: &HomeReview,
    source: &Arc<PinnedDirectory>,
    path: &Path,
    report: &mut CopyReport,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let destination = PinnedDirectory::open_absolute(&review.destination)?;
    destination.validate_destination()?;
    let mut manifest = Manifest {
        version: 1,
        revision: review.revision,
        source: review.source_identity.clone(),
        root: directory_identity(&destination)?,
        entries: Vec::new(),
    };
    inventory(
        source,
        &destination,
        Path::new(""),
        &mut manifest,
        &mut BTreeMap::new(),
        cancel,
    )?;
    review.validate()?;
    let mut log = Log::create(path, manifest)?;
    *report = finalize(review, source, &destination, &mut log, cancel)?;
    Ok(())
}

pub(super) fn resume(
    review: &HomeReview,
    source: &Arc<PinnedDirectory>,
    path: &Path,
    cancel: &AtomicBool,
) -> io::Result<CopyReport> {
    let mut log = Log::open(path)?;
    if log.manifest.version != 1
        || log.manifest.revision != review.revision
        || log.manifest.source != review.source_identity
    {
        return Err(changed(
            "Home finalization receipt does not match the reviewed source.",
        ));
    }
    let destination = PinnedDirectory::open_absolute(&review.destination)?;
    destination.validate_destination()?;
    if directory_identity(&destination)? != log.manifest.root {
        return Err(changed(
            "The imported home was replaced; retained data requires review.",
        ));
    }
    finalize(review, source, &destination, &mut log, cancel)
}

/// Revalidate the original completed home after authorized Flatpak additions.
/// Only Flatpak's repository configuration and runtime change marker may differ;
/// existing app deployments, objects and every other user file remain verified.
pub(super) fn verify_applications(review: &HomeReview) -> io::Result<CopyReport> {
    verify_application_tree(review, Some(Path::new(".local/share/flatpak")))
}

pub(super) fn verify_flatpak_installation(review: &HomeReview) -> io::Result<CopyReport> {
    verify_application_tree(review, Some(Path::new("")))
}

pub(super) fn verify_flatpak_adoption(
    review: &HomeReview,
    witness: &super::super::promotion::AdoptionWitness,
) -> io::Result<CopyReport> {
    adoption::verify_flatpak(review, witness)
}
pub(super) fn verify_completed_tree(review: &HomeReview) -> io::Result<CopyReport> {
    verify_application_tree(review, None)
}

fn verify_application_tree(
    review: &HomeReview,
    installation: Option<&Path>,
) -> io::Result<CopyReport> {
    let source = review.validate()?;
    let destination = PinnedDirectory::open_absolute(&review.destination)?;
    destination.validate_destination()?;
    let log = Log::open(&review.journal.with_extension("home-finalization"))?;
    if log.manifest.version != 1
        || log.manifest.revision != review.revision
        || log.manifest.source != review.source_identity
        || directory_identity(&destination)? != log.manifest.root
        || log.completed.len() != log.manifest.entries.len()
    {
        return Err(changed(
            "Completed home fidelity receipts are missing or changed",
        ));
    }
    let mut report = CopyReport {
        scan_completed: true,
        ..CopyReport::default()
    };
    let cancel = AtomicBool::new(false);
    let mut expected = std::collections::BTreeSet::new();
    for (index, entry) in log.manifest.entries.iter().enumerate() {
        let path = decode(&entry.path)?;
        if matches!(entry.kind, Kind::Special) {
            return Err(changed(
                "Unsupported original home metadata cannot be promoted",
            ));
        }
        if !matches!(entry.kind, Kind::RuntimeSocket) {
            expected.insert(path.clone());
        }
        if matches!(entry.kind, Kind::Directory { .. })
            && installation.is_some_and(|root| application_directory(&path, root))
        {
            verify_application_directory(entry, &source, &destination, &mut report)?;
        } else if installation
            .is_some_and(|root| path == root.join("repo/config") || path == root.join(".changed"))
        {
            // Configuration is independently compared against original remote
            // trust by the application controller, never accepted by this hook.
            let source_parent = parent(
                &source,
                path.parent()
                    .ok_or_else(|| changed("Missing application parent"))?,
            )?;
            validate_source(
                entry,
                &metadata_nofollow(
                    &source_parent.file,
                    Path::new(
                        path.file_name()
                            .ok_or_else(|| changed("Missing application name"))?,
                    ),
                )?,
            )?;
            if installation.is_some_and(|root| path == root.join(".changed")) {
                if !matches!(entry.kind, Kind::File { .. }) || entry.source.size() != 0 {
                    return Err(changed(
                        "A nonempty original change marker is user data, not mutable runtime state",
                    ));
                }
                let target_parent = parent(
                    &destination,
                    path.parent()
                        .ok_or_else(|| changed("Missing change marker parent"))?,
                )?;
                let marker = regular(
                    &target_parent,
                    path.file_name()
                        .ok_or_else(|| changed("Missing change marker name"))?,
                )?;
                if marker.metadata()?.len() != 0 {
                    return Err(changed("Application change marker has unexpected data"));
                }
            }
        } else {
            finish_entry(
                index,
                &source,
                &destination,
                &log,
                &cancel,
                &mut report,
                true,
            )?;
        }
    }
    verify_no_unreviewed_additions(&destination, Path::new(""), &expected, installation, 0)?;
    review.validate()?;
    Ok(report)
}
fn application_directory(path: &Path, installation: &Path) -> bool {
    path.starts_with(installation) || installation.starts_with(path)
}
fn verify_application_directory(
    entry: &Entry,
    source: &Arc<PinnedDirectory>,
    destination: &Arc<PinnedDirectory>,
    report: &mut CopyReport,
) -> io::Result<()> {
    let path = decode(&entry.path)?;
    let original = parent(source, &path)?;
    let target = parent(destination, &path)?;
    validate_source(entry, &Metadata::from_file(&original.file)?)?;
    let Kind::Directory {
        destination: expected,
    } = &entry.kind
    else {
        return Err(changed("Invalid application directory receipt"));
    };
    let actual = Metadata::from_file(&target.file)?;
    let mode = if path.as_os_str().is_empty() {
        0o700
    } else {
        entry.mode & 0o777
    };
    if directory_identity(&target)? != *expected || actual.mode() & 0o777 != mode {
        return Err(changed(
            "An original application directory was replaced or its permissions changed",
        ));
    }
    copy_xattrs(
        &durability::readable_directory(&original.file)?,
        &durability::readable_directory(&target.file)?,
        &path,
        report,
        true,
    )
}
fn verify_no_unreviewed_additions(
    root: &Arc<PinnedDirectory>,
    relative: &Path,
    expected: &std::collections::BTreeSet<PathBuf>,
    installation: Option<&Path>,
    depth: usize,
) -> io::Result<()> {
    if depth > 128 {
        return Err(changed("Application recovery exceeds depth bound"));
    }
    let directory = cap_std::fs::Dir::from_std_file(durability::readable_directory(&root.file)?);
    for child in directory.entries()? {
        let name = child?.file_name();
        let path = relative.join(&name);
        if installation.is_some_and(|root| path.starts_with(root)) {
            continue;
        }
        let metadata = metadata_nofollow(&root.file, Path::new(&name))?;
        if !expected.contains(&path)
            && !(metadata.is_dir()
                && !metadata.is_symlink()
                && installation.is_some_and(|root| application_directory(&path, root)))
        {
            return Err(changed(
                "Unreviewed files were added outside the Flatpak installation",
            ));
        }
        if metadata.is_dir() && !metadata.is_symlink() {
            verify_no_unreviewed_additions(
                &root.open_child(&name, &metadata)?,
                &path,
                expected,
                installation,
                depth + 1,
            )?;
        }
    }
    root.validate_destination()
}

impl Log {
    fn create(path: &Path, manifest: Manifest) -> io::Result<Self> {
        let parent = PinnedDirectory::open_absolute(
            path.parent()
                .ok_or_else(|| changed("Finalization journal has no parent."))?,
        )?;
        let name = path
            .file_name()
            .ok_or_else(|| changed("Finalization journal has no name."))?
            .to_owned();
        let mut temporary = migration::OwnedTemporary::create(&parent, &name)?;
        let file =
            FileLock::try_exclusive(temporary.file.try_clone()?).map_err(io::Error::other)?;
        serde_json::to_writer(&mut temporary.file, &manifest)?;
        temporary.file.write_all(b"\n")?;
        if temporary.file.metadata()?.len() > 512 * 1024 * 1024 {
            return Err(changed(
                "Home finalization manifest exceeds its 512 MiB recovery bound.",
            ));
        }
        durability::sync_file(&temporary.file)?;
        temporary.validate()?;
        cap_primitives::fs::hard_link(
            &parent.file,
            Path::new(&temporary.name),
            &parent.file,
            Path::new(&name),
        )?;
        durability::sync_directory(&parent.file)?;
        let identity = durability::identity(&file)?;
        Ok(Self {
            file,
            parent,
            name,
            identity,
            manifest,
            completed: BTreeMap::new(),
        })
    }
    fn open(path: &Path) -> io::Result<Self> {
        let parent = PinnedDirectory::open_absolute(
            path.parent()
                .ok_or_else(|| changed("Finalization journal has no parent."))?,
        )?;
        parent.validate_destination()?;
        let name = path
            .file_name()
            .ok_or_else(|| changed("Finalization journal has no name."))?
            .to_owned();
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .follow(FollowSymlinks::No)
            .nonblock(true);
        let raw = cap_primitives::fs::open(&parent.file, Path::new(&name), &options)?;
        super::super::super::destination_security::check_private_file(&raw)?;
        if !raw.metadata()?.is_file() || raw.metadata()?.len() > 1024 * 1024 * 1024 {
            return Err(changed(
                "Finalization journal exceeds bounds or is not a regular file.",
            ));
        }
        let file = FileLock::try_exclusive(raw).map_err(io::Error::other)?;
        let mut reader = io::BufReader::new(file.try_clone()?);
        let mut bytes = Vec::new();
        reader.read_until(b'\n', &mut bytes)?;
        if bytes.last() != Some(&b'\n') {
            return Err(changed("Home manifest was not completely published."));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        if manifest.entries.len() > 1_000_000 {
            return Err(changed("Home manifest exceeds entry bounds."));
        }
        let mut offset = bytes.len() as u64;
        let mut completed = BTreeMap::new();
        loop {
            bytes.clear();
            if reader.read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            if bytes.last() != Some(&b'\n') {
                break;
            }
            let record: Done = serde_json::from_slice(&bytes)?;
            if record.index >= manifest.entries.len()
                || completed.insert(record.index, record.destination).is_some()
            {
                return Err(changed(
                    "Finalization log has an invalid or duplicate receipt.",
                ));
            }
            offset += bytes.len() as u64;
        }
        // An interrupted last append has no durable completion receipt. Its
        // operation is revalidated and repeated; no earlier receipt is removed.
        file.set_len(offset)?;
        let mut file = file;
        file.seek(SeekFrom::End(0))?;
        durability::sync_file(&file)?;
        let identity = durability::identity(&file)?;
        Ok(Self {
            file,
            parent,
            name,
            identity,
            manifest,
            completed,
        })
    }
    fn checkpoint(
        &mut self,
        index: usize,
        destination: Option<durability::Identity>,
    ) -> io::Result<()> {
        self.parent.validate_destination()?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let current = cap_primitives::fs::open(&self.parent.file, Path::new(&self.name), &options)?;
        if durability::identity(&current)? != self.identity {
            return Err(changed("Finalization journal was replaced."));
        }
        let bytes = serde_json::to_vec(&Done {
            index,
            destination: destination.clone(),
        })?;
        if bytes.len() > 512 {
            return Err(changed(
                "Home completion receipt exceeds its recovery bound.",
            ));
        }
        self.file.write_all(&bytes)?;
        self.file.write_all(b"\n")?;
        durability::sync_file(&self.file)?;
        self.completed.insert(index, destination);
        #[cfg(test)]
        if INTERRUPT_AFTER_CHECKPOINT.with(|flag| flag.replace(false)) {
            return Err(io::Error::other(
                "Injected interruption after durable home checkpoint.",
            ));
        }
        Ok(())
    }
}

fn regular(parent: &PinnedDirectory, name: &OsStr) -> io::Result<File> {
    parent.validate()?;
    let expected = metadata_nofollow(&parent.file, Path::new(name))?;
    if !expected.is_file() || expected.is_symlink() {
        return Err(changed("An imported regular file was replaced."));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    if !migration::same_snapshot(&expected, &Metadata::from_file(&file)?) {
        return Err(changed("File changed while opening."));
    }
    Ok(file)
}
fn digest(file: &File, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
    let before = Metadata::from_file(file)?;
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let (_, digest) = migration::stream(&mut reader, None, before.len(), cancel)?;
    if !migration::same_snapshot(&before, &Metadata::from_file(file)?) {
        return Err(changed("File changed while verifying home data."));
    }
    Ok(digest)
}
fn inventory(
    source: &Arc<PinnedDirectory>,
    destination: &Arc<PinnedDirectory>,
    path: &Path,
    manifest: &mut Manifest,
    links: &mut BTreeMap<(u64, u64), usize>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    migration::check_cancel(cancel)?;
    let metadata = Metadata::from_file(&source.file)?;
    let directory_index = manifest.entries.len();
    manifest.entries.push(Entry {
        path: NativePath::encode(path),
        source: Snapshot::new(&metadata)?,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode() & 0o7777,
        kind: Kind::Directory {
            destination: directory_identity(destination)?,
        },
    });
    let directory = cap_std::fs::Dir::from_std_file(source.file.try_clone()?);
    for child in directory.entries()? {
        migration::check_cancel(cancel)?;
        let name = child?.file_name();
        let relative = path.join(&name);
        let metadata = metadata_nofollow(&source.file, Path::new(&name))?;
        if metadata.is_dir() && !metadata.is_symlink() {
            let target = metadata_nofollow(&destination.file, Path::new(&name))?;
            inventory(
                &source.open_child(&name, &metadata)?,
                &destination.open_child(&name, &target)?,
                &relative,
                manifest,
                links,
                cancel,
            )?;
            continue;
        }
        let kind = if metadata.is_file() && !metadata.is_symlink() {
            let source_file = regular(source, &name)?;
            let target_file = regular(destination, &name)?;
            let expected = digest(&source_file, cancel)?;
            if digest(&target_file, cancel)? != expected {
                return Err(changed("Imported bytes differ from the source."));
            }
            let hardlink = links.get(&(metadata.dev(), metadata.ino())).copied();
            links
                .entry((metadata.dev(), metadata.ino()))
                .or_insert(manifest.entries.len());
            Kind::File {
                digest: expected,
                destination: durability::identity(&target_file)?,
                hardlink,
            }
        } else if metadata.is_symlink() {
            Kind::Link {
                target: NativePath::encode(&cap_primitives::fs::read_link_contents(
                    &source.file,
                    Path::new(&name),
                )?),
            }
        } else if super::super::super::migration_review::runtime_socket(&metadata) {
            Kind::RuntimeSocket
        } else {
            Kind::Special
        };
        manifest.entries.push(Entry {
            path: NativePath::encode(&relative),
            source: Snapshot::new(&metadata)?,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode() & 0o7777,
            kind,
        });
    }
    if Snapshot::new(&Metadata::from_file(&source.file)?)?
        != manifest.entries[directory_index].source
    {
        return Err(changed("Home changed during finalization review."));
    }
    Ok(())
}

fn decode(path: &NativePath) -> io::Result<PathBuf> {
    path.decode().map_err(|error| changed(&error))
}
fn parent(root: &Arc<PinnedDirectory>, path: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let mut current = Arc::clone(root);
    for part in path.components() {
        let std::path::Component::Normal(name) = part else {
            return Err(changed("Home manifest contains a non-relative path."));
        };
        let metadata = metadata_nofollow(&current.file, Path::new(name))?;
        current = current.open_child(name, &metadata)?;
    }
    Ok(current)
}

fn finalize(
    review: &HomeReview,
    source: &Arc<PinnedDirectory>,
    destination: &Arc<PinnedDirectory>,
    log: &mut Log,
    cancel: &AtomicBool,
) -> io::Result<CopyReport> {
    let mut report = CopyReport { scan_completed: true, metadata_limitations: vec![
        "All hidden files are included. Ownership becomes the importing account's ownership. The import root stays private (0700). Special privilege bits and old OS security labels/ACLs are not restored; individual exceptions are listed.".into()
    ], ..CopyReport::default() };
    // Files and links first; directory permissions and times last, deepest first.
    let mut order: Vec<_> = (0..log.manifest.entries.len()).collect();
    // Hardlink targets must finalize before aliases: preserve file manifest order.
    order.sort_by_key(|index| {
        if matches!(log.manifest.entries[*index].kind, Kind::Directory { .. }) {
            log.manifest.entries.len() * 2 - index
        } else {
            *index
        }
    });
    for index in order {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            report.scan_completed = false;
            break;
        }
        let path = decode(&log.manifest.entries[index].path)?;
        if matches!(log.manifest.entries[index].kind, Kind::Special) {
            report.skipped.push((path, "A FIFO or device entry is unsupported and was not copied. Review it separately before considering the transfer complete.".into()));
            continue;
        }
        match finish_entry(index, source, destination, log, cancel, &mut report, false) {
            Ok(identity) => {
                if !log.completed.contains_key(&index) {
                    log.checkpoint(index, identity)?;
                }
            }
            Err(error) => {
                report.failed.push((path, error.to_string()));
                // Do not restore enclosing directory modes after a child failed.
                report.scan_completed = false;
                break;
            }
        }
    }
    if let Err(error) = review.validate() {
        report.scan_completed = false;
        report.failed.push((PathBuf::new(), error.to_string()));
    }
    Ok(report)
}

fn finish_entry(
    index: usize,
    source: &Arc<PinnedDirectory>,
    destination: &Arc<PinnedDirectory>,
    log: &Log,
    cancel: &AtomicBool,
    report: &mut CopyReport,
    verify_only: bool,
) -> io::Result<Option<durability::Identity>> {
    let entry = &log.manifest.entries[index];
    let path = decode(&entry.path)?;
    if entry.mode & 0o7000 != 0 {
        report.metadata_limitations.push(format!(
            "{}: setuid, setgid and sticky permission bits were not restored.",
            path.display()
        ));
    }
    if let Kind::Directory {
        destination: expected,
    } = &entry.kind
    {
        let source_dir = parent(source, &path)?;
        let target_dir = parent(destination, &path)?;
        validate_source(entry, &Metadata::from_file(&source_dir.file)?)?;
        if directory_identity(&target_dir)? != *expected
            || Snapshot::new(&Metadata::from_file(&source_dir.file)?)? != entry.source
        {
            return Err(changed("Directory changed after home review."));
        }
        let mode = if path.as_os_str().is_empty() {
            0o700
        } else {
            entry.mode & 0o777
        };
        let target_file = durability::readable_directory(&target_dir.file)?;
        if log.completed.contains_key(&index) {
            let actual = Metadata::from_file(&target_file)?;
            if actual.mode() & 0o777 != mode
                || actual.modified()?.into_std()
                    != Metadata::from_file(&source_dir.file)?
                        .modified()?
                        .into_std()
            {
                return Err(changed(
                    "Imported directory permissions or modification time changed; review retained data.",
                ));
            }
        }
        copy_xattrs(
            &durability::readable_directory(&source_dir.file)?,
            &target_file,
            &path,
            report,
            log.completed.contains_key(&index),
        )?;
        if !verify_only {
            target_file.set_permissions(fs::Permissions::from_mode(mode))?;
            let metadata = Metadata::from_file(&source_dir.file)?;
            migration::preserve_directory_times_values(
                &target_dir,
                metadata.accessed()?.into_std(),
                metadata.modified()?.into_std(),
            )?;
            durability::sync_directory(&target_dir.file)?;
        }
        return Ok(Some(expected.clone()));
    }
    let name = path
        .file_name()
        .ok_or_else(|| changed("Home manifest entry has no name."))?;
    let container = path
        .parent()
        .ok_or_else(|| changed("Home manifest entry has no parent."))?;
    let source_parent = parent(source, container)?;
    let target_parent = parent(destination, container)?;
    let source_metadata = metadata_nofollow(&source_parent.file, Path::new(name))?;
    validate_source(entry, &source_metadata)?;
    match &entry.kind {
        Kind::RuntimeSocket => {
            if log.completed.get(&index).is_some_and(Option::is_some) {
                return Err(changed(
                    "A runtime socket omission has an invalid destination receipt.",
                ));
            }
            if !super::super::super::migration_review::runtime_socket(&source_metadata) {
                return Err(changed("A reviewed runtime socket changed its inode type."));
            }
            match metadata_nofollow(&target_parent.file, Path::new(name)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(changed(
                        "A runtime socket omission conflicts with an existing target entry; retained data was not changed.",
                    ));
                }
            }
            report.metadata_limitations.push(format!(
                "{}: a runtime socket endpoint was omitted; applications create new endpoints when started. Adjacent files are included.",
                path.display()
            ));
            Ok(None)
        }
        Kind::Link { target } => {
            let literal = decode(target)?;
            if cap_primitives::fs::read_link_contents(&source_parent.file, Path::new(name))?
                != literal
            {
                return Err(changed("Source link changed."));
            }
            match metadata_nofollow(&target_parent.file, Path::new(name)) {
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && !log.completed.contains_key(&index) =>
                {
                    cap_primitives::fs::symlink_contents(
                        &literal,
                        &target_parent.file,
                        Path::new(name),
                    )?;
                    durability::sync_directory(&target_parent.file)?;
                }
                Ok(metadata) if metadata.is_symlink() => {}
                _ => return Err(changed("Imported link conflicts with existing output.")),
            }
            if cap_primitives::fs::read_link_contents(&target_parent.file, Path::new(name))?
                != literal
            {
                return Err(changed("Imported link target changed."));
            }
            let identity = durability::link_identity(&target_parent.file, name)?;
            if log
                .completed
                .get(&index)
                .is_some_and(|saved| saved.as_ref() != Some(&identity))
            {
                return Err(changed("Imported link was replaced; review retained data."));
            }
            report.verified_files.push(path);
            Ok(Some(identity))
        }
        Kind::File {
            digest: expected,
            destination: baseline,
            hardlink,
        } => {
            let source_file = regular(&source_parent, name)?;
            if digest(&source_file, cancel)? != *expected {
                return Err(changed("Source file bytes changed after home review."));
            }
            let canonical = hardlink
                .map(|first| {
                    let canonical = log
                        .manifest
                        .entries
                        .get(first)
                        .ok_or_else(|| changed("Hardlink receipt index exceeds bounds."))?;
                    if first >= index
                        || canonical.device != entry.device
                        || canonical.inode != entry.inode
                        || !matches!(canonical.kind, Kind::File { .. })
                    {
                        return Err(changed(
                            "Hardlink receipt does not describe the same source inode.",
                        ));
                    }
                    decode(&canonical.path)
                })
                .transpose()?;
            let canonical_file = canonical
                .as_ref()
                .map(|path| {
                    let parent = parent(
                        destination,
                        path.parent()
                            .ok_or_else(|| changed("Hardlink parent missing."))?,
                    )?;
                    Ok::<_, io::Error>((
                        parent,
                        path.file_name()
                            .ok_or_else(|| changed("Hardlink name missing."))?
                            .to_owned(),
                    ))
                })
                .transpose()?;
            let target_file = match regular(&target_parent, name) {
                Ok(file) => file,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && !log.completed.contains_key(&index)
                        && canonical_file.is_some() =>
                {
                    let (first_parent, first_name) = canonical_file
                        .as_ref()
                        .ok_or_else(|| changed("Hardlink target missing."))?;
                    cap_primitives::fs::hard_link(
                        &first_parent.file,
                        Path::new(first_name),
                        &target_parent.file,
                        Path::new(name),
                    )?;
                    durability::sync_directory(&target_parent.file)?;
                    regular(&target_parent, name)?
                }
                Err(error) => return Err(error),
            };
            let identity = durability::identity(&target_file)?;
            let canonical_identity = canonical_file
                .as_ref()
                .map(|(parent, name)| {
                    regular(parent, name).and_then(|file| durability::identity(&file))
                })
                .transpose()?;
            let saved = log.completed.get(&index).and_then(Option::as_ref);
            if saved.is_some_and(|saved| *saved != identity)
                || (saved.is_none()
                    && identity != *baseline
                    && canonical_identity.as_ref() != Some(&identity))
            {
                return Err(changed(
                    "Imported file identity changed; existing data was retained.",
                ));
            }
            if digest(&target_file, cancel)? != *expected {
                return Err(changed(
                    "Imported bytes changed; existing data was retained.",
                ));
            }
            if let Some((first_parent, first_name)) = &canonical_file {
                if canonical_identity.as_ref() != Some(&identity) {
                    if verify_only {
                        return Err(changed("Original imported hardlink topology changed"));
                    }
                    cap_primitives::fs::remove_file(&target_parent.file, Path::new(name))?;
                    cap_primitives::fs::hard_link(
                        &first_parent.file,
                        Path::new(first_name),
                        &target_parent.file,
                        Path::new(name),
                    )?;
                    durability::sync_directory(&target_parent.file)?;
                }
            }
            let target_file = regular(&target_parent, name)?;
            if saved.is_some() {
                let metadata = Metadata::from_file(&target_file)?;
                if metadata.mode() & 0o777 != entry.mode & 0o777
                    || metadata.modified()?.into_std()
                        != Metadata::from_file(&source_file)?.modified()?.into_std()
                {
                    return Err(changed(
                        "Imported permissions or modification time changed; review the retained output.",
                    ));
                }
            }
            copy_xattrs(
                &source_file,
                &target_file,
                &path,
                report,
                saved.is_some() || hardlink.is_some(),
            )?;
            let metadata = Metadata::from_file(&source_file)?;
            if !verify_only {
                target_file.set_permissions(fs::Permissions::from_mode(entry.mode & 0o777))?;
                migration::preserve_time_values(
                    &target_file,
                    metadata.accessed()?.into_std(),
                    metadata.modified()?.into_std(),
                )?;
                durability::sync_file(&target_file)?;
            }
            report.verified_bytes = report
                .verified_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| changed("Home byte count overflow."))?;
            report.verified_files.push(path);
            Ok(Some(durability::identity(&target_file)?))
        }
        _ => Err(changed("Unexpected home manifest entry.")),
    }
}

fn validate_source(entry: &Entry, metadata: &Metadata) -> io::Result<()> {
    if Snapshot::new(metadata)? != entry.source
        || metadata.mode() & 0o7777 != entry.mode
        || metadata.dev() != entry.device
        || metadata.ino() != entry.inode
    {
        return Err(changed(
            "Source entry changed or its home receipt is inconsistent; review again.",
        ));
    }
    Ok(())
}

#[cfg(test)]
thread_local! { static INTERRUPT_AFTER_CHECKPOINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(test)]
pub(super) fn interrupt_after_checkpoint() {
    INTERRUPT_AFTER_CHECKPOINT.with(|flag| flag.set(true));
}

fn copy_xattrs(
    source: &File,
    destination: &File,
    path: &Path,
    report: &mut CopyReport,
    verify: bool,
) -> io::Result<()> {
    use std::{ffi::CString, os::fd::AsRawFd};
    let acl_mode = if path.as_os_str().is_empty() {
        0o700
    } else {
        std::os::unix::fs::MetadataExt::mode(&source.metadata()?) & 0o777
    };
    // SAFETY: descriptor stays live; null/zero first call obtains the bound.
    let length = unsafe { libc::flistxattr(source.as_raw_fd(), std::ptr::null_mut(), 0) };
    if length < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            return crate::portable_transfer::acl::copy(source, destination, verify, acl_mode);
        }
        return Err(error);
    }
    if length > 1024 * 1024 {
        return Err(changed(
            "Extended attribute names exceed the home-transfer bound.",
        ));
    }
    let mut names = vec![0_u8; length as usize];
    // SAFETY: allocated output buffer is exactly the advertised capacity.
    let got =
        unsafe { libc::flistxattr(source.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    names.truncate(got as usize);
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        if matches!(
            name,
            b"system.posix_acl_access" | b"system.posix_acl_default"
        ) {
            continue; // Independently validated and restored above.
        }
        if !name.starts_with(b"user.") {
            report.metadata_limitations.push(format!("{}: OS security attribute {} was not restored; the new system supplies its own access policy.",path.display(),String::from_utf8_lossy(name)));
            continue;
        }
        let name = CString::new(name).map_err(io::Error::other)?;
        // SAFETY: descriptor and NUL-terminated attribute name remain live.
        let length =
            unsafe { libc::fgetxattr(source.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if length < 0 {
            return Err(io::Error::last_os_error());
        }
        if length > 1024 * 1024 {
            return Err(changed(
                "Extended attribute value exceeds the home-transfer bound.",
            ));
        }
        let mut value = vec![0_u8; length as usize];
        // SAFETY: allocated value output buffer has the advertised size.
        let got = unsafe {
            libc::fgetxattr(
                source.as_raw_fd(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if got < 0 {
            return Err(io::Error::last_os_error());
        }
        value.truncate(got as usize);
        let mut actual = vec![0_u8; value.len()];
        // SAFETY: live descriptor/name and a bounded writable output buffer.
        let got = unsafe {
            libc::fgetxattr(
                destination.as_raw_fd(),
                name.as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
            )
        };
        if got >= 0 && got as usize == value.len() && actual == value {
            // This also handles an interruption after permissions were restored
            // but before the completion receipt: no write permission is needed
            // to validate an already-copied user attribute.
            continue;
        }
        if verify {
            return Err(changed(
                "Imported extended attribute changed; review retained data.",
            ));
        }
        // SAFETY: input name/value buffers and destination descriptor stay live.
        if unsafe {
            libc::fsetxattr(
                destination.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // Restore ACL/base modes after user attributes so read-only modes do not block them.
    crate::portable_transfer::acl::copy(source, destination, verify, acl_mode)
}

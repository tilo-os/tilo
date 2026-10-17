use super::storage::{Builder, Sequence, Store};
use super::*;
use crate::migration_review::storage::{IndexValue, SortedIndex};
const CUSTODY: &[&str] = &[".config/tilo", ".local/state/tilo"];
#[derive(Serialize, Deserialize)]
pub(super) struct Name;
impl IndexValue for Name {
    const RECORD_BYTES: usize = 128 * 1024;
}
pub(super) fn directory_identity(directory: &PinnedDirectory) -> io::Result<durable::Identity> {
    durable::identity(&durable::readable_directory(&directory.file)?)
}
fn regular(parent: &PinnedDirectory, name: &OsStr) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    if !file.metadata()?.is_file() {
        return Err(changed("Quarantine file was replaced"));
    }
    Ok(file)
}
pub(super) fn names(
    directory: &PinnedDirectory,
    store: &Arc<Store>,
    cancel: &AtomicBool,
) -> io::Result<SortedIndex<Name>> {
    directory.validate()?;
    let mut names = SortedIndex::with_scratch(store.scratch());
    for entry in cap_primitives::fs::read_base_dir(&directory.file)? {
        super::super::super::migration::check_cancel(cancel)?;
        names.insert(PathBuf::from(entry?.file_name()), Name, cancel)?;
    }
    names.finish(cancel)?;
    directory.validate()?;
    Ok(names)
}
fn leaf(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    metadata: &cap_primitives::fs::Metadata,
    cancel: &AtomicBool,
) -> io::Result<Fact> {
    if metadata.is_dir() {
        return Err(changed("Directory facts require retained ordered metadata"));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o6000 != 0 {
        return Err(changed(
            "Quarantine entry has foreign ownership or privilege bits",
        ));
    }
    let mode = metadata.mode() & 0o777;
    let mut digest = Sha256::new();
    digest.update(mode.to_le_bytes());
    let identity = if metadata.is_file() {
        digest.update(b"file");
        digest.update(metadata.len().to_le_bytes());
        let mut file = regular(parent, name)?;
        let identity = durable::identity(&file)?;
        if cap_primitives::fs::Metadata::from_file(&file)?.ino() != metadata.ino() {
            return Err(changed("Quarantine file changed while opening"));
        }
        let mut buffer = [0u8; 8192];
        let mut count = 0u64;
        loop {
            super::super::super::migration::check_cancel(cancel)?;
            let size = file.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
            count = count
                .checked_add(size as u64)
                .ok_or_else(|| changed("Quarantine byte count overflow"))?;
        }
        let after = cap_primitives::fs::Metadata::from_file(&file)?;
        if count != metadata.len()
            || after.len() != metadata.len()
            || after.mtime() != metadata.mtime()
            || after.mtime_nsec() != metadata.mtime_nsec()
            || after.mode() & 0o777 != mode
        {
            return Err(changed("Quarantine file changed while verifying"));
        }
        identity
    } else if metadata.is_symlink() {
        digest.update(b"link");
        digest.update(
            cap_primitives::fs::read_link_contents(&parent.file, Path::new(name))?
                .as_os_str()
                .as_bytes(),
        );
        durable::link_identity(&parent.file, name)?
    } else {
        return Err(changed(
            "Special files cannot be adopted from a home quarantine",
        ));
    };
    parent.validate()?;
    Ok(Fact {
        identity,
        digest: digest.finalize().into(),
        mode,
        directory: false,
    })
}
// Generated-seed callers require a single regular-file fact, not subtree paths.
pub(super) fn fact(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
) -> io::Result<(Fact, Vec<NativePath>)> {
    let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
    Ok((
        leaf(parent, name, &metadata, &AtomicBool::new(false))?,
        vec![NativePath::encode(path)],
    ))
}
fn inspect(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
    store: &Arc<Store>,
    mut output: Option<&mut Builder<NativePath>>,
    cancel: &AtomicBool,
) -> io::Result<Fact> {
    super::super::super::migration::check_cancel(cancel)?;
    if path.components().count() > 128 {
        return Err(changed("Home adoption tree exceeds depth bounds"));
    }
    let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
    if let Some(items) = &mut output {
        items.push(NativePath::encode(path))?;
    }
    if !metadata.is_dir() {
        return leaf(parent, name, &metadata, cancel);
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o6000 != 0 {
        return Err(changed(
            "Quarantine entry has foreign ownership or privilege bits",
        ));
    }
    let mode = metadata.mode() & 0o777;
    let mut digest = Sha256::new();
    digest.update(mode.to_le_bytes());
    digest.update(b"directory");
    let directory = parent.open_child(name, &metadata)?;
    if directory.identity.dev() != parent.identity.dev() {
        return Err(changed("Quarantine contains another mounted filesystem"));
    }
    let ordered = names(&directory, store, cancel)?;
    for child in ordered.iter() {
        let child = child?.0.into_os_string();
        let fact = inspect(
            &directory,
            &child,
            &path.join(&child),
            store,
            output.as_deref_mut(),
            cancel,
        )?;
        digest.update(child.as_bytes().len().to_le_bytes());
        digest.update(child.as_bytes());
        digest.update(fact.digest);
    }
    directory.validate()?;
    parent.validate()?;
    Ok(Fact {
        identity: directory_identity(&directory)?,
        digest: digest.finalize().into(),
        mode,
        directory: true,
    })
}
pub(super) fn fact_in(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
    store: &Arc<Store>,
    cancel: &AtomicBool,
) -> io::Result<Fact> {
    inspect(parent, name, path, store, None, cancel)
}
pub(super) fn capture(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
    store: &Arc<Store>,
    cancel: &AtomicBool,
) -> io::Result<(Fact, Sequence<NativePath>)> {
    let mut items = store.builder();
    let fact = inspect(parent, name, path, store, Some(&mut items), cancel)?;
    Ok((fact, items.finish()))
}
pub(super) fn review(
    source: &Arc<PinnedDirectory>,
    target: &Arc<PinnedDirectory>,
    home: &Arc<PinnedDirectory>,
    relative: &Path,
    entries: &mut Builder<Entry>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let seeds = seeds::Classifier::review(home)?;
    review_children(source, target, relative, entries, cancel, &seeds)
}
fn review_children(
    source: &Arc<PinnedDirectory>,
    target: &Arc<PinnedDirectory>,
    relative: &Path,
    entries: &mut Builder<Entry>,
    cancel: &AtomicBool,
    seeds: &seeds::Classifier,
) -> io::Result<()> {
    let store = entries.store();
    let ordered = names(source, &store, cancel)?;
    for name in ordered.iter() {
        super::super::super::migration::check_cancel(cancel)?;
        let name = name?.0.into_os_string();
        let path = relative.join(&name);
        let metadata = metadata_nofollow(&source.file, Path::new(&name))?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o6000 != 0 {
            return Err(changed(
                "Quarantine entry has foreign ownership or privilege bits",
            ));
        }
        let parent_mode = source.identity.mode() & 0o777;
        let protected = CUSTODY.iter().any(|protected| path == Path::new(protected));
        let target_metadata = if protected {
            None
        } else {
            match metadata_nofollow(&target.file, Path::new(&name)) {
                Ok(value) => Some(value),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            }
        };
        if let Some(target_metadata) = &target_metadata {
            if metadata.is_dir() && target_metadata.is_dir() {
                let from = source.open_child(&name, &metadata)?;
                if from.identity.dev() != source.identity.dev() {
                    return Err(changed("Quarantine contains another mounted filesystem"));
                }
                let to = target.open_child(&name, target_metadata)?;
                to.validate_destination()?;
                let mut item = store.builder();
                item.push(NativePath::encode(&path))?;
                entries.push(Entry {
                    path: NativePath::encode(&path),
                    fact: shallow_directory(source, &name)?.0,
                    items: item.finish(),
                    parent_mode,
                    kind: Kind::MergedDirectory {
                        target: shallow_directory(target, &name)?.0,
                    },
                    seed: None,
                    structure: None,
                })?;
                review_children(&from, &to, &path, entries, cancel, seeds)?;
                continue;
            }
        }
        let (source_fact, items) = capture(source, &name, &path, &store, cancel)?;
        let kind = if protected {
            Kind::InactiveCustody
        } else if target_metadata.is_some() {
            Kind::Collision {
                target: target_fact(target, &name, &path)?.0,
            }
        } else {
            Kind::Move
        };
        let mut entry = Entry {
            path: NativePath::encode(&path),
            fact: source_fact,
            items,
            parent_mode,
            kind,
            seed: None,
            structure: None,
        };
        seeds.classify(&mut entry)?;
        if matches!(entry.kind, Kind::Move) && structure::needed(&path) {
            entry.structure = Some(structure::capture(source, &name, &path, &store, cancel)?);
            if fact_in(source, &name, &path, &store, cancel)? != entry.fact {
                return Err(changed(
                    "Quarantine changed while reviewing structural proof",
                ));
            }
        }
        entries.push(entry)?;
    }
    source.validate()?;
    target.validate_destination()?;
    Ok(())
}
pub(super) fn shallow_directory(
    root: &Arc<PinnedDirectory>,
    name: &OsStr,
) -> io::Result<(Fact, Vec<NativePath>)> {
    let metadata = metadata_nofollow(&root.file, Path::new(name))?;
    let child = root.open_child(name, &metadata)?;
    let mode = metadata.mode() & 0o777;
    Ok((
        Fact {
            identity: directory_identity(&child)?,
            digest: Sha256::digest(mode.to_le_bytes()).into(),
            mode,
            directory: true,
        },
        Vec::new(),
    ))
}
pub(super) fn target_fact(
    root: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
) -> io::Result<(Fact, Vec<NativePath>)> {
    if metadata_nofollow(&root.file, Path::new(name))?.is_dir() {
        shallow_directory(root, name)
    } else {
        fact(root, name, path)
    }
}
pub(super) fn items(
    root: &Arc<PinnedDirectory>,
    relative: &Path,
    store: &Arc<Store>,
    cancel: &AtomicBool,
) -> io::Result<Sequence<NativePath>> {
    fn walk(
        root: &Arc<PinnedDirectory>,
        relative: &Path,
        store: &Arc<Store>,
        items: &mut Builder<NativePath>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if relative.components().count() > 128 {
            return Err(changed("Home adoption tree exceeds depth bounds"));
        }
        let ordered = names(root, store, cancel)?;
        for name in ordered.iter() {
            super::super::super::migration::check_cancel(cancel)?;
            let name = name?.0.into_os_string();
            let path = relative.join(&name);
            items.push(NativePath::encode(&path))?;
            let metadata = metadata_nofollow(&root.file, Path::new(&name))?;
            if metadata.is_dir() {
                let child = root.open_child(&name, &metadata)?;
                if child.identity.dev() != root.identity.dev() {
                    return Err(changed("Quarantine contains another filesystem"));
                }
                walk(&child, &path, store, items, cancel)?;
            }
        }
        root.validate()
    }
    let mut items = store.builder();
    walk(root, relative, store, &mut items, cancel)?;
    Ok(items.finish())
}

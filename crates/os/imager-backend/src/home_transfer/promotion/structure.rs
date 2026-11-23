//! Original metadata outside the independently verified Flatpak installation.
use super::storage::{Builder, Row, Sequence, Store};
use super::*;
use crate::portable_transfer::acl;
const INSTALLATION: &str = ".local/share/flatpak";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Item {
    path: NativePath,
    fact: Fact,
    acl: Option<[Option<Vec<u8>>; 2]>,
    gid: u32,
}
pub(super) fn needed(path: &Path) -> bool {
    Path::new(INSTALLATION).starts_with(path)
}
impl Row for Item {
    fn encode(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(io::Error::other)
    }
    fn decode(bytes: &[u8], _: &Arc<Store>) -> io::Result<Self> {
        serde_json::from_slice(bytes).map_err(io::Error::other)
    }
}
pub(super) fn capture(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
    store: &Arc<Store>,
    cancel: &AtomicBool,
) -> io::Result<Sequence<Item>> {
    let mut items = store.builder();
    walk(parent, name, path, &mut items, 0, cancel)?;
    Ok(items.finish())
}
fn one(parent: &Arc<PinnedDirectory>, name: &OsStr, path: &Path) -> io::Result<Item> {
    let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
    // SAFETY: geteuid only reads the current process's credentials.
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o6000 != 0 {
        return Err(changed(
            "Relocated metadata ownership or privilege bits changed",
        ));
    }
    let fact = if metadata.is_dir() {
        tree::shallow_directory(parent, name)?.0
    } else {
        tree::fact(parent, name, path)?.0
    };
    let policy = if metadata.is_symlink() {
        None
    } else {
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
        if durable::identity(&file)? != fact.identity
            || std::os::unix::fs::MetadataExt::mode(&file.metadata()?) & 0o777 != fact.mode
        {
            return Err(changed(
                "Relocation structure changed while opening metadata",
            ));
        }
        let before = file.metadata()?;
        let policy = acl::policy(&file)?;
        let after = file.metadata()?;
        if std::os::unix::fs::MetadataExt::ctime(&before)
            != std::os::unix::fs::MetadataExt::ctime(&after)
            || std::os::unix::fs::MetadataExt::ctime_nsec(&before)
                != std::os::unix::fs::MetadataExt::ctime_nsec(&after)
        {
            return Err(changed("Relocation ACL changed during review"));
        }
        Some(policy)
    };
    Ok(Item {
        path: NativePath::encode(path),
        fact,
        acl: policy,
        gid: metadata.gid(),
    })
}
fn walk(
    parent: &Arc<PinnedDirectory>,
    name: &OsStr,
    path: &Path,
    output: &mut Builder<Item>,
    depth: usize,
    cancel: &AtomicBool,
) -> io::Result<()> {
    super::super::super::migration::check_cancel(cancel)?;
    if depth > 128 || output.len() >= ENTRIES {
        return Err(changed("Relocation structure exceeds bounds"));
    }
    let item = one(parent, name, path)?;
    let directory = item.fact.directory;
    output.push(item)?;
    if directory && path != Path::new(INSTALLATION) {
        let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
        let pin = parent.open_child(name, &metadata)?;
        let names = tree::names(&pin, &output.store(), cancel)?;
        for name in names.iter() {
            let name = name?.0.into_os_string();
            walk(&pin, &name, &path.join(&name), output, depth + 1, cancel)?;
        }
        pin.validate()?;
    }
    Ok(())
}
pub(super) fn validate(items: &Sequence<Item>, entry: &Entry) -> io::Result<()> {
    if items.is_empty() || items.len() > ENTRIES {
        return Err(changed("Missing bounded relocation structure"));
    }
    let root = decoded(&entry.path)?;
    let mut required = entry
        .items
        .iter()
        .filter_map(|item| match item.and_then(|p| decoded(&p)) {
            Ok(path) if path.starts_with(INSTALLATION) && path != Path::new(INSTALLATION) => None,
            result => Some(result),
        });
    let mut previous = None;
    for item in items.iter() {
        let item = item?;
        let path = decoded(&item.path)?;
        if !path.starts_with(&root)
            || item.fact.mode > 0o777
            || previous.as_ref().is_some_and(|p| p >= &path)
        {
            return Err(changed("Invalid relocation structural receipt"));
        }
        if let Some(policy) = &item.acl {
            acl::validate_policy(policy)?;
        }
        if required.next().transpose()?.as_ref() != Some(&path) {
            return Err(changed("Relocation structure omits original metadata"));
        }
        previous = Some(path);
    }
    if required.next().transpose()?.is_some() {
        return Err(changed("Relocation structure omits original metadata"));
    }
    Ok(())
}
pub(super) fn verify(
    items: &Sequence<Item>,
    entry: &Entry,
    home: &Arc<PinnedDirectory>,
) -> io::Result<()> {
    validate(items, entry)?;
    for expected in items.iter() {
        let expected = expected?;
        let path = decoded(&expected.path)?;
        let parent = super::parent(
            home,
            path.parent()
                .ok_or_else(|| changed("Missing structural parent"))?,
        )?;
        let actual = one(
            &parent,
            path.file_name()
                .ok_or_else(|| changed("Missing structural name"))?,
            &path,
        )?;
        if actual.fact != expected.fact || actual.acl != expected.acl || actual.gid != expected.gid
        {
            return Err(changed(
                "Original relocated inode, permissions, ACL or non-application data changed",
            ));
        }
    }
    Ok(())
}

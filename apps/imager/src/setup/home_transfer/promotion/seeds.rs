//! Root-attested firstboot defaults may be retained before imported originals move.
use super::*;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
const PROOFS: &str = "/var/lib/tilo/firstboot-home-seeds";
const SEED_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    inode: u64,
    uid: u32,
    mode: u32,
    bytes: u64,
    mtime_ns: i64,
    ctime_ns: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Generated {
    path: String,
    stamp: Stamp,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    schema: u32,
    home_inode: u64,
    filesystem: u64,
    uid: u32,
    files: Vec<Generated>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Witness {
    proof_sha256: String,
    generated: Generated,
    target: Fact,
}
fn filesystem(home: &File) -> io::Result<u64> {
    let mut result = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: fstatvfs initializes this output on success; the FD is live.
    if unsafe { libc::fstatvfs(home.as_raw_fd(), result.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the output.
    Ok(unsafe { result.assume_init() }.f_fsid)
}
fn read(home: &Arc<PinnedDirectory>) -> io::Result<Option<(Proof, String)>> {
    // SAFETY: geteuid only reads this process's credentials.
    let uid = rustix::process::geteuid().as_raw();
    read_at(&Path::new(PROOFS).join(format!("{uid}.json")), home)
}
fn read_at(path: &Path, home: &Arc<PinnedDirectory>) -> io::Result<Option<(Proof, String)>> {
    let mut current = PathBuf::from("/");
    for component in path.components().skip(1) {
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            return Err(changed(
                "Firstboot seed proof namespace is not root controlled",
            ));
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > SEED_LIMIT || before.nlink() != 1 {
        return Err(changed("Firstboot seed proof exceeds bounds"));
    }
    let mut bytes = Vec::new();
    (&file).take(SEED_LIMIT + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len() || stamp(&before)? != stamp(&after)? {
        return Err(changed("Firstboot seed proof changed"));
    }
    let proof: Proof = serde_json::from_slice(&bytes)?;
    let metadata = durable::readable_directory(&home.file)?.metadata()?;
    if proof.schema != 1 || proof.files.len() > 4096 {
        return Err(changed("Firstboot seed proof schema exceeds bounds"));
    }
    if proof.uid != metadata.uid()
        || proof.home_inode != metadata.ino()
        || proof.filesystem != filesystem(&home.file)?
    {
        // A Desktop home or test quarantine is not the account home created
        // by this proof. Its existing files stay ordinary safe conflicts.
        return Ok(None);
    }
    let mut paths = BTreeSet::new();
    for item in &proof.files {
        relative(&item.path)?;
        if !paths.insert(&item.path)
            || item.stamp.uid != proof.uid
            || item.stamp.bytes > SEED_LIMIT
            || item.stamp.mode > 0o777
            || item.sha256.len() != 64
            || !item
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(changed("Firstboot seed proof contains invalid items"));
        }
    }
    Ok(Some((proof, hex::encode(Sha256::digest(bytes)))))
}
fn stamp(metadata: &std::fs::Metadata) -> io::Result<Stamp> {
    fn ns(seconds: i64, fraction: i64) -> io::Result<i64> {
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(fraction))
            .ok_or_else(|| changed("Firstboot seed timestamp exceeds bounds"))
    }
    Ok(Stamp {
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode() & 0o777,
        bytes: metadata.len(),
        mtime_ns: ns(metadata.mtime(), metadata.mtime_nsec())?,
        ctime_ns: ns(metadata.ctime(), metadata.ctime_nsec())?,
    })
}
fn relative(value: &str) -> io::Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 4096
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.starts_with(".config/tilo")
        || path.starts_with(".local/state/tilo")
    {
        return Err(changed("Invalid generated home seed path"));
    }
    Ok(path)
}
fn matches(home: &Arc<PinnedDirectory>, item: &Generated) -> io::Result<bool> {
    let path = relative(&item.path)?;
    let directory = parent(
        home,
        path.parent()
            .ok_or_else(|| changed("Missing seed parent"))?,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| changed("Missing seed name"))?;
    matches_at(&directory, name, item, false)
}
fn matches_at(
    directory: &Arc<PinnedDirectory>,
    name: &OsStr,
    item: &Generated,
    preserved: bool,
) -> io::Result<bool> {
    let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)?;
    let before = file.metadata()?;
    let original_stamp = stamp(&before)?;
    let mut comparable = original_stamp.clone();
    if preserved {
        // rename changes ctime; inode, owner, mode, mtime and bytes remain bound.
        comparable.ctime_ns = item.stamp.ctime_ns;
    }
    if !before.is_file() || before.nlink() != 1 || comparable != item.stamp {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    (&mut file).take(SEED_LIMIT + 1).read_to_end(&mut bytes)?;
    directory.validate_destination()?;
    Ok(bytes.len() as u64 == item.stamp.bytes
        && stamp(&file.metadata()?)? == original_stamp
        && hex::encode(Sha256::digest(bytes)) == item.sha256)
}
pub(super) struct Classifier {
    home: Arc<PinnedDirectory>,
    proof: Option<(Proof, String)>,
}
impl Classifier {
    pub(super) fn review(home: &Arc<PinnedDirectory>) -> io::Result<Self> {
        Ok(Self {
            home: home.clone(),
            proof: read(home)?,
        })
    }
    pub(super) fn classify(&self, entry: &mut Entry) -> io::Result<()> {
        let Some((proof, digest)) = &self.proof else {
            return Ok(());
        };
        let Kind::Collision { target } = &entry.kind else {
            return Ok(());
        };
        let path = decoded(&entry.path)?;
        let Some(generated) = proof
            .files
            .iter()
            .find(|item| Path::new(&item.path) == path)
        else {
            return Ok(());
        };
        if matches(&self.home, generated)? {
            entry.seed = Some(Witness {
                proof_sha256: digest.clone(),
                generated: generated.clone(),
                target: target.clone(),
            });
            entry.kind = Kind::Move;
        }
        Ok(())
    }
}

fn authority(home: &Arc<PinnedDirectory>, witness: &Witness) -> io::Result<()> {
    let (proof, digest) =
        read(home)?.ok_or_else(|| changed("Firstboot seed authority disappeared"))?;
    if digest != witness.proof_sha256 || !proof.files.contains(&witness.generated) {
        return Err(changed(
            "Firstboot seed authority differs from reviewed operation",
        ));
    }
    Ok(())
}
fn retained(home: &Arc<PinnedDirectory>, create: bool) -> io::Result<Arc<PinnedDirectory>> {
    let custody = parent(home, Path::new(".local/state/tilo"))?;
    let name = OsStr::new("firstboot-preserved");
    let directory = match metadata_nofollow(&custody.file, Path::new(name)) {
        Ok(metadata) => custody.open_child(name, &metadata)?,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            custody.create_child(name)?
        }
        Err(error) => return Err(error),
    };
    directory.validate_destination()?;
    destination_security::check_private_file(&directory.file)?;
    Ok(directory)
}
fn backup_name(witness: &Witness) -> std::ffi::OsString {
    hex::encode(Sha256::digest(witness.generated.path.as_bytes())).into()
}
pub(super) fn validate_witness(path: &Path, witness: &Witness) -> io::Result<()> {
    if relative(&witness.generated.path)? != path
        || witness.target.directory
        || witness.target.mode > 0o777
    {
        return Err(changed(
            "Firstboot seed witness does not bind the adoption path",
        ));
    }
    Ok(())
}
pub(super) fn verify_retained(home: &Arc<PinnedDirectory>, witness: &Witness) -> io::Result<()> {
    authority(home, witness)?;
    let backup = retained(home, false)?;
    let name = backup_name(witness);
    if !matches_at(&backup, &name, &witness.generated, true)?
        || tree::fact(&backup, &name, Path::new(&name))?.0 != witness.target
    {
        return Err(changed("Preserved firstboot seed changed"));
    }
    Ok(())
}
pub(super) fn preserve(
    home: &Arc<PinnedDirectory>,
    path: &Path,
    witness: &Witness,
) -> io::Result<()> {
    validate_witness(path, witness)?;
    authority(home, witness)?;
    let backup = retained(home, true)?;
    let name = backup_name(witness);
    if metadata_nofollow(&backup.file, Path::new(&name)).is_ok() {
        return verify_retained(home, witness);
    }
    if !matches(home, &witness.generated)? {
        return Err(changed(
            "Firstboot seed was edited; target remains a conflict",
        ));
    }
    let directory = parent(
        home,
        path.parent()
            .ok_or_else(|| changed("Missing seed parent"))?,
    )?;
    let source_name = CString::new(
        path.file_name()
            .ok_or_else(|| changed("Missing seed name"))?
            .as_bytes(),
    )
    .map_err(io::Error::other)?;
    let backup_name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: pinned parent FDs and terminated names remain live. No existing backup is replaced.
    if unsafe {
        libc::renameat2(
            directory.file.as_raw_fd(),
            source_name.as_ptr(),
            backup.file.as_raw_fd(),
            backup_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    durable::sync_directory(&directory.file)?;
    durable::sync_directory(&backup.file)?;
    verify_retained(home, witness)
}

pub(super) fn verify_pending(
    home: &Arc<PinnedDirectory>,
    path: &Path,
    witness: &Witness,
) -> io::Result<()> {
    validate_witness(path, witness)?;
    authority(home, witness)?;
    let backup = match retained(home, false) {
        Ok(backup) => Some(backup),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let name = backup_name(witness);
    let preserved = backup
        .as_ref()
        .is_some_and(|backup| metadata_nofollow(&backup.file, Path::new(&name)).is_ok());
    let directory = parent(
        home,
        path.parent()
            .ok_or_else(|| changed("Missing seed parent"))?,
    )?;
    let source_name = path
        .file_name()
        .ok_or_else(|| changed("Missing seed name"))?;
    if preserved {
        verify_retained(home, witness)?;
        match metadata_nofollow(&directory.file, Path::new(source_name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(changed("Adoption target appeared after seed preservation")),
        }
    } else if matches(home, &witness.generated)?
        && tree::fact(&directory, source_name, path)?.0 == witness.target
    {
        Ok(())
    } else {
        Err(changed("Unchanged firstboot seed custody was lost"))
    }
}

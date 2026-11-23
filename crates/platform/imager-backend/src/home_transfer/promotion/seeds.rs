//! Root-attested firstboot defaults may be retained before imported originals move.
use super::*;
pub(super) use crate::home_transfer::seeds::RecoveryBinding;
use crate::home_transfer::seeds::{Authority, Generated, relative, stamp};
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
const SEED_LIMIT: u64 = 4 * 1024 * 1024;

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
    proof: Option<Authority>,
}
impl Classifier {
    pub(super) fn review(home: &Arc<PinnedDirectory>) -> io::Result<Self> {
        Ok(Self {
            home: home.clone(),
            proof: Authority::read(&home.file)?,
        })
    }
    pub(super) fn classify(&self, entry: &mut Entry) -> io::Result<()> {
        let Some(proof) = &self.proof else {
            return Ok(());
        };
        let Kind::Collision { target } = &entry.kind else {
            return Ok(());
        };
        let path = decoded(&entry.path)?;
        let Some(generated) = proof.generated(&path) else {
            return Ok(());
        };
        if matches(&self.home, generated)? {
            entry.seed = Some(RecoveryBinding {
                proof_sha256: proof.digest().to_owned(),
                generated: generated.clone(),
                target: target.clone(),
            });
            entry.kind = Kind::Move;
        }
        Ok(())
    }
}

fn retained(home: &Arc<PinnedDirectory>, create: bool) -> io::Result<Arc<PinnedDirectory>> {
    let ownership = parent(home, Path::new(".local/state/tilo"))?;
    let name = OsStr::new("firstboot-preserved");
    let directory = match metadata_nofollow(&ownership.file, Path::new(name)) {
        Ok(metadata) => ownership.open_child(name, &metadata)?,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            ownership.create_child(name)?
        }
        Err(error) => return Err(error),
    };
    directory.validate_destination()?;
    destination_security::check_private_file(&directory.file)?;
    Ok(directory)
}
pub(super) fn validate_proof(path: &Path, proof: &RecoveryBinding) -> io::Result<()> {
    proof.validate(path)
}
pub(super) fn verify_retained(
    home: &Arc<PinnedDirectory>,
    proof: &RecoveryBinding,
) -> io::Result<()> {
    proof.verify_authority(&home.file)?;
    let backup = retained(home, false)?;
    let name = proof.backup_name();
    if !matches_at(&backup, &name, &proof.generated, true)?
        || tree::fact(&backup, &name, Path::new(&name))?.0 != proof.target
    {
        return Err(changed("Preserved firstboot seed changed"));
    }
    Ok(())
}
pub(super) fn preserve(
    home: &Arc<PinnedDirectory>,
    path: &Path,
    proof: &RecoveryBinding,
) -> io::Result<()> {
    validate_proof(path, proof)?;
    proof.verify_authority(&home.file)?;
    let backup = retained(home, true)?;
    let name = proof.backup_name();
    if metadata_nofollow(&backup.file, Path::new(&name)).is_ok() {
        return verify_retained(home, proof);
    }
    if !matches(home, &proof.generated)? {
        return Err(changed(
            "Firstboot seed was edited; target remains a conflict",
        ));
    }
    let directory = parent(
        home,
        path.parent()
            .ok_or_else(|| changed("Missing seed parent"))?,
    )?;
    let source_name = path
        .file_name()
        .ok_or_else(|| changed("Missing seed name"))?;
    host_fs::rename_noreplace_at(
        directory.file.as_fd(),
        source_name,
        backup.file.as_fd(),
        OsStr::new(&name),
    )?;
    durable::sync_directory(&directory.file)?;
    durable::sync_directory(&backup.file)?;
    verify_retained(home, proof)
}

pub(super) fn verify_pending(
    home: &Arc<PinnedDirectory>,
    path: &Path,
    proof: &RecoveryBinding,
) -> io::Result<()> {
    validate_proof(path, proof)?;
    proof.verify_authority(&home.file)?;
    let backup = match retained(home, false) {
        Ok(backup) => Some(backup),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let name = proof.backup_name();
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
        verify_retained(home, proof)?;
        match metadata_nofollow(&directory.file, Path::new(source_name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(changed("Adoption target appeared after seed preservation")),
        }
    } else if matches(home, &proof.generated)?
        && tree::fact(&directory, source_name, path)?.0 == proof.target
    {
        Ok(())
    } else {
        Err(changed("Unchanged firstboot seed ownership was lost"))
    }
}

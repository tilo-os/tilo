use super::*;
use crate::migration::PublishMode;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};

fn name(operation: &str) -> io::Result<String> {
    if !operation_id(operation) {
        return Err(refusal("Invalid migration operation ID."));
    }
    Ok(format!("{operation}.migration.json"))
}
fn parent(directory: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let parent = PinnedDirectory::open_absolute(directory)?;
    parent.validate_destination()?;
    super::super::destination_security::check_private_file(&parent.file)?;
    Ok(parent)
}
pub(super) fn write(plan: &Plan, directory: &Path) -> io::Result<PathBuf> {
    let bytes = plan.bytes()?;
    let name = name(&plan.operation_id)?;
    let parent = parent(directory)?;
    super::super::migration::OwnedTemporary::create(&parent, std::ffi::OsStr::new(&name))?
        .publish(&bytes, PublishMode::IfAbsent)?;
    Ok(directory.join(name))
}
pub(super) fn read(directory: &Path, operation: &str) -> io::Result<Plan> {
    let parent = parent(directory)?;
    let name = name(operation)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(&name), &options)?;
    super::super::destination_security::check_private_file(&file)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_BYTES as u64 {
        return Err(refusal(
            "Migration plan is not a bounded private regular file.",
        ));
    }
    let bytes = crate::record::read_bounded(file, MAX_BYTES as u64, || {
        refusal("Migration plan exceeds its bound.")
    })?;
    parent.validate_destination()?;
    let plan = Plan::parse(&bytes)?;
    if plan.operation_id != operation {
        return Err(refusal(
            "Migration operation ID does not match its private record.",
        ));
    }
    Ok(plan)
}

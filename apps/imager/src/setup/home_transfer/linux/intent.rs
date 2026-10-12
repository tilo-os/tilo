//! Immutable review saved privately before the byte worker starts. Recovery
//! must present the grants again and match them to this exact review.
use super::super::super::migration_store::NativePath;
use super::*;
use std::io::Read;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    source: NativePath,
    destination: NativePath,
    source_identity: durability::Identity,
    parent_identity: durability::Identity,
    revision: [u8; 32],
    summary: InventorySummary,
}

pub(super) fn save(review: &HomeReview) -> io::Result<()> {
    let path = review.journal.with_extension("home-intent");
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| changed("Home intent has no parent."))?,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| changed("Home intent has no name."))?;
    match metadata_nofollow(&parent.file, Path::new(name)) {
        Ok(_) => {
            let saved = recover(&review.source, &review.destination, &review.journal)?;
            if saved.revision != review.revision
                || saved.source_identity != review.source_identity
                || saved.parent_identity != review.parent_identity
            {
                return Err(changed("Existing home review differs from current grants"));
            }
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut temporary = migration::OwnedTemporary::create(&parent, name)?;
    serde_json::to_writer(
        &mut temporary.file,
        &Intent {
            version: 1,
            source: NativePath::encode(&review.source),
            destination: NativePath::encode(&review.destination),
            source_identity: review.source_identity.clone(),
            parent_identity: review.parent_identity.clone(),
            revision: review.revision,
            summary: review.summary.clone(),
        },
    )?;
    durability::sync_file(&temporary.file)?;
    temporary.validate()?;
    cap_primitives::fs::hard_link(
        &parent.file,
        Path::new(&temporary.name),
        &parent.file,
        Path::new(name),
    )?;
    durability::sync_directory(&parent.file)
}

pub(super) fn recover(source: &Path, destination: &Path, journal: &Path) -> io::Result<HomeReview> {
    let review = load(source, destination, journal)?;
    review.validate()?;
    Ok(review)
}
pub(super) fn recover_adoption(
    source: &Path,
    destination: &Path,
    journal: &Path,
    witness: &super::super::promotion::AdoptionWitness,
    prefix: &Path,
) -> io::Result<HomeReview> {
    let review = load(source, destination, journal)?;
    let parent = prefix
        .parent()
        .ok_or_else(|| changed("Missing application subtree parent"))?;
    let mapped = PinnedDirectory::open_absolute(&witness.mapped_path(parent)?)?;
    if directory_identity(&mapped)? != review.parent_identity {
        return Err(changed(
            "Original application copy parent changed during adoption",
        ));
    }
    review.validate_source()?;
    Ok(review)
}
fn load(source: &Path, destination: &Path, journal: &Path) -> io::Result<HomeReview> {
    let path = journal.with_extension("home-intent");
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| changed("Home intent has no parent."))?,
    )?;
    parent.validate_destination()?;
    super::super::super::destination_security::check_private_file(&parent.file)?;
    let name = path
        .file_name()
        .ok_or_else(|| changed("Home intent has no name."))?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    super::super::super::destination_security::check_private_file(&file)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 16384 {
        return Err(changed("Invalid home review record."));
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    let intent: Intent = serde_json::from_slice(&bytes)?;
    if intent.version != 1
        || intent.source.decode().map_err(|error| changed(&error))? != source
        || intent
            .destination
            .decode()
            .map_err(|error| changed(&error))?
            != destination
    {
        return Err(changed(
            "Supplied grants do not match the saved home review.",
        ));
    }
    let review = HomeReview {
        source: source.to_owned(),
        destination: destination.to_owned(),
        journal: journal.to_owned(),
        revision: intent.revision,
        source_identity: intent.source_identity,
        parent_identity: intent.parent_identity,
        summary: intent.summary,
    };
    Ok(review)
}

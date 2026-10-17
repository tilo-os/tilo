//! Exact standalone Flatpak grants. Recovery cannot infer a new selection from
//! whichever directories happen to remain in the destination.
use super::{TransferRequest, durable, flatpak, storage};
use crate::setup::{home_transfer::HomeReview, migration_durability::Identity};
use std::path::Path;
const RECEIPT: &str = "user-transfer-flatpak-namespace.json";
const SKELETON: [&str; 4] = ["", ".local", ".local/share", ".var"];
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    schema: u32,
    skeleton: Vec<Identity>,
    present: Vec<bool>,
}
fn subtrees(request: &TransferRequest) -> Result<Vec<flatpak::DataSubtree>, String> {
    flatpak::data_subtrees(
        &request.source_home,
        &request.target_home,
        &request.private_journals,
        false,
    )
}
fn present(path: &Path) -> Result<bool, String> {
    let mut prefix = std::path::PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => {}
            Ok(_) => {
                return Err("Flatpak subtree traverses a non-directory or symbolic link".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(true)
}
pub(super) fn save(request: &TransferRequest) -> Result<(), String> {
    let receipt = Namespace {
        schema: 1,
        skeleton: SKELETON
            .iter()
            .map(|path| storage::target_identity(&request.target_home.join(path)))
            .collect::<Result<_, _>>()?,
        present: subtrees(request)?
            .iter()
            .map(|tree| present(&tree.source))
            .collect::<Result<_, _>>()?,
    };
    durable::write_artifact(
        request,
        RECEIPT,
        &serde_json::to_value(receipt).map_err(|error| error.to_string())?,
    )
}
fn validate(request: &TransferRequest) -> Result<Namespace, String> {
    let value = durable::read_artifact(request, RECEIPT)?
        .ok_or("Original Flatpak namespace receipt is missing")?;
    let receipt: Namespace = serde_json::from_value(value).map_err(|error| error.to_string())?;
    let trees = subtrees(request)?;
    if receipt.schema != 1
        || receipt.skeleton.len() != SKELETON.len()
        || receipt.present.len() != trees.len()
    {
        return Err("Flatpak namespace receipt is unsupported".into());
    }
    for (path, identity) in SKELETON.iter().zip(&receipt.skeleton) {
        let target = request.target_home.join(path);
        let pinned = crate::setup::migration::PinnedDirectory::open_absolute(&target)
            .map_err(|error| error.to_string())?;
        crate::setup::destination_security::check_private_file(&pinned.file)
            .map_err(|error| error.to_string())?;
        if storage::target_identity(&target)? != *identity {
            return Err("Flatpak target namespace was replaced".into());
        }
    }
    for (tree, expected) in trees.iter().zip(&receipt.present) {
        if present(&tree.source)? != *expected {
            return Err("Flatpak source selection changed".into());
        }
    }
    if present(&request.target_home.join(".local/share/flatpak"))? {
        let installation = crate::setup::migration::PinnedDirectory::open_absolute(
            &request.target_home.join(".local/share/flatpak"),
        )
        .map_err(|error| error.to_string())?;
        installation
            .validate_destination()
            .map_err(|error| error.to_string())?;
    }
    // Only the installation subtree may gain signed application outputs. Every
    // other path is covered by a copied data journal or must remain absent.
    for (parent, allowed) in [
        ("", vec![".local", ".var"]),
        (".local", vec!["share"]),
        (".local/share", vec!["flatpak"]),
        (
            ".var",
            if receipt.present.get(1) == Some(&true) {
                vec!["app"]
            } else {
                vec![]
            },
        ),
    ] {
        let pin = crate::setup::migration::PinnedDirectory::open_absolute(
            &request.target_home.join(parent),
        )
        .map_err(|error| error.to_string())?;
        let directory = cap_std::fs::Dir::from_std_file(
            crate::setup::migration_durability::readable_directory(&pin.file)
                .map_err(|error| error.to_string())?,
        );
        for entry in directory.entries().map_err(|error| error.to_string())? {
            let name = entry.map_err(|error| error.to_string())?.file_name();
            if !allowed.iter().any(|allowed| name == *allowed) {
                return Err("Unreviewed files were added outside the Flatpak installation".into());
            }
        }
        pin.validate_destination()
            .map_err(|error| error.to_string())?;
    }
    Ok(receipt)
}
pub(super) fn validate_grants(request: &TransferRequest) -> Result<(), String> {
    validate(request).map(|_| ())
}
pub(super) fn recover(request: &TransferRequest) -> Result<Vec<HomeReview>, String> {
    let receipt = validate(request)?;
    subtrees(request)?
        .iter()
        .zip(receipt.present)
        .filter(|(_, present)| *present)
        .map(|(tree, _)| HomeReview::recover(&tree.source, &tree.destination, &tree.journal))
        .collect()
}
pub(super) fn verify(
    request: &TransferRequest,
    copies: &[HomeReview],
) -> Result<Vec<crate::setup::migration::CopyReport>, String> {
    let receipt = validate(request)?;
    if copies.len() != receipt.present.iter().filter(|present| **present).count() {
        return Err("Flatpak subtree proof is missing".into());
    }
    copies
        .iter()
        .map(|copy| {
            if copy.destination() == request.target_home.join(".local/share/flatpak") {
                copy.verify_flatpak_installation()
            } else {
                copy.verify_completed_tree()
            }
        })
        .collect()
}
pub(super) fn recover_adoption(
    request: &TransferRequest,
    witness: &crate::setup::home_transfer::promotion::AdoptionWitness,
) -> Result<Vec<HomeReview>, String> {
    witness.validate_application_namespace()?;
    let value = durable::read_artifact(request, RECEIPT)?
        .ok_or("Original application namespace receipt is missing")?;
    let receipt: Namespace = serde_json::from_value(value).map_err(|e| e.to_string())?;
    let trees = subtrees(request)?;
    if receipt.schema != 1
        || receipt.skeleton.len() != SKELETON.len()
        || receipt.present.len() != trees.len()
    {
        return Err("Application namespace receipt changed".into());
    }
    for (path, expected) in SKELETON.iter().zip(&receipt.skeleton) {
        if storage::target_identity(
            &witness
                .mapped_path(Path::new(path))
                .map_err(|e| e.to_string())?,
        )? != *expected
        {
            return Err("Mapped application namespace was replaced".into());
        }
    }
    let mut copies = Vec::new();
    for (tree, expected) in trees.iter().zip(&receipt.present) {
        if present(&tree.source)? != *expected {
            return Err("Original application subtree selection changed".into());
        }
        if *expected {
            let prefix = tree
                .destination
                .strip_prefix(&request.target_home)
                .map_err(|e| e.to_string())?;
            copies.push(HomeReview::recover_adoption(
                &tree.source,
                &tree.destination,
                &tree.journal,
                witness,
                prefix,
            )?);
        }
    }
    Ok(copies)
}
pub(super) fn verify_promotion(
    request: &TransferRequest,
    copies: &[HomeReview],
    applications: bool,
) -> Result<(), String> {
    validate(request)?;
    for copy in copies {
        let report = if applications
            && copy.destination() == request.target_home.join(".local/share/flatpak")
        {
            copy.verify_flatpak_installation()?
        } else {
            copy.verify_completed_tree()?
        };
        if !report.complete() {
            return Err("Application copy fidelity is incomplete".into());
        }
    }
    Ok(())
}

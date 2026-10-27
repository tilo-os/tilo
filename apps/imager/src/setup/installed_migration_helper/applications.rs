//! App-only phase derives consent from the exact retained installed plan. It
//! uses actual read-only mount authority, never an existing writable actor.
use super::*;
use sha2::{Digest, Sha256};

pub(super) fn review(grants: Grants) -> Result<Session, String> {
    let original =
        Plan::read_private(&grants.plans, &grants.operation).map_err(|error| error.to_string())?;
    let plan = original
        .applications_copy_plan()
        .map_err(|error| error.to_string())?;
    let receipts =
        child(&grants.workspace, "receipts-applications").map_err(|error| error.to_string())?;
    let source = migration_handoff::mount_read_only(&plan).map_err(|error| error.to_string())?;
    imager_backend::setup::deferred_handoff::import_selected_from_source(
        &plan,
        source.path(),
        &grants.home,
        &grants.workspace,
        super::current_person(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .map_err(|error| error.to_string())?;
    let quarantine = grants.workspace.join("imported-applications");
    let adoption_journal = grants.workspace.join("adoption-applications.journal");
    let mut session = match std::fs::symlink_metadata(&adoption_journal) {
        Ok(_) => {
            let (transfer, promotion) = source.recover_promotion(
                &quarantine,
                &receipts,
                &grants.home,
                &adoption_journal,
            )?;
            let mut session =
                Session::from_transfer(grants.operation, grants.home, grants.workspace, transfer)?;
            let preview = promotion.preview()?;
            session.pending = serde_json::json!({"event":"ready-to-adopt","operation":session.operation,"recovered":true,"install_items":preview.installed.len(),"collisions":preview.collisions.len(),"inactive_custody":preview.inactive_custody.len()});
            session.promotion = Some(promotion);
            session.copy_attempted = true;
            session.adoption_attempted = true;
            session.state = "ready-to-adopt";
            session
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let transfer = source.review_transfer(&quarantine, &receipts)?;
            Session::from_transfer(grants.operation, grants.home, grants.workspace, transfer)?
        }
        Err(error) => return Err(error.to_string()),
    };
    session.adoption_journal = adoption_journal;
    session.applications_only = true;
    // Keep immutable original consent alongside the source/copy receipts. The
    // derived app-only plan never replaces the user's retained-access policy.
    let bytes = serde_json::to_vec(&original).map_err(|error| error.to_string())?;
    let digest = hex::encode(Sha256::digest(bytes));
    session.application_plan_digest = Some(digest.clone());
    let value =
        serde_json::json!({"schema":1,"operation":session.operation,"original_plan_sha256":digest});
    publish(&session.workspace, "applications-consent.json", &value)?;
    Ok(session)
}
fn publish(root: &Path, name: &str, value: &serde_json::Value) -> Result<(), String> {
    let pin = PinnedDirectory::open_absolute(root).map_err(|error| error.to_string())?;
    pin.validate_destination()
        .map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    super::super::user_transfer::publish_private_artifact(&pin, name, &bytes)
        .map_err(|error| error.to_string())
}
pub(super) fn publish_completion(session: &Session) -> Result<(), String> {
    let pin =
        PinnedDirectory::open_absolute(&session.workspace).map_err(|error| error.to_string())?;
    let consent =
        super::super::user_transfer::read_private_artifact(&pin, "applications-consent.json")
            .map_err(|error| error.to_string())?;
    let consent: serde_json::Value =
        serde_json::from_slice(&consent).map_err(|error| error.to_string())?;
    let digest = session
        .application_plan_digest
        .as_ref()
        .ok_or("Missing source-bound application consent")?;
    if consent
        != serde_json::json!({"schema":1,"operation":session.operation,"original_plan_sha256":digest})
    {
        return Err("Application consent changed before completion".into());
    }
    let value = serde_json::json!({"schema":1,"operation":session.operation,"original_plan_sha256":digest,"complete":true});
    publish(&session.workspace, "applications-complete.json", &value)
}

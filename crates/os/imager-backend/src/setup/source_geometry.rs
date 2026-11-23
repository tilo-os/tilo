//! Canonical source disk geometry retained by offline preparation actors.
use std::io;
pub fn original_source_plans(plan: &super::migration_handoff::Plan) -> io::Result<Vec<Vec<u8>>> {
    plan.validate()?;
    // This is a geometry check, after validating the full retained selection.
    // Home geometry does not need to embed the complete public app inventory;
    // every installation source geometry is still independently checked below.
    let mut geometry = plan.clone();
    geometry.flatpak_inventory = None;
    let mut plans = vec![geometry.bytes()?];
    if plan.categories.flatpak {
        let inventory = plan
            .flatpak_inventory
            .as_ref()
            .ok_or_else(|| io::Error::other("Pending selected installations are missing"))?;
        let inventory =
            super::flatpak_transfer::HandoffInventory::parse(&serde_json::to_vec(inventory)?)
                .map_err(io::Error::other)?;
        inventory
            .validate_operation(&plan.operation_id)
            .map_err(io::Error::other)?;
        for (_, bytes) in inventory.sources() {
            plans.push(bytes.to_vec());
        }
    }
    Ok(plans)
}

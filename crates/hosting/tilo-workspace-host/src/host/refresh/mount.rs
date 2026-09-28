//! A new worker runtime starts from a normally committed current checkpoint.
use super::*;

pub(in crate::host) fn before_action_mount(
    owner: &mut SessionOwner,
    committed: &CommittedAuthority,
    pool: Option<&crate::host_bytes::HostBytes>,
) -> Result<(), String> {
    let refresh = |view: &crate::committed_authority::CommittedView, owner: &mut SessionOwner| {
        let time = view.policy_time()?;
        view.check_policy_time(&time)?;
        owner
            .refresh_checkpoint_with_policy_time(&time)
            .map(|_| ())
            .map_err(|error| error.to_string())
    };
    if let Some(pool) = pool {
        let mut scope = owner
            .prepare_owner_scope(pool, crate::owner::resources::OwnerOperation::Checkpoint)
            .map_err(|error| error.to_string())?;
        owner
            .with_owner_view_scope(&mut scope, committed, refresh)
            .map_err(|error| error.to_string())?
    } else {
        committed
            .with_owner_view(owner, refresh)
            .map_err(|error| error.to_string())?
    }
}

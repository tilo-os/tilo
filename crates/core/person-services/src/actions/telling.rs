//! What this host offers an agent of a person's on that person's log of
//! what they were told: the actions of `tilo.telling` it may call, as rows
//! of the catalogue it reads. A page asks the host directly and is offered
//! no row.
//!
//! The agent tells its person wherever it works for them, and says whether
//! what it told must not be missed. Reading what they were told, and
//! acknowledging it, is theirs alone, and no row.

use tilo_actions::remote::CatalogueRow;
use tilo_resource::Effect as _;
use tilo_workspace_service::workspaces::actions::{DeclaredRow as _, Publisher};

use crate::person_telling::{self, ToldAction};

/// A person's log of what they were told, as their host publishes it.
pub(super) const TELLING: Publisher = Publisher {
    provider: person_telling::PROVIDER,
    kind: "tilo.telling",
};

/// The rows an agent of a person's is offered: telling its person and
/// marking what it told as not to be missed.
pub(super) fn agent_rows() -> Vec<CatalogueRow> {
    ToldAction::declarations()
        .into_iter()
        .filter(|decl| person_telling::offered_to_agent(decl.id))
        .map(|decl| decl.row(TELLING, person_telling::TOLD, "Told"))
        .collect()
}

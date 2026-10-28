//! Native structural operator design and bounded engineering evidence viewer.

mod evidence_flow_view;
mod launch;
mod model;
mod operator_graph;
mod operator_graph_view;
mod placement_view;
mod resource;
mod verification;
mod view;

pub use model::{Assumptions, AttentionCache, StorageReport};
pub use operator_graph::{Connection, OperatorKind, OperatorNode, OperatorState};
pub use resource::{Project, ProjectState};
pub use verification::{CheckStatus, ReceiptStatus, VerificationReceipt, VerificationRecord};
pub use view::ChipDesignShell;

/// The same resource application can be mounted by native and browser hosts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ChipDesignShell)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod operator_graph_tests;

#[cfg(test)]
mod matrix_graph_tests;

#[cfg(test)]
mod architecture_study_tests;

#[cfg(test)]
mod evidence_flow_tests;

#[cfg(test)]
mod placement_tests;

#[cfg(test)]
mod launch_tests;

#[cfg(test)]
mod window_bar_tests;

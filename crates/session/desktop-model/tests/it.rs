// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "active_group.rs"]
pub mod active_group;
#[path = "admitted_projection.rs"]
pub mod admitted_projection;
#[path = "agent_membership.rs"]
pub mod agent_membership;
#[path = "apps.rs"]
pub mod apps;
#[path = "capability.rs"]
pub mod capability;
#[path = "collaborator.rs"]
pub mod collaborator;
#[path = "commands.rs"]
pub mod commands;
#[path = "desktop_doc.rs"]
pub mod desktop_doc;
#[path = "peer_window_merge.rs"]
pub mod peer_window_merge;
#[path = "portable_state.rs"]
pub mod portable_state;
#[path = "projection.rs"]
pub mod projection;
#[path = "room_schema.rs"]
pub mod room_schema;
#[path = "topology.rs"]
pub mod topology;

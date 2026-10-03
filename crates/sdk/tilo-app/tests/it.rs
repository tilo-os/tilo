// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "action_groups.rs"]
pub mod action_groups;
#[path = "control_custody_raw_holder.rs"]
pub mod control_custody_raw_holder;
#[path = "framework.rs"]
pub mod framework;
#[path = "history_actions.rs"]
pub mod history_actions;
#[path = "identity.rs"]
pub mod identity;
#[path = "incremental.rs"]
pub mod incremental;
#[path = "item_actions.rs"]
pub mod item_actions;
#[path = "launch.rs"]
pub mod launch;
#[path = "live_permissions.rs"]
pub mod live_permissions;
#[path = "model_contract.rs"]
pub mod model_contract;
#[path = "scenarios.rs"]
pub mod scenarios;
#[path = "typed_edit_labels.rs"]
pub mod typed_edit_labels;

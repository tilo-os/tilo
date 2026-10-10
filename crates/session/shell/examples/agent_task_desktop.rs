//! A signed-in person's desktop with one empty Notes notebook, written into
//! a `scripts/headless-examples/spawn-headless.sh` profile for the agent-task
//! benchmark (`scripts/perf/agent_task.py`, TIL-759).
//!
//! The same desktop `tests/idle_wakeups.rs` measures: the demo tree cut down
//! to its first workspace and its Notes tab, the tab's record created before
//! the compositor starts (native startup admits only existing records), and a
//! completed single-device identity ceremony, so the desktop is past first-run
//! onboarding. Nothing here runs on the live path: a person's desktop starts
//! empty, and this profile exists only for the benchmark that seeds it.
//!
//! ```text
//! agent_task_desktop --profile DIR
//! ```
//!
//! writes `DIR/state.json`, the tab record under `DIR/data` (the profile's
//! `XDG_DATA_HOME`) and the identity under `DIR/config/tilo/identity` (its
//! `XDG_CONFIG_HOME`), then prints, as one JSON object on stdout, the
//! environment the compositor must be started with to restore it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use getrandom::{SysRng, rand_core::UnwrapErr};
use threshold_identity::ThresholdIdentity;
use tilo_identity_ceremony::IdentityRecord;

/// The native session's launcher-less record scope, named explicitly so an
/// inherited package or workspace variable cannot redirect the records.
const RECORD_SCOPE: &str = "00000000-0000-0000-0000-000000000000";

fn main() -> Result<(), String> {
    let profile = profile_argument()?;
    let state_path = profile.join("state.json");
    let tree = notes_desktop()?;
    shell::shell::persist::save_tree_to(&tree, &state_path)
        .map_err(|error| format!("save the session: {error}"))?;
    seed_tab_records(&profile.join("data"), &tree)?;
    seed_identity(&profile.join("config"))?;
    let environment = serde_json::json!({
        "TILO_STATE_PATH": state_path,
        "TILO_PACKAGE_ID": shell::shell::SHELL_RECORDS_PACKAGE,
        "TILO_WORKSPACE_ID": RECORD_SCOPE,
        "TILO_WORKSPACE_ROOTS": "",
        "TILO_HEADLESS_SIZE": "1920x1080",
        // Nobody touches the desktop between steps; it must not lock.
        "TILO_IDLE_LOCK_SECS": "0",
        "TILO_IDLE_BLANK_SECS": "0",
    });
    println!("{environment}");
    Ok(())
}

fn profile_argument() -> Result<PathBuf, String> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(dir), None) if flag == "--profile" => {
            let dir = PathBuf::from(dir);
            if !dir.is_dir() {
                return Err(format!("--profile {} is not a directory", dir.display()));
            }
            Ok(dir)
        }
        _ => Err("usage: agent_task_desktop --profile DIR".into()),
    }
}

/// The demo's first workspace with only its Notes tab; an untouched demo
/// tree would be migrated away as legacy sample data on load.
fn notes_desktop() -> Result<shell::shell::state::WorkspaceTree, String> {
    let mut tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
    tree.desktop.workspaces.truncate(1);
    let home = tree
        .desktop
        .workspaces
        .first_mut()
        .ok_or("the demo tree has no workspace")?;
    home.name = "Agent task".into();
    home.collaborators.clear();
    let group = home
        .strips
        .first_mut()
        .and_then(|strip| strip.window_groups.first_mut())
        .ok_or("the demo workspace has no window group")?;
    group.tabs.retain(|tab| tab.title == "Notes");
    group.active_tab = group
        .tabs
        .first()
        .ok_or("the demo workspace has no Notes tab")?
        .id;
    Ok(tree)
}

/// Create the saved tabs' documents, then release them so the compositor
/// can open them.
fn seed_tab_records(
    data_home: &Path,
    tree: &shell::shell::state::WorkspaceTree,
) -> Result<(), String> {
    let mut owners = ui::record::RecordOwners::new(data_home.to_path_buf());
    let records = owners
        .open(shell::shell::SHELL_RECORDS_PACKAGE, RECORD_SCOPE)
        .map_err(|error| format!("open the Shell record namespace: {error}"))?;
    for tab in tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        shell::shell::tab_documents::create_scratch_tab_record(&records, tab.id)
            .map_err(|error| format!("create the tab's document: {error}"))?;
    }
    records
        .try_flush()
        .map_err(|error| format!("persist the tabs' documents: {error}"))?;
    drop(records);
    drop(owners);
    Ok(())
}

/// A completed identity ceremony: without one the desktop is in first-run
/// onboarding, which is not the desktop an agent works on.
fn seed_identity(config_home: &Path) -> Result<(), String> {
    let identity_dir = config_home.join("tilo/identity");
    let identity = ThresholdIdentity::bootstrap_single_device(&mut UnwrapErr(SysRng))
        .map_err(|error| format!("create the identity: {error}"))?;
    let record = IdentityRecord::from_threshold_identity(
        &identity,
        "Agent task",
        [128, 128, 128],
        SystemTime::UNIX_EPOCH,
    )
    .map_err(|error| format!("encode the identity: {error}"))?;
    let bytes = record
        .to_json_bytes()
        .map_err(|error| format!("serialize the identity: {error}"))?;
    let provider = runtime_identity::BrokerFileProvider::new(&identity_dir);
    runtime_identity::import_identity_with_provider(&identity_dir, "primary", &bytes, &provider)
        .map_err(|error| format!("persist the identity: {error}"))?;
    Ok(())
}

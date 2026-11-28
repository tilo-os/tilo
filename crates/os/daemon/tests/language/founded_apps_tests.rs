//! A freshly founded workspace takes the first write of each default app, on a device that runs
//! those apps from their signed packages and decides its own writes as the daemon's program
//! does: against the surfaces the installed packages ship, beside the records the system keeps
//! for them. A write the workspace does not take says why in the words the daemon logs.

use serde_json::json;

use crate::language::files_support::person_files;
use crate::language::host;

/// The default apps a computer's image installs, by their directories under `apps/tl`.
const INSTALLED: &[&str] = &[
    "passwords",
    "files",
    "chat",
    "tasks",
    "clocks",
    "contacts",
    "settings",
];

/// The built-in apps whose roots the workspace makes the first time a person reads them, as the
/// session's search and launcher read every app's.
const ROOTED: &[&str] = &[
    "so.tilo.chat",
    "so.tilo.tasks",
    "so.tilo.clocks",
    "so.tilo.contacts",
];

#[test]
fn a_founded_workspace_takes_the_first_write_of_each_installed_default_app() {
    host::run_deciding(INSTALLED, |host| async move {
        let workspace = host.workspace().to_owned();
        let daemon = host.daemon();
        let workspaces = daemon.workspaces();
        let mut refused = Vec::new();

        let opened = workspaces.open_vault(&workspace).await.unwrap();
        let entry = accounts::SecretItem::login("Example", "ada", "correct horse battery staple");
        if let Err(error) = workspaces
            .write_vault(
                &opened,
                vec![accounts::vault_items::VaultChange::Keep(entry)],
                None,
                None,
            )
            .await
        {
            refused.push(format!("the vault: {error}"));
        }
        if let Err(error) = person_files(
            &host,
            &workspace,
            "new-folder",
            json!({"parent": "", "name": "First"}),
        )
        .await
        {
            refused.push(format!("the Files tree: {error}"));
        }
        for app in ROOTED {
            if let Err(error) = workspaces.root_app(&workspace, app, None).await {
                refused.push(format!("{app}'s root: {error}"));
            }
        }
        assert!(
            refused.is_empty(),
            "a founded workspace refused its default apps' first writes:\n{}",
            refused.join("\n")
        );
    });
}

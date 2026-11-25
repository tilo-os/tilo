//! This device's migrations: the one migration engine served as the device's own actions,
//! provider `so.tilo.migrate` on `devices/<this device>/migrate` ([`tilo_migrate::provider`]).
//!
//! A migration runs where the data it reads is: this device's folders, browsers and Thunderbird,
//! and the services it connects to. People, their agents, `tilo migrate` and a migration's view
//! call the same actions through the device's one registry of providers. A run lands in the
//! device's workspace, or another the device is a member of that the step names, as the
//! migration's own agent acting for the person there, each run one History entry, through a
//! session this daemon opens for it in its own process; what only the person may do (keeping a
//! mail account and its sign-in) goes through a session of theirs in that workspace.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow};
use serde_json::Value;
use tilo_actions::remote::Caller;
use tilo_migrate::engine::mcp::{Mcp, Rpc};
use tilo_migrate::engine::state::Place;
use tilo_migrate::engine::writer::Workspace;
use tilo_migrate::provider::{Host, Migrations, PROVIDER, resource};
use zeroize::Zeroizing;

use super::services::Daemon;
use crate::RuntimeServices as _;

/// The name the migration's agent acts under in the workspace.
const AGENT: &str = "migrate";

/// Apps whose secrets this device still reads from the old keeper: the App Publisher's engine
/// signs with the key kept there.
const STILL_KEPT_HERE: &[&str] = &[tilo_app_id::apps::PUBLISHER];

/// Whether an entry of the old keeper belongs to an app the move carried over. An app keeps its
/// entries under its own package id; the device's own work keeps its secrets under ids no
/// package names, and those stay where the device reads them.
fn carried_over(app: &str) -> bool {
    tilo_app_id::apps::FIRST_PARTY_PACKAGES.contains(&app) && !STILL_KEPT_HERE.contains(&app)
}

/// Walk every entry `keeper` holds, handing each app's secret to `adopt`, and take the entry out
/// once `adopt` answered. The secret is handed on and never written down: no log line, record or
/// history carries it. An entry `adopt` refused stays, to be adopted on a later start.
fn walk_old_keeper(workspace: &str, person: &str, keeper: &dyn OldKeeper, adopt: &mut Adopt<'_>) {
    let entries = match keeper.entries() {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(%error, "the old keeper's entries cannot be listed");
            return;
        }
    };
    for (app, purpose, item) in entries {
        if !carried_over(&app) {
            continue;
        }
        let Some(secret) = keeper.held(&app, &purpose, &item) else {
            continue;
        };
        match adopt(&app, &purpose, &item, secret) {
            Ok(()) => match keeper.forget(&app, &purpose, &item) {
                Ok(()) => {
                    tracing::debug!(
                        %app,
                        %purpose,
                        %item,
                        workspace,
                        person,
                        "an old keeper entry is adopted"
                    );
                }
                Err(error) => tracing::warn!(
                    %app,
                    %purpose,
                    %item,
                    %error,
                    "the adopted secret stays in the old keeper"
                ),
            },
            Err(error) => tracing::warn!(
                %app,
                %purpose,
                %item,
                %error,
                "an old keeper entry is not adopted yet; it stays"
            ),
        }
    }
}

/// Adopt one kept secret into `workspace`'s vault as `person`, answering whether the vault
/// keeps it.
type Adopt<'a> = dyn FnMut(&str, &str, &str, Zeroizing<String>) -> Result<(), String> + 'a;

/// The device's old keeper, as its entries are read from it: the platform's app secret store.
trait OldKeeper {
    /// Every entry the keeper holds on this device, as `(app, purpose, item)`.
    fn entries(&self) -> Result<Vec<(String, String, String)>, String>;
    /// The secret `app`'s keeper kept for `purpose` and `item`, when it holds one.
    fn held(&self, app: &str, purpose: &str, item: &str) -> Option<Zeroizing<String>>;
    /// Take the entry out once its secret is in the vault.
    fn forget(&self, app: &str, purpose: &str, item: &str) -> Result<(), String>;
}

/// This device's secret store, the old keeper the native apps kept their secrets in.
struct Secrets;

impl OldKeeper for Secrets {
    fn entries(&self) -> Result<Vec<(String, String, String)>, String> {
        let listed = tilo_secrets::SecretStore::for_app(tilo_app_id::apps::MIGRATE)
            .entries_blocking()
            .map_err(|error| error.to_string())?;
        Ok(listed
            .into_iter()
            .filter_map(|(app, key)| {
                // An item secret's key is its purpose and item, `purpose/item`. A bare key is
                // the device's own, kept apart from any document's item.
                let (purpose, item) = key.split_once('/')?;
                Some((app, purpose.to_owned(), item.to_owned()))
            })
            .collect())
    }

    fn held(&self, app: &str, purpose: &str, item: &str) -> Option<Zeroizing<String>> {
        let secret = tilo_secrets::SecretStore::for_app(app)
            .item(purpose, item)
            .get_blocking()
            .ok()
            .flatten()?;
        let text = std::str::from_utf8(secret.expose()).ok()?.trim();
        (!text.is_empty()).then(|| Zeroizing::new(text.to_owned()))
    }

    fn forget(&self, app: &str, purpose: &str, item: &str) -> Result<(), String> {
        tilo_secrets::SecretStore::for_app(app)
            .item(purpose, item)
            .delete_blocking()
            .map_err(|error| error.to_string())
    }
}

impl Daemon {
    /// Keep the secrets the apps the move carried over still need in the person's vault, as the
    /// person, once: every entry the old keeper holds for an app is tagged with its app, purpose
    /// and item, written through the vault's own `add`, and taken out of the keeper only once
    /// the vault keeps it. An entry the vault already keeps tagged for that app and purpose is
    /// not added again, so a second start adds nothing.
    pub fn adopt_kept_secrets(&self) {
        use std::sync::atomic::Ordering;
        if self.old_secrets.load(Ordering::Acquire) {
            return;
        }
        let Ok(workspace) = self.mcp_workspace(None) else {
            return;
        };
        let workspaces = self.workspaces();
        let held = tilo_rt::app()
            .block_on(workspaces.state(&workspace))
            .ok()
            .flatten()
            .is_some();
        if !held {
            // The person's workspace is not here yet: the first app launched in it founds it,
            // and adopts them then.
            return;
        }
        self.old_secrets.store(true, Ordering::Release);
        let person = hex::encode(self.workspaces().device_cert().user_public_key);
        let workspaces = self.workspaces();
        walk_old_keeper(
            &workspace,
            &person,
            &Secrets,
            &mut |app, purpose, item, secret| {
                tilo_rt::app()
                    .block_on(
                        workspaces.adopt_secret(&workspace, &person, app, purpose, item, secret),
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|adopted| adopted.map(drop))
            },
        );
    }

    /// Serve this device's migrations from now on.
    pub fn serve_migrations(self: &Arc<Self>) {
        let Ok(workspace) = self.mcp_workspace(None) else {
            tracing::warn!("this device names no own workspace, so nothing migrates over it");
            return;
        };
        let host = DaemonHost {
            daemon: Arc::downgrade(self),
            workspace,
            dir: self.dir(),
        };
        self.provide(
            PROVIDER,
            Arc::new(Migrations::new(
                resource(&self.device_key()),
                Arc::new(host),
            )),
        );
    }
}

/// What this daemon gives its migrations.
struct DaemonHost {
    daemon: Weak<Daemon>,
    workspace: String,
    /// The device's directory: its synced folders' settings, and its migrations.
    dir: PathBuf,
}

impl DaemonHost {
    fn daemon(&self) -> Result<Arc<Daemon>> {
        self.daemon
            .upgrade()
            .ok_or_else(|| anyhow!("this device's daemon is stopping"))
    }
}

impl Host for DaemonHost {
    fn workspace(&self) -> String {
        self.workspace.clone()
    }

    fn member(&self, workspace: &str) -> bool {
        self.daemon()
            .is_ok_and(|daemon| daemon.mcp_workspace(Some(workspace)).is_ok())
    }

    fn place(&self, workspace: &str) -> Place {
        Place::at(&self.scratch(workspace))
    }

    fn home(&self) -> PathBuf {
        std::env::var_os("HOME").map_or_else(|| self.dir.clone(), PathBuf::from)
    }

    fn folders(&self) -> BTreeMap<String, PathBuf> {
        tilo_api::daemon::folders::Folders::load(&self.dir)
            .map(|folders| {
                folders
                    .workspaces
                    .into_iter()
                    .filter(|(_, folder)| !folder.removed)
                    .map(|(id, folder)| (id, folder.path))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn scratch(&self, workspace: &str) -> PathBuf {
        self.dir.join("migrate").join(workspace)
    }

    fn open(&self, _: &Caller, workspace: &str) -> Result<Box<dyn Workspace>> {
        let daemon = self.daemon()?;
        let session = daemon.agent_session_in(AGENT, workspace)?;
        let own = Arc::downgrade(&daemon);
        let workspace = workspace.to_owned();
        Ok(Box::new(Mcp::open(
            Box::new(InProcess(session)),
            Box::new(move || {
                let daemon = own
                    .upgrade()
                    .ok_or_else(|| anyhow!("this device's daemon is stopping"))?;
                let (tree, person) = daemon.program_tree_in(&workspace)?;
                Ok(
                    Box::new(InProcess(tilo_mcp::McpServer::admitted(tree, person)?))
                        as Box<dyn Rpc>,
                )
            }),
        )?))
    }
}

/// A session of this daemon's, asked in its own process.
struct InProcess<H>(tilo_mcp::McpServer<H>);

impl<H> Rpc for InProcess<H>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
{
    fn exchange(&mut self, request: Value) -> Result<Option<Value>> {
        // A step of a migration runs on a thread of its own, never one of the runtime's.
        tilo_rt::app()
            .block_on(self.0.handle(request))
            .map_err(|error| anyhow!("the session could not be asked: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    /// The old keeper, in memory: what a test writes is what the walk reads.
    struct Fake {
        held: Mutex<BTreeMap<(String, String, String), String>>,
    }

    impl Fake {
        fn with(entries: &[(&str, &str, &str, &str)]) -> Self {
            let held = entries
                .iter()
                .map(|(app, purpose, item, secret)| {
                    (
                        ((*app).to_owned(), (*purpose).to_owned(), (*item).to_owned()),
                        (*secret).to_owned(),
                    )
                })
                .collect();
            Self {
                held: Mutex::new(held),
            }
        }

        fn holds(&self, app: &str, purpose: &str, item: &str) -> bool {
            self.held.lock().unwrap().contains_key(&(
                app.to_owned(),
                purpose.to_owned(),
                item.to_owned(),
            ))
        }
    }

    impl OldKeeper for Fake {
        fn entries(&self) -> Result<Vec<(String, String, String)>, String> {
            Ok(self.held.lock().unwrap().keys().cloned().collect())
        }

        fn held(&self, app: &str, purpose: &str, item: &str) -> Option<Zeroizing<String>> {
            self.held
                .lock()
                .unwrap()
                .get(&(app.to_owned(), purpose.to_owned(), item.to_owned()))
                .map(|secret| Zeroizing::new(secret.clone()))
        }

        fn forget(&self, app: &str, purpose: &str, item: &str) -> Result<(), String> {
            self.held.lock().unwrap().remove(&(
                app.to_owned(),
                purpose.to_owned(),
                item.to_owned(),
            ));
            Ok(())
        }
    }

    /// A walk hands every app's held entry to `adopt` with its tags, takes it out of the keeper,
    /// and a second walk adds nothing.
    #[test]
    fn every_old_apps_entry_is_adopted_once_and_taken_out() {
        let keeper = Fake::with(&[
            (
                tilo_app_id::apps::NOTES,
                "bot-token",
                "default",
                "made-up-token",
            ),
            (tilo_app_id::apps::TOTP, "seed", "first", "made-up-seed"),
        ]);
        let adopted = Mutex::new(Vec::new());
        let mut adopt = |app: &str, purpose: &str, item: &str, secret: Zeroizing<String>| {
            adopted.lock().unwrap().push((
                app.to_owned(),
                purpose.to_owned(),
                item.to_owned(),
                secret.to_string(),
            ));
            Ok(())
        };
        walk_old_keeper("workspace", "person", &keeper, &mut adopt);
        assert_eq!(
            *adopted.lock().unwrap(),
            [
                (
                    tilo_app_id::apps::NOTES.to_owned(),
                    "bot-token".to_owned(),
                    "default".to_owned(),
                    "made-up-token".to_owned()
                ),
                (
                    tilo_app_id::apps::TOTP.to_owned(),
                    "seed".to_owned(),
                    "first".to_owned(),
                    "made-up-seed".to_owned()
                )
            ],
            "both apps' entries are adopted, each tagged"
        );
        assert!(!keeper.holds(tilo_app_id::apps::NOTES, "bot-token", "default"));
        assert!(!keeper.holds(tilo_app_id::apps::TOTP, "seed", "first"));
        walk_old_keeper("workspace", "person", &keeper, &mut adopt);
        assert_eq!(
            adopted.lock().unwrap().len(),
            2,
            "a second walk adds nothing"
        );
    }

    /// An adoption that failed keeps its entry, so a later start adopts it.
    #[test]
    fn a_refused_adoption_keeps_the_entry() {
        let keeper = Fake::with(&[(
            tilo_app_id::apps::NOTES,
            "bot-token",
            "default",
            "made-up-token",
        )]);
        let mut adopt = |_: &str, _: &str, _: &str, _: Zeroizing<String>| Err("not yet".to_owned());
        walk_old_keeper("workspace", "person", &keeper, &mut adopt);
        assert!(keeper.holds(tilo_app_id::apps::NOTES, "bot-token", "default"));
    }

    /// What the device itself still reads from the keeper is not an app's secret to move: only
    /// the app's entry is adopted.
    #[test]
    fn an_entry_the_device_still_reads_stays_where_it_is() {
        let keeper = Fake::with(&[
            (
                tilo_app_id::apps::PUBLISHER,
                "publisher-key",
                "signing",
                "made-up-key",
            ),
            ("Tilo", "account-vault", "key", "made-up-key"),
            (
                tilo_app_id::apps::NOTES,
                "bot-token",
                "default",
                "made-up-token",
            ),
        ]);
        let adopted = Mutex::new(Vec::new());
        let mut adopt = |app: &str, _: &str, _: &str, _: Zeroizing<String>| {
            adopted.lock().unwrap().push(app.to_owned());
            Ok(())
        };
        walk_old_keeper("workspace", "person", &keeper, &mut adopt);
        assert_eq!(
            *adopted.lock().unwrap(),
            [tilo_app_id::apps::NOTES.to_owned()]
        );
        assert!(keeper.holds(tilo_app_id::apps::PUBLISHER, "publisher-key", "signing"));
        assert!(keeper.holds("Tilo", "account-vault", "key"));
        assert!(!keeper.holds(tilo_app_id::apps::NOTES, "bot-token", "default"));
    }
}

//! Account credentials through the certified native route of the app asking.
//!
//! The session answers the apps it serves for the secrets of the person's workspaces, which no
//! app sandbox reaches, asking the device's daemon, which keeps each workspace's vault. On an
//! app's own route it answers:
//!
//! - Browser: the website sessions the person selected, listed and opened, and forgotten by
//!   someone who administers the workspace: each is one of the workspace's accounts, its cookies
//!   an entry of the workspace's vault, decided where the daemon opens it
//!   ([`tilo_person_services::website_session_asks`]);
//! - Settings, the keeper of the workspace's accounts: keeping an account's password in the
//!   workspace's vault, which it is never released back, and naming the entry it is kept in;
//! - any app's engine: the sign-in of the workspace's account that names the vault entry it
//!   asks for, read from this device's own copy of the workspace's vault
//!   ([`crate::account_passwords`]), for a person whose role lets them use Passwords;
//! - Browser, where this session holds a workspace's Browser: the text of a vault item for
//!   one page action that fills a field from it by reference, which the workspace's server
//!   keeps and decides about ([`crate::item_lending`]). The session keeps no vault for it.
//!
//! Every request is decided on the certificate the broker attested on the
//! app's connection, against the workspace that connection is in: a current
//! device of the person this workspace's identity presents here, who may
//! still read that app's record in the workspace — and, to forget a
//! selection, who administers the workspace. A website session's cookies leave the
//! workspace's vault on the app's own channel only.

use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use std::sync::Arc;
use tilo_app_host::services::{
    BrowserServiceSessionRequestContext as Context, BrowserServiceSessionServices, ItemAnswering,
};
use tilo_desktop_model::WorkspaceId;

const UNAVAILABLE: &str = "Selected website session unavailable";
use ui::apps::BROWSER;

/// The person's account credentials, as the apps it serves reach it on the action API.
pub(crate) struct ServiceSessions {
    /// How the person's workspaces stand, as the daemon publishes them.
    view: Arc<crate::workspace_view::WorkspaceView>,
    /// What Browser's asks for vault items are answered with.
    items: Arc<crate::item_lending::ItemLending>,
    /// The device's daemon, which keeps each workspace's vault and accounts: where an account's
    /// password is kept and released, and its accounts are read.
    daemon: tilo_session_client::daemon_documents::DaemonShell,
}

impl ServiceSessions {
    pub(crate) fn new(
        view: Arc<crate::workspace_view::WorkspaceView>,
        items: Arc<crate::item_lending::ItemLending>,
        daemon: tilo_session_client::daemon_documents::DaemonShell,
    ) -> Self {
        Self {
            view,
            items,
            daemon,
        }
    }

    /// The certified person's own Browser route, readable on Browser's record in the workspace
    /// (and, for `admin`, administered by this workspace), now and rechecked after.
    fn authorize_selection(&self, context: &Context, admin: bool) -> Result<(), String> {
        context.revalidate()?;
        let person = hex::encode(context.identity_public_key());
        if context.app_id() != BROWSER || context.person() != person {
            return Err("Account credentials requires the certified person's own app".into());
        }
        let workspace = WorkspaceId::parse(context.workspace_id())
            .ok_or("Invalid account-credentials workspace")?;
        // The daemon keeps the workspace, and decides whether the certificate's person still
        // reads the app's own record there: what the broker itself asks of a join.
        if admin && !self.view.administers(&workspace.to_string()) {
            return Err("This person no longer administers the workspace".into());
        }
        let route = context.read_context().route();
        tilo_session_client::daemon_documents::DaemonDocuments(self.daemon.clone())
            .authorizes(
                &route.cluster_id,
                &route.app_id,
                &route.document_id,
                context.read_context().certificate(),
            )
            .map_err(|_| "This person can no longer read the app".to_owned())?;
        let selected = tilo_identity::store::selected_actor_id_prompt(context.workspace_id())
            .map_err(|_| "This workspace's selected identity is unavailable")?;
        if selected.as_deref() != Some(context.person()) {
            return Err("This workspace's selected identity changed".into());
        }
        context.revalidate()
    }
}

impl ServiceSessions {
    /// [`BrowserServiceSessionServices::command`], on this thread.
    fn command_now(&self, context: &Context, command: &Command) -> Result<Reply, String> {
        self.authorize_command(context, command)?;
        if !command.valid() {
            return Err("Invalid account credentials request".into());
        }
        if *command == (Command::LendsItems {}) {
            // Answered by what this session is: it says so before its server can be asked.
            return Ok(Reply::ItemsLent {
                lent: self.items.lends(context.workspace_id()),
            });
        }
        // An item's use ([`BrowserServiceSessionServices::item_use`]), an account's password and
        // a website session ([`BrowserServiceSessionServices::answered_later`]) are waited for,
        // and never here.
        Err("This is not answered on the connection's dispatch".into())
    }
}

impl BrowserServiceSessionServices for ServiceSessions {
    fn authorize(&self, context: &Context) -> Result<(), String> {
        self.authorize_selection(context, false)
    }

    fn authorize_command(&self, context: &Context, command: &Command) -> Result<(), String> {
        if !command.browser_only() {
            // An account's password is decided once, by the workspace's vault, for the device
            // that asks ([`tilo_workspace_service::workspaces::Workspaces::account_secret`]).
            return context.revalidate();
        }
        self.authorize_selection(context, matches!(command, Command::Forget { .. }))
    }

    /// Decided on the blocking pool: the workspace's selected identity is read from disk.
    fn command(self: Arc<Self>, context: &Context, command: Command) -> ItemAnswering {
        let context = context.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || self.command_now(&context, &command))
                .await
                .map_err(|error| error.to_string())?
        })
    }

    fn answered_later(&self, context: &Context, command: Command) -> ItemAnswering {
        let ready = |answer: Result<Reply, String>| -> ItemAnswering {
            Box::pin(std::future::ready(answer))
        };
        if !command.valid() {
            return ready(Err("Invalid account credentials request".into()));
        }
        let not_yet = |why: &str| -> ItemAnswering {
            tracing::warn!(target: crate::account_passwords::LOG, app = context.app_id(), why, "an account's password was asked for before this device holds its workspaces");
            ready(Err(
                tilo_person_services::account_logins::VAULT_NOT_HERE.into()
            ))
        };
        if self.daemon.link().is_err() {
            return not_yet("it reaches no device daemon");
        }
        let Ok(tasks) = tilo_rt::try_app() else {
            return not_yet("the session's tasks are not running");
        };
        let daemon = self.daemon.clone();
        let workspace = context.workspace_id().to_owned();
        if command.website_session() {
            let identity = context.identity_public_key();
            let answering = tasks.spawn(async move {
                website_session(&daemon, &workspace, identity, command).await
            });
            return Box::pin(async move { answering.await.map_err(|_| UNAVAILABLE.to_owned())? });
        }
        let app = context.app_id().to_owned();
        let by = crate::account_passwords::Asker {
            person: context.identity_public_key(),
            device: context.read_context().certificate().device_public_key,
        };
        let answering = tasks.spawn(async move {
            crate::account_passwords::answer(&daemon, &workspace, &app, by, command).await
        });
        Box::pin(async move {
            answering
                .await
                .map_err(|_| tilo_person_services::account_logins::VAULT_NOT_HERE.to_owned())?
        })
    }

    fn item_use(&self, context: &Context, command: Command) -> ItemAnswering {
        // Browser's ask is decided as every request of Browser's is: the certified person's
        // own connection, in a workspace this device is still in. What that refuses is told
        // with its reason. Any other app is told that only Browser asks.
        if context.app_id() == BROWSER
            && let Some(invocation) = command.item_use()
            && let Err(reason) = self.authorize_selection(context, false)
        {
            return Box::pin(std::future::ready(Ok(Reply::item_refused(
                invocation, &reason,
            ))));
        }
        self.items
            .answer(context.app_id(), context.workspace_id(), &command)
    }

    fn disconnect(&self, _peer_id: u64) {}
}

/// One of the workspace's website sessions listed, opened or forgotten for the person
/// `identity`, as the daemon decides it where the workspace's vault is
/// (`tilo_workspace_service::workspaces::Workspaces::website_session_ask`).
async fn website_session(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    workspace: &str,
    identity: [u8; 32],
    command: Command,
) -> Result<Reply, String> {
    let answer = crate::account_passwords::vault(
        daemon,
        runtime_collab::wire::shell::VaultCall::WebsiteSession {
            workspace: workspace.to_owned(),
            person: identity,
            command,
        },
    )
    .await?;
    let reply: Reply = serde_json::from_str(&answer).map_err(|_| UNAVAILABLE.to_owned())?;
    if !reply.valid() {
        return Err(UNAVAILABLE.into());
    }
    Ok(reply)
}

//! What a workspace service keeps for a person: their settings and the accounts they connected,
//! the model providers they chose and their models' answers, the vault's logins and secrets as
//! they and their agents use them, what they were told, and what
//! the agents learned about them.
//!
//! All of it is kept in the person's own workspace and its vault, on the workspaces a device
//! holds ([`Workspaces`]). [`PersonServices`] is those workspaces with what is kept here for
//! their people beside them; its actions are registered on the device's one actions registry
//! like any other provider's.
#![cfg(not(target_arch = "wasm32"))]
#![deny(rust_2018_idioms)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::sync::Arc;

use tilo_workspace_service::workspaces::{WeakWorkspaces, Workspaces};

/// The person's mail accounts and model providers, kept where their workspace runs: a page has
/// no mail connection and no vault of its own.
mod model_providers;
pub use model_providers::OpenAiSignIn;
/// A person's own model, asked for whatever this host runs for them.
pub mod model_answers;
pub mod person_learned;
mod person_settings;
/// What the person was told, kept where their workspace runs.
pub mod person_telling;
pub mod this_computer;
pub use person_telling::ToTell;
/// Where a person's devices are reached while their Tilo is closed.
pub use person_telling::addresses as push_addresses;
/// What the person keeps to sign in with, in the workspace's vault.
mod person_vault;
pub use person_vault::{NO_CLIPBOARD, SecretClipboard};
/// The accounts of the workspace, signed in with entries of that vault.
pub mod account_logins;
pub mod accounts;
/// A secret used by reference by a turn this server carries out itself.
pub mod hosted_use;
/// The vault items a workspace's machine hands the programs it starts.
#[cfg(feature = "agents")]
pub mod machine_secrets;
/// The logins a page in a window of an app's own signs in with, kept in that same vault.
pub mod page_logins;
/// The workspace's vault as the person services reach it.
mod vault_at;
/// One vault item's secret handed to a process of an app's own that a turn placed here.
pub mod vault_release;
/// Browser's asks for those website sessions, decided where the vault opens.
#[cfg(feature = "apps")]
pub mod website_session_asks;
/// The website sessions a person selected, each an entry of that vault.
pub mod website_sessions;
pub(crate) use vault_at::VaultAt;
/// The person's providers as the device's actions answer them.
mod actions;
/// The services a person connected by their address.
mod connections;
pub use actions::register;

#[cfg(test)]
tilo_automerge_isolate::test_worker!();

/// What these services' tests build on: the workspace service's fixtures, with the person
/// services registered on every host they open.
#[cfg(test)]
mod fixtures;
/// A call that waits for its person's yes, as the person's services' tests meet it.
#[cfg(test)]
mod waiting;

/// The workspaces a device holds, with what is kept here for their people.
#[derive(Clone)]
pub struct PersonServices {
    workspaces: Workspaces,
    inner: Arc<Kept>,
}

/// What the person services keep beside the workspaces, for as long as they live.
struct Kept {
    /// What answers people's model calls, each with that person's own provider, model and key
    /// ([`model_answers`]).
    models: Arc<model_answers::Models>,
    /// What uses of a vault item by reference keep here ([`hosted_use`]).
    vault_uses: hosted_use::Uses,
    /// What its people were told, one writer at a time ([`person_telling`]).
    telling: person_telling::Telling,
    /// Moves whenever a person's own settings kept here (their mail accounts) changed, so a
    /// window following them reads them again ([`person_settings`]).
    person_settings_moved: tokio::sync::watch::Sender<u64>,
    /// Where this device puts a secret its person copies from the vault
    /// ([`person_vault::SecretClipboard`]): none until its embedder says.
    secret_clipboard: std::sync::OnceLock<Arc<dyn person_vault::SecretClipboard>>,
    /// The dispatch an app's asked use of a vault item is kept under, and the records they are
    /// written through ([`vault_release`]).
    vault_asks: std::sync::OnceLock<(
        Arc<dyn tilo_actions::remote::asked::AskedDispatch>,
        Arc<dyn tilo_actions::remote::asked::AskedRecords>,
    )>,
    /// The device's record of every invocation it was asked for, when this host serves one:
    /// what a model call it answers writes what it cost into
    /// ([`Invocations::note_spend`](tilo_invocations::Invocations::note_spend)).
    invocations: std::sync::OnceLock<Arc<tilo_invocations::Invocations>>,
    /// The calendar server an iCloud account's calendars are asked at, when a test stands one
    /// of its own in ([`PersonServices::keep_icloud_calendars_at`]).
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    icloud_calendars_at: std::sync::RwLock<Option<String>>,
    /// The credential store a test signs drives in through
    /// ([`PersonServices::cloud_sign_in_through`]); the committed registrations otherwise.
    #[cfg(any(test, feature = "test-support"))]
    cloud_sign_in: std::sync::RwLock<Option<account_credentials::CredentialStore>>,
    /// What signs an `Apple ID` in, when a test stands a provider of its own in
    /// ([`PersonServices::icloud_sign_in_through`]); Apple's otherwise.
    #[cfg(any(test, feature = "test-support"))]
    icloud_sign_in: std::sync::RwLock<Option<Arc<dyn account_credentials::icloud::ICloudAuth>>>,
    /// What controls this device's cloud mounts, when a test stands a service of its own in
    /// ([`PersonServices::cloud_control_through`]); the device's files service otherwise.
    #[cfg(any(test, feature = "test-support"))]
    cloud_control: std::sync::RwLock<Option<Arc<dyn person_settings::cloud_control::CloudControl>>>,
}

impl Default for Kept {
    fn default() -> Self {
        let models = Arc::new(model_answers::Models::new());
        Self {
            vault_uses: hosted_use::Uses::proving_within(models.mail_reach()),
            models,
            telling: person_telling::Telling::default(),
            person_settings_moved: tokio::sync::watch::channel(0).0,
            secret_clipboard: std::sync::OnceLock::new(),
            vault_asks: std::sync::OnceLock::new(),
            invocations: std::sync::OnceLock::new(),
            #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
            icloud_calendars_at: std::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-support"))]
            cloud_sign_in: std::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-support"))]
            icloud_sign_in: std::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-support"))]
            cloud_control: std::sync::RwLock::default(),
        }
    }
}

impl PersonServices {
    /// `workspaces` with what is kept on them for their people: the same, however often it is
    /// asked for.
    #[must_use]
    pub fn of(workspaces: &Workspaces) -> Self {
        Self {
            workspaces: workspaces.clone(),
            inner: workspaces.kept::<Kept>(),
        }
    }

    /// The workspaces these services keep for their people on.
    #[must_use]
    pub fn workspaces(&self) -> &Workspaces {
        &self.workspaces
    }

    /// The device's one record of the invocations it was asked for, once its dispatch serves it:
    /// what a turn of an agent is followed by, and what a live activity shows.
    #[must_use]
    pub fn invocations(&self) -> Option<Arc<tilo_invocations::Invocations>> {
        self.inner.invocations.get().cloned()
    }

    /// These services, by weak reference to their workspaces.
    #[must_use]
    pub fn downgrade(&self) -> WeakPersonServices {
        WeakPersonServices(self.workspaces.downgrade())
    }
}

/// [`PersonServices`], by weak reference: what a task keeps without keeping the workspaces alive.
#[derive(Clone)]
pub struct WeakPersonServices(WeakWorkspaces);

impl WeakPersonServices {
    /// The services, while their workspaces are alive.
    #[must_use]
    pub fn upgrade(&self) -> Option<PersonServices> {
        self.0
            .upgrade()
            .map(|workspaces| PersonServices::of(&workspaces))
    }
}

/// The person services are the workspaces they keep for: every call of the workspaces' own
/// reaches them through it.
impl std::ops::Deref for PersonServices {
    type Target = Workspaces;

    fn deref(&self) -> &Workspaces {
        &self.workspaces
    }
}

//! `WhatsApp` through its SDK, under the account's own key.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tilo_messaging::{AccountState, SendError};
use tilo_messaging_protocol::external_broker::{ExternalAuthority, ExternalBroker};
use tilo_messaging_protocol::journal::Owner;
use tilo_messaging_protocol::supervisor::whatsapp::{Network, WhatsappBootstrap, WhatsappLinking};
use tilo_messaging_protocol::supervisor::{AccountAuthority, AccountSupervisor};

use super::consent::{self, Consent, Policy};
use super::guard::{AccountGuard, Running};

/// The SDK's network, allowed only while the person's consent and the
/// account's check both hold.
struct Egress {
    account: Arc<AccountGuard>,
    consent: Arc<Consent>,
}

impl AccountAuthority for Egress {
    fn check(&self) -> Result<(), SendError> {
        self.account.check()?;
        self.consent.check()
    }
}

/// A running `WhatsApp` account.
pub(crate) struct Account {
    guard: Arc<AccountGuard>,
    supervisor: Arc<AccountSupervisor>,
    /// 0: not started; 1: starting; 2: started.
    activation: AtomicU8,
}

impl Running for Account {
    fn guard(&self) -> &Arc<AccountGuard> {
        &self.guard
    }
    fn supervisor(&self) -> &Arc<AccountSupervisor> {
        &self.supervisor
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        self.retire();
    }
}

/// Marks a start that did not finish as failed (or locked, when the check
/// closed), so only an explicit retry starts it again.
struct Activation<'a> {
    account: &'a Account,
    finished: bool,
}

impl Drop for Activation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let state = if self.account.guard.check().is_err() {
                AccountState::Locked
            } else {
                AccountState::Failed
            };
            let _ = self.account.supervisor.unavailable(state);
            self.account.activation.store(0, Ordering::Release);
        }
    }
}

impl Account {
    pub(crate) fn new(guard: Arc<AccountGuard>, supervisor: Arc<AccountSupervisor>) -> Self {
        Self {
            guard,
            supervisor,
            activation: AtomicU8::new(0),
        }
    }

    /// The SDK's linking, once started.
    pub(crate) fn linking(&self) -> Result<Arc<WhatsappLinking>, SendError> {
        self.guard.check()?;
        self.supervisor.whatsapp_linking()
    }

    /// Start the SDK on a reviewed profile and resume the identity it was
    /// linked to. Blocks; call it on a blocking worker.
    pub(crate) fn resume(
        &self,
        root: std::path::PathBuf,
        scope: String,
        expected: &str,
        policy: &Policy,
    ) -> Result<Arc<WhatsappLinking>, SendError> {
        self.start(root, scope, policy)?;
        let session = self.supervisor.whatsapp_resume(expected)?;
        tokio::runtime::Handle::current().block_on(async { session.resume() })?;
        Ok(session)
    }

    /// Start the SDK after the person's network consent. Blocks; call it on
    /// a blocking worker.
    pub(crate) fn start(
        &self,
        root: std::path::PathBuf,
        scope: String,
        policy: &Policy,
    ) -> Result<(), SendError> {
        self.guard.check()?;
        let consent = consent::request(self.guard.identity.clone(), policy)?;
        self.guard.check()?;
        let network = Network::native(Arc::new(Egress {
            account: self.guard.clone(),
            consent,
        }))?;
        if !root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        let network = network.guarded(self.guard.clone());
        tokio::runtime::Handle::current().block_on(self.activate(|authority| {
            assemble(
                &self.guard.identity.owner,
                root,
                scope,
                network,
                authority,
                self.guard.clone(),
            )
        }))
    }

    async fn activate<F, A>(&self, assemble: A) -> Result<(), SendError>
    where
        A: FnOnce(Arc<dyn ExternalAuthority>) -> F + Send,
        F: Future<Output = Result<(Arc<ExternalBroker>, WhatsappBootstrap), SendError>> + Send,
    {
        self.activation
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::AlreadyReserved)?;
        let mut activation = Activation {
            account: self,
            finished: false,
        };
        self.guard.check()?;
        let (broker, bootstrap) = assemble(self.guard.clone()).await?;
        self.guard.check()?;
        self.supervisor.install_whatsapp(broker, bootstrap).await?;
        self.activation.store(2, Ordering::Release);
        activation.finished = true;
        drop(activation);
        Ok(())
    }
}

async fn assemble(
    owner: &Owner,
    root: std::path::PathBuf,
    scope: String,
    network: Network,
    authority: Arc<dyn ExternalAuthority>,
    guard: Arc<AccountGuard>,
) -> Result<(Arc<ExternalBroker>, WhatsappBootstrap), SendError> {
    guard.check()?;
    let bootstrap = WhatsappBootstrap::open_profile(
        &root.join("sdk"),
        owner.clone(),
        guard.credential.profile_key(),
        network,
        guard.clone(),
    )
    .await?;
    guard.check()?;
    let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
        &root.join("journal"),
        owner,
        guard.credential.profile_key(),
    )
    .map_err(|_| SendError::Unavailable)?;
    guard.check()?;
    let history = tilo_messaging_protocol::signal_broker::History::open(
        &root.join("history"),
        owner,
        guard.credential.profile_key(),
    )?;
    guard.check()?;
    let runtime = tilo_messaging_protocol::whatsapp_runtime::WhatsappRuntime::new(
        owner.clone(),
        bootstrap.client(),
        journal,
    )
    .map_err(|_| SendError::Unavailable)?;
    let broker = ExternalBroker::new(Box::new(runtime), history, scope, authority)?;
    guard.check()?;
    Ok((Arc::new(broker), bootstrap))
}

/// What a `WhatsApp` enrollment holds once its SDK runs: the link and, once
/// the person linked, the verified JID.
pub(crate) struct Held {
    linker: Arc<WhatsappLinking>,
    jid: Option<String>,
}

/// `WhatsApp`'s steps in the one enrollment flow.
pub(crate) struct Whatsapp;

impl super::flow::Provider for Whatsapp {
    type Held = Held;
    type Account = Account;
    const SERVICE: tilo_messaging_protocol::Service = tilo_messaging_protocol::Service::Whatsapp;
    const ORIGIN: &'static str = "https://web.whatsapp.com";
    const ORDER: super::flow::Order = super::flow::Order::StartThenLink;

    fn key_title(name: &str) -> String {
        format!("WhatsApp account key for {name}")
    }

    fn begin(
        &self,
        _service: &Arc<super::Service>,
        _identity: &super::identity::Identity,
        held: &mut Option<Held>,
        _account: Option<&Arc<Account>>,
    ) -> Result<tilo_messaging::Pairing, SendError> {
        let linker = held.as_ref().ok_or(SendError::Unavailable)?.linker.clone();
        linker.check_linking()?;
        tokio::runtime::Handle::current().block_on(linker.begin())
    }

    fn finish(
        &self,
        held: &mut Option<Held>,
        attempt: String,
    ) -> Result<Option<String>, SendError> {
        let held = held.as_mut().ok_or(SendError::Unavailable)?;
        let jid = held.linker.finish(&attempt)?;
        let number = jid
            .strip_suffix("@s.whatsapp.net")
            .ok_or(SendError::Revoked)?
            .to_owned();
        held.jid = Some(jid);
        Ok(Some(format!("+{number}")))
    }

    fn approved(
        &self,
        service: &Arc<super::Service>,
        identity: &super::identity::Identity,
        credential: super::identity::Credential,
        _name: &str,
        _held: Option<Held>,
    ) -> Result<(Arc<Account>, String, Option<Held>), SendError> {
        let scope = format!("whatsapp:{}", identity.owner.account);
        let account = service.account(identity, credential, &scope, Account::new)?;
        let root = service.private_root(&identity.owner)?;
        account.start(root, scope.clone(), &service.policy())?;
        let linker = account.linking()?;
        Ok((account, scope, Some(Held { linker, jid: None })))
    }

    fn confirm(
        &self,
        service: &Arc<super::Service>,
        identity: &super::identity::Identity,
        name: &str,
        held: &Held,
        account: &Arc<Account>,
    ) -> Result<(), SendError> {
        account.guard().check()?;
        let jid = held.jid.clone().ok_or(SendError::Revoked)?;
        if account.state() != tilo_messaging::AccountState::Ready {
            return Err(SendError::Revoked);
        }
        service.publish(
            identity,
            crate::session::Publication {
                account: identity.owner.account.clone(),
                name: name.into(),
                generation: identity.owner.linking_generation,
                key: account.guard().credential.item.clone(),
                kind: crate::session::PublicationKind::Whatsapp { jid },
            },
        )?;
        account.guard().check()
    }

    fn resume(
        &self,
        service: &Arc<super::Service>,
        identity: &super::identity::Identity,
        credential: super::identity::Credential,
        scope: String,
        row: &accounts::Account,
    ) -> Result<Arc<Account>, SendError> {
        let expected = row
            .config
            .get("whatsapp_jid")
            .and_then(serde_json::Value::as_str)
            .ok_or(SendError::Revoked)?
            .to_owned();
        let account = service.account(identity, credential, &scope, Account::new)?;
        let root = service.private_root(&identity.owner)?;
        let policy = service.policy();
        let session = account.resume(root, scope, &expected, &policy)?;
        // A resumed session that stops verifying retires its account.
        let watched = Arc::downgrade(&account);
        tokio::runtime::Handle::current().spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let Some(account) = watched.upgrade() else {
                    break;
                };
                if session.verify_resume().is_err() {
                    account.retire();
                }
                if matches!(account.state(), AccountState::Locked | AccountState::Failed) {
                    policy.cancellation.cancel();
                    session.cancel();
                    break;
                }
            }
        });
        Ok(account)
    }

    fn alive(held: &Held) -> bool {
        held.linker.check_linking().is_ok()
    }

    fn cancel(held: &Held) {
        held.linker.cancel();
    }
}

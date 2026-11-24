//! One vault item's secret handed to a process of an app's own ([`person_vault::RELEASE`]).
//!
//! Some protocols need the raw secret inside the app's own process: a gateway login, voice, an
//! SSH or VNC handshake, a database login. A call the vault makes itself (a web call with the
//! key added where the vault is) cannot do that. So the app holds a read grant on that one item,
//! given once by its person as an asked record, and the release is addressed at the item
//! (`vault.item:<id>#release`), so the grant "Approve and grant" writes for it covers that item
//! alone.
//!
//! Whoever runs the process vouches for what it is ([`AppProcess`]): the device's daemon, which
//! started it and knows it by its pid. The item is handed over ([`Workspaces::release_to`]) only
//! where all of these hold:
//!
//! - the process is a sandboxed launch of the app's own package that a turn of work placed here
//!   ([`crate::work::takes_turn`]): today its held connection, while this device holds the
//!   connection's lease. A process apps share (the desktop session, a tilolang process, a
//!   connector running inside the daemon) never receives a secret;
//! - this device may read the workspace's vault for the app;
//! - the vault keeps the item, and a grant of the workspace's lets the app read it.
//!
//! An app's held connection reaches the vault's web call the same way
//! ([`Workspaces::web_call_to`]): the vault makes the request, so the key never reaches the app,
//! and the grant "Approve and grant" writes covers that item at that place alone.
//!
//! The process's network is its package's grant, as for every launch: the item's places say only
//! where a secret the vault sends itself may go ([`person_vault::WEB_CALL`]).
//!
//! The secret is answered on that process's connection alone. Nothing here writes it down: the
//! item's uses say when, where and for which app, never what was handed. Every other caller of
//! the action reaches the vault's provider, which refuses it saying what is missing
//! ([`refusal_for`]).

use std::sync::Arc;

use accounts::vault_items::{Allowance, ItemId, UsedFor};
use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, TreePath, decide};
use tilo_actions::InvocationId;
use tilo_actions::remote::asked::{AskedDispatch, AskedFacts, AskedRecords, Fact};
use tilo_actions::remote::person_vault::{self, Found, ItemRef, Released};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use zeroize::Zeroizing;

use super::{WeakWorkspaces, Workspaces};
use crate::work::Turn;

/// The name the asks of an app's use of a vault item are kept under in the store of asked
/// records: a release of the item, and a web call that carries its key.
const VAULT_ASKS: &str = "vault-asks";

/// What a release is written down as among the item's uses.
const RELEASE_USE: &str = "tilo.vault/release";

/// What a person's, or a page's, call of a release is refused with.
pub const BY_A_PERSON: &str = "This call is a person's, or a page's they opened. A person reads a \
                               vault item's secret in Passwords; a release hands one only to a \
                               sandboxed process of an app's own.";

/// What an agent's call of a release is refused with.
pub const BY_AN_AGENT: &str = "An agent never holds a vault item's secret. Use the item by its \
                               reference, as vault#web-call does, and the vault adds the secret \
                               where it is.";

/// What a release addressed at something that is no vault item is refused with.
pub const NOT_AN_ITEM: &str =
    "A release is addressed at the vault item it hands out: vault.item:<id>#release";

/// What a release of an item the workspace's vault does not keep is refused with.
pub const NOT_KEPT: &str = "This workspace's vault keeps no such item.";

/// What an app's call that reached the vault from a process apps share is refused with: the
/// desktop session, or code running inside this computer's daemon, such as a tilolang app's
/// connector. Only the device's daemon vouches for a process of the app's own, on that process's
/// own connection, so any call that reaches the vault's provider came from somewhere else.
#[must_use]
pub fn from_a_shared_process(app: &str) -> String {
    format!(
        "{app} asked from code running in a process apps share: the desktop session, or this \
         computer's daemon, where a tilolang app's connector runs. A vault item is released only \
         to a sandboxed process of {app}'s own that a turn placed on this device; a connector \
         gets one once it runs in a process of its own."
    )
}

/// What a release asked from a tilolang process is refused with.
#[must_use]
pub fn from_a_tilolang_process(app: &str) -> String {
    format!(
        "{app} asked from a tilolang process, which runs the code of the language every app \
         shares. A vault item is released only to a sandboxed process of {app}'s own that a \
         turn placed on this device."
    )
}

/// What a release asked from a launch of the app no turn placed (a window, a process started
/// for a call) is refused with.
#[must_use]
pub fn from_an_unplaced_process(app: &str) -> String {
    format!(
        "{app} asked from a window or a process started for a call, which no turn placed on this \
         device. A vault item is released only to {app}'s held connection."
    )
}

/// What a release to a connection this device no longer holds is refused with.
#[must_use]
pub fn held_elsewhere(app: &str) -> String {
    format!("{app}'s connection is held by another device now, so this device releases it nothing.")
}

/// What a release on a device that may not read the workspace's vault for the app is refused
/// with: why not, in the vault's own words.
#[must_use]
pub fn device_may_not_read(app: &str, why: &str) -> String {
    format!("This device may not read the workspace's vault for {app}: {why}")
}

/// What a release waits on while its person is asked whether the app may read the item.
#[must_use]
pub fn asked_first(app: &str) -> String {
    format!(
        "{app} may not read this vault item yet: your person is asked whether it may, once. Call \
         again once they allowed it."
    )
}

/// What a web call waits on while its person is asked whether the app may call that service with
/// that item: once per app, item and place.
#[must_use]
pub fn call_asked_first(app: &str) -> String {
    format!(
        "{app} may not call this service with this vault item yet: your person is asked whether \
         it may, once. Call again once they allowed it."
    )
}

/// Why `caller`'s release, which reached the vault's provider rather than the process it would
/// be answered to, is refused: every such caller is.
#[must_use]
pub fn refusal_for(caller: &Caller) -> String {
    let acting = caller.acting();
    if acting.is_agent() {
        return BY_AN_AGENT.to_owned();
    }
    match acting.app() {
        Some(app) => from_a_shared_process(app.as_str()),
        None => BY_A_PERSON.to_owned(),
    }
}

/// What process of the app asks for a release, as the daemon that started it knows it by its
/// pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppProcess {
    /// A sandboxed launch of the app's own package, for the turn whose lease `lease` names.
    Placed { lease: String },
    /// A sandboxed launch of the app's own package no turn placed: a window, or a process
    /// started for a call.
    Unplaced,
    /// A tilolang process the daemon started for the app, which runs the language's code every
    /// app shares.
    Tilolang,
}

impl AppProcess {
    /// The app's held connection.
    #[must_use]
    pub fn connection(app: &str) -> Self {
        Self::Placed {
            lease: Turn::connection(app, Vec::new()).lease(),
        }
    }
}

/// Where a grant that lets an app read `item` stands in `workspace`'s policy: what "Approve and
/// grant" on an asked release writes, the release being addressed at the item.
fn read_node(workspace: &str, item: &ItemRef) -> TreePath {
    TreePath::workspace(workspace)
        .app(person_vault::PROVIDER)
        .kind(person_vault::ITEM_KIND)
        .resource(item.to_string())
        .action(person_vault::RELEASE)
}

/// What kind of item a person reads it as.
pub(super) const fn kind_word(kind: accounts::vault_items::ItemKind) -> &'static str {
    use accounts::vault_items::ItemKind;
    match kind {
        ItemKind::Login => "Login",
        ItemKind::Key => "Key",
        ItemKind::CodeGenerator => "Code generator",
        ItemKind::SshKey => "SSH key",
        ItemKind::Value => "Value",
    }
}

impl Workspaces {
    /// Whether a grant of `workspace`'s lets `app` read `item`.
    pub(super) async fn app_reads(&self, workspace: &str, app: &str, item: &ItemRef) -> bool {
        let Some(policy) = self.policy_of(workspace).await else {
            return false;
        };
        let decision = decide(
            policy.grants.iter(),
            &[Grantee::app(app)],
            &read_node(workspace, item),
            ActionKind::Read,
            &Context::at(tilo_clock::unix_now_ms()),
        );
        decision.answer == Answer::Allow && decision.grant().is_some()
    }

    /// Vouch that `process` is `app`'s held connection in `workspace`, on this device, and that
    /// this device may read the workspace's vault for it: what a release, a find and a web call
    /// are answered to, and nothing else.
    pub(super) async fn held_connection(
        &self,
        (app, workspace): (&str, &str),
        process: &AppProcess,
    ) -> Result<(), ActionCallError> {
        let refused = ActionCallError::refused;
        let lease = match process {
            AppProcess::Placed { lease } => lease,
            AppProcess::Unplaced => return Err(refused(from_an_unplaced_process(app))),
            AppProcess::Tilolang => return Err(refused(from_a_tilolang_process(app))),
        };
        let here = self.lease_device(workspace).await;
        let holder = self.lease_holder(workspace, lease).await.ok().flatten();
        if holder != Some(here) {
            return Err(refused(held_elsewhere(app)));
        }
        let cert = self.device_cert();
        let device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
        self.using_vault(workspace, &device, Some(app), false)
            .await
            .map_err(|why| refused(device_may_not_read(app, &why)))?;
        Ok(())
    }

    /// The item of `workspace`'s vault tagged for `app` and `purpose`, the newest such: what an
    /// app finds a secret it was moved to by. A read of the vault, not an action: only the app's
    /// held connection reaches it ([`Self::find_tagged`]).
    ///
    /// # Errors
    /// The vault does not open here or does not read.
    pub async fn item_tagged(
        &self,
        workspace: &str,
        app: &str,
        purpose: &str,
    ) -> Result<Option<ItemRef>, String> {
        let app = person_vault::app_tag(app);
        let purpose = person_vault::purpose_tag(purpose);
        let items = self.open_vault(workspace).await?.items();
        Ok(items
            .facts()
            .into_iter()
            .filter(|facts| facts.tags.contains(&app) && facts.tags.contains(&purpose))
            .max_by_key(|facts| facts.added_unix)
            .and_then(|facts| ItemRef::from_id(facts.id.as_str())))
    }

    /// Answer `app`'s held connection the reference of the item tagged for the app and purpose
    /// `request` names ([`person_vault::FIND`]): the item a secret the app was moved to is kept
    /// as. It reads the vault and releases nothing, so its person is not asked; a release of the
    /// item afterwards is, and names the asking app. Every process that is not the app's held
    /// connection is refused as a release's is.
    ///
    /// # Errors
    /// Refused for a process no turn placed here, a connection another device holds, and a device
    /// that may not read the vault for the app.
    pub async fn find_tagged(
        &self,
        (app, workspace): (&str, &str),
        process: &AppProcess,
        request: &InvokeRequest,
    ) -> Result<Found, ActionCallError> {
        self.held_connection((app, workspace), process).await?;
        let looking: person_vault::FindItem = super::person_vault::argument(
            request,
            "an item to find names the app and the purpose it was tagged for",
        )?;
        let reference = self
            .item_tagged(workspace, &looking.app, &looking.purpose)
            .await
            .map_err(ActionCallError::refused)?;
        Ok(Found { reference })
    }

    /// Keep `secret` in `workspace`'s vault as the item an app the person was moved to still
    /// needs, tagged with its app, its purpose and the item it was kept as, as the person: what
    /// the device's old keeper held. An item already tagged for that app and purpose is answered
    /// and nothing is added, so running it again does nothing. Only the person's own device
    /// adopts: `person` must be the person this device's certificate names.
    ///
    /// # Errors
    /// `person` is not this device's person, or the vault does not open, keep or read.
    pub async fn adopt_secret(
        &self,
        workspace: &str,
        person: &str,
        app: &str,
        purpose: &str,
        item: &str,
        secret: Zeroizing<String>,
    ) -> Result<ItemRef, String> {
        if person != hex::encode(self.device_cert().user_public_key) {
            return Err(
                "Only the person's own device adopts a keeper's secret into their vault".to_owned(),
            );
        }
        if let Some(kept) = self.item_tagged(workspace, app, purpose).await? {
            return Ok(kept);
        }
        let adding = person_vault::AddItem {
            kind: person_vault::ItemKind::Value,
            belongs_to: vec![format!("https://{app}")],
            username: String::new(),
            sent_in: String::new(),
            algorithm: String::new(),
            digits: None,
            period: None,
            title: String::new(),
            notes: String::new(),
            fields: Vec::new(),
            tags: vec![
                person_vault::app_tag(app),
                person_vault::purpose_tag(purpose),
                person_vault::item_tag(item),
            ],
            generate: None,
            secret: secret.to_string(),
        };
        let acting = tilo_action_policy::Acting::person(person);
        self.changing_vault(workspace, Some(&acting), |items| {
            super::person_vault::add(items, adding)
        })
        .await
        .map(|view| view.reference)
    }

    /// Hand the secret of the item `request` is addressed at to `process`, a process of `app`'s
    /// own in `workspace` as the daemon that started it vouches for it, answered on its
    /// connection alone. The use is written into the item first, naming the app and never the
    /// secret.
    ///
    /// Where no grant lets the app read the item yet, its person is asked, once, as an asked
    /// record whose "Approve and grant" writes that grant; the call is refused saying so, and
    /// the app calls again once they allowed it.
    ///
    /// # Errors
    /// Refused, in words a person reads, for a process no turn placed here or one apps share,
    /// a connection another device holds, a device that may not read the vault for the app, an
    /// item the vault does not keep, and an item no grant lets the app read.
    pub async fn release_to(
        &self,
        (app, workspace): (&str, &str),
        process: &AppProcess,
        request: &InvokeRequest,
    ) -> Result<Released, ActionCallError> {
        let refused = ActionCallError::refused;
        let item = person_vault::released_item(&request.target).ok_or_else(|| {
            ActionCallError::InvalidRequest {
                reason: NOT_AN_ITEM.to_owned(),
            }
        })?;
        self.held_connection((app, workspace), process).await?;
        let cert = self.device_cert();
        let person = hex::encode(cert.user_public_key);
        let id = ItemId::parse(item.id()).ok_or_else(|| refused(NOT_KEPT.to_owned()))?;
        let facts = self
            .open_vault(workspace)
            .await
            .map_err(|why| refused(device_may_not_read(app, &why)))?
            .items()
            .facts()
            .into_iter()
            .find(|facts| facts.id == id)
            .ok_or_else(|| refused(NOT_KEPT.to_owned()))?;
        let caller = Caller::local_app(app)
            .acting_for(person.clone())
            .in_workspace(workspace);
        if !self.app_reads(workspace, app, &item).await {
            self.ask_release(workspace, &caller, request, &item, &facts)
                .map_err(refused)?;
            return Err(refused(asked_first(app)));
        }
        let what = format!("{app}'s connection");
        let released = self
            .changing_vault(workspace, Some(caller.acting()), |items| {
                items.use_as_kept(
                    &id,
                    &facts.places,
                    app,
                    UsedFor {
                        using: RELEASE_USE,
                        what: &what,
                        allowed: Some(Allowance::Always),
                    },
                )
            })
            .await
            .map_err(refused)?;
        let secret: Zeroizing<String> = released.secret;
        Ok(Released {
            secret: String::clone(&secret),
        })
    }

    /// Ask the person `caller` acts for whether its app may read `item`, as an asked record
    /// whose "Approve and grant" writes the grant on that item alone: unless one asks already.
    fn ask_release(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        item: &ItemRef,
        facts: &accounts::vault_items::ItemFacts,
    ) -> Result<(), String> {
        let (store, records) = self
            .vault_asks()
            .ok_or("This device keeps no asked records, so nobody can be asked")?;
        let app = caller.acting().app().map(|app| app.to_string());
        let target = person_vault::release_target(item);
        let waits = store.waiting_in(workspace).into_iter().any(|waiting| {
            waiting.caller.acting().app().map(|app| app.to_string()) == app
                && store
                    .kept_ask(&waiting.asked.record)
                    .is_some_and(|kept| kept.request.target == target)
        });
        if waits {
            return Ok(());
        }
        let app = app.unwrap_or_default();
        let title = if facts.title.trim().is_empty() {
            "a vault item".to_owned()
        } else {
            facts.title.clone()
        };
        let places: Vec<&str> = facts.places.iter().map(|place| place.as_str()).collect();
        let asked = AskedFacts {
            words: format!(
                "{app} asks to read {title} in its own connection, for a protocol that needs \
                 the secret there. Approve and grant, and it reads this one item each time its \
                 connection starts."
            ),
            on: Some(title),
            facts: vec![
                Fact::new("Item", kind_word(facts.kind)),
                Fact::new("Belongs to", places.join(" ")),
            ],
            verb: "read".to_owned(),
            plural: Some("this vault item".to_owned()),
            kind: person_vault::ITEM_KIND.to_owned(),
            effect: Some(tilo_actions::ActionKind::Read),
            ..AskedFacts::by(super::asking::asked_by(caller))
        };
        let request = InvokeRequest {
            target,
            ..request.clone()
        };
        records
            .write_asked(workspace, caller, &request, &asked)
            .map(drop)
    }

    /// The store an app's asks are kept in, and the records they are written through: once the
    /// host named its store.
    pub(super) fn vault_asks(
        &self,
    ) -> Option<(
        Arc<tilo_actions::remote::asked::AskedStore>,
        Arc<dyn AskedRecords>,
    )> {
        let store = self.asked_store()?;
        let records = self.inner.vault_asks.get()?.1.clone();
        Some((store, records))
    }

    /// Write an app's asks into `store`, answered through [`VaultAsks`].
    pub(super) fn keep_vault_asks_in(&self, store: &Arc<tilo_actions::remote::asked::AskedStore>) {
        let dispatch: Arc<dyn AskedDispatch> = Arc::new(VaultAsks(self.downgrade()));
        let records = self.told(store.for_dispatch(VAULT_ASKS, Arc::downgrade(&dispatch)));
        let _ = self.inner.vault_asks.set((dispatch, records));
    }
}

/// The dispatch an app's asked use of a vault item is kept under. Its person's answer runs
/// nothing: the secret never goes to a record, so their "Approve and grant" is the whole of it,
/// written before this hears of it, and the app calls again on its own connection.
struct VaultAsks(WeakWorkspaces);

impl VaultAsks {
    /// Forget the record `request`'s approve names: its answer is in.
    fn settle(&self, request: &InvokeRequest) {
        if let (Some(workspaces), Some(approval)) = (self.0.upgrade(), request.approval())
            && let Some(store) = workspaces.asked_store()
        {
            store.settled(&approval.record);
        }
    }
}

impl AskedDispatch for VaultAsks {
    fn context(&self, _: InvocationId) -> Value {
        Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        request: InvokeRequest,
        _: &Value,
    ) -> Result<(), ActionCallError> {
        self.settle(&request);
        Ok(())
    }

    fn declined(&self, _: &Caller, _: InvocationId) -> Result<(), ActionCallError> {
        Ok(())
    }
}

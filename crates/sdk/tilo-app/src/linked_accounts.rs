//! Accounts elsewhere a person links to who they are in a room: their
//! Discord account in a room bridged to a Discord server, say.
//!
//! A connector keeps the room's record (the bridged server) and publishes
//! actions on it that any member may ask the room's host to run for
//! themselves ([`tilo_actions::MEMBER_OWN_TAG`]), which the profile card and
//! Settings use the same way for every service:
//!
//! - a read, [`READ_ACTION`], answering a [`Reading`]: whether the room is
//!   connected to the service at all, the reader's linked account if any,
//!   and the actions below;
//! - the connect action, when the host offers signing in with the service,
//!   answering a [`LinkStart`]: the service's consent page, which sends the
//!   person back to the room's host and from there to Tilo with a one-time
//!   link token;
//! - the offer read and the accept action, which take such a token (from
//!   the consent page's return, or from a button the service's side
//!   handed out): the read answers the account it offers ([`OfferReading`]),
//!   the accept links it;
//! - the unlink action.
//!
//! The room's host runs them: the connector's code stays there, and the
//! page or shell only asks, through a host's [`RoomRecordActions`].
//! [`RoomAccounts`] asks the room's connectors ([`connector_apps`]) for the
//! read and keeps what it learns as [`LinkedAccount`]s; a host lends it to
//! the panels that show them as [`LinkedAccounts`].

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

/// The mark a connected service's buttons carry, by its connector's app
/// id: Discord's own, a link for any other.
#[must_use]
pub fn service_icon(app: &str) -> crate::icons::Icon {
    match app {
        "so.tilo.discord" => crate::icons::Icon::Discord,
        _ => crate::icons::Icon::Link,
    }
}

/// The read action a connector's room record publishes, answering a
/// [`Reading`] for the person who reads it.
pub const READ_ACTION: &str = "my-link";

/// A connector's answer to [`READ_ACTION`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Reading {
    /// The service's name ("Discord").
    pub service: String,
    /// The room is connected to the service (a server is bridged).
    pub connected: bool,
    #[serde(default)]
    pub account: Option<AccountName>,
    /// The action that starts signing in on the service's consent page,
    /// answering a [`LinkStart`]; none when the host offers no sign-in.
    #[serde(default)]
    pub connect: Option<String>,
    /// How to link from the service's side instead ("In the Discord server,
    /// type /link."), when it offers that.
    #[serde(default)]
    pub elsewhere: Option<String>,
    /// The action that unlinks the reader's account, when they linked one.
    #[serde(default)]
    pub unlink: Option<String>,
    /// The read that answers the [`OfferReading`] of a link token
    /// (`{token}`).
    #[serde(default)]
    pub offer: String,
    /// The action that links what a link token offers (`{token}`).
    #[serde(default)]
    pub accept: String,
}

/// How a service names an account: its unique name, and the name it shows
/// when that is another.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AccountName {
    pub user_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

impl AccountName {
    /// The account as a line shows it: the name it shows and its handle
    /// when they differ ("Ada L · @ada.l"), else the handle alone.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.display_name {
            Some(shown) if *shown != self.user_name => format!("{shown} · @{}", self.user_name),
            _ => self.handle(),
        }
    }

    /// The unique name as a handle: `@ada.l`.
    #[must_use]
    pub fn handle(&self) -> String {
        format!("@{}", self.user_name)
    }
}

/// What the connect action answers: the service's consent page to go to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStart {
    pub url: String,
}

/// What the offer read answers: the account a link token offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OfferReading {
    /// The service's name ("Discord").
    pub service: String,
    pub account: AccountName,
    /// The person started it here (signing in on the consent page), so it
    /// needs no second yes.
    #[serde(default)]
    pub started_here: bool,
}

/// Where a person's account with one service stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Not linked. `connect`: the host offers signing in here;
    /// `elsewhere`: how to link from the service's side.
    Unlinked {
        connect: bool,
        elsewhere: Option<String>,
    },
    /// On the way to the service's consent page.
    Connecting,
    /// A link token came with the page: asking which account it offers.
    Looking,
    /// The account a link token offers, to say yes or no to.
    Offered(OfferReading),
    Linked(AccountName),
}

/// One service the room is connected to, and the person's account there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedAccount {
    /// The connector's app id, which choices name it by.
    pub app: String,
    /// The service's name ("Discord").
    pub service: String,
    pub state: LinkState,
    /// What went wrong last, in words that say what to do.
    pub problem: Option<String>,
}

/// What shows and changes a person's linked accounts in a room. Asking
/// never waits: what comes of it shows in [`Self::accounts`] once the room's
/// host answered, and [`Self::revision`] moves.
pub trait LinkedAccountsHost {
    /// Every service the room is connected to, with the person's account,
    /// as last heard.
    fn accounts(&self) -> Vec<LinkedAccount>;
    /// Moves whenever [`Self::accounts`] changed.
    fn revision(&self) -> u64;
    /// Ask again where every account stands.
    fn refresh(&self);
    /// Ask again where the account with `app`'s service stands.
    fn refresh_app(&self, app: &str) {
        let _ = app;
        self.refresh();
    }
    /// Start linking the person's account with `app`'s service: off to its
    /// consent page.
    fn link(&self, app: &str);
    /// Unlink the person's account with `app`'s service.
    fn unlink(&self, app: &str);
    /// A link token came with the page: find the account it offers.
    fn offer(&self, token: &str) {
        let _ = token;
    }
    /// Say yes to the account `app`'s service offers.
    fn accept(&self, app: &str) {
        let _ = app;
    }
    /// Say no to it.
    fn decline(&self, app: &str) {
        let _ = app;
    }
}

/// A host's [`LinkedAccountsHost`], as it lends it to a panel. The default
/// has none: no service, and asking does nothing.
#[derive(Clone, Default)]
pub struct LinkedAccounts(Option<Rc<dyn LinkedAccountsHost>>);

impl LinkedAccounts {
    #[must_use]
    pub fn new(host: Rc<dyn LinkedAccountsHost>) -> Self {
        Self(Some(host))
    }

    /// Whether a host stands behind it.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.0.is_some()
    }

    #[must_use]
    pub fn accounts(&self) -> Vec<LinkedAccount> {
        self.0
            .as_ref()
            .map(|host| host.accounts())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.as_ref().map_or(0, |host| host.revision())
    }

    pub fn refresh(&self) {
        if let Some(host) = &self.0 {
            host.refresh();
        }
    }

    pub fn refresh_app(&self, app: &str) {
        if let Some(host) = &self.0 {
            host.refresh_app(app);
        }
    }

    pub fn link(&self, app: &str) {
        if let Some(host) = &self.0 {
            host.link(app);
        }
    }

    pub fn unlink(&self, app: &str) {
        if let Some(host) = &self.0 {
            host.unlink(app);
        }
    }

    pub fn offer(&self, token: &str) {
        if let Some(host) = &self.0 {
            host.offer(token);
        }
    }

    pub fn accept(&self, app: &str) {
        if let Some(host) = &self.0 {
            host.accept(app);
        }
    }

    pub fn decline(&self, app: &str) {
        if let Some(host) = &self.0 {
            host.decline(app);
        }
    }
}

/// How often a panel waiting on the room's host asks again.
pub const LINKING_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// How many times a shown panel that knows no service yet asks again, at
/// [`LINKING_RECHECK`] doubling each time, before it waits to be opened
/// anew.
pub const UNANSWERED_RETRIES: u32 = 4;

/// How many times a link token's offer is asked for before the panel says
/// why it found none. Coming back from a consent page, the room's host
/// records the offer a moment after the page arrives.
pub const OFFER_TRIES: u32 = 10;

/// Asks again while a panel shows the accounts:
///
/// - while a link token waits for its offer: the room's host records what
///   came back from a consent page a moment after the page is back;
/// - while it knows no service at all: the read it asked on opening can
///   fail or go unanswered on a busy page or host, and the panel would
///   offer nothing to link until it was opened again. A room with no
///   connector answers no reading either, so these asks are few and
///   spaced out ([`UNANSWERED_RETRIES`]).
#[derive(Debug, Default)]
pub struct LinkingRecheck {
    due: Option<ui_core::time::Instant>,
    unanswered: Option<Unanswered>,
}

/// The asks again of a shown panel that knows no service yet.
#[derive(Debug)]
struct Unanswered {
    due: ui_core::time::Instant,
    asked: u32,
}

impl LinkingRecheck {
    /// At `now`, with `accounts` shown (or not, `shown` false): asks again
    /// once [`LINKING_RECHECK`] passed since it last did while a token waits
    /// for its offer, and with backoff while no service is known. When to
    /// call again, `None` while there is nothing to ask after.
    pub fn follow(
        &mut self,
        accounts: &LinkedAccounts,
        shown: bool,
        now: ui_core::time::Instant,
    ) -> Option<ui_core::time::Instant> {
        let known = accounts.accounts();
        self.follow_offers(accounts, &known, now);
        self.follow_unanswered(accounts, shown && known.is_empty(), now);
        self.due()
    }

    fn follow_offers(
        &mut self,
        accounts: &LinkedAccounts,
        known: &[LinkedAccount],
        now: ui_core::time::Instant,
    ) {
        let waiting: Vec<&str> = known
            .iter()
            .filter(|account| account.state == LinkState::Looking)
            .map(|account| account.app.as_str())
            .collect();
        if waiting.is_empty() {
            self.due = None;
            return;
        }
        match self.due {
            Some(due) if now < due => {}
            Some(_) => {
                for app in waiting {
                    accounts.refresh_app(app);
                }
                self.due = now.checked_add(LINKING_RECHECK);
            }
            None => self.due = now.checked_add(LINKING_RECHECK),
        }
    }

    fn follow_unanswered(
        &mut self,
        accounts: &LinkedAccounts,
        nothing_known: bool,
        now: ui_core::time::Instant,
    ) {
        if !nothing_known {
            self.unanswered = None;
            return;
        }
        let Some(unanswered) = &mut self.unanswered else {
            // The panel asked on opening; the first ask again comes after.
            self.unanswered = now
                .checked_add(LINKING_RECHECK)
                .map(|due| Unanswered { due, asked: 0 });
            return;
        };
        if now < unanswered.due || unanswered.asked >= UNANSWERED_RETRIES {
            return;
        }
        accounts.refresh();
        if !accounts.accounts().is_empty() {
            self.unanswered = None;
            return;
        }
        unanswered.asked = unanswered.asked.saturating_add(1);
        match LINKING_RECHECK
            .checked_mul(1 << unanswered.asked)
            .and_then(|wait| now.checked_add(wait))
        {
            Some(due) => unanswered.due = due,
            None => unanswered.asked = UNANSWERED_RETRIES,
        }
    }

    /// When to call [`Self::follow`] again.
    #[must_use]
    pub fn due(&self) -> Option<ui_core::time::Instant> {
        let unanswered = self
            .unanswered
            .as_ref()
            .filter(|unanswered| unanswered.asked < UNANSWERED_RETRIES)
            .map(|unanswered| unanswered.due);
        match (self.due, unanswered) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// The answer to one action the room's host ran.
pub type ActionAnswer = Result<serde_json::Value, String>;

/// Of `room_apps`, the apps whose room record the room holds, those that
/// may publish the read ([`READ_ACTION`]): connectors, which bridge a
/// service elsewhere and so run on the room's host (its secret store, its
/// worker), never in the person's desktop. An app the desktop runs itself
/// (`runs_here`) publishes no reading, and asking its record would only
/// have the host open it to answer that it has no such action.
pub fn connector_apps(
    room_apps: impl IntoIterator<Item = String>,
    runs_here: impl Fn(&str) -> bool,
) -> Vec<String> {
    room_apps
        .into_iter()
        .filter(|app| !runs_here(app))
        .collect()
}

/// How a host asks the room's host to run an action of a room record for
/// the person, on the web and natively alike.
pub trait RoomRecordActions {
    /// The apps whose room record this room holds and may publish the read
    /// ([`READ_ACTION`]): the room's connectors ([`connector_apps`]).
    fn apps(&self) -> Vec<String>;
    /// Ask the room's host to run `action` on `app`'s room record (its own
    /// resource) with `arguments`, as the person; `done` gets the answer,
    /// later, on this thread.
    fn invoke(
        &self,
        app: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        done: Box<dyn FnOnce(ActionAnswer)>,
    );
    /// Go to `url` (a service's consent page), leaving this page; it comes
    /// back with a link token.
    fn open(&self, url: &str) {
        let _ = url;
    }
}

/// What [`RoomAccounts`] learned so far.
#[derive(Default)]
struct Known {
    /// Each connector's last reading, by app id.
    readings: BTreeMap<String, Reading>,
    /// The link token the page came with, until a connector's offer read
    /// answered it or it was asked for too often.
    token: Option<String>,
    /// How often the token's offer was asked for.
    tries: u32,
    /// The offer each connector answered for the token.
    offers: BTreeMap<String, OfferReading>,
    /// The connectors whose consent page the page is on its way to.
    connecting: BTreeSet<String>,
    /// What went wrong last, per connector.
    problems: BTreeMap<String, String>,
    /// The connectors asked for the token's offer, or to take it, and not
    /// answered yet: readings that cross ask only once, so a token is
    /// never spent twice.
    asking: BTreeSet<String>,
    accepting: BTreeSet<String>,
}

/// Linked accounts read from a room's records by its host, through a
/// [`RoomRecordActions`]. A link token the page came with is kept here, on
/// this device, until it is used up or declined.
pub struct RoomAccounts {
    actions: Rc<dyn RoomRecordActions>,
    known: Rc<RefCell<Known>>,
    revision: Rc<Cell<u64>>,
    /// Called when what is known changed, to draw it again.
    changed: Rc<dyn Fn()>,
}

impl RoomAccounts {
    /// Accounts asked through `actions`; `changed` is called whenever an
    /// answer changed them.
    #[must_use]
    pub fn new(actions: Rc<dyn RoomRecordActions>, changed: Rc<dyn Fn()>) -> Self {
        Self {
            actions,
            known: Rc::default(),
            revision: Rc::default(),
            changed,
        }
    }

    /// Read `app`'s account again, and the offer of a waiting token.
    fn read(&self, app: &str) {
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            READ_ACTION,
            serde_json::Map::new(),
            Box::new(move |answer| {
                let reading = answer.and_then(|value| {
                    serde_json::from_value::<Reading>(value).map_err(|e| e.to_string())
                });
                let mut known = this.known.borrow_mut();
                let before = known.readings.get(&owned).cloned();
                match reading {
                    Ok(reading) => {
                        known.readings.insert(owned.clone(), reading);
                    }
                    // No reading: the record is no connector, or the host
                    // could not answer now; the next refresh asks again.
                    Err(_) => {
                        known.readings.remove(&owned);
                    }
                }
                let after = known.readings.get(&owned).cloned();
                let ask = known.token.is_some()
                    && after.as_ref().is_some_and(|reading| reading.connected)
                    && !known.offers.contains_key(&owned);
                drop(known);
                if before != after {
                    this.bump();
                }
                if ask {
                    this.ask_offer(&owned);
                }
            }),
        );
    }

    /// Ask `app` which account the waiting token offers.
    fn ask_offer(&self, app: &str) {
        let (action, token) = {
            let mut known = self.known.borrow_mut();
            let Some(reading) = known.readings.get(app) else {
                return;
            };
            let Some(token) = known.token.clone() else {
                return;
            };
            let action = reading.offer.clone();
            if action.is_empty() || !known.asking.insert(app.to_owned()) {
                return;
            }
            (action, token)
        };
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            &action,
            token_argument(&token),
            Box::new(move |answer| {
                let offer = answer.and_then(|value| {
                    serde_json::from_value::<OfferReading>(value).map_err(|e| e.to_string())
                });
                let mut known = this.known.borrow_mut();
                known.asking.remove(&owned);
                if known.token.as_deref() != Some(token.as_str()) {
                    return;
                }
                let started_here = match offer {
                    Ok(offer) => {
                        let started_here = offer.started_here;
                        known.offers.insert(owned.clone(), offer);
                        known.problems.remove(&owned);
                        started_here
                    }
                    Err(problem) => {
                        known.tries = known.tries.saturating_add(1);
                        if known.tries >= OFFER_TRIES {
                            known.token = None;
                            known.problems.insert(owned.clone(), problem);
                        }
                        false
                    }
                };
                drop(known);
                this.bump();
                // Signing in on the consent page was the yes.
                if started_here {
                    this.accept(&owned);
                }
            }),
        );
    }

    fn clone_handle(&self) -> Self {
        Self {
            actions: Rc::clone(&self.actions),
            known: Rc::clone(&self.known),
            revision: Rc::clone(&self.revision),
            changed: Rc::clone(&self.changed),
        }
    }

    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
        (self.changed)();
    }
}

fn token_argument(token: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("token".into(), token.into());
    arguments
}

impl LinkedAccountsHost for RoomAccounts {
    fn accounts(&self) -> Vec<LinkedAccount> {
        let known = self.known.borrow();
        known
            .readings
            .iter()
            .filter(|(_, reading)| reading.connected)
            .map(|(app, reading)| LinkedAccount {
                app: app.clone(),
                service: reading.service.clone(),
                state: if let Some(offer) = known.offers.get(app) {
                    LinkState::Offered(offer.clone())
                } else if known.token.is_some() {
                    LinkState::Looking
                } else if let Some(account) = &reading.account {
                    LinkState::Linked(account.clone())
                } else if known.connecting.contains(app) {
                    LinkState::Connecting
                } else {
                    LinkState::Unlinked {
                        connect: reading.connect.is_some(),
                        elsewhere: reading.elsewhere.clone(),
                    }
                },
                problem: known.problems.get(app).cloned(),
            })
            .collect()
    }

    fn revision(&self) -> u64 {
        self.revision.get()
    }

    fn refresh(&self) {
        for app in self.actions.apps() {
            self.read(&app);
        }
    }

    fn refresh_app(&self, app: &str) {
        self.read(app);
    }

    fn link(&self, app: &str) {
        let Some(action) = self
            .known
            .borrow()
            .readings
            .get(app)
            .and_then(|reading| reading.connect.clone())
        else {
            return;
        };
        {
            let mut known = self.known.borrow_mut();
            known.problems.remove(app);
            known.connecting.insert(app.to_owned());
        }
        self.bump();
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            &action,
            serde_json::Map::new(),
            Box::new(move |answer| {
                match answer.and_then(|value| {
                    serde_json::from_value::<LinkStart>(value).map_err(|e| e.to_string())
                }) {
                    Ok(start) => this.actions.open(&start.url),
                    Err(problem) => {
                        let mut known = this.known.borrow_mut();
                        known.connecting.remove(&owned);
                        known.problems.insert(owned.clone(), problem);
                        drop(known);
                        this.bump();
                    }
                }
            }),
        );
    }

    fn unlink(&self, app: &str) {
        let Some(action) = self
            .known
            .borrow()
            .readings
            .get(app)
            .and_then(|reading| reading.unlink.clone())
        else {
            return;
        };
        self.known.borrow_mut().problems.remove(app);
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            &action,
            serde_json::Map::new(),
            Box::new(move |answer| {
                if let Err(problem) = answer {
                    this.known
                        .borrow_mut()
                        .problems
                        .insert(owned.clone(), problem);
                }
                this.read(&owned);
            }),
        );
    }

    fn offer(&self, token: &str) {
        {
            let mut known = self.known.borrow_mut();
            known.token = Some(token.to_owned());
            known.tries = 0;
            known.offers.clear();
        }
        self.bump();
        let ready: Vec<String> = self
            .known
            .borrow()
            .readings
            .iter()
            .filter(|(_, reading)| reading.connected)
            .map(|(app, _)| app.clone())
            .collect();
        if ready.is_empty() {
            // Asked once the readings are in.
            self.refresh();
        }
        for app in ready {
            self.ask_offer(&app);
        }
    }

    fn accept(&self, app: &str) {
        let (action, token) = {
            let mut known = self.known.borrow_mut();
            let Some(reading) = known.readings.get(app) else {
                return;
            };
            let Some(token) = known.token.clone() else {
                return;
            };
            let action = reading.accept.clone();
            if !known.accepting.insert(app.to_owned()) {
                return;
            }
            (action, token)
        };
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            &action,
            token_argument(&token),
            Box::new(move |answer| {
                {
                    let mut known = this.known.borrow_mut();
                    known.accepting.remove(&owned);
                    known.token = None;
                    known.offers.remove(&owned);
                    match answer {
                        Ok(_) => known.problems.remove(&owned),
                        Err(problem) => known.problems.insert(owned.clone(), problem),
                    };
                }
                this.bump();
                this.read(&owned);
            }),
        );
    }

    fn decline(&self, app: &str) {
        {
            let mut known = self.known.borrow_mut();
            known.token = None;
            known.offers.remove(app);
        }
        self.bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A room whose host answers at once, from a connector's readings.
    #[derive(Default)]
    struct Host {
        reading: RefCell<Reading>,
        /// What the token offers, or why nothing.
        offer: RefCell<Option<Result<OfferReading, String>>>,
        /// The apps asked, in order.
        asked: RefCell<Vec<String>>,
        /// Where the page went.
        opened: RefCell<Vec<String>>,
        /// The actions asked, in order.
        actions: RefCell<Vec<String>>,
        /// Whether answers wait in `waiting` until let go.
        holding: Cell<bool>,
        waiting: RefCell<Vec<Box<dyn FnOnce()>>>,
    }

    impl RoomRecordActions for Rc<Host> {
        fn apps(&self) -> Vec<String> {
            vec!["so.tilo.chat".into(), "so.tilo.discord".into()]
        }

        fn invoke(
            &self,
            app: &str,
            action: &str,
            arguments: serde_json::Map<String, serde_json::Value>,
            done: Box<dyn FnOnce(ActionAnswer)>,
        ) {
            self.asked.borrow_mut().push(app.to_owned());
            self.actions.borrow_mut().push(action.to_owned());
            if app != "so.tilo.discord" {
                return done(Err("unknown_action".into()));
            }
            let answer = match action {
                READ_ACTION => {
                    Ok(serde_json::to_value(self.reading.borrow().clone()).unwrap_or_default())
                }
                "connect" => Ok(serde_json::json!({"url": "https://discord.example/authorize"})),
                "link-offer" => {
                    assert_eq!(arguments["token"], "T0KEN");
                    self.offered()
                        .map(|offer| serde_json::to_value(offer).unwrap_or_default())
                }
                "accept-link" => {
                    let offer = self.offered();
                    offer.map(|offer| {
                        self.reading.borrow_mut().account = Some(offer.account);
                        serde_json::Value::Null
                    })
                }
                _ => {
                    self.reading.borrow_mut().account = None;
                    Ok(serde_json::Value::Null)
                }
            };
            if self.holding.get() {
                self.waiting
                    .borrow_mut()
                    .push(Box::new(move || done(answer)));
            } else {
                done(answer);
            }
        }

        fn open(&self, url: &str) {
            self.opened.borrow_mut().push(url.to_owned());
        }
    }

    fn discord() -> Reading {
        Reading {
            service: "Discord".into(),
            connected: true,
            connect: Some("connect".into()),
            elsewhere: Some("In the Discord server, type /link.".into()),
            offer: "link-offer".into(),
            accept: "accept-link".into(),
            ..Reading::default()
        }
    }

    fn kim() -> OfferReading {
        OfferReading {
            service: "Discord".into(),
            account: AccountName {
                user_name: "kim".into(),
                display_name: None,
            },
            started_here: false,
        }
    }

    impl Host {
        fn offered(&self) -> Result<OfferReading, String> {
            self.offer
                .borrow()
                .clone()
                .unwrap_or_else(|| Err("no offer".into()))
        }
    }

    fn host(offer: Result<OfferReading, String>) -> Rc<Host> {
        Rc::new(Host {
            reading: RefCell::new(discord()),
            offer: RefCell::new(Some(offer)),
            ..Host::default()
        })
    }

    #[test]
    fn only_connectors_are_asked_for_a_reading() {
        let room = ["so.tilo.chat", "so.tilo.contacts", "so.tilo.discord"].map(String::from);
        let runs_here = |app: &str| matches!(app, "so.tilo.chat" | "so.tilo.contacts");
        assert_eq!(connector_apps(room, runs_here), ["so.tilo.discord"]);
    }

    #[test]
    fn connecting_goes_to_the_consent_page() {
        let host = host(Err("gone".into()));
        let accounts = RoomAccounts::new(Rc::new(Rc::clone(&host)), Rc::new(|| {}));
        assert!(
            accounts.accounts().is_empty(),
            "nothing is known before asking"
        );
        accounts.refresh();
        assert_eq!(
            accounts.accounts()[0].state,
            LinkState::Unlinked {
                connect: true,
                elsewhere: Some("In the Discord server, type /link.".into())
            }
        );
        accounts.link("so.tilo.discord");
        assert_eq!(*host.opened.borrow(), ["https://discord.example/authorize"]);
        assert_eq!(accounts.accounts()[0].state, LinkState::Connecting);
    }

    #[test]
    fn a_token_shows_its_offer_and_a_yes_links_it() {
        let host = host(Ok(kim()));
        let accounts = RoomAccounts::new(Rc::new(Rc::clone(&host)), Rc::new(|| {}));
        accounts.offer("T0KEN");
        assert_eq!(accounts.accounts()[0].state, LinkState::Offered(kim()));
        accounts.accept("so.tilo.discord");
        assert_eq!(
            accounts.accounts()[0].state,
            LinkState::Linked(kim().account)
        );
    }

    #[test]
    fn an_offer_started_here_links_without_a_second_yes() {
        let host = host(Ok(OfferReading {
            started_here: true,
            ..kim()
        }));
        let accounts = RoomAccounts::new(Rc::new(Rc::clone(&host)), Rc::new(|| {}));
        accounts.refresh();
        accounts.offer("T0KEN");
        assert_eq!(
            accounts.accounts()[0].state,
            LinkState::Linked(kim().account)
        );
    }

    #[test]
    fn readings_that_cross_spend_the_token_once() {
        let host = host(Ok(OfferReading {
            started_here: true,
            ..kim()
        }));
        host.holding.set(true);
        let accounts = RoomAccounts::new(Rc::new(Rc::clone(&host)), Rc::new(|| {}));
        // The page came back with a token, and the card opening asked again.
        accounts.offer("T0KEN");
        accounts.refresh();
        loop {
            let waiting = std::mem::take(&mut *host.waiting.borrow_mut());
            if waiting.is_empty() {
                break;
            }
            for answer in waiting {
                answer();
            }
        }
        let asked = host.actions.borrow();
        let count = |action: &str| asked.iter().filter(|a| *a == action).count();
        assert_eq!(
            (count("link-offer"), count("accept-link")),
            (1, 1),
            "{asked:?}"
        );
        let account = &accounts.accounts()[0];
        assert_eq!(account.state, LinkState::Linked(kim().account));
        assert_eq!(account.problem, None);
    }

    #[test]
    fn a_declined_offer_is_dropped() {
        let host = host(Ok(kim()));
        let accounts = RoomAccounts::new(Rc::new(Rc::clone(&host)), Rc::new(|| {}));
        accounts.offer("T0KEN");
        accounts.decline("so.tilo.discord");
        assert!(matches!(
            accounts.accounts()[0].state,
            LinkState::Unlinked { .. }
        ));
    }

    #[test]
    fn a_token_with_no_offer_is_asked_after_then_says_why() {
        let host = host(Err("This link ran out.".into()));
        let accounts = LinkedAccounts::new(Rc::new(RoomAccounts::new(
            Rc::new(Rc::clone(&host)),
            Rc::new(|| {}),
        )));
        accounts.offer("T0KEN");
        assert_eq!(accounts.accounts()[0].state, LinkState::Looking);
        let mut recheck = LinkingRecheck::default();
        let mut now = ui_core::time::Instant::now();
        let mut rounds = 0;
        while let Some(due) = recheck.follow(&accounts, true, now) {
            now = due;
            rounds += 1;
            assert!(rounds <= 2 * OFFER_TRIES, "the asks end");
        }
        let account = &accounts.accounts()[0];
        assert!(matches!(account.state, LinkState::Unlinked { .. }));
        assert_eq!(account.problem.as_deref(), Some("This link ran out."));
    }

    #[test]
    fn a_room_with_no_connector_is_asked_again_a_few_times_then_left() {
        let host = host(Err("gone".into()));
        host.reading.borrow_mut().connected = false;
        let accounts = LinkedAccounts::new(Rc::new(RoomAccounts::new(
            Rc::new(Rc::clone(&host)),
            Rc::new(|| {}),
        )));
        let mut recheck = LinkingRecheck::default();
        let mut now = ui_core::time::Instant::now();
        let mut waits = Vec::new();
        while let Some(due) = recheck.follow(&accounts, true, now) {
            waits.push(due.duration_since(now));
            now = due;
            assert!(waits.len() <= 10, "the asks again end");
        }
        assert_eq!(host.asked.borrow().len() / 2, UNANSWERED_RETRIES as usize);
    }
}

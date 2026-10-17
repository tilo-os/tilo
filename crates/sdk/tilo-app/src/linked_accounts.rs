//! Accounts elsewhere a person links to who they are in a room: their
//! Discord account in a room bridged to a Discord server, say.
//!
//! A connector keeps the room's record (the bridged server) and publishes
//! three actions on it that any member may ask the room's host to run for
//! themselves ([`tilo_actions::MEMBER_OWN_TAG`]), which the profile card and
//! Settings use the same way for every service:
//!
//! - a read, [`READ_ACTION`], answering a [`Reading`]: whether the room is
//!   connected to the service at all, the reader's linked account if any,
//!   and the actions that link and unlink it;
//! - the link action the reading names, answering a [`LinkCode`]: a code the
//!   host made and keeps only the hash of, which comes back to the asking
//!   device alone, with the line saying what to do with it;
//! - the unlink action the reading names.
//!
//! The room's host runs them: the connector's code stays there, and the
//! page or shell only asks, through a host's [`RoomRecordActions`].
//! [`RoomAccounts`] asks every room record that answers the read and keeps
//! what it learns as [`LinkedAccount`]s; a host lends it to the panels that
//! show them as [`LinkedAccounts`].

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

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
    /// The reader's linked account there.
    #[serde(default)]
    pub account: Option<AccountName>,
    /// The reader started linking and the code is still good.
    #[serde(default)]
    pub pending: bool,
    /// The action that starts linking, answering a [`LinkCode`].
    pub link: String,
    /// The action that unlinks the reader's account, when they linked one.
    #[serde(default)]
    pub unlink: Option<String>,
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
    /// The account as a line shows it: the name it shows and its username
    /// when they differ ("Ada L · ada.l"), else the username alone.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.display_name {
            Some(shown) if *shown != self.user_name => format!("{shown} · {}", self.user_name),
            _ => self.user_name.clone(),
        }
    }
}

/// What starting a link answers: the code, and what to do with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCode {
    pub code: String,
    /// The line saying what to do with it ("In Discord, type /link with
    /// this code. It works for 10 minutes.").
    pub instruction: String,
}

/// Where a person's account with one service stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    Unlinked,
    /// A code was made; the person still has to send it.
    Linking(LinkCode),
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
    /// Start linking the person's account with `app`'s service.
    fn link(&self, app: &str);
    /// Unlink the person's account with `app`'s service.
    fn unlink(&self, app: &str);
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
}

/// The answer to one action the room's host ran.
pub type ActionAnswer = Result<serde_json::Value, String>;

/// How a host asks the room's host to run an action of a room record for
/// the person, on the web and natively alike.
pub trait RoomRecordActions {
    /// The apps whose room record this room holds.
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
}

/// What [`RoomAccounts`] learned so far.
#[derive(Default)]
struct Known {
    /// Each connector's last reading, by app id.
    readings: BTreeMap<String, Reading>,
    /// The code each link made, until the account is linked or the code
    /// expired.
    codes: BTreeMap<String, LinkCode>,
}

/// Linked accounts read from a room's records by its host, through a
/// [`RoomRecordActions`]. The code a link made is kept here, on this
/// device, until the connector reports the account linked or the code
/// expired.
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

    /// Read `app`'s account again.
    fn read(&self, app: &str) {
        let (known, revision, changed) = (
            Rc::clone(&self.known),
            Rc::clone(&self.revision),
            Rc::clone(&self.changed),
        );
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            READ_ACTION,
            serde_json::Map::new(),
            Box::new(move |answer| {
                let reading = answer.and_then(|value| {
                    serde_json::from_value::<Reading>(value).map_err(|e| e.to_string())
                });
                let mut known = known.borrow_mut();
                let before = (
                    known.readings.get(&owned).cloned(),
                    known.codes.get(&owned).cloned(),
                );
                if let Ok(reading) = reading {
                    if reading.account.is_some() || !reading.pending {
                        known.codes.remove(&owned);
                    }
                    known.readings.insert(owned.clone(), reading);
                } else {
                    // No reading: the record is no connector, or the host
                    // could not answer now; the next refresh asks again.
                    known.readings.remove(&owned);
                }
                let after = (
                    known.readings.get(&owned).cloned(),
                    known.codes.get(&owned).cloned(),
                );
                drop(known);
                if before != after {
                    revision.set(revision.get().wrapping_add(1));
                    changed();
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
                state: match (&reading.account, known.codes.get(app)) {
                    (Some(account), _) => LinkState::Linked(account.clone()),
                    (None, Some(code)) => LinkState::Linking(code.clone()),
                    (None, None) => LinkState::Unlinked,
                },
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

    fn link(&self, app: &str) {
        let Some(action) = self
            .known
            .borrow()
            .readings
            .get(app)
            .map(|reading| reading.link.clone())
        else {
            return;
        };
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            &action,
            serde_json::Map::new(),
            Box::new(move |answer| {
                match answer.and_then(|value| {
                    serde_json::from_value::<LinkCode>(value).map_err(|e| e.to_string())
                }) {
                    Ok(code) => {
                        this.known.borrow_mut().codes.insert(owned.clone(), code);
                        this.bump();
                    }
                    Err(error) => tracing::warn!(app = owned, %error, "linking refused"),
                }
                this.read(&owned);
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
        let this = self.clone_handle();
        let owned = app.to_owned();
        self.actions.invoke(
            app,
            &action,
            serde_json::Map::new(),
            Box::new(move |answer| {
                if let Err(error) = answer {
                    tracing::warn!(app = owned, %error, "unlinking refused");
                }
                this.read(&owned);
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A room whose host answers at once, from a connector's readings.
    #[derive(Default)]
    struct Host {
        reading: RefCell<Reading>,
    }

    impl RoomRecordActions for Rc<Host> {
        fn apps(&self) -> Vec<String> {
            vec!["so.tilo.chat".into(), "so.tilo.discord".into()]
        }

        fn invoke(
            &self,
            app: &str,
            action: &str,
            _: serde_json::Map<String, serde_json::Value>,
            done: Box<dyn FnOnce(ActionAnswer)>,
        ) {
            if app != "so.tilo.discord" {
                return done(Err("unknown_action".into()));
            }
            let answer = match action {
                READ_ACTION => {
                    serde_json::to_value(self.reading.borrow().clone()).unwrap_or_default()
                }
                "link-account" => {
                    self.reading.borrow_mut().pending = true;
                    serde_json::json!({"code": "ABCDEFGHJKLM", "instruction": "Type it"})
                }
                _ => {
                    self.reading.borrow_mut().account = None;
                    serde_json::Value::Null
                }
            };
            done(Ok(answer));
        }
    }

    #[test]
    fn accounts_follow_what_the_host_answers() {
        let host = Rc::new(Host {
            reading: RefCell::new(Reading {
                service: "Discord".into(),
                connected: true,
                link: "link-account".into(),
                ..Reading::default()
            }),
        });
        let woken = Rc::new(Cell::new(0));
        let wake = Rc::clone(&woken);
        let accounts = RoomAccounts::new(
            Rc::new(Rc::clone(&host)),
            Rc::new(move || wake.set(wake.get() + 1)),
        );
        assert!(
            accounts.accounts().is_empty(),
            "nothing is known before asking"
        );
        accounts.refresh();
        assert_eq!(accounts.accounts()[0].state, LinkState::Unlinked);
        assert_eq!(accounts.accounts().len(), 1, "Chat answers no reading");
        assert!(woken.get() > 0);

        accounts.link("so.tilo.discord");
        assert_eq!(
            accounts.accounts()[0].state,
            LinkState::Linking(LinkCode {
                code: "ABCDEFGHJKLM".into(),
                instruction: "Type it".into()
            })
        );

        {
            let mut reading = host.reading.borrow_mut();
            reading.pending = false;
            reading.account = Some(AccountName {
                user_name: "ada".into(),
                display_name: Some("Ada L".into()),
            });
            reading.unlink = Some("unlink-mine".into());
        }
        accounts.refresh();
        assert!(matches!(accounts.accounts()[0].state, LinkState::Linked(_)));

        accounts.unlink("so.tilo.discord");
        assert_eq!(accounts.accounts()[0].state, LinkState::Unlinked);
    }
}

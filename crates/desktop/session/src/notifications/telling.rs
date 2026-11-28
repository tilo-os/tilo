//! The person's log of what their assistant told them, read live into the one centre the shell
//! shows.
//!
//! Everything a person's assistant tells them joins one log on the host of their workspace, and a
//! device of theirs that runs reads it live (`tilo.telling`'s `list`, asked through the daemon this
//! session's own shell connection reaches). Each thing that still waits becomes a notice in the
//! centre, under the name of whoever told it, with the buttons it offers; a thing answered on
//! another device of theirs, or taken back, goes from the centre as soon as the log says so. What
//! the person does with a notice is written back into the log, so the answer is theirs wherever
//! they read it next.
//!
//! The words are the ones the host tells outside Tilo ([`person_telling::words_outside`]), so a
//! notice reads the same on every device, and a thing whose words may not leave the host keeps
//! them off a banner. A mention is the one kind this host does not take from the log: the session
//! already tells the person when someone names them in a record it follows
//! ([`crate::mentions`]), and the same mention twice is the same news twice.
//!
//! A thing's notice keeps its number for as long as it stands, so the row under the pointer and the
//! page opened from it are the same notice a reading later. That number is carried in the notice's
//! attributed id, so the centre a session opens with the last one's history finds the row it kept
//! rather than a second copy of it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_person_services::person_telling::{self, Kind, Offered, Told, ToldAction, ToldList};
use tilo_resource::Effect as _;
use tilo_service::notice::Place;
use tilo_session_core::desktop::notifications::{
    INLINE_REPLY, Notification, NotificationSeverity, PERSISTENT_DISMISS,
};

/// What tells the centre a notice is one of the person's log, and which thing it is.
const TAG: &str = "tilo.telling\u{1f}";

/// What the person did on a notice of the log, to carry out on the next pass where the daemon is
/// reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    /// The workspace the call is made in.
    pub(crate) workspace: String,
    /// What it asks.
    pub(crate) target: tilo_actions::remote::RemoteActionTarget,
    pub(crate) arguments: Map<String, Value>,
    /// What it was, for a log line when the daemon refuses it.
    pub(crate) what: String,
}

/// The person's log of what their assistant told them, as the centre shows it.
#[derive(Default)]
pub(crate) struct Telling {
    /// The place in the log read up to.
    after: Cell<u64>,
    /// The notice each thing stands as, by the thing's id.
    numbers: RefCell<BTreeMap<String, u64>>,
    /// The thing each notice stands for, by the notice's number.
    things: RefCell<BTreeMap<u64, Told>>,
    /// What the person did, to ask the daemon on the next pass.
    asks: RefCell<Vec<Invocation>>,
    /// Where the person asked to go, with the workspace it is in.
    places: RefCell<Vec<(String, Place)>>,
    /// The workspace the log is read in: where a thing that names nowhere is answered.
    place: RefCell<String>,
}

impl Telling {
    /// The place in the log this has read up to.
    pub(crate) fn after(&self) -> u64 {
        self.after.get()
    }

    /// Take what the log says into the centre: a thing still waiting stands there (or is brought
    /// up to date where it changed), and one that waits no more goes. `quiet` keeps a new one off
    /// a banner, as a held notification is kept.
    pub(crate) fn take(
        &self,
        list: ToldList,
        desktop: &tilo_session_core::desktop::state::DesktopNotifications,
        quiet: bool,
        workspace: &str,
        keep: Option<u64>,
    ) {
        if !workspace.is_empty() {
            workspace.clone_into(&mut self.place.borrow_mut());
        }
        self.after.set(self.after.get().max(list.newest));
        for told in list.told {
            if told.waits() {
                self.show(&told, desktop, quiet);
            } else {
                // The notice the person is reading whole stays while they read it, even though
                // the log says they have seen it.
                self.withdraw(&told.id, desktop, keep);
            }
        }
    }

    /// The thing the notice `number` stands for, while it stands for one.
    pub(crate) fn of(&self, number: u64) -> Option<Told> {
        self.things.borrow().get(&number).cloned()
    }

    /// The person opened the notice `number`: they saw the thing, and went where it says.
    pub(crate) fn opened(&self, number: u64, device: &str) {
        let Some(told) = self.of(number) else {
            return;
        };
        self.acknowledge(&told, person_telling::BY_OPENED, device);
        if let Some(about) = &told.about
            && !about.app.is_empty()
        {
            self.places.borrow_mut().push((
                about.workspace.clone(),
                Place {
                    app: about.app.clone(),
                    record: about.record.clone(),
                    resource: about.resource.clone(),
                    anchor: String::new(),
                },
            ));
        }
    }

    /// The person chose the button `key` on the notice `number`: the thing's own action runs as
    /// them, and the thing is answered by that button's id.
    pub(crate) fn acted(&self, number: u64, key: &str, device: &str) {
        let Some(told) = self.of(number) else {
            return;
        };
        let Some(offered) = told.actions.iter().find(|offered| offered.id == key) else {
            return;
        };
        self.invoke_action(&told, offered);
        self.acknowledge(&told, &offered.id, device);
    }

    /// The person put the notice `number` away: the thing is answered as dismissed, and the row
    /// goes.
    pub(crate) fn dismissed(
        &self,
        number: u64,
        device: &str,
        desktop: &tilo_session_core::desktop::state::DesktopNotifications,
    ) {
        let Some(told) = self.of(number) else {
            return;
        };
        self.acknowledge(&told, person_telling::BY_DISMISSED, device);
        self.withdraw(&told.id, desktop, None);
    }

    /// What the person did since the last look, oldest first.
    pub(crate) fn take_asks(&self) -> Vec<Invocation> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// Where the person asked to go since the last look, with the workspace each is in.
    pub(crate) fn take_places(&self) -> Vec<(String, Place)> {
        std::mem::take(&mut *self.places.borrow_mut())
    }

    /// Show `told` in the centre, or bring the notice it already stands as up to date.
    fn show(
        &self,
        told: &Told,
        desktop: &tilo_session_core::desktop::state::DesktopNotifications,
        quiet: bool,
    ) {
        // The session tells the person of a mention itself, once, from the record it names them
        // in: taking it from the log too would say the same thing twice.
        if told.kind == Kind::Mention {
            return;
        }
        let (title, body) = person_telling::words_outside(told);
        let from = told
            .from
            .as_ref()
            .map(|from| from.name.trim())
            .filter(|name| !name.is_empty())
            .unwrap_or("Tilo")
            .to_owned();
        let insists = told.insists.is_some();
        let mut notification = Notification::new(title, body)
            .with_app_name(from)
            .with_severity(if insists {
                NotificationSeverity::Warning
            } else {
                NotificationSeverity::Info
            });
        // Where the host keeps a thing's words to itself, its notice keeps them off a banner as
        // any sender's preview does.
        notification.show_preview = told.outside;
        if insists {
            notification.dismiss_after = PERSISTENT_DISMISS;
        }
        // The thing's buttons in the order the shell draws them: the answer it expects first
        // stands last, so it is the one that ends the row.
        let (expected, others): (Vec<&Offered>, Vec<&Offered>) = told
            .actions
            .iter()
            .filter(|offered| offered.id != INLINE_REPLY)
            .partition(|offered| offered.main);
        notification.actions = others
            .into_iter()
            .chain(expected)
            .map(|offered| (offered.id.clone(), offered.label.clone()))
            .collect();
        let tag = tag_of(&told.id);
        notification.app_id = Some(tag.clone());
        let existing = self
            .numbers
            .borrow()
            .get(&told.id)
            .copied()
            .or_else(|| kept_number(desktop, &tag));
        let number = existing.unwrap_or(notification.id);
        notification.id = number;
        // A notice that changed is written again where it stood, keeping its number so the row,
        // its hit target and any page opened from it stay the same notice.
        if existing.is_some() {
            desktop.withdraw(number);
        }
        if quiet {
            desktop.post_quiet(notification);
        } else {
            desktop.post(notification);
        }
        self.numbers.borrow_mut().insert(told.id.clone(), number);
        self.things.borrow_mut().insert(number, told.clone());
    }

    /// Take the notice the thing `id` stands as out of the centre.
    fn withdraw(
        &self,
        id: &str,
        desktop: &tilo_session_core::desktop::state::DesktopNotifications,
        keep: Option<u64>,
    ) {
        let number = self
            .numbers
            .borrow_mut()
            .remove(id)
            .or_else(|| kept_number(desktop, &tag_of(id)));
        let Some(number) = number else {
            return;
        };
        if Some(number) == keep {
            // The person is reading it: the row and the page are the same notice until they go
            // back to the list.
            self.numbers.borrow_mut().insert(id.to_owned(), number);
            return;
        }
        self.things.borrow_mut().remove(&number);
        desktop.withdraw(number);
    }

    /// Ask the thing's own action as the person, in the workspace the thing is in.
    fn invoke_action(&self, told: &Told, offered: &Offered) {
        let workspace = match &told.about {
            Some(about) if !about.workspace.is_empty() => about.workspace.clone(),
            _ => return,
        };
        let target = if offered.target.record.is_empty() {
            tilo_actions::remote::application_action_target(
                &workspace,
                &offered.target.app,
                offered.target.resource.clone(),
                offered.target.action.clone(),
            )
        } else {
            tilo_actions::remote::application_resource_target(
                &workspace,
                &offered.target.app,
                &offered.target.record,
                offered.target.resource.clone(),
                offered.target.action.clone(),
            )
        };
        self.asks.borrow_mut().push(Invocation {
            workspace,
            target,
            arguments: offered.arguments.clone(),
            what: format!("{} on {}", offered.target.action, offered.target.resource),
        });
    }

    /// Say the person answered the thing `told` by `by`, on `device`: in the workspace the thing
    /// names, or the one its log is read in where it names none.
    fn acknowledge(&self, told: &Told, by: &str, device: &str) {
        let workspace = told
            .about
            .as_ref()
            .map(|about| about.workspace.clone())
            .filter(|workspace| !workspace.is_empty())
            .unwrap_or_else(|| self.place.borrow().clone());
        if workspace.is_empty() {
            return;
        }
        let acknowledge = ToldAction::Acknowledge {
            told: told.id.clone(),
            device: Some(device.to_owned()),
            by: Some(by.to_owned()),
        };
        self.asks.borrow_mut().push(Invocation {
            workspace,
            target: person_telling::target(person_telling::TOLD, acknowledge.id()),
            arguments: acknowledge.arguments(),
            what: format!("answering what was told as {by}"),
        });
    }
}

/// What the session's loop follows the person's log with: one held ask at a time, so the log is
/// read live without asking again while an answer is still on its way.
#[derive(Clone, Default)]
pub(crate) struct Reading {
    out: Rc<Cell<bool>>,
}

impl Reading {
    /// Whether an ask is on its way.
    pub(crate) fn out(&self) -> bool {
        self.out.get()
    }

    /// An ask went out: nothing else is asked until it answers.
    pub(crate) fn begin(&self) {
        self.out.set(true);
    }

    /// Its answer landed, or was given up: the next pass asks again.
    pub(crate) fn end(&self) {
        self.out.set(false);
    }
}

/// The attributed id a notice of the thing `id` carries: what finds the row again in the centre
/// the last session kept.
fn tag_of(id: &str) -> String {
    format!("{TAG}{id}")
}

/// The number of the notice the centre kept for the attributed id `tag`, while it keeps one.
fn kept_number(
    desktop: &tilo_session_core::desktop::state::DesktopNotifications,
    tag: &str,
) -> Option<u64> {
    let log = desktop.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .find(|notification| notification.app_id.as_deref() == Some(tag))
        .map(|notification| notification.id)
}

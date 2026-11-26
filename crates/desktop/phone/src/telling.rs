//! What the person is told, shown on the phone while its app runs.
//!
//! Everything the person's assistant tells them joins one log on the host of their workspace, and
//! a device of theirs that runs reads it live ([`tilo_person_services::person_telling::LIST`],
//! asked through the device daemon as every one of their own actions is). This is the phone's
//! reader: each thing that still waits becomes a notification on the phone, under a name its own
//! device's push key makes ([`tilo_person_services::person_telling::known_as`]) — the same name
//! the host's push to a closed app shows it under, so a push and the log's own reading are one
//! notification, not two. Its buttons are notification actions, and each opens the phone at what
//! the thing is about. A thing answered on another device of theirs, or taken back, is withdrawn
//! from the phone as soon as the log says so, which is why the log is read again on every look.
//!
//! The words are the ones the host tells outside Tilo
//! ([`tilo_person_services::person_telling::words_outside`]): where a person keeps a workspace's
//! words on the host, the notification says only where to look, so a phone left on a table never
//! shows them.

use std::collections::BTreeMap;

use tilo_person_services::person_telling::{self, About, Told};
use tilo_process_services::notify::{Notification, Notifier};

/// How the link a notification opens begins: `tilo-phone://told/<thing>[/<button>]`.
pub const LINK: &str = "tilo-phone://told/";

/// One notification as it stands on the phone.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    tag: String,
    title: String,
    body: String,
    /// The buttons it offers, by label.
    buttons: Vec<String>,
    /// How often it was raised: a thing that insists takes a new notification each time.
    raised: u32,
}

/// A tap on one of the phone's notifications: what it is about, and the button pressed where one
/// was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tap {
    /// The thing, by its id in the person's log.
    pub told: String,
    /// The button pressed, by its id; none for a tap on the notification itself.
    pub action: Option<String>,
}

/// The phone's reading of what the person was told.
#[derive(Default)]
pub struct Watching {
    /// The place in the log read up to.
    after: u64,
    /// What stands on the phone now, by the thing's id.
    shown: BTreeMap<String, Shown>,
    /// Where each thing shown is, as the log says: what a tap opens.
    about: BTreeMap<String, About>,
    /// This device's push auth secret, where it handed an address over: the name it knows a thing
    /// by, as a push says it.
    auth: Option<Vec<u8>>,
}

impl Watching {
    /// The reading of a device whose push auth secret is `auth`, where it handed an address over.
    #[must_use]
    pub fn new(auth: Option<Vec<u8>>) -> Self {
        Self {
            auth,
            ..Self::default()
        }
    }

    /// This device's push auth secret, as the names the host's pushes use.
    pub fn holding(&mut self, auth: Vec<u8>) {
        self.auth = Some(auth);
    }

    /// Read what the person was told in `workspace` and show, raise or withdraw what changed
    /// since the last look. Answers the names of the things shown.
    ///
    /// # Errors
    /// The daemon did not answer what the person was told.
    pub fn catch_up(
        &mut self,
        device: &tilo_runtime::in_process::Device,
        workspace: &str,
        notifier: &dyn Notifier,
    ) -> Result<(), String> {
        let list = device
            .told(workspace, self.after, 0, "Phone")
            .map_err(|error| format!("{error:#}"))?;
        self.after = self.after.max(list.newest);
        for told in list.told {
            self.show_or_withdraw(&told, notifier);
        }
        Ok(())
    }

    /// Show `told` where it waits for the person, raise it where it insists and was raised again,
    /// and withdraw it where it waits no more.
    fn show_or_withdraw(&mut self, told: &Told, notifier: &dyn Notifier) {
        let tag = self.tag_of(&told.id);
        if !told.waits() || !told.reaches {
            if self.shown.remove(&told.id).is_some() {
                self.about.remove(&told.id);
                let _ = notifier.withdraw(&tag);
            }
            return;
        }
        if let Some(about) = &told.about {
            self.about.insert(told.id.clone(), about.clone());
        }
        let (title, body) = person_telling::words_outside(told);
        let buttons: Vec<String> = told
            .actions
            .iter()
            .take(tilo_process_services::notify::MAX_BUTTONS)
            .map(|action| action.label.clone())
            .collect();
        let raised = told.insists.map_or(1, |insists| insists.raised);
        let stands = Shown {
            tag: tag.clone(),
            title: title.clone(),
            body: body.clone(),
            buttons: buttons.clone(),
            raised,
        };
        let known = self.shown.get(&told.id);
        if known == Some(&stands) {
            return;
        }
        // A telling sounds; a look that found the same thing again replaces it silently.
        let silent = known.is_some();
        let mut notice = Notification::new(tag, title, body)
            .with_activation(format!("{LINK}{}", hex::encode(told.id.as_bytes())));
        for (action, label) in told
            .actions
            .iter()
            .take(tilo_process_services::notify::MAX_BUTTONS)
            .zip(buttons)
        {
            notice = notice.with_button(
                label,
                format!(
                    "{LINK}{}/{}",
                    hex::encode(told.id.as_bytes()),
                    hex::encode(action.id.as_bytes())
                ),
            );
        }
        if silent {
            notice = notice.quietly();
        }
        if notifier.show(&notice).is_ok() {
            self.shown.insert(told.id.clone(), stands);
        }
    }

    /// The name this device knows the thing `id` by: its push name where it handed an address
    /// over, and its own id where it did not (nothing is pushed to it then, so nothing can have
    /// shown it there).
    #[must_use]
    fn tag_of(&self, id: &str) -> String {
        match &self.auth {
            Some(auth) => format!("tilo-told-{}", person_telling::known_as(auth, id)),
            None => format!("tilo-told-{id}"),
        }
    }

    /// What a link one of the phone's notifications opens names: none for a link this app does
    /// not open.
    #[must_use]
    pub fn tapped(&self, link: &str) -> Option<Tap> {
        let rest = link.trim().strip_prefix(LINK)?;
        let (thing, action) = match rest.split_once('/') {
            Some((thing, action)) => (thing, Some(action)),
            None => (rest, None),
        };
        let decode = |text: &str| {
            let bytes = hex::decode(text).ok()?;
            String::from_utf8(bytes).ok()
        };
        Some(Tap {
            told: decode(thing)?,
            action: action.and_then(decode),
        })
    }

    /// Where the thing `id` is, as the log last said.
    #[must_use]
    pub fn about_of(&self, id: &str) -> Option<&About> {
        self.about.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link opens the thing it names, and the button it names beside it.
    #[test]
    fn a_link_names_the_thing_and_the_button_pressed() {
        let watching = Watching::default();
        assert_eq!(watching.tapped("tilo-phone://elsewhere/x"), None);
        assert_eq!(
            watching.tapped(&format!("{LINK}{}", hex::encode(b"told-1"))),
            Some(Tap {
                told: "told-1".into(),
                action: None
            })
        );
        assert_eq!(
            watching.tapped(&format!(
                "{LINK}{}/{}",
                hex::encode(b"told-1"),
                hex::encode(b"reply")
            )),
            Some(Tap {
                told: "told-1".into(),
                action: Some("reply".into())
            })
        );
        assert_eq!(watching.tapped(&format!("{LINK}not-hex")), None);
    }
}

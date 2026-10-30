//! `org.freedesktop.impl.portal.AppChooser`: which app opens a file or a
//! link. The frontend's `OpenURI` asks it when there is no default (or the
//! app asked to ask), with the installed apps that handle the type.
//!
//! The choice is asked as a question of kind [`APP_CHOOSER_QUESTION`]
//! ([`AppChooserQuestion`], the offered apps by desktop id with their names
//! and icons), answered by the chosen app's index. While it waits the
//! frontend may offer other apps (`UpdateChoices`, after an install); the
//! question changes in place and only an app offered when the person
//! answered can be the reply.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Str};

use crate::reply::{Ended, OrFail, Reply, Results, answer};
use crate::{Portal, PortalCaller, lockdown, question_handle, text_option};

/// The kind of the question choosing an app asks.
pub const APP_CHOOSER_QUESTION: &str = "portal.app-chooser";

/// One app offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppChoice {
    /// Its desktop id, without `.desktop`.
    pub id: String,
    /// Its name, as its entry gives it; the id when it is not installed.
    pub name: String,
    /// Its icon name or path, when its entry names one.
    pub icon: Option<String>,
}

/// What the sheet shows for choosing an app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppChooserQuestion {
    /// The apps offered, in the frontend's order: the answer's `choice`
    /// counts into it.
    pub choices: Vec<AppChoice>,
    /// The app chosen last time for this type, to preselect.
    pub last_choice: Option<String>,
    /// The type to open (`image/png`, `x-scheme-handler/https`).
    pub content_type: Option<String>,
    /// What is opened, a link or a file name, as the app gave it.
    pub uri: Option<String>,
    pub filename: Option<String>,
}

/// Desktop ids as offered: bounded, no control characters, no repeats.
fn valid_choices(choices: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    choices
        .into_iter()
        .filter(|id| !id.is_empty() && crate::plain_text(id, 255) && seen.insert(id.clone()))
        .collect()
}

/// `ids` with what their installed entries say about them.
fn described(ids: &[String]) -> Vec<AppChoice> {
    let scan = desktop_entry::cache::cached_scan();
    ids.iter()
        .map(|id| {
            let entry = scan.find(id);
            AppChoice {
                id: id.clone(),
                name: entry.map_or_else(|| id.clone(), |entry| entry.name.clone()),
                icon: entry.and_then(|entry| entry.icon.clone()),
            }
        })
        .collect()
}

/// The `org.freedesktop.impl.portal.AppChooser` interface.
pub struct AppChooser {
    portal: Portal,
    /// The question each waiting request asked, by request path.
    waiting: Arc<Mutex<HashMap<String, AppChooserQuestion>>>,
}

impl AppChooser {
    #[must_use]
    pub fn new(portal: Portal) -> Self {
        Self {
            portal,
            waiting: Arc::default(),
        }
    }

    async fn choose(
        &self,
        caller: &PortalCaller,
        path: &str,
        question: AppChooserQuestion,
    ) -> Result<Results, Ended> {
        self.waiting
            .lock_recover()
            .insert(path.to_owned(), question.clone());
        let asked = self
            .portal
            .ask(caller, path, APP_CHOOSER_QUESTION, &question)
            .await;
        let offered = self.waiting.lock_recover().remove(path).unwrap_or(question);
        let index = asked?.choice.or_fail()?;
        let chosen = offered.choices.get(index).or_fail()?;
        let mut results = Results::new();
        results.insert(
            "choice".into(),
            OwnedValue::from(Str::from(chosen.id.clone())),
        );
        Ok(results)
    }
}

#[interface(name = "org.freedesktop.impl.portal.AppChooser")]
impl AppChooser {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    async fn choose_application(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, Some(&parent_window))
                .await?;
            if lockdown::current_policy().disable_application_handlers {
                return Err(Ended::Cancelled);
            }
            let choices = valid_choices(choices);
            if choices.is_empty() {
                return Err(Ended::Cancelled);
            }
            let question = AppChooserQuestion {
                choices: described(&choices),
                last_choice: text_option(&options, "last_choice", 255),
                content_type: text_option(&options, "content_type", 255),
                uri: text_option(&options, "uri", 8192),
                filename: text_option(&options, "filename", 4096),
            };
            let results = self
                .portal
                .requests
                .run(
                    connection,
                    path,
                    auth.owner.as_str(),
                    self.choose(&auth.caller, path, question),
                )
                .await;
            // The launch the frontend makes for the choice is activated
            // with the token the app brought.
            results.map(|mut results| {
                if let Some(token) = text_option(&options, "activation_token", 4096) {
                    results.insert(
                        "activation_token".into(),
                        OwnedValue::from(Str::from(token)),
                    );
                }
                results
            })
        })
        .await
    }

    async fn update_choices(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        choices: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        let path = handle.as_str();
        let owner = self.portal.frontend(&header).await?;
        if !self.portal.requests.is_owned(path, owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "no such request of this portal frontend".into(),
            ));
        }
        let choices = described(&valid_choices(choices));
        let details = self
            .waiting
            .lock_recover()
            .get_mut(path)
            .map(|question| {
                question.choices = choices;
                serde_json::to_value(&*question).unwrap_or_default()
            })
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no chooser waits there".into()))?;
        self.portal
            .questions
            .update(&question_handle(path), details);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_deduplicated_and_bounded() {
        assert_eq!(
            valid_choices(vec![
                "a".into(),
                "a".into(),
                String::new(),
                "b\n".into(),
                "b".into()
            ]),
            ["a", "b"]
        );
    }
}

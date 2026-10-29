//! `org.freedesktop.impl.portal.Screenshot`.
//!
//! An interactive screenshot is asked as a question of kind
//! [`SCREENSHOT_QUESTION`]: the person takes it (or not). A
//! non-interactive one needs the person's access decision for the app
//! ([`crate::consent`]), unless the frontend already checked its
//! permission store. Either way the picture is the session's capture of
//! what the displays show ([`crate::Capture`]), handed back as a file URI.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tilo_service::Question;
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::consent::AccessQuestion;
use crate::file_chooser::file_uri;
use crate::reply::{Ended, OrFail, Outcome, Reply, Results, answer};
use crate::{Portal, PortalCaller, lockdown, option, question_handle};

/// The kind of the question an interactive screenshot asks.
pub const SCREENSHOT_QUESTION: &str = "portal.screenshot";
/// The capability a non-interactive screenshot needs.
pub const SCREENSHOT_CAPABILITY: &str = "screenshot";

/// What the sheet shows for an interactive screenshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotQuestion {
    /// The window the app asks from, when it named one.
    pub parent_window: Option<String>,
    /// The app asked for the sheet to keep its window from input.
    pub modal: bool,
}

/// How a screenshot request is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    /// The person takes it in a sheet.
    Ask,
    /// The app's access decides; asked once when it has none.
    Access,
    /// The frontend checked its permission store already.
    Granted,
}

fn decision(options: &HashMap<String, OwnedValue>) -> Decision {
    if option::<bool>(options, "interactive").unwrap_or(false) {
        Decision::Ask
    } else if option::<bool>(options, "permission_store_checked").unwrap_or(false) {
        Decision::Granted
    } else {
        Decision::Access
    }
}

/// The `org.freedesktop.impl.portal.Screenshot` interface.
pub struct Screenshot {
    portal: Portal,
}

impl Screenshot {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

/// Decide request `path` of `caller` as `options` ask, then capture.
async fn take(
    portal: &Portal,
    caller: &PortalCaller,
    path: &str,
    parent_window: &str,
    options: &HashMap<String, OwnedValue>,
) -> Outcome {
    let handle = question_handle(path);
    match decision(options) {
        Decision::Ask => {
            let question = ScreenshotQuestion {
                parent_window: Some(parent_window.trim())
                    .filter(|window| !window.is_empty())
                    .map(str::to_owned),
                modal: option::<bool>(options, "modal").unwrap_or(true),
            };
            portal
                .questions
                .ask(Question {
                    handle,
                    kind: SCREENSHOT_QUESTION.into(),
                    app_id: Some(caller.app_id.clone()),
                    label: caller.app_id.clone(),
                    details: serde_json::to_value(question).unwrap_or_default(),
                })
                .await
                .ok_or(Ended::Cancelled)?;
        }
        Decision::Access => {
            portal
                .consents
                .decide(
                    caller,
                    &handle,
                    AccessQuestion {
                        capability: SCREENSHOT_CAPABILITY.into(),
                        reason: None,
                        may_remember: true,
                        text: None,
                    },
                )
                .await?;
        }
        Decision::Granted => {}
    }
    let picture = portal.capture.screenshot().await.map_err(|error| {
        tracing::warn!(%error, "screenshot not captured");
        Ended::Failed
    })?;
    let uri = file_uri(&picture).or_fail()?;
    let mut results = Results::new();
    results.insert(
        "uri".into(),
        OwnedValue::from(zbus::zvariant::Str::from(uri)),
    );
    Ok(results)
}

#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    async fn screenshot(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            if lockdown::current_policy().disable_save_to_disk {
                return Err(Ended::Cancelled);
            }
            let path = handle.as_str();
            self.portal
                .requests
                .run(
                    connection,
                    path,
                    auth.owner.as_str(),
                    take(&self.portal, &auth.caller, path, &parent_window, &options),
                )
                .await
        })
        .await
    }

    /// Picking a colour off the screen needs a picker the session does not
    /// draw yet: the request fails rather than answering a colour nobody
    /// picked.
    async fn pick_color(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            self.portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            Err(Ended::Failed)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_asks_checked_skips_and_otherwise_access_decides() {
        let with = |pairs: &[(&str, bool)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), OwnedValue::from(*value)))
                .collect::<HashMap<_, _>>()
        };
        assert_eq!(decision(&with(&[("interactive", true)])), Decision::Ask);
        assert_eq!(
            decision(&with(&[
                ("interactive", true),
                ("permission_store_checked", true)
            ])),
            Decision::Ask,
            "an interactive screenshot is always the person's"
        );
        assert_eq!(
            decision(&with(&[("permission_store_checked", true)])),
            Decision::Granted
        );
        assert_eq!(decision(&HashMap::new()), Decision::Access);
    }
}

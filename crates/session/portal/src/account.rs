//! `org.freedesktop.impl.portal.Account`: an app asking who the person is.
//!
//! The person is asked first (a question of kind [`ACCOUNT_QUESTION`] with
//! the app's reason); only what they allow leaves: their login name, their
//! name, and their picture when they set one.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Str};

use crate::reply::{Ended, Reply, Results, answer};
use crate::{Portal, text_option};

/// The kind of the question sharing who the person is asks.
pub const ACCOUNT_QUESTION: &str = "portal.account";

/// What the sheet shows when an app asks who the person is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountQuestion {
    /// Why the app asks, in its words.
    pub reason: Option<String>,
}

/// The `org.freedesktop.impl.portal.Account` interface.
pub struct Account {
    portal: Portal,
}

impl Account {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Account")]
impl Account {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn get_user_information(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        window: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, Some(&window))
                .await?;
            let question = AccountQuestion {
                reason: text_option(&options, "reason", 1024).filter(|reason| !reason.is_empty()),
            };
            let asked = self
                .portal
                .ask(&auth.caller, path, ACCOUNT_QUESTION, question);
            self.portal
                .requests
                .run(connection, path, auth.owner.as_str(), asked)
                .await?;
            let person = self.portal.host.person().ok_or(Ended::Failed)?;
            let mut results = Results::new();
            results.insert("id".into(), OwnedValue::from(Str::from(person.id)));
            results.insert("name".into(), OwnedValue::from(Str::from(person.name)));
            if let Some(image) = person.image {
                results.insert("image".into(), OwnedValue::from(Str::from(image)));
            }
            Ok(results)
        })
        .await
    }
}

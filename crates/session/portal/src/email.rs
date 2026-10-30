//! `org.freedesktop.impl.portal.Email`: an app handing a message to the
//! person's mail app.
//!
//! The person sees what is about to be composed (a question of kind
//! [`EMAIL_QUESTION`]: recipients, subject, how many attachments) and the
//! message opens in the app they chose for `mailto:` links. Attachments are
//! the document-portal URIs the frontend made ([`crate::documents`]),
//! passed on as they are.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::reply::{Ended, Reply, Results, answer};
use crate::{Portal, lockdown, option, text_option};

/// The kind of the question composing a message asks.
pub const EMAIL_QUESTION: &str = "portal.email";

/// The most recipients or attachments of one kind a message may carry.
const MAX_ITEMS: usize = 128;

/// A message an app asks to compose: also what the sheet shows, but the
/// body, which the mail app shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailQuestion {
    pub addresses: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    #[serde(skip)]
    pub body: Option<String>,
    pub attachments: Vec<String>,
}

impl EmailQuestion {
    /// The message `options` describe; `None` when one of them is no list
    /// of plain, bounded strings.
    fn from_options(options: &HashMap<String, OwnedValue>) -> Option<Self> {
        let mut addresses = strings(options, "addresses")?;
        if let Some(address) = text_option(options, "address", 4096).filter(|a| !a.is_empty()) {
            addresses.push(address);
        }
        let attachments = strings(options, "attachments")?;
        (addresses.len() <= MAX_ITEMS
            && attachments
                .iter()
                .all(|uri| crate::documents::exported_file(uri).is_ok()))
        .then_some(())?;
        Some(Self {
            addresses,
            cc: strings(options, "cc")?,
            bcc: strings(options, "bcc")?,
            subject: text_option(options, "subject", 998),
            body: option::<String>(options, "body").filter(|body| body.len() <= 256 * 1024),
            attachments,
        })
    }

    /// The message as a `mailto:` link; attachments ride as `attachment`
    /// fields, which the common mail apps read.
    fn mailto(&self) -> String {
        let mut query = Vec::new();
        let mut field = |key: &str, value: &str| {
            if !value.is_empty() {
                query.push(format!("{key}={}", encode(value)));
            }
        };
        field("cc", &self.cc.join(","));
        field("bcc", &self.bcc.join(","));
        field("subject", self.subject.as_deref().unwrap_or_default());
        field("body", self.body.as_deref().unwrap_or_default());
        for attachment in &self.attachments {
            field("attachment", attachment);
        }
        let mut uri = format!("mailto:{}", encode(&self.addresses.join(",")));
        if !query.is_empty() {
            uri.push('?');
            uri.push_str(&query.join("&"));
        }
        uri
    }
}

/// List option `key`: absent is empty; `None` when it is no list of at
/// most [`MAX_ITEMS`] plain strings.
fn strings(options: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<String>> {
    if !options.contains_key(key) {
        return Some(Vec::new());
    }
    let values = option::<Vec<String>>(options, key)?;
    (values.len() <= MAX_ITEMS
        && values
            .iter()
            .all(|value| !value.is_empty() && crate::plain_text(value, 8192)))
    .then_some(values)
}

/// Everything but the unreserved characters and `@`, percent-encoded.
fn encode(value: &str) -> String {
    const QUERY_VALUE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~')
        .remove(b'@');
    percent_encoding::utf8_percent_encode(value, QUERY_VALUE).to_string()
}

/// The `org.freedesktop.impl.portal.Email` interface.
pub struct Email {
    portal: Portal,
}

impl Email {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Email")]
impl Email {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        4
    }

    async fn compose_email(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
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
            let message = EmailQuestion::from_options(&options).ok_or(Ended::Failed)?;
            let asked = self
                .portal
                .ask(&auth.caller, path, EMAIL_QUESTION, &message);
            self.portal
                .requests
                .run(connection, path, auth.owner.as_str(), asked)
                .await?;
            let token = text_option(&options, "activation_token", 4096);
            self.portal
                .host
                .open(&message.mailto(), token.as_deref())
                .map_err(|error| {
                    tracing::warn!(%error, "the mail app did not open");
                    Ended::Failed
                })?;
            Ok(Results::new())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(pairs: Vec<(&str, OwnedValue)>) -> HashMap<String, OwnedValue> {
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    fn list(values: &[&str]) -> OwnedValue {
        OwnedValue::try_from(zbus::zvariant::Value::from(
            values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>(),
        ))
        .unwrap()
    }

    #[test]
    fn a_message_becomes_one_mailto_link() {
        let message = EmailQuestion::from_options(&options(vec![
            (
                "address",
                OwnedValue::from(zbus::zvariant::Str::from("ada@example.org")),
            ),
            ("cc", list(&["bob@example.org"])),
            (
                "subject",
                OwnedValue::from(zbus::zvariant::Str::from("Hi & bye")),
            ),
        ]))
        .unwrap();
        let message = EmailQuestion {
            attachments: vec!["file:///run/user/1000/doc/ab/notes.txt".into()],
            ..message
        };
        assert_eq!(
            message.mailto(),
            "mailto:ada@example.org?cc=bob@example.org&subject=Hi%20%26%20bye\
             &attachment=file%3A%2F%2F%2Frun%2Fuser%2F1000%2Fdoc%2Fab%2Fnotes.txt"
        );
    }

    #[test]
    fn controls_and_host_paths_are_refused() {
        assert!(EmailQuestion::from_options(&options(vec![("cc", list(&["a\nb"]))])).is_none());
        for attachment in ["/etc/passwd", "file:///etc/passwd"] {
            assert!(
                EmailQuestion::from_options(&options(vec![("attachments", list(&[attachment]))]))
                    .is_none()
            );
        }
    }
}

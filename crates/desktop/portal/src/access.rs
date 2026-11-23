//! `org.freedesktop.impl.portal.Access`: the frontend's generic "may this
//! app...?" dialog (the camera, the location and other permissions the
//! frontend keeps in its own permission store), asked as a question of
//! kind [`crate::consent::ACCESS_QUESTION`] with the app's own words.

use std::collections::HashMap;

use tilo_service::Question;
use zbus::fdo;
use zbus::message::{Flags, Header};
use zbus::names::{InterfaceName, MemberName};
use zbus::object_server::{DispatchResult2, SignalEmitter};
use zbus::zvariant::OwnedValue;

use crate::consent::{ACCESS_QUESTION, AccessQuestion, AccessText};
use crate::reply::{Ended, Reply, Results, answer};
use crate::{Portal, question_handle};

/// The bound on each of the app's texts.
const MAX_TEXT: usize = 1024;

fn bounded(text: String) -> String {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// The `org.freedesktop.impl.portal.Access` interface.
pub struct Access {
    portal: Portal,
}

impl Access {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

/// `AccessDialog`'s arguments: handle, app id, parent window, title,
/// subtitle, body and options.
type AccessDialogCall = (
    zbus::zvariant::OwnedObjectPath,
    String,
    String,
    String,
    String,
    String,
    HashMap<String, OwnedValue>,
);

/// The interface as the frontend introspects it.
const INTROSPECTION: &str = r#"<interface name="org.freedesktop.impl.portal.Access">
  <method name="AccessDialog">
    <arg type="o" name="handle" direction="in"/>
    <arg type="s" name="app_id" direction="in"/>
    <arg type="s" name="parent_window" direction="in"/>
    <arg type="s" name="title" direction="in"/>
    <arg type="s" name="subtitle" direction="in"/>
    <arg type="s" name="body" direction="in"/>
    <arg type="a{sv}" name="options" direction="in"/>
    <arg type="u" name="response" direction="out"/>
    <arg type="a{sv}" name="results" direction="out"/>
  </method>
</interface>"#;

impl Access {
    /// `AccessDialog`: ask the person with the app's own words.
    async fn access_dialog(
        &self,
        header: &zbus::message::Header<'_>,
        connection: &zbus::Connection,
        call: AccessDialogCall,
    ) -> Reply {
        let (handle, app_id, parent_window, title, subtitle, body, _options) = call;
        answer(async {
            let auth = self
                .portal
                .authenticate(header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            let path = handle.as_str();
            let question = AccessQuestion {
                capability: "access".into(),
                reason: None,
                // The frontend keeps the answer in its permission store.
                may_remember: false,
                text: Some(AccessText {
                    title: bounded(title),
                    subtitle: bounded(subtitle),
                    body: bounded(body),
                }),
            };
            let asked = self.portal.questions.ask(Question {
                handle: question_handle(path),
                kind: ACCESS_QUESTION.into(),
                app_id: Some(auth.caller.app_id.clone()),
                label: auth.caller.app_id.clone(),
                details: serde_json::to_value(question).unwrap_or_default(),
            });
            self.portal
                .requests
                .run(connection, path, auth.owner.as_str(), async {
                    asked.await.ok_or(Ended::Cancelled)?;
                    let mut results = Results::new();
                    let choices: Vec<(String, String)> = Vec::new();
                    if let Ok(value) = OwnedValue::try_from(zbus::zvariant::Value::new(choices)) {
                        results.insert("choices".into(), value);
                    }
                    Ok(results)
                })
                .await
        })
        .await
    }
}

// Written out rather than derived: `AccessDialog` takes seven arguments
// besides the call's header and connection, more than a method may.
#[zbus::export::async_trait::async_trait]
impl zbus::object_server::Interface for Access {
    fn name() -> InterfaceName<'static> {
        InterfaceName::from_static_str_unchecked("org.freedesktop.impl.portal.Access")
    }

    async fn get(
        &self,
        _property: &str,
        _server: &zbus::ObjectServer,
        _connection: &zbus::Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<OwnedValue>> {
        None
    }

    async fn get_all(
        &self,
        _server: &zbus::ObjectServer,
        _connection: &zbus::Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        Ok(HashMap::new())
    }

    fn set<'call>(
        &'call self,
        _property: &'call str,
        _value: &'call zbus::zvariant::Value<'_>,
        _server: &'call zbus::ObjectServer,
        _connection: &'call zbus::Connection,
        _header: Option<&'call Header<'_>>,
        _emitter: &'call SignalEmitter<'_>,
    ) -> DispatchResult2<'call> {
        DispatchResult2::NotFound
    }

    async fn set_mut(
        &mut self,
        _property: &str,
        _value: &zbus::zvariant::Value<'_>,
        _server: &zbus::ObjectServer,
        _connection: &zbus::Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<()>> {
        None
    }

    fn call<'call>(
        &'call self,
        _server: &'call zbus::ObjectServer,
        connection: &'call zbus::Connection,
        message: &'call zbus::Message,
        name: MemberName<'call>,
    ) -> DispatchResult2<'call> {
        if name.as_str() != "AccessDialog" {
            return DispatchResult2::NotFound;
        }
        DispatchResult2::Async(Box::pin(async move {
            let header = message.header();
            let reply = match message.body().deserialize::<AccessDialogCall>() {
                Ok(call) => Ok(self.access_dialog(&header, connection, call).await),
                Err(error) => Err(fdo::Error::InvalidArgs(error.to_string())),
            };
            if header.primary().flags().contains(Flags::NoReplyExpected) {
                return Ok(());
            }
            match reply {
                Ok(reply) => connection.reply(&header, &reply).await,
                Err(error) => connection.reply_dbus_error(&header, error).await,
            }
            .map(|_serial| ())
            .map_err(|error| match error {
                zbus::Error::FDO(error) => *error,
                error => fdo::Error::Failed(error.to_string()),
            })
        }))
    }

    fn call_mut<'call>(
        &'call mut self,
        _server: &'call zbus::ObjectServer,
        _connection: &'call zbus::Connection,
        _message: &'call zbus::Message,
        _name: MemberName<'call>,
    ) -> DispatchResult2<'call> {
        DispatchResult2::NotFound
    }

    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        for line in INTROSPECTION.lines() {
            let _ = writeln!(writer, "{:level$}{line}", "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apps_text_is_bounded_on_a_character_boundary() {
        assert_eq!(bounded("short".into()), "short");
        let long = "é".repeat(MAX_TEXT);
        let cut = bounded(long);
        assert!(cut.len() <= MAX_TEXT);
        assert!(cut.chars().all(|c| c == 'é'));
    }
}

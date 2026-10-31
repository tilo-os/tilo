//! One application's settings over its collaboration channel.
//!
//! A sandboxed app cannot read the session's settings document. Its channel
//! answers its settings calls instead
//! ([`crate::ChannelHandle::settings`]): the app reads its view and follows
//! its changes with [`AppSettingsHandle::read`], and writes its own entries
//! with [`AppSettingsHandle::update`]. With the `settings-host` feature,
//! [`host::AppSettingsServer`] answers them from a
//! `settings_store::SettingsSource`; what an app sees and may write is
//! `settings_store::app_scope`'s.

use runtime_portal::NotAvailable;

use crate::wire::{Reply, SettingsCall};

/// The most bytes one settings view or write carries on the wire.
pub const MAX_SETTINGS_WIRE_BYTES: usize = 1024 * 1024;

/// The most bytes one display request or reply carries on the wire.
const MAX_DISPLAY_WIRE_BYTES: usize = 64 * 1024;

pub use crate::DisplayOrigin;

/// What [`crate::ChannelHandle::settings`] answers.
#[derive(Clone)]
pub struct AppSettingsHandle {
    pub(crate) channel: crate::ChannelHandle,
}

/// One version of an app's settings view: its revision and the JSON
/// document of its sections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsView {
    pub revision: u64,
    pub document_json: Vec<u8>,
}

impl SettingsView {
    fn from_reply(reply: Reply) -> Result<Self, NotAvailable> {
        let Reply::Settings { revision, document } = reply else {
            return Err(NotAvailable::Refused(format!(
                "the collaboration host answered settings with {reply:?}"
            )));
        };
        if document.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err(NotAvailable::Refused(
                "settings view exceeds the wire bound".into(),
            ));
        }
        Ok(Self {
            revision,
            document_json: document,
        })
    }
}

impl AppSettingsHandle {
    /// The view once its revision differs from `after`; at once for 0.
    ///
    /// # Errors
    /// The host refuses or is unavailable.
    pub async fn read(&self, after: u64) -> Result<SettingsView, NotAvailable> {
        SettingsView::from_reply(
            self.channel
                .settings_call(SettingsCall::Read { after })
                .await?,
        )
    }

    /// Invoke the host's separately admitted display transaction service.
    ///
    /// # Errors
    /// Refuses oversized requests, unavailable authority and lost replies.
    pub async fn display_control(
        &self,
        command: &[u8],
        origin: DisplayOrigin,
    ) -> Result<Vec<u8>, NotAvailable> {
        if command.len() > MAX_DISPLAY_WIRE_BYTES {
            return Err(NotAvailable::Refused(
                "display request exceeds the wire bound".into(),
            ));
        }
        let reply = self
            .channel
            .settings_call(SettingsCall::Display {
                command: command.to_vec(),
                origin,
            })
            .await?;
        let Reply::Answer(bytes) = reply else {
            return Err(NotAvailable::Refused(format!(
                "the collaboration host answered display control with {reply:?}"
            )));
        };
        if bytes.len() > MAX_DISPLAY_WIRE_BYTES {
            return Err(NotAvailable::Refused(
                "display reply exceeds the wire bound".into(),
            ));
        }
        Ok(bytes)
    }

    /// Merge `values_json`, `{"<app id>": {...}}`, into the app's entry in
    /// `section`, answering the view after the write. `action` names the
    /// action that made the change, for the session's history.
    ///
    /// # Errors
    /// Refuses oversized writes, and the host's refusal.
    pub async fn update(
        &self,
        section: &str,
        values_json: &[u8],
        action: Option<&str>,
    ) -> Result<SettingsView, NotAvailable> {
        if values_json.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err(NotAvailable::Refused(
                "settings write exceeds the wire bound".into(),
            ));
        }
        SettingsView::from_reply(
            self.channel
                .settings_call(SettingsCall::Update {
                    section: section.to_owned(),
                    values: values_json.to_vec(),
                    action: action.map(str::to_owned),
                })
                .await?,
        )
    }
}

#[cfg(feature = "settings-host")]
pub mod host {
    //! An app's settings calls answered from a settings source.

    use std::cell::RefCell;
    use std::sync::Arc;

    use serde_json::{Map, Value};
    use settings_store::SettingsSource;
    use settings_store::app_scope::{Reach, update_at, view_at};

    use super::MAX_SETTINGS_WIRE_BYTES;

    /// Asked before every call; an error refuses it (the channel's
    /// admission was withdrawn).
    pub type Revalidate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

    /// An independently admitted typed display delegate; the caller's identity
    /// is captured by the broker, never read from the request.
    pub type DisplayControl =
        Arc<dyn Fn(&[u8], super::DisplayOrigin) -> Result<Vec<u8>, String> + Send + Sync>;

    /// One app's `AppSettings` capability over `source`.
    pub struct AppSettingsServer {
        app_id: String,
        /// What the app reaches of the document.
        reach: Reach,
        display: Option<DisplayControl>,
        dictionary_owner: Option<[u8; 32]>,
        source: Arc<dyn SettingsSource>,
        revalidate: Revalidate,
        /// The view last answered and the revision it was answered as, so
        /// a change elsewhere in the document wakes no reader.
        answered: RefCell<(u64, Map<String, Value>)>,
        /// Woken by the source on every change of its document.
        changed: Arc<tokio::sync::Notify>,
        /// Keeps this server listening to the source.
        _listener: Arc<settings_store::Listener>,
    }

    impl AppSettingsServer {
        #[must_use]
        pub fn new(
            app_id: impl Into<String>,
            source: Arc<dyn SettingsSource>,
            revalidate: Revalidate,
        ) -> Self {
            let changed = Arc::new(tokio::sync::Notify::new());
            let wake = Arc::clone(&changed);
            let listener: Arc<settings_store::Listener> = Arc::new(move || wake.notify_waiters());
            source.changes().listen(&listener);
            Self {
                app_id: app_id.into(),
                reach: Reach::App,
                display: None,
                dictionary_owner: None,
                source,
                revalidate,
                answered: RefCell::new((0, Map::new())),
                changed,
                _listener: listener,
            }
        }

        /// Let the app reach `reach` of the document: [`Reach::System`] for
        /// the device's settings app the host granted it to.
        #[must_use]
        pub fn with_reach(mut self, reach: Reach) -> Self {
            self.reach = reach;
            self
        }

        /// Attach the broker's independent display authority, without extending
        /// generic settings write permissions.
        #[must_use]
        pub fn with_display(mut self, display: Option<DisplayControl>) -> Self {
            self.display = display;
            self
        }

        /// Host-certified local descriptor owner. Remote collaborators and
        /// delegated agents are never given this projection authority.
        #[must_use]
        pub fn with_dictionary_owner(mut self, owner: Option<[u8; 32]>) -> Self {
            self.dictionary_owner = owner;
            self
        }

        /// The current view, with a revision that changes exactly when the
        /// view does.
        fn current(&self) -> (u64, Map<String, Value>) {
            let mut view = view_at(&self.source.load(), &self.app_id, self.reach);
            settings_store::personal_dictionary::host::project(
                self.source.as_ref(),
                &self.app_id,
                self.dictionary_owner,
                &mut view,
            );
            let mut answered = self.answered.borrow_mut();
            if answered.0 == 0 || answered.1 != view {
                answered.0 = answered.0.wrapping_add(1).max(1);
                answered.1 = view;
            }
            answered.clone()
        }
    }

    fn encode(view: &Map<String, Value>) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(view).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err("settings view exceeds the wire bound".into());
        }
        Ok(bytes)
    }

    impl AppSettingsServer {
        /// The view, as its revision and JSON, once its revision differs
        /// from `after`; at once for 0.
        ///
        /// # Errors
        /// The channel's admission was withdrawn, or the view is too large.
        #[expect(
            clippy::future_not_send,
            reason = "the host answers each app on its single-threaded broker task"
        )]
        pub async fn read(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
            loop {
                (self.revalidate)()?;
                // Armed before looking, so a change between the look and the
                // wait still wakes it.
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let (revision, view) = self.current();
                if revision != after {
                    return Ok((revision, encode(&view)?));
                }
                changed.await;
            }
        }

        /// Run a display transaction for the device's settings app.
        ///
        /// # Errors
        /// The app holds no display authority, or the transaction failed.
        pub fn display(
            &self,
            command: &[u8],
            origin: super::DisplayOrigin,
        ) -> Result<Vec<u8>, String> {
            (self.revalidate)()?;
            if command.len() > 64 * 1024 {
                return Err("display request exceeds the wire bound".into());
            }
            let display = self
                .display
                .as_ref()
                .ok_or("display control is unavailable to this application")?;
            let reply = display(command, origin)?;
            if reply.len() > 64 * 1024 {
                return Err("display reply exceeds the wire bound".into());
            }
            Ok(reply)
        }

        /// Merge `values` (a JSON object) into the app's entry in
        /// `section`, answering the view after the write.
        ///
        /// # Errors
        /// The write is malformed, too large or outside the app's reach.
        pub fn update(
            &self,
            section: &str,
            values: &[u8],
            action: Option<&str>,
        ) -> Result<(u64, Vec<u8>), String> {
            (self.revalidate)()?;
            if values.len() > MAX_SETTINGS_WIRE_BYTES {
                return Err("settings write exceeds the wire bound".into());
            }
            let Ok(Value::Object(values)) = serde_json::from_slice(values) else {
                return Err("a settings write is a JSON object".into());
            };
            // The action the app says made the change, for the history; who
            // invoked it is not the app's to say.
            let origin = settings_store::ChangeOrigin {
                action: action
                    .filter(|action| !action.is_empty())
                    .map(str::to_owned),
                principal: None,
                invoked_by: None,
            };
            if !settings_store::personal_dictionary::host::command(
                self.source.as_ref(),
                &self.app_id,
                self.dictionary_owner,
                section,
                &values,
            )
            .map_err(|e| e.to_string())?
            {
                update_at(
                    self.source.as_ref(),
                    &self.app_id,
                    self.reach,
                    section,
                    values,
                    &origin,
                )
                .map_err(|e| e.to_string())?;
            }
            let (revision, view) = self.current();
            Ok((revision, encode(&view)?))
        }
    }
}

//! One application's settings over its collaboration channel.
//!
//! A sandboxed app cannot read the session's settings document. Its channel
//! hands it an `AppSettings` capability instead
//! ([`crate::ChannelHandle::settings`]): the app reads its view and follows
//! its changes with [`AppSettingsHandle::read`], and writes its own entries
//! with [`AppSettingsHandle::update`]. With the `settings-host` feature,
//! [`host::AppSettingsServer`] is that capability over a
//! `settings_store::SettingsSource`; what an app sees and may write is
//! `settings_store::app_scope`'s.

use std::rc::Rc;

use runtime_portal::NotAvailable;

use crate::collab_capnp::app_settings;

/// The most bytes one settings view or write carries on the wire.
pub const MAX_SETTINGS_WIRE_BYTES: usize = 1024 * 1024;

pub use crate::DisplayOrigin;

/// The capability [`crate::ChannelHandle::settings`] answers.
#[derive(Clone)]
pub struct AppSettingsHandle {
    pub(crate) inner: Rc<app_settings::Client>,
}

/// One version of an app's settings view: its revision and the JSON
/// document of its sections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsView {
    pub revision: u64,
    pub document_json: Vec<u8>,
}

impl SettingsView {
    fn from_reply(revision: u64, document: &[u8]) -> Result<Self, NotAvailable> {
        if document.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "settings view exceeds the wire bound".into(),
            )));
        }
        Ok(Self {
            revision,
            document_json: document.to_vec(),
        })
    }
}

impl From<app_settings::Client> for AppSettingsHandle {
    fn from(client: app_settings::Client) -> Self {
        Self {
            inner: Rc::new(client),
        }
    }
}

impl AppSettingsHandle {
    /// The view once its revision differs from `after`; at once for 0.
    pub async fn read(&self, after: u64) -> Result<SettingsView, NotAvailable> {
        let mut request = self.inner.read_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        let reply = reply.get()?;
        SettingsView::from_reply(reply.get_revision(), reply.get_document_json()?)
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
        if command.len() > 64 * 1024 {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "display request exceeds the wire bound".into(),
            )));
        }
        let mut request = self.inner.display_control_request();
        request.get().set_command_json(command);
        match origin {
            DisplayOrigin::LocalHost => request
                .get()
                .set_origin(crate::collab_capnp::DisplayRequestOrigin::LocalHost),
            DisplayOrigin::Published(invocation) => {
                request
                    .get()
                    .set_origin(crate::collab_capnp::DisplayRequestOrigin::Published);
                request.get().set_published_invocation(&invocation);
            }
        }
        let reply = request.send().promise.await?;
        let bytes = reply.get()?.get_reply_json()?;
        if bytes.len() > 64 * 1024 {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "display reply exceeds the wire bound".into(),
            )));
        }
        Ok(bytes.to_vec())
    }

    /// Merge `values_json`, `{"<app id>": {...}}`, into the app's entry in
    /// `section`, answering the view after the write. `action` names the
    /// action that made the change, for the session's history.
    pub async fn update(
        &self,
        section: &str,
        values_json: &[u8],
        action: Option<&str>,
    ) -> Result<SettingsView, NotAvailable> {
        if values_json.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "settings write exceeds the wire bound".into(),
            )));
        }
        let mut request = self.inner.update_request();
        request.get().set_section(section);
        request.get().set_values_json(values_json);
        request.get().set_action(action.unwrap_or_default());
        let reply = request.send().promise.await?;
        let reply = reply.get()?;
        SettingsView::from_reply(reply.get_revision(), reply.get_document_json()?)
    }
}

#[cfg(feature = "settings-host")]
pub mod host {
    //! The `AppSettings` capability over a settings source.

    use std::cell::RefCell;
    use std::sync::Arc;

    use capnp::capability::Promise;
    use serde_json::{Map, Value};
    use settings_store::SettingsSource;
    use settings_store::app_scope::{Reach, update_at, view_at};

    use super::MAX_SETTINGS_WIRE_BYTES;
    use crate::collab_capnp::app_settings;

    /// Asked before every call; an error refuses it (the channel's
    /// admission was withdrawn).
    pub type Revalidate = Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>;

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

    fn encode(view: &Map<String, Value>) -> Result<Vec<u8>, capnp::Error> {
        let bytes = serde_json::to_vec(view).map_err(|e| capnp::Error::failed(e.to_string()))?;
        if bytes.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err(capnp::Error::failed(
                "settings view exceeds the wire bound".into(),
            ));
        }
        Ok(bytes)
    }

    #[allow(
        refining_impl_trait,
        reason = "the generated server methods return a future; a Promise is one"
    )]
    impl app_settings::Server for AppSettingsServer {
        fn read(
            self: std::rc::Rc<Self>,
            params: app_settings::ReadParams,
            mut results: app_settings::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let after = match params.get() {
                Ok(params) => params.get_after(),
                Err(error) => return Promise::err(error),
            };
            Promise::from_future(async move {
                loop {
                    (self.revalidate)()?;
                    // Armed before looking, so a change between the look
                    // and the wait still wakes it.
                    let changed = self.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    let (revision, view) = self.current();
                    if revision != after {
                        let mut reply = results.get();
                        reply.set_revision(revision);
                        reply.set_document_json(&encode(&view)?);
                        return Ok(());
                    }
                    changed.await;
                }
            })
        }

        fn display_control(
            self: std::rc::Rc<Self>,
            params: app_settings::DisplayControlParams,
            mut results: app_settings::DisplayControlResults,
        ) -> Promise<(), capnp::Error> {
            let answered = (|| {
                (self.revalidate)()?;
                let bytes = params.get()?.get_command_json()?;
                if bytes.len() > 64 * 1024 {
                    return Err(capnp::Error::failed(
                        "display request exceeds the wire bound".into(),
                    ));
                }
                let display = self.display.as_ref().ok_or_else(|| {
                    capnp::Error::failed(
                        "display control is unavailable to this application".into(),
                    )
                })?;
                let original = params.get()?.get_published_invocation()?;
                let origin = match params
                    .get()?
                    .get_origin()
                    .map_err(|_| capnp::Error::failed("invalid display origin".into()))?
                {
                    crate::collab_capnp::DisplayRequestOrigin::LocalHost if original.is_empty() => {
                        super::DisplayOrigin::LocalHost
                    }
                    crate::collab_capnp::DisplayRequestOrigin::Published => {
                        super::DisplayOrigin::Published(original.try_into().map_err(|_| {
                            capnp::Error::failed("invalid original display invocation".into())
                        })?)
                    }
                    _ => {
                        return Err(capnp::Error::failed(
                            "original display origin is unavailable".into(),
                        ));
                    }
                };
                let reply = display(bytes, origin).map_err(capnp::Error::failed)?;
                if reply.len() > 64 * 1024 {
                    return Err(capnp::Error::failed(
                        "display reply exceeds the wire bound".into(),
                    ));
                }
                results.get().set_reply_json(&reply);
                Ok(())
            })();
            match answered {
                Ok(()) => Promise::ok(()),
                Err(error) => Promise::err(error),
            }
        }

        fn update(
            self: std::rc::Rc<Self>,
            params: app_settings::UpdateParams,
            mut results: app_settings::UpdateResults,
        ) -> Promise<(), capnp::Error> {
            if let Err(error) = (self.revalidate)() {
                return Promise::err(error);
            }
            let written = (|| {
                let params = params.get()?;
                let section = params.get_section()?.to_str()?;
                let bytes = params.get_values_json()?;
                if bytes.len() > MAX_SETTINGS_WIRE_BYTES {
                    return Err(capnp::Error::failed(
                        "settings write exceeds the wire bound".into(),
                    ));
                }
                let Ok(Value::Object(values)) = serde_json::from_slice(bytes) else {
                    return Err(capnp::Error::failed(
                        "a settings write is a JSON object".into(),
                    ));
                };
                // The action the app says made the change, for the history;
                // who invoked it is not the app's to say.
                let origin = settings_store::ChangeOrigin {
                    action: Some(params.get_action()?.to_str()?)
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
                .map_err(|e| capnp::Error::failed(e.to_string()))?
                {
                    update_at(
                        self.source.as_ref(),
                        &self.app_id,
                        self.reach,
                        section,
                        values,
                        &origin,
                    )
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
                }
                let (revision, view) = self.current();
                let mut reply = results.get();
                reply.set_revision(revision);
                reply.set_document_json(&encode(&view)?);
                Ok(())
            })();
            match written {
                Ok(()) => Promise::ok(()),
                Err(error) => Promise::err(error),
            }
        }
    }
}

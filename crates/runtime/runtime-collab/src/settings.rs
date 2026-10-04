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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker refuses or disconnects.
    pub async fn read(&self, after: u64) -> Result<SettingsView, NotAvailable> {
        let mut request = self.inner.read_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        let reply = reply.get()?;
        SettingsView::from_reply(reply.get_revision(), reply.get_document_json()?)
    }

    /// Merge `values_json`, `{"<app id>": {...}}`, into the app's entry in
    /// `section`, answering the view after the write.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the write is too large, the broker
    /// refuses it (another app's entry, a section the app may only read) or
    /// the broker disconnects.
    pub async fn update(
        &self,
        section: &str,
        values_json: &[u8],
    ) -> Result<SettingsView, NotAvailable> {
        if values_json.len() > MAX_SETTINGS_WIRE_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "settings write exceeds the wire bound".into(),
            )));
        }
        let mut request = self.inner.update_request();
        request.get().set_section(section);
        request.get().set_values_json(values_json);
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
    use std::time::Duration;

    use capnp::capability::Promise;
    use serde_json::{Map, Value};
    use settings_store::SettingsSource;
    use settings_store::app_scope::{app_view, update_for_app};

    use super::MAX_SETTINGS_WIRE_BYTES;
    use crate::collab_capnp::app_settings;

    /// How often a waiting [`app_settings::Server::read`] looks for a
    /// change. Every change bumps the source's revision; looking is a
    /// counter read and, for the session's records, one `stat` of the copy.
    const CHANGE_POLL: Duration = Duration::from_millis(200);

    /// Asked before every call; an error refuses it (the channel's
    /// admission was withdrawn).
    pub type Revalidate = Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>;

    /// One app's `AppSettings` capability over `source`.
    pub struct AppSettingsServer {
        app_id: String,
        source: Arc<dyn SettingsSource>,
        revalidate: Revalidate,
        /// The view last answered and the revision it was answered as, so
        /// a change elsewhere in the document wakes no reader.
        answered: RefCell<(u64, Map<String, Value>)>,
    }

    impl AppSettingsServer {
        #[must_use]
        pub fn new(
            app_id: impl Into<String>,
            source: Arc<dyn SettingsSource>,
            revalidate: Revalidate,
        ) -> Self {
            Self {
                app_id: app_id.into(),
                source,
                revalidate,
                answered: RefCell::new((0, Map::new())),
            }
        }

        /// The current view, with a revision that changes exactly when the
        /// view does.
        fn current(&self) -> (u64, Map<String, Value>) {
            let view = app_view(&self.source.load(), &self.app_id);
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
                    let seen = self.source.revision();
                    let (revision, view) = self.current();
                    if revision != after {
                        let mut reply = results.get();
                        reply.set_revision(revision);
                        reply.set_document_json(&encode(&view)?);
                        return Ok(());
                    }
                    while self.source.revision() == seen {
                        tokio::time::sleep(CHANGE_POLL).await;
                        (self.revalidate)()?;
                    }
                }
            })
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
                let section = params
                    .get_section()?
                    .to_str()
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
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
                update_for_app(self.source.as_ref(), &self.app_id, section, values)
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
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

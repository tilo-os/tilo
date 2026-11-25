//! One application's settings over its collaboration channel.
//!
//! A sandboxed app cannot read the session's settings document. Its channel
//! answers its settings calls instead
//! ([`crate::RecordHandle::settings`]): the app reads its view and follows
//! its changes with [`AppSettingsHandle::read`], and writes its own entries
//! with [`AppSettingsHandle::update`]. With the `settings-host` feature,
//! [`host::AppSettingsServer`] answers them from a
//! `settings_store::SettingsSource`: an app sees and writes its own preferences, reads the
//! sections every app's toolkit follows, and sets a setting of the person's or the device's only
//! where it holds a grant for that setting's setter ([`host::AppSettingsServer::with_grants`]).

use runtime_portal::NotAvailable;

use crate::wire::{Reply, SettingsCall};

/// The most bytes one settings view or write carries on the wire.
pub const MAX_SETTINGS_WIRE_BYTES: usize = 1024 * 1024;

/// The most bytes one display request or reply carries on the wire.
const MAX_DISPLAY_WIRE_BYTES: usize = 64 * 1024;

pub use crate::DisplayOrigin;

/// What [`crate::RecordHandle::settings`] answers.
#[derive(Clone)]
pub struct AppSettingsHandle {
    pub(crate) channel: crate::RecordHandle,
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
    pub async fn read(&self, after: u64) -> Result<SettingsView, NotAvailable> {
        SettingsView::from_reply(
            self.channel
                .settings_call(SettingsCall::Read { after })
                .await?,
        )
    }

    /// Invoke the host's separately admitted display transaction service.
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

    use std::sync::Arc;
    use std::sync::Mutex;

    use serde_json::{Map, Value};
    use settings_store::SettingsSource;
    use settings_store::placement::{
        APP_DEVICE_LOCAL_SECTION, APP_PERSONAL_SECTION, MAX_PERSONAL_VALUE_BYTES,
    };

    use super::MAX_SETTINGS_WIRE_BYTES;

    /// Asked before every call; an error refuses it (the channel's
    /// admission was withdrawn).
    pub type Revalidate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

    /// What the host asks of a granted write before it is made: the settings as they are, the
    /// section and what is written to it; an error refuses the write with it.
    pub type Check = Arc<
        dyn Fn(&Map<String, Value>, &str, &Map<String, Value>) -> Result<(), String> + Send + Sync,
    >;

    /// An independently admitted typed display delegate; the caller's identity
    /// is captured by the broker, never read from the request.
    pub type DisplayControl =
        Arc<dyn Fn(&[u8], super::DisplayOrigin) -> Result<Vec<u8>, String> + Send + Sync>;

    /// One app's `AppSettings` capability over `source`.
    pub struct AppSettingsServer {
        app_id: String,
        /// The setters of the person's and the device's settings the app may invoke, as patterns
        /// over their addresses (`settings_store::workspace::EVERY_SETTER`); an app holds none unless granted.
        grants: Vec<String>,
        /// What a granted write must also pass.
        check: Option<Check>,
        display: Option<DisplayControl>,
        source: Arc<dyn SettingsSource>,
        revalidate: Revalidate,
        /// The view last answered and the revision it was answered as, so
        /// a change elsewhere in the document wakes no reader.
        answered: Mutex<(u64, Map<String, Value>)>,
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
                grants: Vec::new(),
                check: None,
                display: None,
                source,
                revalidate,
                answered: Mutex::new((0, Map::new())),
                changed,
                _listener: listener,
            }
        }

        /// Let the app invoke the setters `grants` cover, and read the sections they name.
        #[must_use]
        pub fn with_grants(mut self, grants: Vec<String>) -> Self {
            self.grants = grants;
            self
        }

        /// Hold every granted write to `check` as well.
        #[must_use]
        pub fn with_check(mut self, check: Check) -> Self {
            self.check = Some(check);
            self
        }

        /// Let the device's settings app reach the displays' own transactions.
        #[must_use]
        pub fn with_display(mut self, display: Option<DisplayControl>) -> Self {
            self.display = display;
            self
        }

        /// The current view, with a revision that changes exactly when the
        /// view does.
        #[must_use]
        pub fn view(&self) -> (u64, Map<String, Value>) {
            let view = view(self.source.as_ref(), &self.app_id, &self.grants);
            let mut answered = self
                .answered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if answered.0 == 0 || answered.1 != view {
                answered.0 = answered.0.wrapping_add(1).max(1);
                answered.1 = view;
            }
            answered.clone()
        }
    }

    /// What `app` sees of `source` with `grants`: its own preferences, the shared sections, the
    /// sections its grants name, and the personal dictionary's delivery to it.
    #[must_use]
    pub fn view(source: &dyn SettingsSource, app: &str, grants: &[String]) -> Map<String, Value> {
        let document = source.load();
        let mut view = app_view(&document, app);
        for (section, values) in &document {
            if !view.contains_key(section) && granted(grants, section, None) {
                view.insert(section.clone(), values.clone());
            }
        }
        settings_store::personal_dictionary::delivery::project(&document, app, &mut view);
        view
    }

    /// The shared sections every app reads: how text, motion and colour look to the person.
    pub const APP_READABLE_SECTIONS: &[&str] = &[
        "locale",
        "text_assistance",
        "accessibility",
        "appearance",
        settings_store::appearance::DEVICE_SECTION,
        settings_store::identity::SECTION,
    ];

    /// Whether one of `grants` covers the setter of `field` in `section` (`None`: any setter of
    /// the section, for reading it). A grant is the setter's address with `*` standing for any
    /// part of one segment; the device a device's section names is the one in front, matched by
    /// `*` alone.
    fn granted(grants: &[String], section: &str, field: Option<&str>) -> bool {
        if settings_store::placement::per_app(section) {
            return false;
        }
        let id = settings_store::placement::address(section, None, "*").id;
        let verb = field.map_or_else(|| "set-*".to_owned(), settings_store::workspace::verb);
        let address = format!("{id}#{verb}");
        grants.iter().any(|grant| matches(grant, &address))
    }

    /// Whether `pattern` matches `address` segment by segment, `*` in a pattern's segment
    /// standing for any text within it.
    fn matches(pattern: &str, address: &str) -> bool {
        let (pattern, address): (Vec<_>, Vec<_>) = (
            pattern.split(['/', '#']).collect(),
            address.split(['/', '#']).collect(),
        );
        pattern.len() == address.len()
            && pattern
                .iter()
                .zip(&address)
                .all(|(pattern, part)| settings_store::index::glob_matches(pattern, part))
    }

    /// The largest preferences an app keeps on the device alone.
    const MAX_DEVICE_LOCAL_VALUE_BYTES: usize = 256 * 1024;

    /// What app `app_id` reads of `document`: its own preferences and the shared sections.
    #[must_use]
    pub fn app_view(document: &Map<String, Value>, app_id: &str) -> Map<String, Value> {
        let mut view = Map::new();
        for section in [APP_PERSONAL_SECTION, APP_DEVICE_LOCAL_SECTION] {
            if let Some(entry @ Value::Object(_)) =
                document.get(section).and_then(|held| held.get(app_id))
            {
                view.insert(
                    section.to_owned(),
                    Value::Object(Map::from_iter([(app_id.to_owned(), entry.clone())])),
                );
            }
        }
        for section in APP_READABLE_SECTIONS {
            if let Some(value @ Value::Object(_)) = document.get(*section) {
                view.insert((*section).to_owned(), value.clone());
            }
        }
        view
    }

    /// Write `values` to `section` as app `app_id`: only its own preferences, whole, under its
    /// own id.
    fn update_for_app(
        source: &dyn SettingsSource,
        app_id: &str,
        section: &str,
        values: Map<String, Value>,
        origin: &settings_store::ChangeOrigin,
    ) -> Result<(), String> {
        let limit = match section {
            APP_PERSONAL_SECTION => MAX_PERSONAL_VALUE_BYTES,
            APP_DEVICE_LOCAL_SECTION => MAX_DEVICE_LOCAL_VALUE_BYTES,
            _ => return Err(format!("{app_id} may not write the {section} settings")),
        };
        let mut entries = values.into_iter();
        let (Some((entry, Value::Object(keys))), None) = (entries.next(), entries.next()) else {
            return Err(format!("{app_id} writes one object under its own id"));
        };
        if app_id.is_empty() || entry != app_id {
            return Err(format!("{app_id} may not write the settings of {entry}"));
        }
        let mut stored = match source.load().get(section).and_then(|held| held.get(app_id)) {
            Some(Value::Object(stored)) => stored.clone(),
            _ => Map::new(),
        };
        stored.extend(keys);
        let size = serde_json::to_vec(&stored).map_or(usize::MAX, |bytes| bytes.len());
        if size > limit {
            return Err(format!(
                "{app_id}'s {section} settings would be {size} bytes, more than {limit}"
            ));
        }
        source
            .update_section_by(
                section,
                Map::from_iter([(app_id.to_owned(), Value::Object(stored))]),
                origin,
            )
            .map_err(|error| error.to_string())
    }

    /// Merge `values` (a JSON object, at most [`MAX_SETTINGS_WIRE_BYTES`]) into `section` as app
    /// `app`: through the setters `grants` cover, held to `check` as well, or else into its own
    /// preferences. `action` names the action that made the change, for the history.
    ///
    /// # Errors
    /// The write is out of bounds, no JSON object, beyond the app's grants, or the source
    /// refused it.
    pub fn update(
        source: &dyn SettingsSource,
        app: &str,
        (grants, check): (&[String], Option<&Check>),
        (section, values, action): (&str, &[u8], Option<&str>),
    ) -> Result<(), String> {
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
            actor: None,
        };
        if settings_store::personal_dictionary::delivery::command(source, app, section, &values)
            .map_err(|e| e.to_string())?
        {
            return Ok(());
        }
        if section == settings_store::personal_dictionary::SECTION {
            return Err("the personal dictionary is edited through its own command".to_owned());
        }
        if !values.is_empty()
            && values
                .keys()
                .all(|field| granted(grants, section, Some(field)))
        {
            // A field the section declares read only is changed by an action the section
            // declares, never by a record change an app writes.
            if let Some(refusal) = source.readonly_refusal(section, &values) {
                return Err(refusal);
            }
            if let Some(check) = check {
                check(&source.load(), section, &values)?;
            }
            // Each field is set by its own setter, which the store records it under.
            source
                .update_section_by(section, values, &origin)
                .map_err(|error| error.to_string())?;
        } else {
            update_for_app(source, app, section, values, &origin)?;
        }
        Ok(())
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
        pub async fn read(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
            loop {
                (self.revalidate)()?;
                // Armed before looking, so a change between the look and the
                // wait still wakes it.
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let (revision, view) = self.view();
                if revision != after {
                    return Ok((revision, encode(&view)?));
                }
                changed.await;
            }
        }

        /// Run a display transaction for the device's settings app.
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
        pub fn update(
            &self,
            section: &str,
            values: &[u8],
            action: Option<&str>,
        ) -> Result<(u64, Vec<u8>), String> {
            (self.revalidate)()?;
            update(
                self.source.as_ref(),
                &self.app_id,
                (&self.grants, self.check.as_ref()),
                (section, values, action),
            )?;
            let (revision, view) = self.view();
            Ok((revision, encode(&view)?))
        }
    }
}

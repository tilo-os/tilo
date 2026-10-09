//! Authenticated settings-channel projection of the one local dictionary.
//! The projection is never written to app settings or exported room records.

use super::{Edit, Service, Snapshot, Vocabulary};
use crate::{ChangeSignal, Listener, SettingsSource};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub const VIEW_KEY: &str = "__personal_dictionary";
pub const EDIT_KEY: &str = "__personal_dictionary_edit";

#[derive(Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub owner: [u8; 32],
    pub snapshot: Snapshot,
}

#[derive(Serialize, Deserialize)]
pub struct Command {
    pub owner: [u8; 32],
    pub revision: u64,
    pub edit: Edit,
}

/// Wrap the session's existing settings source with host-only dictionary
/// authority. The owner callback resolves the host's authenticated identity.
pub struct Host {
    source: Arc<dyn SettingsSource>,
    service: Arc<Service>,
    owner: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>,
    changes: Arc<ChangeSignal>,
    _listener: Arc<Listener>,
    migrated_writer: Mutex<HashSet<[u8; 32]>>,
    legacy_owner: Option<[u8; 32]>,
    #[cfg(feature = "watch")]
    _dictionary_watch: Option<crate::watch::SettingsWatch>,
}

impl Host {
    pub fn new(
        source: Arc<dyn SettingsSource>,
        service: Arc<Service>,
        owner: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>,
    ) -> Self {
        let changes = Arc::new(ChangeSignal::default());
        let ring = Arc::clone(&changes);
        let listener: Arc<Listener> = Arc::new(move || ring.ring());
        source.changes().listen(&listener);
        service.changes().listen(&listener);
        #[cfg(feature = "watch")]
        let dictionary_watch = service.watch().ok();
        let host = Self {
            source,
            service,
            owner,
            changes,
            _listener: listener,
            migrated_writer: Mutex::new(HashSet::new()),
            legacy_owner: None,
            #[cfg(feature = "watch")]
            _dictionary_watch: dictionary_watch,
        };
        host.dictionary_service();
        host
    }
}

impl Host {
    /// Attach the verified original profile root, never the active account
    /// supplied by an app. Missing provenance leaves old words unexported.
    #[must_use]
    pub fn with_legacy_owner(mut self, owner: Option<[u8; 32]>) -> Self {
        self.legacy_owner = owner;
        self.dictionary_service();
        self
    }

    fn migrate_writer(&self) {
        let Some(owner) = self
            .service
            .owner()
            .filter(|owner| self.legacy_owner == Some(*owner))
        else {
            return;
        };
        if self
            .migrated_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&owner)
        {
            return;
        }
        let document = self.source.load();
        let Some(writer) = document
            .get(crate::continuity::APP_PERSONAL_SECTION)
            .and_then(|section| section.get("so.tilo.word-processor"))
            .and_then(Value::as_object)
        else {
            return;
        };
        let Some(words) = writer
            .get("dictionary_words")
            .cloned()
            .and_then(|words| serde_json::from_value::<Vec<String>>(words).ok())
        else {
            return;
        };
        let Some(reader) = self.service.reader_for(owner) else {
            return;
        };
        let Ok(snapshot) = reader.snapshot() else {
            return;
        };
        if self
            .service
            .edit(&reader, snapshot.revision, Edit::ImportWriter { words })
            .is_ok()
        {
            self.migrated_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(owner);
            let mut cleared = writer.clone();
            cleared.remove("dictionary_words");
            // Publish only after the private dictionary committed. A failed
            // cleanup retains recoverable legacy data but it is never projected.
            let _ = self.source.update_section(
                crate::continuity::APP_PERSONAL_SECTION,
                Map::from_iter([("so.tilo.word-processor".to_owned(), Value::Object(cleared))]),
            );
        }
    }
}

impl SettingsSource for Host {
    fn load(&self) -> Map<String, Value> {
        let mut document = self.source.load();
        if let Some(writer) = document
            .get_mut(crate::continuity::APP_PERSONAL_SECTION)
            .and_then(|section| section.get_mut("so.tilo.word-processor"))
            .and_then(Value::as_object_mut)
        {
            writer.remove("dictionary_words");
        }
        document
    }
    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        self.source.update_section(name, values)
    }
    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &crate::ChangeOrigin,
    ) -> std::io::Result<()> {
        self.source.update_section_by(name, values, origin)
    }
    fn update_section_with(
        &self,
        name: &str,
        origin: &crate::ChangeOrigin,
        change: &mut crate::SectionChange<'_>,
    ) -> std::io::Result<()> {
        self.source.update_section_with(name, origin, change)
    }
    fn revision(&self) -> u64 {
        self.source.revision()
    }
    fn changes(&self) -> &ChangeSignal {
        &self.changes
    }
    fn dictionary_legacy_owner(&self) -> Option<[u8; 32]> {
        self.legacy_owner
    }
    fn dictionary_service(&self) -> Option<Arc<Service>> {
        let owner = (self.owner)();
        if self.service.owner() != owner {
            self.service.bind(owner);
        }
        if owner.is_some() {
            self.migrate_writer();
        }
        Some(Arc::clone(&self.service))
    }
}

/// Add a virtual read view only for a host-certified local person. `owner`
/// comes from the admitted descriptor, never from app JSON. No dictionary
/// bytes are placed in `source.load()` or a persisted settings document.
pub fn project(
    source: &dyn SettingsSource,
    app: &str,
    owner: Option<[u8; 32]>,
    view: &mut Map<String, Value>,
) {
    let Some(service) = source.dictionary_service() else {
        return;
    };
    let Some(owner) = owner.filter(|owner| Some(*owner) == service.owner()) else {
        return;
    };
    let Some(snapshot) = service
        .reader_for(owner)
        .and_then(|reader| reader.snapshot().ok())
    else {
        return;
    };
    let section = view
        .entry(crate::continuity::APP_DEVICE_LOCAL_SECTION.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(section) = section.as_object_mut() else {
        return;
    };
    let entry = section
        .entry(app.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(entry) = entry.as_object_mut()
        && let Ok(delivery) = serde_json::to_value(Delivery { owner, snapshot })
    {
        entry.insert(VIEW_KEY.to_owned(), delivery);
    }
}

/// Handle a dictionary command within the app's already-authorized own
/// device-local entry. Local app owners may add explicit words; only Settings
/// and Writer can edit/remove/reset. Commands never change permissions or stores.
///
/// # Errors
/// Unowned/malformed commands, revoked bindings, stale edits or storage errors.
pub fn command(
    source: &dyn SettingsSource,
    app: &str,
    owner: Option<[u8; 32]>,
    section: &str,
    values: &Map<String, Value>,
) -> std::io::Result<bool> {
    if app.is_empty() {
        return Err(denied());
    }
    if section != crate::continuity::APP_DEVICE_LOCAL_SECTION {
        return Ok(false);
    }
    let own = values.get(app).and_then(Value::as_object);
    let Some(own) = own else {
        return Ok(false);
    };
    if own.contains_key(VIEW_KEY) {
        return Err(denied());
    }
    let Some(command) = own.get(EDIT_KEY) else {
        return Ok(false);
    };
    if values.len() != 1 || own.len() != 1 {
        return Err(denied());
    }
    let command: Command =
        serde_json::from_value(command.clone()).map_err(std::io::Error::other)?;
    if !matches!(command.edit, Edit::Add(_))
        && !["so.tilo.settings", "so.tilo.word-processor"].contains(&app)
    {
        return Err(denied());
    }
    if matches!(command.edit, Edit::ImportWriter { .. })
        && (app != "so.tilo.word-processor"
            || source.dictionary_legacy_owner() != owner
            || owner.is_none())
    {
        return Err(denied());
    }
    let service = source.dictionary_service().ok_or_else(denied)?;
    if owner != Some(command.owner) || service.owner() != owner {
        return Err(denied());
    }
    let reader = service.reader_for(command.owner).ok_or_else(denied)?;
    service.edit(&reader, command.revision, command.edit)?;
    Ok(true)
}

fn denied() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "personal dictionary authority is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn alternate_account_cannot_adopt_unscoped_primary_profile_words() {
        let directory = tempfile::tempdir().unwrap();
        let service = Arc::new(Service::new(directory.path().to_owned()).unwrap());
        let document = Arc::new(crate::HostDocument::new(
            serde_json::from_value(serde_json::json!({
                "app_personal": {"so.tilo.word-processor": {"dictionary_words": ["PrivateAlice"]}}
            }))
            .unwrap(),
            |_| Ok(()),
        ));
        let active = Arc::new(Mutex::new(Some([2; 32])));
        let account = Arc::clone(&active);
        let host = Host::new(
            document.clone(),
            service.clone(),
            Arc::new(move || *account.lock().unwrap()),
        )
        .with_legacy_owner(Some([1; 32]));
        assert!(
            service
                .reader()
                .unwrap()
                .snapshot()
                .unwrap()
                .words
                .is_empty()
        );
        assert!(
            host.load()["app_personal"]["so.tilo.word-processor"]
                .get("dictionary_words")
                .is_none()
        );
        let values = Map::from_iter([(
            "so.tilo.word-processor".into(),
            serde_json::json!({EDIT_KEY: Command {owner: [2; 32], revision: 0, edit: Edit::ImportWriter {words: vec!["PrivateAlice".into()]}}}),
        )]);
        assert!(
            command(
                &host,
                "so.tilo.word-processor",
                Some([2; 32]),
                crate::continuity::APP_DEVICE_LOCAL_SECTION,
                &values
            )
            .is_err()
        );
        assert_eq!(
            document.load()["app_personal"]["so.tilo.word-processor"]["dictionary_words"],
            serde_json::json!(["PrivateAlice"])
        );
        *active.lock().unwrap() = Some([1; 32]);
        host.dictionary_service();
        assert_eq!(
            service
                .reader()
                .unwrap()
                .snapshot()
                .unwrap()
                .hints("en")
                .collect::<Vec<_>>(),
            ["PrivateAlice"]
        );
        assert!(
            document.load()["app_personal"]["so.tilo.word-processor"]
                .get("dictionary_words")
                .is_none()
        );
    }

    #[test]
    fn host_imports_and_removes_legacy_writer_words_without_exporting_them() {
        let directory = tempfile::tempdir().unwrap();
        let service = Arc::new(Service::new(directory.path().to_owned()).unwrap());
        let document = Arc::new(crate::HostDocument::new(serde_json::from_value(serde_json::json!({
            "app_personal": {"so.tilo.word-processor": {"dictionary_words": ["Tilo", "Élodie"], "show_ruler": true}}
        })).unwrap(), |_| Ok(())));
        let host = Host::new(
            document.clone(),
            service.clone(),
            Arc::new(|| Some([1; 32])),
        )
        .with_legacy_owner(Some([1; 32]));
        assert_eq!(
            service
                .reader()
                .unwrap()
                .snapshot()
                .unwrap()
                .hints("en")
                .collect::<Vec<_>>(),
            ["Tilo", "Élodie"]
        );
        assert!(
            document.load()["app_personal"]["so.tilo.word-processor"]
                .get("dictionary_words")
                .is_none()
        );
        assert_eq!(
            document.load()["app_personal"]["so.tilo.word-processor"]["show_ruler"],
            true
        );
        document
            .update_section(
                "app_personal",
                Map::from_iter([(
                    "so.tilo.word-processor".to_owned(),
                    serde_json::json!({"dictionary_words": ["Tilo"], "show_ruler": true}),
                )]),
            )
            .unwrap();
        assert!(
            host.load()["app_personal"]["so.tilo.word-processor"]
                .get("dictionary_words")
                .is_none()
        );
        let bob = Host::new(document, service.clone(), Arc::new(|| Some([2; 32])));
        assert!(
            service
                .reader()
                .unwrap()
                .snapshot()
                .unwrap()
                .words
                .is_empty()
        );
        assert!(
            bob.load()["app_personal"]["so.tilo.word-processor"]
                .get("dictionary_words")
                .is_none()
        );
    }

    #[test]
    fn authenticated_projection_and_edits_are_local_revocable_and_never_persisted() {
        let directory = tempfile::tempdir().unwrap();
        let service = Arc::new(Service::new(directory.path().to_owned()).unwrap());
        let account = Arc::new(Mutex::new(Some([1; 32])));
        let identity = Arc::clone(&account);
        let document = Arc::new(crate::HostDocument::new(Map::new(), |_| Ok(())));
        let host = Host::new(
            document.clone(),
            service.clone(),
            Arc::new(move || *identity.lock().unwrap()),
        );
        let mut view = Map::new();
        project(&host, "so.tilo.osk", None, &mut view);
        assert!(view.is_empty(), "visitors receive no personal words");
        project(&host, "so.tilo.osk", Some([2; 32]), &mut view);
        assert!(view.is_empty(), "another account receives no projection");
        let values = Map::from_iter([(
            "so.tilo.settings".to_owned(),
            Value::Object(Map::from_iter([(
                EDIT_KEY.to_owned(),
                serde_json::to_value(Command {
                    owner: [1; 32],
                    revision: 0,
                    edit: Edit::Add(super::super::Word::new("Tilo", None).unwrap()),
                })
                .unwrap(),
            )])),
        )]);
        assert!(
            command(
                &host,
                "so.tilo.settings",
                None,
                crate::continuity::APP_DEVICE_LOCAL_SECTION,
                &values
            )
            .is_err()
        );
        assert!(
            command(
                &host,
                "so.tilo.settings",
                Some([1; 32]),
                crate::continuity::APP_DEVICE_LOCAL_SECTION,
                &values
            )
            .unwrap()
        );
        assert!(
            document.load().is_empty(),
            "no personal words or commands enter settings records"
        );
        for app in ["so.tilo.osk", "so.tilo.dictation", "so.tilo.word-processor"] {
            let mut view = Map::new();
            project(&host, app, Some([1; 32]), &mut view);
            let delivery: Delivery = serde_json::from_value(
                view[crate::continuity::APP_DEVICE_LOCAL_SECTION][app][VIEW_KEY].clone(),
            )
            .unwrap();
            assert_eq!(delivery.snapshot.hints("nl").collect::<Vec<_>>(), ["Tilo"]);
        }
        assert!(
            command(
                &host,
                "so.tilo.settings",
                Some([1; 32]),
                crate::continuity::APP_DEVICE_LOCAL_SECTION,
                &values
            )
            .is_err(),
            "stale edits fail"
        );
        let old_reader = service.reader().unwrap();
        *account.lock().unwrap() = Some([2; 32]);
        view.clear();
        project(&host, "so.tilo.osk", Some([1; 32]), &mut view);
        assert!(view.is_empty());
        assert!(old_reader.snapshot().is_err());
        project(&host, "so.tilo.osk", Some([2; 32]), &mut view);
        let delivery: Delivery = serde_json::from_value(
            view[crate::continuity::APP_DEVICE_LOCAL_SECTION]["so.tilo.osk"][VIEW_KEY].clone(),
        )
        .unwrap();
        assert!(delivery.snapshot.words.is_empty());
        assert!(
            command(
                &host,
                "so.tilo.settings",
                Some([1; 32]),
                crate::continuity::APP_DEVICE_LOCAL_SECTION,
                &values
            )
            .is_err()
        );
        *account.lock().unwrap() = None;
        view.clear();
        project(&host, "so.tilo.osk", Some([2; 32]), &mut view);
        assert!(view.is_empty());
    }

    #[test]
    fn writer_migration_claims_the_unscoped_store_once_and_reset_cannot_resurrect_it() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::new(directory.path().to_owned()).unwrap();
        let alice = service.bind(Some([1; 32])).unwrap();
        let migrated = service
            .edit(
                &alice,
                0,
                Edit::ImportWriter {
                    words: vec!["Tilo".into(), "Élodie".into(), "東京".into(), "Tilo".into()],
                },
            )
            .unwrap();
        assert_eq!(migrated.words.len(), 3);
        let reset = service
            .edit(&alice, migrated.revision, Edit::Reset)
            .unwrap();
        assert!(
            service
                .edit(
                    &alice,
                    reset.revision,
                    Edit::ImportWriter {
                        words: vec!["Tilo".into()]
                    }
                )
                .unwrap()
                .words
                .is_empty()
        );
        let bob = service.bind(Some([2; 32])).unwrap();
        assert!(
            service
                .edit(
                    &bob,
                    0,
                    Edit::ImportWriter {
                        words: vec!["Tilo".into()]
                    }
                )
                .unwrap()
                .words
                .is_empty()
        );
        let restarted = Service::new(directory.path().to_owned()).unwrap();
        let alice = restarted.bind(Some([1; 32])).unwrap();
        let snapshot = alice.snapshot().unwrap();
        assert!(
            restarted
                .edit(
                    &alice,
                    snapshot.revision,
                    Edit::ImportWriter {
                        words: vec!["Tilo".into()]
                    }
                )
                .unwrap()
                .words
                .is_empty()
        );
    }
}

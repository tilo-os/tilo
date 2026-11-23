//! The person's settings following them to their other devices.
//!
//! The sections that are the person's ([`settings_store::placement`]) are one resource of the
//! workspace whose only member is them, which every device of theirs takes part in
//! ([`settings_model::person`]): each leaf under `tilo.<section>.<leaf>`, an object under a leaf
//! travelling as one value. A write of one goes to that resource first, through its typed action
//! at the device's daemon ([`Roaming`], the settings' [`settings_store::RoamingSettings`]), and
//! the settings document keeps a copy every process reads at once; what another of their devices
//! wrote there is taken into that copy as the session's follow of the resource reads it — so the
//! theme, the language or an app's preference chosen on the laptop is what this desktop reads
//! next, and nothing of this machine's (`app_device_local`, the clock, the display) ever leaves
//! it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use settings_model::person::{self, Leaf, PersonSettings as Resource};
use settings_store::person_displays::{PLACEMENTS_KEY, Placements};
use settings_store::placement::Holder;
use tilo_lock::MutexRecover as _;
use tilo_resource::Resource as _;
use tilo_session_client::daemon_documents::DaemonShell;

use crate::followed_resource::FollowedResource;

/// The person's settings, kept in step with their other devices.
#[derive(Default)]
pub(crate) struct PersonSettings {
    /// The resource, once the session follows it.
    followed: RefCell<Option<FollowedResource<Resource>>>,
    /// What the resource held as last read, by key: what a write merges the person's displays'
    /// places with.
    theirs: Arc<Mutex<BTreeMap<String, Value>>>,
    /// The reading the settings document last took in: `None` before the first, which also
    /// offers what this device kept before it had the resource.
    seen: RefCell<Option<u64>>,
}

/// The workspace whose only member is the person, which every device of theirs takes part in
/// from sign-in.
fn persons_workspace(person: &[u8; 32]) -> String {
    tilo_server_protocol::servers::person_workspace_id(person)
}

/// The person's resource, as the settings write to it: every write of a setting the person holds
/// goes to it first, through its typed action at the device's daemon.
struct Roaming {
    daemon: DaemonShell,
    workspace: String,
    theirs: Arc<Mutex<BTreeMap<String, Value>>>,
}

impl Roaming {
    /// `leaves` as the resource's action takes them. Each display's place is its own: what
    /// another device placed meanwhile stays.
    fn leaves(&self, leaves: &[(String, String, Value)]) -> Result<Vec<Leaf>, String> {
        let theirs = self.theirs.lock_recover();
        leaves
            .iter()
            .map(|(section, leaf, value)| {
                let key = person::key(section, leaf)?;
                let value = match theirs.get(&key) {
                    Some(held) if key == PLACEMENTS_KEY && !value.is_null() => {
                        placements([held, value])
                    }
                    _ => value.clone(),
                };
                let value = (!value.is_null())
                    .then(|| serde_json::to_string(&value).map_err(|error| error.to_string()))
                    .transpose()?;
                if value
                    .as_ref()
                    .is_some_and(|value| value.len() > person::MAX_PERSONAL_VALUE_BYTES)
                {
                    return Err(format!("{key} is too large for the person's devices"));
                }
                Ok(Leaf {
                    section: section.clone(),
                    leaf: leaf.clone(),
                    value,
                })
            })
            .collect()
    }
}

impl settings_store::RoamingSettings for Roaming {
    fn write(
        &self,
        leaves: &[(String, String, Value)],
        _origin: &settings_store::ChangeOrigin,
    ) -> Result<(), String> {
        let leaves = self.leaves(leaves)?;
        let (daemon, workspace) = (self.daemon.clone(), self.workspace.clone());
        // The settings are written from whatever thread changed them: the daemon is asked on its
        // own, and what it applied comes back with the follow.
        self.daemon.runtime().spawn(async move {
            if let Err(error) = set_leaves(&daemon, &workspace, leaves).await {
                tracing::warn!(%error, "a setting did not reach the person's other devices");
            }
        });
        Ok(())
    }
}

/// Run the resource's action setting `leaves`, at the device's daemon.
async fn set_leaves(
    daemon: &DaemonShell,
    workspace: &str,
    leaves: Vec<Leaf>,
) -> Result<(), String> {
    let mut arguments = Map::new();
    arguments.insert(
        "leaves".to_owned(),
        serde_json::to_value(leaves).map_err(|error| error.to_string())?,
    );
    let request = tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: format!(
                "{}:{}:{}",
                tilo_actions::remote::RESOURCE_PROVIDER,
                person::APP,
                person::RESOURCE
            ),
            address: tilo_actions::ActionAddress::new(Resource::KIND, person::SET_LEAVES),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    daemon
        .dispatch(
            Some(workspace),
            &tilo_actions::remote::RemoteActionMessage::Invoke(request),
        )
        .await
        .map(drop)
}

impl PersonSettings {
    /// Follow the person's resource once the session reaches the device's daemon, writing the
    /// settings the person holds there from then on, and take into the settings document what it
    /// holds as it moves. Returns whether the settings here changed.
    pub(crate) fn sync(
        &self,
        daemon: &DaemonShell,
        person: &[u8; 32],
        wake: &tilo_service::Wake,
    ) -> bool {
        let Some(source) = settings_store::source() else {
            return false;
        };
        let Some(store) = source.store() else {
            return false;
        };
        let workspace = persons_workspace(person);
        if self.followed.borrow().is_none() {
            let followed = FollowedResource::<Resource>::follow(
                daemon,
                (&workspace, person::APP, person::RESOURCE),
                tilo_action_policy::PersonId::from_root(*person).as_str(),
                None,
                Arc::clone(wake),
            );
            store.keep_roaming_in(Some(Arc::new(Roaming {
                daemon: daemon.clone(),
                workspace: workspace.clone(),
                theirs: Arc::clone(&self.theirs),
            })));
            *self.followed.borrow_mut() = Some(followed);
            *self.seen.borrow_mut() = None;
        }
        let Some(followed) = self.followed.borrow().clone() else {
            return false;
        };
        // Until the resource was read, nothing of it is known.
        if followed.held().is_none() {
            return false;
        }
        let revision = followed.revision();
        if *self.seen.borrow() == Some(revision) {
            return false;
        }
        let theirs = values(&followed.get());
        theirs.clone_into(&mut self.theirs.lock_recover());
        let document = settings_store::load();
        if self.seen.borrow().is_none() {
            // What this device kept before it had the person's resource (chosen as it was set
            // up, or before it held the workspace) is the person's where the resource holds
            // nothing of it yet.
            offer(daemon, &workspace, &document, &theirs);
        }
        let changed = take(store, &document, &theirs);
        *self.seen.borrow_mut() = Some(revision);
        changed
    }
}

/// Write into the person's resource each setting `document` holds that the person holds and
/// `theirs` does not.
fn offer(
    daemon: &DaemonShell,
    workspace: &str,
    document: &Map<String, Value>,
    theirs: &BTreeMap<String, Value>,
) {
    let mut leaves = Vec::new();
    for (section, values) in document {
        if settings_store::placement::holder(section) != Holder::Person {
            continue;
        }
        let Value::Object(values) = values else {
            continue;
        };
        for (leaf, value) in values {
            let Ok(key) = person::key(section, leaf) else {
                continue;
            };
            if theirs.contains_key(&key) {
                continue;
            }
            let Ok(text) = serde_json::to_string(value) else {
                continue;
            };
            if text.len() > person::MAX_PERSONAL_VALUE_BYTES {
                tracing::warn!(%key, "a setting is too large for the person's devices");
                continue;
            }
            leaves.push(Leaf {
                section: section.clone(),
                leaf: leaf.clone(),
                value: Some(text),
            });
        }
    }
    if leaves.is_empty() {
        return;
    }
    let (daemon, workspace) = (daemon.clone(), workspace.to_owned());
    daemon.runtime().clone().spawn(async move {
        if let Err(error) = set_leaves(&daemon, &workspace, leaves).await {
            tracing::warn!(%error, "the settings here did not reach the person's other devices");
        }
    });
}

/// Take into the settings document every setting `theirs` holds that it does not: what the person
/// set on their other devices. Returns whether the document changed.
fn take(
    store: &settings_store::Store,
    document: &Map<String, Value>,
    theirs: &BTreeMap<String, Value>,
) -> bool {
    let mut sections = BTreeMap::<&str, Map<String, Value>>::new();
    for (key, value) in theirs {
        let Some((section, leaf)) = person::leaf_of(key) else {
            continue;
        };
        // Only a section the person holds is theirs to set from another device.
        if settings_store::placement::holder(section) != Holder::Person
            || document
                .get(section)
                .and_then(Value::as_object)
                .and_then(|values| values.get(leaf))
                == Some(value)
        {
            continue;
        }
        sections
            .entry(section)
            .or_default()
            .insert(leaf.to_owned(), value.clone());
    }
    if sections.is_empty() {
        return false;
    }
    let sections = sections
        .into_iter()
        .map(|(section, values)| (section.to_owned(), values))
        .collect();
    match store.take_roaming(sections) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "what the person's other devices set was not kept");
            false
        }
    }
}

/// The settings the person's resource holds, by key.
fn values(state: &<Resource as tilo_resource::Resource>::State) -> BTreeMap<String, Value> {
    state
        .leaves
        .iter()
        .filter_map(|(key, text)| {
            let value = tilo_failure::ignored!(
                "a value no build of this person's wrote is no setting of theirs",
                serde_json::from_str(text).ok()
            )?;
            Some((key.clone(), value))
        })
        .collect()
}

/// The placements `written` hold, merged display by display.
fn placements<'a>(written: impl IntoIterator<Item = &'a Value>) -> Value {
    Placements::merge(
        written
            .into_iter()
            .filter_map(Placements::from_value)
            .map(|placement| ("", placement)),
    )
    .to_value()
}

#[cfg(test)]
mod tests {
    use tilo_action_policy::PersonId;
    use tilo_desktop_model::WorkspaceId;
    use tilo_resource::Reader;

    use settings_store::SettingsSource as _;

    use super::*;
    use crate::daemon_fixture::TestDaemon;

    /// A settings store this test's device keeps, installed as the process's, as a session's is.
    fn installed(dir: &std::path::Path) -> Arc<settings_store::Store> {
        let store = settings_store::Store::open(dir).expect("a settings store");
        let source: Arc<dyn settings_store::SettingsSource> = Arc::clone(&store);
        settings_store::install_source(Some(source));
        store
    }

    fn wake() -> tilo_service::Wake {
        Arc::new(|| {})
    }

    /// A JSON object as a settings section.
    fn section(values: Value) -> Map<String, Value> {
        values.as_object().cloned().expect("a section is an object")
    }

    /// The person's settings as `daemon` holds them, once it holds any.
    fn recorded(daemon: &TestDaemon, workspace: &str) -> Option<BTreeMap<String, Value>> {
        let bytes = tilo_rt::app()
            .block_on(daemon.workspaces().resource_document(
                workspace,
                person::APP,
                person::RESOURCE,
            ))
            .expect("the daemon reads the workspace")?;
        let doc = automerge::AutoCommit::load(&bytes).expect("a document");
        let reader = Reader::<Resource>::read(&doc).expect("the person's settings read");
        Some(values(reader.state()))
    }

    /// The workspace whose only member is the person, founded at `daemon`.
    fn persons(daemon: &TestDaemon) -> ([u8; 32], String) {
        let person = daemon.certificate().user_public_key;
        let workspace = persons_workspace(&person);
        daemon.found(
            WorkspaceId::parse(&workspace).expect("the person's workspace has an id"),
            "Person",
        );
        (person, workspace)
    }

    /// What the person holds travels to their other devices: each section of theirs, none of this
    /// machine's, and an app's entry as one value.
    #[test]
    fn only_the_persons_sections_travel_and_an_apps_entry_travels_whole() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let dir = tempfile::tempdir().expect("a settings directory");
        let store = installed(dir.path());
        // What this device kept before it had the person's resource: the sections that are the
        // person's, and ones that are this machine's alone.
        store
            .update_section(
                "appearance",
                section(serde_json::json!({"mode": "dark", "wallpaper": "dunes"})),
            )
            .expect("this device's appearance is kept");
        store
            .update_section(
                settings_model::APP_PERSONAL_SECTION,
                section(serde_json::json!({"so.tilo.monitor": {"confirm_kill": true}})),
            )
            .expect("an app's entry is kept");
        store
            .update_section(
                settings_model::APP_DEVICE_LOCAL_SECTION,
                section(serde_json::json!({"so.tilo.monitor": {"device": 1}})),
            )
            .expect("this machine's entry is kept");
        store
            .update_section("power", section(serde_json::json!({"lock_minutes": 1})))
            .expect("this machine's power is kept");

        let daemon = TestDaemon::start();
        let (person, workspace) = persons(&daemon);
        let shell = daemon.shell();
        let wake = wake();
        let settings = PersonSettings::default();
        let theirs = tilo_test_env::wait_for("the person's settings to reach their record", || {
            let _ = settings.sync(&shell, &person, &wake);
            recorded(&daemon, &workspace)
                .filter(|theirs| theirs.contains_key("tilo.appearance.mode"))
        });

        let mut keys: Vec<&str> = theirs.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "tilo.app_personal.so.tilo.monitor",
                "tilo.appearance.mode",
                "tilo.appearance.wallpaper",
            ],
            "this device's sections stay on it"
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                theirs
                    .get("tilo.app_personal.so.tilo.monitor")
                    .expect("the app's entry travelled")
            )
            .expect("the entry's value"),
            serde_json::json!({"confirm_kill": true}),
            "an app's entry is one value, as it travels"
        );
        let document = store.load();
        assert_eq!(
            document["power"]["lock_minutes"], 1,
            "the machine keeps its power"
        );
        assert_eq!(
            document[settings_model::APP_DEVICE_LOCAL_SECTION]["so.tilo.monitor"],
            serde_json::json!({"device": 1}),
            "the machine keeps its own sections"
        );
        assert_eq!(document["appearance"]["mode"], "dark");
    }

    /// A setting one device sets reaches the person's resource, and another of their devices reads
    /// it there: the change names the person, is made on their own device, and needs no
    /// invocation because nobody else asked for it.
    #[test]
    fn a_device_sets_a_setting_and_the_persons_other_devices_see_it() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let dir = tempfile::tempdir().expect("a settings directory");
        let _store = installed(dir.path());
        let daemon = TestDaemon::start();
        let (person, workspace) = persons(&daemon);
        let shell = daemon.shell();
        let wake = wake();
        let settings = PersonSettings::default();
        // This device follows the person's resource, and has read it.
        tilo_test_env::wait_until("the person's settings are followed", || {
            let _ = settings.sync(&shell, &person, &wake);
            settings
                .followed
                .borrow()
                .as_ref()
                .is_some_and(|followed| followed.held() == Some(true))
        });

        // This device sets a setting the person holds, as the settings' own setter does.
        settings_store::appearance::MODE
            .set(Some("light".to_owned()))
            .expect("the setting is written");

        // Another of the person's devices follows the resource and reads what this one set.
        let other = FollowedResource::<Resource>::follow(
            &shell,
            (&workspace, person::APP, person::RESOURCE),
            PersonId::from_root(person).as_str(),
            None,
            Arc::clone(&wake),
        );
        tilo_test_env::wait_until("another device reads the setting this one set", || {
            other
                .get()
                .leaves
                .get("tilo.appearance.mode")
                .is_some_and(|text| {
                    serde_json::from_str::<Value>(text).ok() == Some(serde_json::json!("light"))
                })
        });

        let bytes = tilo_rt::app()
            .block_on(daemon.workspaces().resource_document(
                &workspace,
                person::APP,
                person::RESOURCE,
            ))
            .expect("the daemon reads the workspace")
            .expect("the person's settings are written");
        let doc = automerge::AutoCommit::load(&bytes).expect("a document");
        let context = doc
            .get_changes(&[])
            .into_iter()
            .rev()
            .find_map(|change| {
                change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)
            })
            .expect("the person's settings keep the history of what set them");
        let invoker = context
            .invoker
            .as_ref()
            .expect("the change names who set it");
        assert_eq!(
            invoker.acting.person_id(),
            Some(PersonId::from_root(person).as_str()),
            "the device's write is its person's"
        );
        assert!(
            invoker.device.is_none(),
            "made on the person's own device, for nobody else"
        );
        assert_eq!(
            tilo_server_protocol::invocation::verify_recorded(
                &context,
                &workspace,
                &tilo_user_identity::RevocationList::new(),
                u64::MAX,
            ),
            Ok(None),
            "a person's own change carries no invocation and needs none"
        );
    }
}

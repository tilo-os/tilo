//! The notices an app raises on a device with no desktop session, carried into the record of the
//! person's devices ([`ServedNotices`]).
//!
//! An app tells its person something in a `host-notice` document of its own record; a desktop
//! session shows those as notifications, and what it shows of this device's its session keeps in
//! the record of the person's devices. A device that serves pages runs the app with no session,
//! so nothing kept its notices there: this device reads the notices its apps wrote
//! ([`Workspaces::app_notices`]) and shows each in the record under an id of its own
//! ([`shared_id`]), from which every device of the person's and every page of theirs raises it.
//!
//! What the person answers there — dismissing the notice, or choosing one of the actions the app
//! offered — is carried out on the device the notice was sent on: dismissing marks the app's
//! notice seen in its record, choosing runs that action of the app on what the notice opens, and
//! either takes the notice out of the record of their devices
//! ([`Devices::retiring_notice`]), which takes every copy away. A notice whose time in the record
//! runs out, which its app withdraws, or which leaves the record elsewhere is taken off the app's
//! record too, so no device raises it again.

use std::collections::BTreeMap;

use tilolang_tilo::action::EDIT_FLAG_ACTION;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::tilolang::{
    NOTICE_ACTIONS, NOTICE_BODY, NOTICE_DEVICE, NOTICE_OPENS, NOTICE_SEEN, NOTICE_TITLE, NOTICE_TO,
    NOTICE_TYPE,
};

use crate::tl_records::Opened;
use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace::{
    self,
    devices::{DEVICES, Devices, MAX_NOTICE_SHOWN_MS, NoticeAction, SharedNotice},
};

/// The package an app's record is read through here: the one this build carries for it, the one
/// this device installed, or one compiled here (a test's).
#[derive(Clone)]
pub enum Package {
    /// The package this build carries ([`crate::tl_records::built_in`]).
    BuiltIn,
    /// The package this device installed, verified as a launch of it verifies it.
    Installed(runtime_portal::process_link::Module),
    /// A package compiled here, as a test's app is.
    #[cfg(any(test, feature = "test-support"))]
    Compiled(crate::tl_records::Packages),
}

/// A notice an app wrote in its record, as this device reads it: what it says, what a person may
/// choose on it, and the device the app addressed it to (empty for every device of the person's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raised {
    /// The id the notice document keeps in the app's record.
    pub doc: String,
    pub title: String,
    pub body: String,
    /// The actions it offers, each its key and the words of its label.
    pub actions: Vec<(String, String)>,
    /// The device it is for, empty for every device of the person's.
    pub device: String,
}

/// What this device carried out of an answer to one of its notices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carried {
    /// The app whose notice it was, and the notice document in its record.
    pub app: String,
    pub doc: String,
    /// The key of the action the person chose: empty when they dismissed it.
    pub action: String,
}

/// The record id this device shows `doc`, an app's notice, under: 32 bytes of the device's key,
/// the workspace and the app the notice is of, and the notice itself, hashed. One notice is one
/// record on every device that reads it, and one device never keeps the same notice twice.
#[must_use]
pub fn shared_id(device: &[u8; 32], workspace: &str, app: &str, doc: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-device-notice-v1\0");
    hasher.update(device);
    hasher.update(b"\0");
    hasher.update(workspace.as_bytes());
    hasher.update(b"\0");
    hasher.update(app.as_bytes());
    hasher.update(b"\0");
    hasher.update(doc.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// The notices `opened`, an app's record, holds for `person` that they have not seen: the
/// documents a device of theirs shows.
///
/// # Errors
/// The record's notices cannot be read.
pub fn raised(opened: &Opened, person: &str) -> Result<Vec<Raised>, String> {
    let runtime = opened.host().runtime();
    let mut out = Vec::new();
    for doc in opened
        .host()
        .store()
        .resources_of(NOTICE_TYPE)
        .map_err(|error| error.to_string())?
    {
        let at = ResourceRef::root(doc.clone(), NOTICE_TYPE.to_owned());
        let to = match runtime.read(&at, NOTICE_TO) {
            Ok(Value::Actor(actor)) => actor
                .member_id()
                .and_then(|member| member.person_id())
                .map(str::to_owned),
            _ => None,
        };
        if to.as_deref() != Some(person) {
            continue;
        }
        if matches!(runtime.read(&at, NOTICE_SEEN), Ok(Value::Bool(true))) {
            continue;
        }
        let text = |field: &str| match runtime.read(&at, field) {
            Ok(Value::Text(text)) => text,
            _ => String::new(),
        };
        let names: Vec<String> = match runtime.read(&at, NOTICE_ACTIONS) {
            Ok(Value::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Text(name) => Some(name.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        out.push(Raised {
            actions: labelled(opened, &at, &names),
            device: text(NOTICE_DEVICE),
            title: text(NOTICE_TITLE),
            body: text(NOTICE_BODY),
            doc,
        });
    }
    Ok(out)
}

/// The actions `names` the notice `at` offers, each with the words the app that keeps what it
/// opens labels it by: one the app does not declare keeps the name it was offered by.
fn labelled(opened: &Opened, at: &ResourceRef, names: &[String]) -> Vec<(String, String)> {
    let package = match opened.host().runtime().read(at, NOTICE_OPENS) {
        Ok(Value::Ref(reference)) => tilolang_tilo::schema::lookup(&reference.target.ty)
            .and_then(|schema| opened.host().package(&schema.app)),
        _ => None,
    };
    names
        .iter()
        .map(|name| {
            let label = package
                .as_ref()
                .and_then(|package| {
                    package
                        .meta
                        .actions
                        .iter()
                        .find(|action| action.name == *name)
                })
                .map_or_else(String::new, |action| action.label.clone());
            let label = if label.is_empty() {
                name.clone()
            } else {
                label
            };
            (name.clone(), label)
        })
        .collect()
}

/// `person` saw the notice `doc` — they dismissed it, chose on it, or its time there ran out — so
/// it is marked seen in the app's record and no device of theirs raises it again.
///
/// # Errors
/// The record refused the write.
pub fn seen(opened: &Opened, person: &str, doc: &str) -> Result<(), String> {
    let at = ResourceRef::root(doc.to_owned(), NOTICE_TYPE.to_owned());
    let by = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
    opened
        .host()
        .store()
        .set_register(
            &at,
            NOTICE_SEEN,
            tilolang_tilo::runtime::store::Scalar::Bool(true),
            EDIT_FLAG_ACTION,
            &by,
        )
        .map_err(|error| error.to_string())
}

/// Run `action`, one of the app's actions by the name the notice `doc` offered it by, on what the
/// notice opens, as `person`.
///
/// # Errors
/// The notice opens nothing, the app does not declare the action, or the action was refused.
pub fn choose(opened: &Opened, person: &str, doc: &str, action: &str) -> Result<(), String> {
    let at = ResourceRef::root(doc.to_owned(), NOTICE_TYPE.to_owned());
    let opens = match opened.host().runtime().read(&at, NOTICE_OPENS) {
        Ok(Value::Ref(reference)) => ResourceRef::clone(&reference.target),
        other => return Err(format!("the notice opens nothing it can run: {other:?}")),
    };
    let schema = tilolang_tilo::schema::lookup(&opens.ty)
        .ok_or_else(|| format!("no app keeps `{}`", opens.ty))?;
    let package = opened
        .host()
        .package(&schema.app)
        .ok_or_else(|| format!("{} is not loaded", schema.app))?;
    let named = package
        .meta
        .actions
        .iter()
        .find(|declared| declared.name == action)
        .ok_or_else(|| format!("{} declares no action `{action}`", schema.app))?;
    let by = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
    opened
        .host()
        .runtime()
        .invoke_action(&by, (named.id.as_str(), vec![Value::resource(&opens)]))
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The notices this device's apps raise, kept in the record of the person's devices while they
/// are shown and taken out as the person answers them.
pub struct ServedNotices {
    workspaces: Workspaces,
    /// This device's key: what its notices are from, and who answers for them.
    device: [u8; 32],
    /// The person whose notices these are.
    person: String,
    /// The workspace holding the record of their devices.
    record: String,
    /// The id each notice this device has in the record is shown under, by the workspace, app and
    /// notice document it came from.
    out: BTreeMap<(String, String, String), String>,
    /// The package each app's record is read through here, by workspace and app: the one this
    /// device installed, or the one this build carries.
    packages: BTreeMap<(String, String), Package>,
}

impl ServedNotices {
    /// The notices of `person` their device `device` raises in the record kept in their own
    /// workspace.
    #[must_use]
    pub fn new(workspaces: Workspaces, device: [u8; 32], person: [u8; 32]) -> Self {
        Self {
            workspaces,
            device,
            person: hex::encode(person),
            record: tilo_server_protocol::servers::person_workspace_id(&person),
            out: BTreeMap::new(),
            packages: BTreeMap::new(),
        }
    }

    /// Show the notices `app` raised in `workspace` that the person has not seen, named as
    /// `app_name`, in the record of their devices at `now_ms`; take out of it the ones the app
    /// withdrew since the last look. Answers how many it showed. `package` is the app's package
    /// as this device has it, `None` for the one this build carries.
    ///
    /// # Errors
    /// The app's record cannot be read, or the person's record refused a write.
    pub async fn project(
        &mut self,
        workspace: &str,
        app: &str,
        app_name: &str,
        package: Option<&Package>,
        now_ms: i64,
    ) -> Result<usize, String> {
        let raised = self
            .workspaces
            .app_notices(workspace, app, package, &self.person)
            .await?;
        self.packages.insert(
            (workspace.to_owned(), app.to_owned()),
            package.cloned().unwrap_or(Package::BuiltIn),
        );
        let me = hex::encode(self.device);
        let read: Vec<&Raised> = raised
            .iter()
            .filter(|notice| notice.device.is_empty() || notice.device.as_str() == me.as_str())
            .collect();
        // What this device showed of the app and the app no longer raises leaves the record: the
        // app withdrew it, or its notice was seen.
        let gone: Vec<((String, String, String), String)> = self
            .out
            .iter()
            .filter(|((at, of, doc), _)| {
                at == workspace && of == app && !read.iter().any(|notice| &notice.doc == doc)
            })
            .map(|(key, id)| (key.clone(), id.clone()))
            .collect();
        for (key, id) in gone {
            self.retire(&id).await;
            self.out.remove(&key);
        }
        let mut shown = 0;
        for notice in read {
            let key = (workspace.to_owned(), app.to_owned(), notice.doc.clone());
            if self.out.contains_key(&key) {
                continue;
            }
            let id = shared_id(&self.device, workspace, app, &notice.doc);
            let shared = SharedNotice {
                from: me.clone(),
                app_id: app.to_owned(),
                app_name: app_name.to_owned(),
                title: notice.title.clone(),
                body: notice.body.clone(),
                actions: notice
                    .actions
                    .iter()
                    .map(|(key, label)| NoticeAction {
                        key: key.clone(),
                        label: label.clone(),
                    })
                    .collect(),
                shown_ms: now_ms,
                until_ms: now_ms.saturating_add(MAX_NOTICE_SHOWN_MS),
                ..SharedNotice::default()
            };
            person_workspace::apply_on(
                &self.workspaces,
                &self.record,
                DEVICES,
                Devices::showing_notice(&id, &shared),
            )
            .await?;
            self.out.insert(key, id);
            shown += 1;
        }
        Ok(shown)
    }

    /// Carry out what the person answered in the record of their devices at `now_ms` of the
    /// notices this device sent: a dismissal marks the app's notice seen, a chosen action runs,
    /// and either takes the notice off every device of theirs. A notice whose time ran out, or
    /// which left the record, is taken off the app's record too. Answers what it carried out.
    ///
    /// # Errors
    /// The record of their devices cannot be read.
    pub async fn follow(&mut self, now_ms: i64) -> Result<Vec<Carried>, String> {
        let record =
            person_workspace::read_on::<Devices>(&self.workspaces, &self.record, DEVICES).await?;
        let me = hex::encode(self.device);
        let held: Vec<(String, SharedNotice)> = record
            .notices
            .iter()
            .filter(|(_, notice)| notice.from == me)
            .map(|(id, notice)| (id.as_str().to_owned(), notice.clone()))
            .collect();
        let mine: Vec<(String, String, String, String)> = self
            .out
            .iter()
            .map(|((workspace, app, doc), id)| {
                (workspace.clone(), app.clone(), doc.clone(), id.clone())
            })
            .collect();
        let mut carried = Vec::new();
        // What this device kept in the record before it restarted, which no app's record raises
        // for it any more, is stale: it leaves, and a notice the app still holds is raised again.
        for (id, _) in &held {
            if !self.out.values().any(|shown| shown == id) {
                self.retire(id).await;
            }
        }
        let mut finished: Vec<(String, String, String)> = Vec::new();
        for (workspace, app, doc, id) in mine {
            let Some((_, notice)) = held.iter().find(|(at, _)| at == &id) else {
                // The notice left the record without this device saying so (its time there ran
                // out on another device, or a session showing it took it away): it is done with.
                self.finish(&workspace, &app, &doc, &id).await;
                finished.push((workspace, app, doc));
                continue;
            };
            let answered = !notice.answered_on.is_empty();
            if !answered && notice.until_ms > now_ms {
                continue;
            }
            if answered && !notice.chosen.is_empty() {
                match self.run(&workspace, &app, &doc, &notice.chosen).await {
                    Ok(()) => carried.push(Carried {
                        app: app.clone(),
                        doc: doc.clone(),
                        action: notice.chosen.clone(),
                    }),
                    Err(error) => {
                        tracing::warn!(%error, %id, "a notice's action did not run");
                    }
                }
            }
            self.finish(&workspace, &app, &doc, &id).await;
            finished.push((workspace, app, doc));
        }
        for key in finished {
            self.out.remove(&key);
        }
        Ok(carried)
    }

    /// Mark the app's notice `doc` seen, and take it off the person's devices as `id`.
    async fn finish(&self, workspace: &str, app: &str, doc: &str, id: &str) {
        if let Err(error) = self
            .workspaces
            .notice_seen(
                workspace,
                app,
                self.packages.get(&(workspace.to_owned(), app.to_owned())),
                &self.person,
                doc,
            )
            .await
        {
            tracing::warn!(%error, %id, "a notice was not marked seen");
        }
        self.retire(id).await;
    }

    /// Run the action `action` the person chose on the notice `doc`, as them.
    async fn run(&self, workspace: &str, app: &str, doc: &str, action: &str) -> Result<(), String> {
        self.workspaces
            .notice_action(
                workspace,
                app,
                self.packages.get(&(workspace.to_owned(), app.to_owned())),
                &self.person,
                doc,
                action,
            )
            .await
    }

    /// Take the notice `id` off the person's devices.
    async fn retire(&self, id: &str) {
        if let Err(error) = person_workspace::apply_on(
            &self.workspaces,
            &self.record,
            DEVICES,
            Devices::retiring_notice(id),
        )
        .await
        {
            tracing::warn!(%error, %id, "a notice was not taken off the person's devices");
        }
    }
}

//! The notices an app raises on a device with no desktop session: what the app's record says,
//! what the person answered in the record of their devices, and how that answer reaches the app.
//! What the record itself holds is read through the language ([`crate::tl_records::Package`]).

use std::collections::BTreeMap;

use crate::tl_records::{Package, Raised};
use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace::{
    self,
    devices::{DEVICES, Devices, MAX_NOTICE_SHOWN_MS, NoticeAction, SharedNotice},
};

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
    /// The notices this device rang on its own when their moment came
    /// ([`crate::apps::wakeups::Wakeups::look`]), by workspace, app and notice: the person was
    /// told, and is not told again when the notice arrives.
    rang: std::collections::BTreeSet<crate::apps::wakeups::Rang>,
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
            rang: std::collections::BTreeSet::new(),
        }
    }

    /// This device rang `rang` on its own when its moment came: the notice it is shows nowhere
    /// from here when it arrives.
    pub fn rang_here(&mut self, rang: impl IntoIterator<Item = crate::apps::wakeups::Rang>) {
        self.rang.extend(rang);
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
            .filter(|notice| {
                !self
                    .rang
                    .contains(&(workspace.to_owned(), app.to_owned(), notice.doc.clone()))
            })
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
        let mut shown: usize = 0;
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
            shown = shown.saturating_add(1);
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

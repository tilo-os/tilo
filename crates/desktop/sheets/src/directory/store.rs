//! The directories as last read, the joins asked for, and the one app task
//! that carries both out.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::directory::Status;
use crate::invite as sheet;
use tilo_net::Client;
use tilo_startup_metrics::JOIN_FLOW_TARGET;
use tokio::sync::mpsc;

use super::said;
use tilo_workspace_directory::{
    self as client, Granted, Listed, Person, SiteError, home_listing, listings_of,
};

/// Called whenever something landed, from the task that read or joined: what
/// wakes the caller's loop to take it in.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A workspace this person chose to join: the name it was listed under, and
/// the workspace itself with the way this device comes into it — an
/// invitation it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// The name the workspace was listed under, or the address a link named
    /// it by: empty when only the workspace's own governance names it, which
    /// the session reads once its host admits this device.
    pub name: String,
    /// The workspace joined, and how this device comes into it.
    pub granted: Granted,
}

/// What the task is asked.
enum Ask {
    /// Read the community site and, when named, this one too.
    Read {
        community: Option<url::Url>,
        site: Option<String>,
    },
    /// Join what the person pasted: an invitation's text, or a link that
    /// carries one or names the one its workspace keeps.
    Take { text: String, person: Person },
}

/// What the task answers.
enum Answer {
    /// What the sites list, and what could not be read.
    Read(Snapshot),
    /// Where joining what the person pasted stands, named `name` when it got
    /// far enough to know it.
    Taken {
        key: String,
        name: String,
        granted: Result<Granted, String>,
    },
}

/// What a site answered when its directory was read.
struct Snapshot {
    listings: Vec<Listed>,
    failures: Vec<String>,
}

/// The directories as last read, the joins asked for, and the task that
/// carries both out.
#[derive(Default)]
pub struct Store {
    pub(crate) status: std::cell::RefCell<Status>,
    pub(crate) listed: std::cell::RefCell<Vec<Listed>>,
    pub(crate) note: std::cell::RefCell<Option<String>>,
    /// The join failures not yet said where the person is: the sheet leaves
    /// when a join starts, so what became of it is said off the sheet too.
    say: std::cell::RefCell<Vec<String>>,
    /// One join at a time.
    pub(crate) joining: std::cell::RefCell<Option<String>>,
    /// The join whose enrollment into its workspace is on its way, by the
    /// workspace's key, while it is.
    settling: std::cell::RefCell<Option<String>>,
    /// Why joining a listing failed, by its key, until it is joined.
    pub(crate) failures: std::cell::RefCell<BTreeMap<String, String>>,
    /// The workspaces joined since the caller last took them, for the shell
    /// to add.
    joined: std::cell::RefCell<Vec<Joined>>,
    /// The name a site listed each workspace under, by its id: a join whose
    /// slot is made only once the host admits it keeps the name it was
    /// listed by ([`Store::listed_name`]).
    names: std::cell::RefCell<BTreeMap<String, String>>,
    asks: std::cell::RefCell<Option<mpsc::UnboundedSender<Ask>>>,
    answers: std::cell::RefCell<Option<mpsc::UnboundedReceiver<Answer>>>,
    /// What wakes the caller's loop as the task answers, so a directory being
    /// read or a join shown is shown as it lands.
    wake: std::cell::RefCell<Option<Wake>>,
}

impl Store {
    /// Wake the caller's loop `wake` as the task answers.
    pub fn waking(&self, wake: Wake) {
        *self.wake.borrow_mut() = Some(wake);
    }

    /// The task, started the first time anything is asked of it: one app task
    /// reads the sites and asks their entries, in turn.
    fn worker(&self) -> Option<mpsc::UnboundedSender<Ask>> {
        let asking = self.asks.borrow().clone();
        if let Some(asks) = asking {
            return Some(asks);
        }
        let (asks, waiting) = mpsc::unbounded_channel();
        let (answers, read) = mpsc::unbounded_channel();
        let wake = self.wake.borrow().clone();
        let runtime = match tilo_rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, "the workspace directory is not read");
                return None;
            }
        };
        runtime.spawn(work(waiting, answers, wake));
        *self.answers.borrow_mut() = Some(read);
        *self.asks.borrow_mut() = Some(asks.clone());
        Some(asks)
    }

    /// Read the directories again: `community`, and `site` when the person
    /// named one.
    pub fn read(&self, community: Option<url::Url>, site: Option<String>) {
        *self.status.borrow_mut() = Status::Loading;
        *self.note.borrow_mut() = None;
        let Some(asks) = self.worker() else {
            return;
        };
        if let Err(error) = asks.send(Ask::Read { community, site }) {
            tracing::warn!(%error, "the workspace directory task is gone");
        }
    }

    /// The listing `key` names, while it is listed.
    pub fn listing(&self, key: &str) -> Option<Listed> {
        self.listed
            .borrow()
            .iter()
            .find(|listing| listing.key() == key)
            .cloned()
    }

    /// Join `listing`, which the sheet knows as `key`, through the open entry
    /// its directory publishes, as a pasted link to it does: this device
    /// signs its own admission there.
    ///
    /// # Errors
    /// The listing publishes no entry: its workspace lets people in by
    /// invitation only.
    pub fn join(&self, key: &str, listing: &Listed) -> Result<(), String> {
        let name = listing.entry.name.trim();
        let workspace = &listing.entry.workspace;
        let Some(host) = listing.entry_host.clone().filter(|_| !workspace.is_empty()) else {
            return Err(format!(
                "{name} takes people in by invitation: paste the invite link someone in it sends you"
            ));
        };
        self.granted(
            key,
            name,
            Granted {
                workspace: workspace.clone(),
                way: client::Way::Entry(Box::new(host)),
            },
        );
        Ok(())
    }

    /// Join what the person pasted into the sheet — an invitation's text or a
    /// short link — off the caller's loop.
    ///
    /// # Errors
    /// Why nothing was asked: nothing was pasted.
    pub fn take(&self, text: &str, person: &Person) -> Result<(), String> {
        let text: String = text.split_whitespace().collect();
        if text.is_empty() {
            return Err("Paste a link or a code first.".to_owned());
        }
        let Some(asks) = self.worker() else {
            return Err("the directory has nowhere to ask".to_owned());
        };
        *self.joining.borrow_mut() = Some(text.clone());
        self.failures.borrow_mut().remove(&text);
        *self.note.borrow_mut() = None;
        if let Err(error) = asks.send(Ask::Take {
            text,
            person: person.clone(),
        }) {
            *self.joining.borrow_mut() = None;
            return Err(format!("that was not asked of its host: {error}"));
        }
        Ok(())
    }

    /// The join whose enrollment is on its way, by the workspace it is for, while
    /// it is: the sheet says so until it lands.
    pub fn expect_enrollment(&self, workspace: &str) {
        *self.settling.borrow_mut() = Some(workspace.to_owned());
        *self.note.borrow_mut() = Some(crate::word("directory-joining"));
    }

    /// Whether a join is still being settled.
    pub fn settling(&self) -> bool {
        self.settling.borrow().is_some()
    }

    /// What became of the enrollment into the workspace of `key`: the join is
    /// settled, either way.
    ///
    /// A failure is said where the person pressed, and the row offers to
    /// join again.
    pub fn enrolled(&self, key: &str, name: &str, result: &Result<(), String>) {
        if self.settling.borrow().as_deref() != Some(key) {
            return;
        }
        *self.settling.borrow_mut() = None;
        match result {
            Ok(()) => *self.note.borrow_mut() = None,
            Err(reason) => {
                self.failing(crate::words(
                    "directory-join-failed",
                    &[("name", name), ("error", reason)],
                ));
                self.failures
                    .borrow_mut()
                    .insert(key.to_owned(), reason.clone());
            }
        }
    }

    /// The listing this caller is joining, by its key, while it is.
    pub fn joining(&self) -> Option<String> {
        self.joining.borrow().clone()
    }

    /// Take the workspaces joined since this was last asked.
    pub fn take_joined(&self) -> Vec<Joined> {
        std::mem::take(&mut *self.joined.borrow_mut())
    }

    /// Hand over the invitation the workspace made: the link it answered
    /// with, else the native link that names it at its host.
    ///
    /// # Errors
    /// `text` is not an invitation's bytes.
    pub fn hand_over(
        &self,
        made: &mut sheet::Making,
        mut created: sheet::Created,
        text: &str,
    ) -> Result<(), String> {
        if created.link.is_empty() {
            let bytes = tilo_workspace_join::invitation_bytes(text)?;
            created.link = tilo_workspace_join::native_invitation_link(&bytes);
        }
        made.handed_over(created);
        Ok(())
    }

    /// Take what the task answered since the last call. Whether anything
    /// arrived.
    pub fn answered(&self) -> bool {
        let mut answered = false;
        let mut read = self.answers.borrow_mut();
        let Some(read) = read.as_mut() else {
            return false;
        };
        while let Ok(answer) = read.try_recv() {
            answered = true;
            self.took(answer);
        }
        answered
    }

    /// Take in one answer from the task: a directory read, or where a join
    /// stands.
    fn took(&self, answer: Answer) {
        match answer {
            Answer::Read(Snapshot { listings, failures }) => {
                if listings.is_empty() && !failures.is_empty() {
                    *self.status.borrow_mut() = Status::Failed(failures.join("; "));
                    return;
                }
                *self.listed.borrow_mut() = listings;
                *self.status.borrow_mut() = Status::Listed;
                // A site that could not be read is missing from the list:
                // say so, so a short list is never taken for everything
                // there is.
                if !self.settling() {
                    *self.note.borrow_mut() = (!failures.is_empty()).then(|| {
                        crate::words("directory-sites-unread", &[("error", &failures.join("; "))])
                    });
                }
            }
            Answer::Taken { key, name, granted } => {
                *self.joining.borrow_mut() = None;
                match granted {
                    Ok(granted) => self.granted(&key, &name, granted),
                    Err(reason) => {
                        tracing::info!(
                            target: JOIN_FLOW_TARGET,
                            name = %name,
                            %reason,
                            outcome = "failed",
                            "join: the pasted link or code joined no workspace"
                        );
                        self.failing(crate::words("directory-code-failed", &[("error", &reason)]));
                        self.failures.borrow_mut().insert(key, reason);
                    }
                }
            }
        }
    }

    /// A join the task settled the way in for: keep the name it is listed by
    /// for the slot made once the host admits this device, and hand it to the
    /// caller.
    fn granted(&self, key: &str, name: &str, granted: Granted) {
        self.failures.borrow_mut().remove(key);
        self.names
            .borrow_mut()
            .insert(granted.workspace.clone(), name.to_owned());
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            workspace = %granted.workspace,
            name = %name,
            outcome = "granted",
            "join: the workspace is joined and its slot is next"
        );
        self.joined.borrow_mut().push(Joined {
            name: name.to_owned(),
            granted,
        });
    }

    /// The name the site listed `workspace` under, once a join was granted:
    /// a slot the shell makes when the host admits this device shows the
    /// workspace by the name it was chosen under, not its id. `None` when the
    /// site knew none: an invitation's text names only the workspace, and the
    /// workspace's own name is the one the session reads from its governance.
    pub fn listed_name(&self, workspace: &str) -> Option<String> {
        self.names
            .borrow()
            .get(workspace)
            .filter(|name| !name.trim().is_empty())
            .cloned()
    }

    /// What became of the last read or join, when it went wrong.
    pub fn set_note(&self, note: Option<String>) {
        *self.note.borrow_mut() = note;
    }

    /// Say a join that failed: the sheet says it, and so does the caller
    /// where the person is now, since a join the person chose took the sheet
    /// with it when it started.
    pub fn failing(&self, said: String) {
        *self.note.borrow_mut() = Some(said.clone());
        self.say.borrow_mut().push(said);
    }

    /// Take what became of the joins that failed since this was last asked,
    /// each as the sheet says it: the person is told wherever they are.
    pub fn take_failures_to_say(&self) -> Vec<String> {
        std::mem::take(&mut *self.say.borrow_mut())
    }
}

/// The one app task: read what is asked, join where it is asked, and answer
/// with what the sites said.
async fn work(
    mut asks: mpsc::UnboundedReceiver<Ask>,
    answers: mpsc::UnboundedSender<Answer>,
    wake: Option<Wake>,
) {
    let client = client::client();
    while let Some(ask) = asks.recv().await {
        let answer = match ask {
            Ask::Read { community, site } => Answer::Read(read(&client, community, site).await),
            Ask::Take { text, person } => {
                let taken = client::take(&client, &text, &person).await;
                Answer::Taken {
                    key: text,
                    name: taken.name,
                    granted: taken.granted.map_err(|error| said(&error)),
                }
            }
        };
        if answers.send(answer).is_err() {
            return;
        }
        if let Some(wake) = &wake {
            wake();
        }
    }
}

/// What the community site and `site` list, in the order the sheet shows
/// them.
async fn read(
    client: &Result<Client, SiteError>,
    community: Option<url::Url>,
    site: Option<String>,
) -> Snapshot {
    let mut sites = Vec::new();
    if let Some(site) = site
        .map(|site| site.trim().to_owned())
        .filter(|site| !site.is_empty())
    {
        match client::site_entry(&site) {
            Ok(site) => sites.push(site),
            Err(error) => {
                return Snapshot {
                    listings: Vec::new(),
                    failures: vec![said(&error)],
                };
            }
        }
    }
    if let Some(community) = community {
        sites.push(community);
    }
    let client = match client {
        Ok(client) => client,
        Err(error) => {
            return Snapshot {
                listings: Vec::new(),
                failures: vec![format!(
                    "no network to read the directory over: {}",
                    said(error)
                )],
            };
        }
    };
    let mut listings = Vec::new();
    let mut failures = Vec::new();
    let mut directories = BTreeSet::new();
    for site in &sites {
        match client::read_site(client, site).await {
            Ok(read) => {
                let mut listed = Vec::new();
                if let Some(directory) = &read.directory
                    && directories.insert(directory.to_string())
                {
                    match client::fetch_directory(client, directory).await {
                        Ok(page) => listed = listings_of(page, directory, Some(site)),
                        Err(error) => failures.push(said(&error)),
                    }
                }
                if let Some(home) = &read.home
                    && !listed.iter().any(|listing| listing.entry.community)
                {
                    listed.insert(0, home_listing(site, home));
                }
                listings.extend(listed);
            }
            Err(error) => failures.push(said(&error)),
        }
    }
    tracing::info!(
        target: JOIN_FLOW_TARGET,
        sites = %sites
            .iter()
            .map(url::Url::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        listed = listings.len(),
        unread = failures.len(),
        failures = %failures.join("; "),
        "directory: the sites were read"
    );
    Snapshot { listings, failures }
}

/// A store with no task behind it still says what a join the caller settles
/// became: those words are the sheet's, whether they come from an invitation
/// or from the caller's own enrollment.
#[cfg(test)]
mod tests {
    #[test]
    fn a_join_on_its_way_is_not_a_failure_to_say() {
        let store = super::Store::default();
        store.expect_enrollment("1d198715");
        assert!(
            store.take_failures_to_say().is_empty(),
            "the sheet says the join is on its way; the person is not told it failed"
        );
        store.enrolled(
            "1d198715",
            "Lounge",
            &Err("the workspace's host refused: invitation was cancelled".into()),
        );
        assert_eq!(
            store.take_failures_to_say(),
            [
                "Couldn't join Lounge: the workspace's host refused: invitation was cancelled"
                    .to_owned()
            ],
            "the refusal is said where the person is, in the host's own words"
        );
        assert!(
            store.take_failures_to_say().is_empty(),
            "and it is said once"
        );
    }
}

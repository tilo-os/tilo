//! What a workspace's host finds in what the workspace holds, for a device that holds only part
//! of it.
//!
//! A page holds the records of the apps it opened, and no others: a note in a notebook nobody
//! opened on the page, or an issue in a tracker it never showed, is not in its store, so its
//! records source cannot find it. The workspace's host holds every record and ranks the entries
//! each record's app publishes to search ([`Answers`]). This source asks it, over the
//! session the device already holds with the host, and shows what it answers.
//!
//! The answer comes over the wire, so a query asks and returns at once; the answer is kept for
//! that query, wakes the shell, and the next pass lists it ([`Source::poll`]). A host that has not
//! answered within [`PATIENCE`] is no longer waited for: the search ends with the rows the other
//! sources brought, and this source says nothing.

use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::Caller;
use tilo_workspace_records::search::{Answers, Found};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};
use ui::time::{Duration, Instant};

use crate::model::{Hit, InvokeTarget, Source, Target};

/// How long a search waits on the host's answer before it no longer counts it as coming.
pub const PATIENCE: Duration = Duration::from_secs(2);

/// What the source last asked, and the host's answer to it.
#[derive(Default)]
struct Asked {
    /// The query asked, as the source was asked it.
    needle: String,
    /// When it was asked.
    at: Option<Instant>,
    /// The host's answer, once it came: empty when the host refused.
    found: Option<Vec<Found>>,
    /// Whether the answer came since the palette last looked.
    fresh: bool,
}

/// The workspace host's own search, as one of a device's search sources.
pub struct HostSearch {
    search: Arc<dyn Answers>,
    caller: Caller,
    workspace: String,
    /// What asks the device's shell for the pass that lists an answer.
    wake: Arc<dyn Fn() + Send + Sync>,
    asked: Arc<Mutex<Asked>>,
}

impl HostSearch {
    /// `search`, the host of `workspace`, asked as `caller`; `wake` asks for the pass that
    /// lists its answer.
    #[must_use]
    pub fn new(
        search: Arc<dyn Answers>,
        caller: Caller,
        workspace: impl Into<String>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            search,
            caller,
            workspace: workspace.into(),
            wake,
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> std::sync::MutexGuard<'_, Asked> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The row of `found`, the host's `rank`-th best: it runs the action the host says opens
    /// the resource, else the app's `open` at the resource, as a record's own entries open. A
    /// result of no app is no row: nothing here opens it.
    fn hit(&self, rank: usize, found: &Found) -> Option<Hit> {
        let app = found.app.clone()?;
        let target = match &found.open {
            Some(action) => InvokeTarget {
                arguments: found.arguments.clone(),
                ..InvokeTarget::action(&app, &self.workspace, &found.resource, action)
            },
            None => InvokeTarget::open(&app, &self.workspace, &found.resource, &found.resource),
        };
        let rank = i32::try_from(rank).unwrap_or(MAX_PROVIDER_SCORE);
        Some(Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: format!(
                "{app}\u{1f}{}\u{1f}{}",
                found.record.as_deref().unwrap_or_default(),
                found.resource
            ),
            title: found.label.clone(),
            subtitle: crate::availability::name_for(&app).map(str::to_owned),
            facts: Vec::new(),
            score: MAX_PROVIDER_SCORE.saturating_sub(rank).max(1),
            icon: None,
            target: Target::Invoke(Box::new(target)),
        })
    }
}

impl Source for HostSearch {
    fn id(&self) -> &'static str {
        // What the workspace's records hold, as the host ranks them: the same source to the
        // person as the records this device holds, switched off and on with them.
        "records"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-records")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut asked = self.asked();
        if asked.needle == needle {
            return asked
                .found
                .iter()
                .flatten()
                .enumerate()
                .filter_map(|(rank, found)| self.hit(rank, found))
                .collect();
        }
        *asked = Asked {
            needle: needle.to_owned(),
            at: Some(ui::time::now()),
            found: None,
            fresh: false,
        };
        drop(asked);
        let answer = Arc::clone(&self.asked);
        let wake = Arc::clone(&self.wake);
        let asking = needle.to_owned();
        self.search.find(&self.caller, needle).then(move |found| {
            let mut asked = answer.lock().unwrap_or_else(PoisonError::into_inner);
            // An answer to a query typed over since is no row of the one shown.
            if asked.needle != asking {
                return;
            }
            asked.found = Some(found.and_then(Result::ok).unwrap_or_default());
            asked.fresh = true;
            drop(asked);
            wake();
        });
        Vec::new()
    }

    fn poll(&self) -> bool {
        std::mem::take(&mut self.asked().fresh)
    }

    fn waiting(&self) -> bool {
        let asked = self.asked();
        asked.found.is_none()
            && asked
                .at
                .is_some_and(|at| ui::time::now().saturating_duration_since(at) < PATIENCE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{ActionCallError, Pending, pending};

    /// Where the host's answer to one query is completed.
    type Answer = tilo_actions::remote::Completer<Result<Vec<Found>, ActionCallError>>;

    /// A host whose answers the test hands over.
    #[derive(Default)]
    struct Host {
        asked: Mutex<Vec<(String, Answer)>>,
    }

    impl Answers for Host {
        fn find(&self, _: &Caller, needle: &str) -> Pending<Result<Vec<Found>, ActionCallError>> {
            let (done, answer) = pending();
            self.asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((needle.to_owned(), done));
            answer
        }
    }

    impl Host {
        fn answer(&self, needle: &str, found: Vec<Found>) {
            let done = {
                let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
                let at = asked
                    .iter()
                    .position(|(asked, _)| asked == needle)
                    .expect("the query was asked");
                asked.remove(at).1
            };
            done.complete(Ok(found));
        }
    }

    fn note(label: &str) -> Found {
        Found {
            resource: format!("notebook/pages/{label}"),
            kind: "notes.page".into(),
            label: label.into(),
            open: Some("open".into()),
            arguments: serde_json::Map::new(),
            app: Some("so.tilo.notes".into()),
            record: Some("1".into()),
            when: None,
            ..Found::default()
        }
    }

    #[test]
    fn the_hosts_answer_is_listed_once_it_comes_for_the_query_shown() {
        let host = Arc::new(Host::default());
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wakes = Arc::clone(&woken);
        let source = HostSearch::new(
            Arc::clone(&host) as Arc<dyn Answers>,
            Caller::local_person("ana"),
            "w",
            Arc::new(move || {
                wakes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        assert!(source.query("net 30").is_empty(), "asked, not answered yet");
        assert!(source.waiting(), "its answer is coming");
        assert!(!source.poll(), "nothing came yet");

        host.answer("net 30", vec![note("Supplier contract")]);
        assert_eq!(
            woken.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the answer wakes the shell"
        );
        assert!(source.poll(), "the answer came");
        assert!(!source.waiting());
        let rows = source.query("net 30");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Supplier contract");
        assert!(
            matches!(&rows[0].target, Target::Invoke(invoke)
                if invoke.app == "so.tilo.notes"
                    && invoke.address.resource == "notebook/pages/Supplier contract"),
            "the row opens the note: {:?}",
            rows[0].target
        );

        // A result that names no action opens through its app's `open`, at the resource, as the
        // host answers what it ranks.
        assert!(source.query("acme").is_empty());
        let mut issue = note("Renew the Acme contract");
        issue.open = None;
        issue.app = Some("so.tilo.tasks".into());
        host.answer("acme", vec![issue]);
        assert!(source.poll());
        let rows = source.query("acme");
        assert!(
            matches!(&rows[..], [row] if matches!(&row.target, Target::Invoke(invoke)
                if invoke.app == "so.tilo.tasks"
                    && invoke.address.action == crate::model::OPEN_ACTION
                    && invoke.arguments.get(crate::model::OPEN_ANCHOR)
                        == Some(&serde_json::Value::from("notebook/pages/Renew the Acme contract")))),
            "the issue opens in Tasks: {rows:?}"
        );

        // A query typed over before its answer came: the late answer lists nothing for it.
        assert!(source.query("net").is_empty());
        assert!(source.query("net 3").is_empty());
        host.answer("net", vec![note("Old")]);
        assert!(
            !source.poll(),
            "an answer to an earlier query changes nothing"
        );
        assert!(source.query("net 3").is_empty());
        assert!(source.waiting(), "the query shown still waits");
    }
}

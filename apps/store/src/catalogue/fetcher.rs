//! The Store's network reads that a person waits on, off the UI thread and
//! off the serialized package worker: an app page's details, and a search of
//! Flathub for an app the kept catalogue does not have yet.
//!
//! Details are kept on disk ([`super::details`]) and the page shows the kept
//! ones at once; the fetch only updates them. Searches take the latest query
//! only: a query typed over is never fetched.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use ui::WakeHandle;
use ui::lock::MutexRecover as _;

use super::details::AppDetails;
use crate::model::StoreItem;

/// What the fetcher answers.
#[derive(Clone, Debug)]
pub enum Fetched {
    /// An app's details, fresh from its source.
    Details(Box<AppDetails>),
    /// The details of `id` could not be fetched; the page keeps what it has.
    DetailsFailed { id: String, error: String },
    /// Flathub's apps for `query`, for the search's own section after the
    /// apps found on this device.
    Search {
        query: String,
        items: Vec<StoreItem>,
    },
}

enum Job {
    Details(String),
    Search,
}

/// The fetch thread's handle.
pub struct Fetcher {
    jobs: mpsc::Sender<Job>,
    answers: mpsc::Receiver<Fetched>,
    /// The query the next search job reads: typing over a query replaces it
    /// before the thread gets to it.
    latest_query: Arc<Mutex<Option<String>>>,
}

impl Fetcher {
    /// Start the thread; `wake` is woken whenever an answer is ready. Its
    /// localized reads are in `locale`.
    ///
    /// # Errors
    ///
    /// Returns why the thread could not start.
    pub fn spawn(wake: WakeHandle, locale: String) -> Result<Self, String> {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (answer_tx, answers) = mpsc::channel();
        let latest_query = Arc::new(Mutex::new(None::<String>));
        let latest = Arc::clone(&latest_query);
        std::thread::Builder::new()
            .name("store-fetcher".into())
            .spawn(move || {
                for job in job_rx {
                    let answer = match job {
                        Job::Details(id) => details(&id, &locale),
                        Job::Search => {
                            let Some(query) = latest.lock_recover().take() else {
                                continue;
                            };
                            search(query, &locale)
                        }
                    };
                    if answer_tx.send(answer).is_err() {
                        break;
                    }
                    wake.wake();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            jobs,
            answers,
            latest_query,
        })
    }

    /// Fetch the details of the Flathub app `id`.
    pub fn details(&self, id: &str) {
        let _ = self.jobs.send(Job::Details(id.to_owned()));
    }

    /// Search Flathub for `query`, replacing a search not yet started.
    pub fn search(&self, query: &str) {
        let replaced = self
            .latest_query
            .lock_recover()
            .replace(query.to_owned())
            .is_some();
        if !replaced {
            let _ = self.jobs.send(Job::Search);
        }
    }

    /// The next answer, if one is ready.
    #[must_use]
    pub fn try_recv(&self) -> Option<Fetched> {
        self.answers.try_recv().ok()
    }
}

fn details(id: &str, locale: &str) -> Fetched {
    let appstream = crate::flathub_client::fetch_appstream_document(id, locale);
    let summary = crate::flathub_client::fetch_summary(id);
    if let (Err(error), Err(_)) = (&appstream, &summary) {
        return Fetched::DetailsFailed {
            id: id.to_owned(),
            error: error.to_string(),
        };
    }
    let details = AppDetails::from_flathub(
        id,
        locale,
        appstream.as_ref().ok(),
        summary.as_ref().ok(),
        super::now_unix(),
    );
    if let Err(error) = super::details::save(&details) {
        tracing::warn!(%error, app = id, "could not keep app details");
    }
    Fetched::Details(Box::new(details))
}

fn search(query: String, locale: &str) -> Fetched {
    let items = match crate::flathub_client::search_localized(&query, locale) {
        Ok(records) => records
            .iter()
            .map(|record| crate::sources::flathub_record_to_item(record, &[]))
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "could not search Flathub");
            Vec::new()
        }
    };
    Fetched::Search { query, items }
}

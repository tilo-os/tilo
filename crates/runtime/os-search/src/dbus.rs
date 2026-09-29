//! GNOME Shell and KRunner search over the session bus.
//!
//! Both ask this runtime to match what a person types and hand back only an
//! entry id to open; neither ever sees more than the catalogue offered the
//! owner. One bus name serves both, each at its own object path, so one
//! D-Bus activation file starts the runtime for either.

use std::collections::HashMap;
use std::sync::Arc;

use zbus::interface;
use zbus::zvariant::Value;

use crate::{OsSearch, SearchEntry};

/// The runtime's name on the session bus.
pub const BUS_NAME: &str = "so.tilo.Runtime";
/// Where GNOME Shell finds `org.gnome.Shell.SearchProvider2`.
pub const GNOME_PATH: &str = "/so/tilo/Runtime/SearchProvider";
/// Where KRunner finds `org.kde.krunner1`.
pub const KRUNNER_PATH: &str = "/so/tilo/Runtime/KRunner";

/// Results one query answers at most; a host shows a handful.
const RESULTS: usize = 20;
/// KRunner's category relevance for a match that may be what was meant.
const POSSIBLE_MATCH: i32 = 30;

/// A result's properties, as both hosts read them (`a{sv}`).
type Properties = HashMap<&'static str, Value<'static>>;

/// Serve both providers over `builder`'s connection.
///
/// # Errors
///
/// Returns zbus errors registering either object.
pub fn serve(
    builder: zbus::connection::Builder<'_>,
    search: &Arc<OsSearch>,
) -> zbus::Result<zbus::connection::Builder<'_>> {
    builder
        .serve_at(
            GNOME_PATH,
            GnomeShell {
                search: Arc::clone(search),
            },
        )?
        .serve_at(
            KRUNNER_PATH,
            KRunner {
                search: Arc::clone(search),
            },
        )
}

/// Serve both providers on the session bus under [`BUS_NAME`].
///
/// # Errors
///
/// Returns zbus errors: no session bus, or the name already owned.
pub async fn serve_on_session_bus(search: &Arc<OsSearch>) -> zbus::Result<zbus::Connection> {
    serve(zbus::connection::Builder::session()?, search)?
        .name(BUS_NAME)?
        .build()
        .await
}

fn open_quietly(search: &OsSearch, id: &str) -> impl Future<Output = ()> + Send + '_ {
    let id = id.to_owned();
    async move {
        if let Err(error) = search.open(&id).await {
            tracing::info!(?error, "a host search result did not open");
        }
    }
}

fn ids(entries: Vec<SearchEntry>) -> Vec<String> {
    entries
        .into_iter()
        .map(|entry| entry.id.to_string())
        .collect()
}

/// `org.gnome.Shell.SearchProvider2`.
struct GnomeShell {
    search: Arc<OsSearch>,
}

#[interface(name = "org.gnome.Shell.SearchProvider2")]
impl GnomeShell {
    fn get_initial_result_set(&self, terms: Vec<String>) -> Vec<String> {
        ids(self.search.search(&terms.join(" "), RESULTS))
    }

    /// Searched afresh: narrowing `previous_results` would keep a result
    /// the owner lost access to since.
    fn get_subsearch_result_set(
        &self,
        _previous_results: Vec<String>,
        terms: Vec<String>,
    ) -> Vec<String> {
        self.get_initial_result_set(terms)
    }

    fn get_result_metas(&self, identifiers: Vec<String>) -> Vec<Properties> {
        identifiers
            .iter()
            .filter_map(|id| self.search.entry(id))
            .map(|entry| {
                let mut meta = Properties::new();
                meta.insert("id", Value::from(entry.id.to_string()));
                meta.insert("name", Value::from(entry.title));
                if let Some(detail) = entry.detail {
                    meta.insert("description", Value::from(detail));
                }
                meta
            })
            .collect()
    }

    async fn activate_result(&self, identifier: String, _terms: Vec<String>, _timestamp: u32) {
        open_quietly(&self.search, &identifier).await;
    }

    /// There is no Tilo search window to hand the terms to outside Tilo;
    /// every result already opens where it lives.
    fn launch_search(&self, _terms: Vec<String>, _timestamp: u32) {}
}

/// `org.kde.krunner1`.
struct KRunner {
    search: Arc<OsSearch>,
}

#[interface(name = "org.kde.krunner1")]
impl KRunner {
    /// Secondary actions on a match: opening is the only one.
    fn actions(&self) -> Vec<(String, String, String)> {
        Vec::new()
    }

    #[zbus(name = "Match")]
    fn find(&self, query: String) -> Vec<(String, String, String, i32, f64, Properties)> {
        let found = self.search.search(&query, RESULTS);
        let count = found.len().max(1) as f64;
        found
            .into_iter()
            .enumerate()
            .map(|(at, entry)| {
                let mut properties = Properties::new();
                if let Some(detail) = entry.detail {
                    properties.insert("subtext", Value::from(detail));
                }
                // Best first, as ranked; KRunner orders by relevance.
                let relevance = 1.0 - at as f64 / count;
                (
                    entry.id.to_string(),
                    entry.title,
                    String::new(),
                    POSSIBLE_MATCH,
                    relevance,
                    properties,
                )
            })
            .collect()
    }

    async fn run(&self, match_id: String, _action_id: String) {
        open_quietly(&self.search, &match_id).await;
    }
}

#[cfg(test)]
#[path = "dbus_tests.rs"]
mod tests;

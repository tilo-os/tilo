//! Independent, bounded catalogue reads: this device's Flatpak metadata and Flathub's feed on
//! threads of their own, and the workspace's Tilo repositories through this device's package
//! cache, which reads them for the Store (its `catalogue` reading).

use crate::resource::CATALOG;
use std::collections::{HashMap, HashSet, VecDeque};
use ui::time::Instant;

use crate::device::{self, FlatpakStatus};

use super::{
    Done, Duration, LocaleContext, RegistryConfig, State, StoreItem, StoreSource,
    sync_authoritative_catalogue,
};
use crate::catalogue::flathub::FlathubFeed;
use crate::flathub_client::{Remote, installed_flatpaks};
use crate::sources::{
    installed_flatpak_to_item, installed_tilo_to_store_item, merge, tilo_to_store_item_on_channel,
};

const MAX_ACTIVE_READS: usize = 4;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(90);

/// Why a repository's catalogue gave no rows, as the package cache refused it, typed: a reworded
/// detail never changes what the Store says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The system's interface declaration cannot be read: the system needs an update before any
    /// repository can be judged.
    SystemUpdateNeeded,
    /// The repository's catalogue is authentic but expired.
    Expired,
    /// The repository serves a catalogue older than the one this device already accepted.
    BehindAccepted,
    /// The repository did not answer within the read's deadline.
    TimedOut,
}

/// Why a catalogue read gave no rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CatalogueError {
    /// Refused for a reason the Store explains.
    Refused {
        refusal: Refusal,
        /// For logs; never decides anything.
        detail: String,
    },
    /// Anything else: the action API, the package cache, the repository.
    Failed(String),
}

impl CatalogueError {
    #[must_use]
    pub(crate) const fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { refusal, .. } => Some(*refusal),
            Self::Failed(_) => None,
        }
    }
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { refusal, detail } => write!(f, "{refusal:?}: {detail}"),
            Self::Failed(error) => f.write_str(error),
        }
    }
}

impl From<String> for CatalogueError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl From<&str> for CatalogueError {
    fn from(error: &str) -> Self {
        Self::Failed(error.to_owned())
    }
}

/// What the package cache answered a `catalogue` reading: its entries, or why it gave none.
pub(crate) fn catalogue_of(
    answer: Result<serde_json::Value, String>,
) -> Result<serde_json::Value, CatalogueError> {
    let mut value = answer.map_err(CatalogueError::Failed)?;
    if let Some(reason) = value.get("refused").and_then(serde_json::Value::as_str) {
        let detail = value
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let refusal = match reason {
            "system-update-needed" => Refusal::SystemUpdateNeeded,
            "expired" => Refusal::Expired,
            "behind-accepted" => Refusal::BehindAccepted,
            other => {
                return Err(CatalogueError::Failed(format!(
                    "the package cache refused the catalogue ({other}): {detail}"
                )));
            }
        };
        return Err(CatalogueError::Refused { refusal, detail });
    }
    value
        .get_mut("entries")
        .map(serde_json::Value::take)
        .ok_or_else(|| CatalogueError::Failed("the package cache answered no catalogue".into()))
}

/// A read of this device's own data, run on a thread of its own so no source waits for another.
fn off_thread<T: Send + Sync + 'static>(
    name: &str,
    read: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> ui::Arriving<T> {
    let arriving = ui::Arriving::pending(ui::arriving::Source::Local);
    let writer = arriving.writer();
    let refused = writer.clone();
    let started = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || writer.finish(read()));
    if let Err(error) = started {
        refused.fail(format!("could not start reading: {error}"));
    }
    arriving
}

/// The answer of a read that ran off this thread, once it is there.
fn arrived<T: Clone>(read: &ui::Arriving<T>) -> Option<Result<T, String>> {
    match read.get() {
        ui::Arrival::Pending { .. } => None,
        ui::Arrival::Ready(value) => Some(Ok(value)),
        ui::Arrival::Failed(error) => Some(Err(error)),
    }
}

enum Source {
    /// The catalogue this device last verified from a repository, read without the network so
    /// the repository's apps are described at once.
    LastVerified(RegistryConfig),
    Native(RegistryConfig),
    Flatpak(Remote),
}

enum Read {
    Status(ui::Arriving<FlatpakStatus>),
    LastVerified(RegistryConfig, ui::InvocationCall),
    Native(RegistryConfig, ui::InvocationCall),
    Flatpak(Remote, ui::Arriving<String>),
    Flathub(ui::Arriving<FlathubFeed>),
}

enum Response {
    Status(Result<FlatpakStatus, String>),
    LastVerified(RegistryConfig, Result<serde_json::Value, CatalogueError>),
    Native(RegistryConfig, Result<serde_json::Value, CatalogueError>),
    Flatpak(Remote, Result<String, String>),
    Flathub(Result<FlathubFeed, String>),
}

impl Read {
    fn poll(&self) -> Option<Response> {
        match self {
            Self::Status(read) => arrived(read).map(Response::Status),
            Self::LastVerified(registry, call) => super::packages::answered(call)
                .map(|answer| Response::LastVerified(registry.clone(), catalogue_of(answer))),
            Self::Native(registry, call) => super::packages::answered(call)
                .map(|answer| Response::Native(registry.clone(), catalogue_of(answer))),
            Self::Flatpak(remote, read) => {
                arrived(read).map(|result| Response::Flatpak(remote.clone(), result))
            }
            Self::Flathub(read) => arrived(read).map(Response::Flathub),
        }
    }

    /// Whether the package cache answers it through the action API.
    const fn is_call(&self) -> bool {
        matches!(self, Self::LastVerified(..) | Self::Native(..))
    }

    fn timed_out(&self) -> Response {
        let error = CATALOG.t("store-source-timeout");
        match self {
            Self::Status(_) => Response::Status(Err(error)),
            Self::LastVerified(registry, _) => {
                Response::LastVerified(registry.clone(), Err(timed_out(error)))
            }
            Self::Native(registry, _) => Response::Native(registry.clone(), Err(timed_out(error))),
            Self::Flatpak(remote, _) => Response::Flatpak(remote.clone(), Err(error)),
            Self::Flathub(_) => Response::Flathub(Err(error)),
        }
    }
}

struct ActiveRead {
    read: Read,
    started: Instant,
}

pub(super) struct Refresh {
    active: Vec<ActiveRead>,
    queued: VecDeque<Source>,
    registries: Option<Vec<RegistryConfig>>,
    native: HashMap<String, Vec<crate::registry_client::VerifiedEntry>>,
    /// Repositories that answered this refresh; their catalogue replaces the
    /// last verified one and is never replaced by it.
    answered: HashSet<String>,
    expired: Vec<String>,
    flatpak: HashMap<String, Vec<StoreItem>>,
    feed: Option<FlathubFeed>,
    cached: Vec<StoreItem>,
    errors: Vec<(u8, String)>,
    deadline: Instant,
    retired: bool,
    locale: LocaleContext,
    pub(super) done: Option<Done>,
}

impl Refresh {
    pub(super) fn start(state: &mut State, done: Option<Done>) -> Self {
        Self::start_with_feed(state, done, None, None, true)
    }

    /// A refresh generation. `feed_read` is a Flathub read already under way
    /// (a restarted generation keeps it); `feed` one already answered.
    fn start_with_feed(
        state: &mut State,
        done: Option<Done>,
        feed_read: Option<ActiveRead>,
        feed: Option<FlathubFeed>,
        seeded: bool,
    ) -> Self {
        let mut active = Vec::new();
        if state.background_reads {
            active.push(ActiveRead {
                read: Read::Status(off_thread("store-flatpak-status", || Ok(device::status()))),
                started: ui::time::now(),
            });
        }
        let answered = feed.is_some();
        if let Some(read) = feed_read {
            active.push(read);
        } else if !answered && state.background_reads {
            // The kept catalogue answers at once while it is fresh; reading
            // Flathub's whole catalogue again runs in the background and
            // never blocks the other sources.
            let locale = crate::catalogue::desktop_locale();
            let force = state.force_refresh;
            active.push(ActiveRead {
                read: Read::Flathub(off_thread("store-flathub", move || {
                    crate::catalogue::flathub::load_or_fetch(&locale, force)
                })),
                started: ui::time::now(),
            });
        }
        let mut refresh = Self {
            active,
            queued: VecDeque::new(),
            registries: None,
            native: HashMap::new(),
            answered: HashSet::new(),
            expired: Vec::new(),
            flatpak: HashMap::new(),
            feed: feed.or_else(|| state.flathub.clone()),
            cached: state.base_items.clone(),
            errors: Vec::new(),
            deadline: ui::time::now() + REFRESH_TIMEOUT,
            retired: false,
            locale: LocaleContext::capture(),
            done,
        };
        // The workspace's registries are a reading: its list comes from the
        // desktop's action API, or from the list a test's worker was started
        // with. Every other refresh re-reads the workspace's list, so a
        // registry added or disabled reaches the Store on the next one.
        let repositories = if seeded && !state.registries.is_empty() {
            crate::config::RegistriesFile {
                registries: state.registries.clone(),
            }
        } else {
            crate::config::repositories().unwrap_or_default()
        };
        refresh.repositories(state, repositories);
        refresh
    }

    /// Drain the old bounded requests before starting a replacement generation.
    /// A cancelled response does not promise its source stopped serving it.
    pub(super) fn retire(&mut self) {
        self.retired = true;
        self.queued.clear();
    }

    pub(super) fn is_retired(&self) -> bool {
        self.retired
    }

    /// Replace read generation after a real mutation, retaining the original
    /// refresh invocation and its deadline. Old responses cannot merge later.
    pub(super) fn restart(self, state: &mut State) -> Self {
        let deadline = self.deadline;
        self.locale.apply();
        let feed_read = self
            .active
            .into_iter()
            .find(|active| matches!(active.read, Read::Flathub(_)));
        let feed = if feed_read.is_some() { None } else { self.feed };
        let mut refreshed = Self::start_with_feed(state, self.done, feed_read, feed, false);
        refreshed.deadline = deadline;
        refreshed
    }

    pub(super) fn poll(&mut self, state: &mut State) -> (bool, bool) {
        self.locale.apply();
        if self.active.iter().any(|active| active.read.is_call()) {
            state.packages.poll();
        }
        let mut responses = Vec::new();
        for index in (0..self.active.len()).rev() {
            let Some(active) = self.active.get(index) else {
                continue;
            };
            let response =
                if active.started.elapsed() >= READ_TIMEOUT || ui::time::now() >= self.deadline {
                    Some(active.read.timed_out())
                } else {
                    active.read.poll()
                };
            if let Some(response) = response {
                self.active.swap_remove(index);
                responses.push(response);
            }
        }
        if self.retired {
            // The public feed is independent of installed/remote authority.
            // Reuse its one bounded task, never respawn it on each mutation.
            for response in responses {
                if let Response::Flathub(Ok(feed)) = response {
                    state.flathub = Some(feed.clone());
                    self.feed = Some(feed);
                }
            }
            return (false, self.active.is_empty());
        }
        let mut changed = !responses.is_empty();
        if changed {
            self.cached.clone_from(&state.base_items);
        }
        for response in responses {
            self.apply(state, response);
        }
        if self.admit(state) && !changed {
            self.cached.clone_from(&state.base_items);
            changed = true;
        }
        if changed {
            self.rebuild(state);
        }
        (changed, self.active.is_empty() && self.queued.is_empty())
    }

    /// Start queued reads while slots are free; answers whether a source was settled without a
    /// read (this computer answers no package cache to read repositories through).
    fn admit(&mut self, state: &mut State) -> bool {
        let mut settled = false;
        while self.active.len() < MAX_ACTIVE_READS {
            let Some(source) = self.queued.pop_front() else {
                break;
            };
            if ui::time::now() >= self.deadline {
                self.errors.push((0, CATALOG.t("store-source-timeout")));
                continue;
            }
            let read = match source {
                Source::LastVerified(registry) => {
                    match state
                        .packages
                        .cache_call("catalogue", catalogue_arguments(&registry, false))
                    {
                        Ok(Some(call)) => Read::LastVerified(registry, call),
                        Ok(None) => {
                            // This device's package cache is not found yet: asked again on the
                            // next pass.
                            self.queued.push_front(Source::LastVerified(registry));
                            break;
                        }
                        Err(_) => {
                            let error = CATALOG.t("store-package-rows-unavailable");
                            self.apply(state, Response::LastVerified(registry, Err(error.into())));
                            settled = true;
                            continue;
                        }
                    }
                }
                Source::Native(registry) => {
                    match state
                        .packages
                        .cache_call("catalogue", catalogue_arguments(&registry, true))
                    {
                        Ok(Some(call)) => Read::Native(registry, call),
                        Ok(None) => {
                            self.queued.push_front(Source::Native(registry));
                            break;
                        }
                        Err(_) => {
                            let error = CATALOG.t("store-package-rows-unavailable");
                            self.apply(state, Response::Native(registry, Err(error.into())));
                            settled = true;
                            continue;
                        }
                    }
                }
                Source::Flatpak(remote) => {
                    let read = {
                        let remote = remote.clone();
                        off_thread("store-flatpak-catalogue", move || {
                            Ok(device::catalogue(&remote))
                        })
                    };
                    Read::Flatpak(remote, read)
                }
            };
            self.active.push(ActiveRead {
                read,
                started: ui::time::now(),
            });
        }
        settled
    }

    fn apply(&mut self, state: &mut State, response: Response) {
        match response {
            Response::Status(Ok(status)) => {
                state.flatpak_error = None;
                state.flatpak_available = status.available;
                state.flatpak_installed = installed_flatpaks(&status.installed);
                state.installed_origins =
                    crate::flathub_client::installed_origins(&status.installed);
                state.remotes =
                    crate::flathub_client::refreshed_remotes(&status.remotes, &state.remotes);
                // Keep ready configured Flatpak sources independent of a
                // queue of slow native registries, within the same four slots.
                for remote in state.remotes.iter().rev().filter(|remote| !remote.disabled) {
                    self.queued.push_front(Source::Flatpak(remote.clone()));
                }
            }
            Response::Status(Err(error)) => {
                state.flatpak_available = false;
                state.flatpak_error =
                    Some(format!("Could not read Flatpak on this device: {error}"));
            }
            Response::LastVerified(registry, result) => self.last_verified(registry, result),
            Response::Native(registry, result) => self.native_response(registry, result),
            Response::Flatpak(remote, result) => {
                if let Some(current) = state
                    .remotes
                    .iter_mut()
                    .find(|item| item.resource_id() == remote.resource_id())
                {
                    current.catalogue_error = result
                        .as_ref()
                        .err()
                        .map(|error| crate::flathub_client::remote_error(&remote, error));
                }
                if let Ok(rows) = result {
                    self.flatpak.insert(
                        remote.resource_id(),
                        crate::sources::remote_catalogue_items(
                            &rows,
                            &remote,
                            &state.flatpak_installed,
                        ),
                    );
                }
            }
            Response::Flathub(Ok(feed)) => {
                state.flathub = Some(feed.clone());
                self.feed = Some(feed);
            }
            Response::Flathub(Err(error)) => self
                .errors
                .push((1, format!("Could not load Flathub: {error}"))),
        }
    }

    fn repositories(&mut self, state: &mut State, file: crate::config::RegistriesFile) {
        state.registries.clone_from(&file.registries);
        // The stored catalogues are local reads: ask for them first, so a
        // slow repository is already described while it answers.
        let enabled = || file.registries.iter().filter(|registry| registry.enabled);
        self.queued
            .extend(enabled().cloned().map(Source::LastVerified));
        self.queued.extend(enabled().cloned().map(Source::Native));
        self.registries = Some(file.registries);
    }

    fn last_verified(
        &mut self,
        registry: RegistryConfig,
        result: Result<serde_json::Value, CatalogueError>,
    ) {
        if self.answered.contains(&registry.name) {
            return;
        }
        if let Ok(Ok(entries)) = result.map(serde_json::from_value) {
            self.native.insert(registry.name, entries);
        }
    }

    fn native_response(
        &mut self,
        registry: RegistryConfig,
        result: Result<serde_json::Value, CatalogueError>,
    ) {
        let entries = result.and_then(|entries| {
            serde_json::from_value(entries).map_err(|error| {
                CatalogueError::Failed(format!("Invalid verified catalogue: {error}"))
            })
        });
        match entries {
            Ok(entries) => {
                self.answered.insert(registry.name.clone());
                self.native.insert(registry.name, entries);
            }
            Err(error) => {
                // An expired catalogue is the repository's to renew, not a
                // connection to check: say which, and that installed apps run on.
                if error.refusal() == Some(Refusal::Expired) {
                    self.expired.push(registry.name.clone());
                }
                self.errors.push((0, native_error(&registry.name, &error)));
            }
        }
    }

    fn native_items(&self, state: &State) -> Vec<StoreItem> {
        let installed = state.packages.installed();
        let mut items = self
            .cached
            .iter()
            .filter(|item| {
                let StoreSource::Tilo { registry_name } = &item.source else {
                    return false;
                };
                self.registries.as_ref().is_none_or(|registries| {
                    registries
                        .iter()
                        .any(|registry| registry.enabled && registry.name == *registry_name)
                }) && !self.native.contains_key(registry_name)
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(registries) = &self.registries {
            for registry in registries {
                if let Some(entries) = self.native.get(&registry.name) {
                    items.extend(entries.iter().map(|entry| {
                        tilo_to_store_item_on_channel(
                            entry,
                            &registry.name,
                            installed,
                            registry.channel,
                        )
                    }));
                }
            }
        }
        for item in &mut items {
            item.installed = installed.state(&item.id, item.latest_version());
        }
        for app in installed.workspace.keys() {
            if !items.iter().any(|item| item.id == *app) {
                items.push(installed_tilo_to_store_item(app, installed));
            }
        }
        items
    }

    fn flatpak_items(&self, state: &State) -> Vec<StoreItem> {
        let mut items = self
            .feed
            .as_ref()
            .map_or_else(Vec::new, |feed| feed.items.clone());
        for remote in state.remotes.iter().filter(|remote| !remote.disabled) {
            if let Some(rows) = self.flatpak.get(&remote.resource_id()) {
                for item in rows {
                    if !items.iter().any(|existing| existing.id == item.id) {
                        items.push(item.clone());
                    }
                }
            }
        }
        for item in self.cached.iter().filter(|item| match item.source {
            StoreSource::Tilo { .. } => false,
            // Once Flathub's catalogue is read, it alone says which of its
            // apps exist; the kept rows only stand in until then.
            StoreSource::Flathub => self.feed.is_none(),
            StoreSource::FlatpakRemote { .. } => true,
        }) {
            if let StoreSource::FlatpakRemote {
                name, user_managed, ..
            } = &item.source
                && !state.remotes.iter().any(|remote| {
                    !remote.disabled && remote.name == *name && remote.user_managed == *user_managed
                })
            {
                continue;
            }
            if !items.iter().any(|existing| existing.id == item.id) {
                items.push(item.clone());
            }
        }
        for (id, version) in &state.flatpak_installed {
            if !items.iter().any(|item| item.id == *id) {
                items.push(installed_flatpak_to_item(id, version));
            }
        }
        if let Some(reference) = &state.opened_ref {
            // Opening a reference is an explicit source/branch choice. A
            // catalogue response must not silently install a different offer.
            items.retain(|item| item.id != reference.app_id);
            let version = state
                .flatpak_installed
                .iter()
                .find_map(|(id, version)| (id == &reference.app_id).then(|| version.clone()));
            items.insert(0, reference.clone().into_store_item(version));
        }
        for item in &mut items {
            let available = match &item.installed {
                crate::InstalledState::Installed { available, .. } => available.clone(),
                crate::InstalledState::Not => item
                    .versions
                    .first()
                    .map_or_else(String::new, |version| version.version.clone()),
            };
            item.installed = state
                .flatpak_installed
                .iter()
                .find(|(id, _)| *id == item.id)
                .map_or(crate::InstalledState::Not, |(_, version)| {
                    crate::InstalledState::Installed {
                        version: version.clone(),
                        available,
                        pinned: false,
                        user_managed: false,
                    }
                });
            crate::sources::apply_flatpak_origin(item, &state.installed_origins);
        }
        items
    }

    fn rebuild(&self, state: &mut State) {
        state.base_items = merge(self.native_items(state), self.flatpak_items(state));
        // Who blocked which app: read again with each rebuild, so a block set
        // elsewhere shows here too. Blocked apps stay in the catalogue, so a
        // link still opens their page, which says who blocked them; the
        // screens and search list them nowhere.
        state.blocks = ui::registry::BlockList::load();
        // Pending or failed alike: what these list is not current yet.
        state.expired_repositories.clone_from(&self.expired);
        state.unanswered_repositories = self
            .registries
            .iter()
            .flatten()
            .filter(|registry| registry.enabled && !self.answered.contains(&registry.name))
            .map(|registry| registry.name.clone())
            .collect();
        // Preserve the established source error order rather than letting
        // response timing put an optional popular-feed outage before a
        // configured repository failure in the bounded public summary.
        let mut errors = self.errors.iter().collect::<Vec<_>>();
        errors.sort_by_key(|(priority, _)| *priority);
        state.catalogue_error = (!errors.is_empty()).then(|| {
            errors
                .iter()
                .map(|(_, message)| message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        });
        state.details.clear();
        sync_authoritative_catalogue(state);
    }
}

fn timed_out(detail: String) -> CatalogueError {
    CatalogueError::Refused {
        refusal: Refusal::TimedOut,
        detail,
    }
}

/// The `catalogue` reading's arguments for `registry`: asked of the repository now when `fresh`.
fn catalogue_arguments(
    registry: &RegistryConfig,
    fresh: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("registry".into(), registry.name.clone().into());
    arguments.insert("fresh".into(), fresh.into());
    arguments
}

/// What the Store says when the repository `name` could not be read.
///
/// Decided by the package cache's typed refusal alone, never by the text of an error:
/// an expired catalogue is the repository's to renew; while the system's
/// interface declaration cannot be read the system needs an update, and
/// saying so is the way out, where transport details would read as an
/// unreachable repository.
fn native_error(name: &str, error: &CatalogueError) -> String {
    match error.refusal() {
        Some(Refusal::Expired) => CATALOG.t_args(
            "store-repository-expired",
            &[("name", name.to_owned().into())],
        ),
        Some(Refusal::SystemUpdateNeeded) => CATALOG.t_args(
            "store-system-update-needed",
            &[("name", name.to_owned().into())],
        ),
        Some(Refusal::TimedOut) => CATALOG.t_args(
            "store-repository-timeout",
            &[("name", name.to_owned().into())],
        ),
        Some(Refusal::BehindAccepted) => format!("Could not load {name}: {}", error_detail(error)),
        None => format!("Could not load {name}: {error}"),
    }
}

/// What a refusal's detail says, for a refusal the Store has no words of its own for.
fn error_detail(error: &CatalogueError) -> &str {
    match error {
        CatalogueError::Refused { detail, .. } | CatalogueError::Failed(detail) => detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = "Tilo main";

    fn main_registry() -> RegistryConfig {
        RegistryConfig {
            name: MAIN.into(),
            base_url: "https://apps.tilo.so".into(),
            owner_pubkey_hex: "11".repeat(32),
            enabled: true,
            channel: app_cli::ReleaseChannel::Stable,
        }
    }

    fn catalogue_with_mail(version: &str) -> serde_json::Value {
        let mut entry = crate::sources::tests::tilo_entry("so.tilo.mail", "Mail", version, [3; 32]);
        entry.description = "Read and write email.".into();
        serde_json::to_value([crate::sources::tests::verified(entry)]).unwrap()
    }

    fn refresh_with_bundled_mail() -> (State, Refresh) {
        let mut state = super::super::tests::state_with_items(
            Vec::new(),
            crate::search::StoreDirectory::default(),
        );
        let mut refresh = Refresh::start(&mut state, None);
        refresh.repositories(
            &mut state,
            crate::config::RegistriesFile {
                registries: vec![main_registry()],
            },
        );
        // The installed package's manifest describes it while no repository answers.
        state
            .packages
            .set_installed(crate::sources::tests::described(
                crate::sources::tests::installed_package("so.tilo.mail", "0.4.0", false),
                "so.tilo.mail",
                package_actions::PackageAbout {
                    name: "Mail".into(),
                    category: Some("Office".into()),
                    ..package_actions::PackageAbout::default()
                },
                2_400_000,
            ));
        (state, refresh)
    }

    fn mail(state: &State) -> &StoreItem {
        state
            .base_items
            .iter()
            .find(|item| item.id == "so.tilo.mail")
            .expect("the installed app is always listed")
    }

    fn details(state: &State) -> serde_json::Value {
        let mut app = crate::app::StoreState::new();
        app.set_items(state.base_items.clone());
        app.set_unanswered_repositories(state.unanswered_repositories.clone());
        app.set_expired_repositories(state.expired_repositories.clone());
        app.admit(crate::StoreEffect::ShowDetails {
            app: "so.tilo.mail".into(),
        })
        .unwrap();
        app.inspect_catalogue(Some("so.tilo.mail")).unwrap()
    }

    tilo_test_env::scenario! {
        fn a_refresh_keeps_opened_references_and_installed_apps;

        phase refreshing_preserves_the_opened_reference_over_catalogue_offers;
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("editor.flatpakref");
            std::fs::write(
                &path,
                "[Flatpak Ref]\nName=org.example.Editor\nTitle=Editor\nBranch=beta\nUrl=https://example.org/repo/\n",
            )
            .unwrap();
            let mut state = super::super::tests::state_with_items(
                vec![super::super::tests::item("so.tilo.editor", "Editor")],
                crate::search::StoreDirectory::default(),
            );
            let opened = super::super::open_reference(Some(path), None, &mut state).unwrap();
            let mut refresh = Refresh::start(&mut state, None);
            refresh.errors.clear();
            let record = serde_json::from_value(serde_json::json!({
                "flatpakAppId": "org.example.Editor", "name": "Editor",
                "currentReleaseVersion": "1.0"
            }))
            .unwrap();
            refresh.apply(
                &mut state,
                Response::Flathub(Ok(FlathubFeed::from_resources(
                    "en-US",
                    &[record],
                    &[],
                    true,
                    0,
                ))),
            );
            refresh.rebuild(&mut state);
            let item = state
                .base_items
                .iter()
                .find(|item| item.id == opened.id)
                .expect("an explicitly opened reference survives native name deduplication");
            assert_eq!(item.install_ref_path, opened.install_ref_path);
            assert_eq!(item.latest_version(), "beta");
            assert_eq!(
                item.source_url.as_deref(),
                Some("https://example.org/repo/")
            );
            assert_eq!(item.action_kind(true), crate::ActionKind::FlatpakInstall);
            // Reopening details must not replace the reference with Flathub's offer,
            // including when the detail response is served from the worker cache.
            for _ in 0..2 {
                let install = super::super::tests::detail_installation_item(&mut state, &opened.id);
                assert_eq!(install.install_ref_path, opened.install_ref_path);
                assert_eq!(install.latest_version(), "beta");
                assert_eq!(
                    install.source_url.as_deref(),
                    Some("https://example.org/repo/")
                );
            }
        }

        phase refreshing_a_remote_replaces_its_cached_detail_for_installation;
        {
            let mut state = super::super::tests::state_with_items(
                Vec::new(),
                crate::search::StoreDirectory::default(),
            );
            let remote = Remote {
                name: "example".into(),
                url: "https://example.org/repo/".into(),
                title: "Example".into(),
                disabled: false,
                user_managed: true,
                catalogue_error: None,
            };
            state.remotes.push(remote.clone());
            for version in ["1.0", "2.0"] {
                let mut refresh = Refresh::start(&mut state, None);
                refresh.errors.clear();
                refresh.apply(
                    &mut state,
                    Response::Flatpak(
                        remote.clone(),
                        Ok(format!(
                            "org.example.Editor\tEditor\tAn editor\t{version}\tbeta\n"
                        )),
                    ),
                );
                refresh.rebuild(&mut state);
                let install =
                    super::super::tests::detail_installation_item(&mut state, "org.example.Editor");
                assert_eq!(install.latest_version(), version);
                assert!(install.install_ref_path.is_none());
                assert!(
                    matches!(install.source, StoreSource::FlatpakRemote { name, branch, user_managed: true, .. }
                    if name == "example" && branch == "beta")
                );
            }
        }

        phase installed_flatpak_survives_native_name_collision_and_remains_selected;
        {
            let mut native = super::super::tests::item("so.tilo.editor", "Editor");
            native.installed = crate::InstalledState::Installed {
                version: "1".into(),
                available: "1".into(),
                pinned: false,
                user_managed: false,
            };
            let mut state = super::super::tests::state_with_items(
                vec![native],
                crate::search::StoreDirectory::default(),
            );
            let mut refresh = Refresh::start(&mut state, None);
            refresh.apply(
                &mut state,
                Response::Status(Ok(FlatpakStatus {
                    available: true,
                    installed: "org.example.Editor\t2.0\tflathub\tuser\n".into(),
                    remotes: concat!(
                        "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\n",
                        "flathub\thttps://dl.flathub.org/repo/\tFlathub\tuser\n",
                    )
                    .into(),
                })),
            );
            for remote in state.remotes.clone() {
                refresh.apply(
                    &mut state,
                    Response::Flatpak(
                        remote,
                        Ok("org.example.Editor\tEditor\tEdit documents\t2.0\tstable\tflathub\n".into()),
                    ),
                );
            }
            refresh.rebuild(&mut state);
            let item = state.base_items.iter().find(|item| item.id == "org.example.Editor")
                .expect("the actually installed account app must not disappear behind a native name collision");
            assert!(
                matches!(&item.installed, crate::InstalledState::Installed { version, user_managed: true, .. } if version == "2.0")
            );
            assert_eq!(item.action_kind(true), crate::ActionKind::FlatpakOpen);
            assert_eq!(state.remotes.len(), 2);
            let mut app = crate::app::StoreState::new();
            app.set_items(state.base_items);
            app.set_flatpak_available(true);
            app.admit(crate::StoreEffect::SetSearch {
                query: "org.example.Editor".into(),
            })
            .unwrap();
            app.admit(crate::StoreEffect::ShowDetails {
                app: "org.example.Editor".into(),
            })
            .unwrap();
            let exact = app.inspect_catalogue(Some("org.example.Editor")).unwrap();
            assert_eq!(exact["matching"], 1);
            assert_eq!(exact["selected_app"], "org.example.Editor");
            assert_eq!(exact["detail"]["installed"], true);
            assert_eq!(exact["detail"]["name"], "Editor");
            app.admit(crate::StoreEffect::SetSearch {
                query: "Editor".into(),
            })
            .unwrap();
            assert!(
                app.visible_items()
                    .iter()
                    .any(|item| item.id == "org.example.Editor")
            );
            app.admit(crate::StoreEffect::SetSearch {
                query: String::new(),
            })
            .unwrap();
            app.admit(crate::StoreEffect::ShowView {
                view: "installed".into(),
            })
            .unwrap();
            assert!(
                app.visible_items()
                    .iter()
                    .any(|item| item.id == "org.example.Editor")
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_refresh_shows_the_last_verified_catalogue;

        phase a_bundled_app_with_no_reachable_catalogue_shows_its_manifest_details;
        {
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            refresh.apply(
                &mut state,
                Response::LastVerified(main_registry(), Err("no verified catalogue".into())),
            );
            refresh.apply(
                &mut state,
                Response::Native(main_registry(), Err("error sending request".into())),
            );
            refresh.rebuild(&mut state);

            let item = mail(&state);
            assert_eq!(item.name, "Mail");
            assert_eq!(item.categories, ["Office"]);
            assert_eq!(item.latest_version(), "0.4.0");
            assert_eq!(item.versions[0].size_bytes, 2_400_000);
            assert!(
                item.description.is_empty(),
                "no row repeats a sentence about missing details: {:?}",
                item.description
            );
            assert!(!item.installed.update_available());
            assert_eq!(item.action_kind(true), crate::ActionKind::Open);

            // The details page names the repository that has not answered, once.
            let shown = details(&state);
            assert_eq!(shown["detail"]["name"], "Mail");
            let notice = shown["detail"]["notice"].as_str().unwrap();
            assert!(notice.contains(MAIN), "{notice}");
            assert!(notice.contains("description and updates"), "{notice}");
        }

        phase a_failed_refresh_keeps_the_last_verified_catalogue;
        {
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            refresh.apply(
                &mut state,
                Response::LastVerified(main_registry(), Ok(catalogue_with_mail("0.5.0"))),
            );
            refresh.apply(
                &mut state,
                Response::Native(main_registry(), Err("error sending request".into())),
            );
            refresh.rebuild(&mut state);

            let item = mail(&state);
            assert_eq!(item.description, "Read and write email.");
            assert!(
                matches!(&item.source, StoreSource::Tilo { registry_name } if registry_name == MAIN)
            );
            assert!(item.installed.update_available());
            assert!(details(&state)["detail"].get("notice").is_none());
            assert!(
                state
                    .catalogue_error
                    .as_deref()
                    .is_some_and(|error| error.contains(MAIN))
            );
        }

        phase a_verified_catalogue_fills_the_details_and_outranks_the_stored_one;
        {
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            refresh.apply(
                &mut state,
                Response::Native(main_registry(), Ok(catalogue_with_mail("0.6.0"))),
            );
            // A stored catalogue answering late must not replace the fresh one.
            refresh.apply(
                &mut state,
                Response::LastVerified(main_registry(), Ok(catalogue_with_mail("0.5.0"))),
            );
            refresh.rebuild(&mut state);

            let item = mail(&state);
            assert_eq!(item.name, "Mail");
            assert_eq!(item.description, "Read and write email.");
            assert_eq!(item.latest_version(), "0.6.0");
            assert!(state.unanswered_repositories.is_empty());
            assert!(details(&state)["detail"].get("notice").is_none());
        }
    }

    tilo_test_env::scenario! {
        fn a_refused_or_expired_catalogue_says_why;

        phase a_repository_the_system_is_too_old_to_read_says_the_system_needs_an_update;
        // When this device's package cache cannot read the system's interface declaration it
        // refuses the repository; the Store says the device needs a system
        // update, naming the repository, as visibly as an outage and without
        // the RPC's internals.
        {
            crate::resource::CATALOG.ensure();
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            refresh.apply(
                &mut state,
                Response::Native(
                    main_registry(),
                    Err(CatalogueError::Refused {
                        refusal: Refusal::SystemUpdateNeeded,
                        detail: "/usr/share/tilo/system-interfaces.toml is not a system-interface \
                             declaration: invalid type: integer `2`, expected a sequence"
                            .into(),
                    }),
                ),
            );
            refresh.rebuild(&mut state);

            let error = state
                .catalogue_error
                .as_deref()
                .expect("reported, not silent");
            let says = crate::resource::CATALOG.t_args(
                "store-system-update-needed",
                &[("name", ui::i18n::ArgValue::Text(MAIN.into()))],
            );
            assert!(error.lines().any(|line| line == says), "{error}");
            assert!(error.contains(MAIN), "{error}");
            assert!(!error.contains("remote exception"), "{error}");
            assert!(
                state
                    .unanswered_repositories
                    .iter()
                    .any(|name| name == MAIN),
                "the repository is not shown as answered and empty"
            );
        }

        phase a_refusal_reads_the_same_whatever_its_detail_says;
        // The Store's state follows the package cache's typed refusal alone: the same
        // refusal with any wording of its detail, or with none, reads the same,
        // and a failure whose text merely mentions a refusal is not one.
        {
            crate::resource::CATALOG.ensure();
            let state_after = |error: CatalogueError| {
                let (mut state, mut refresh) = refresh_with_bundled_mail();
                refresh.apply(&mut state, Response::Native(main_registry(), Err(error)));
                refresh.rebuild(&mut state);
                (state.catalogue_error, state.expired_repositories)
            };
            for refusal in [
                Refusal::SystemUpdateNeeded,
                Refusal::Expired,
                Refusal::TimedOut,
            ] {
                let states: Vec<_> = [
                    "",
                    "the declaration is unreadable",
                    "command timed out after 8000 ms",
                    "[expired-catalogue] Failed: remote exception: something else entirely",
                ]
                .into_iter()
                .map(|detail| {
                    state_after(CatalogueError::Refused {
                        refusal,
                        detail: detail.into(),
                    })
                })
                .collect();
                assert!(
                    states.windows(2).all(|pair| pair[0] == pair[1]),
                    "{refusal:?}: {states:?}"
                );
            }
            let (error, expired) = state_after(CatalogueError::Failed(
                "[expired-catalogue] this system needs an update to read app repositories: \
             command timed out after 8000 ms"
                    .into(),
            ));
            assert!(expired.is_empty());
            let error = error.unwrap();
            for refused in ["store-system-update-needed", "store-repository-expired"] {
                let says = crate::resource::CATALOG
                    .t_args(refused, &[("name", ui::i18n::ArgValue::Text(MAIN.into()))]);
                assert!(!error.lines().any(|line| line == says), "{error}");
            }
        }

        phase a_refusal_the_package_cache_answers_is_typed_whatever_its_detail_says;
        // The package cache answers a refusal typed: whatever its detail says, the Store reads
        // the refusal, and the same state follows. An answer that names no catalogue fails.
        {
            crate::resource::CATALOG.ensure();
            let mut seen = Vec::new();
            for detail in [
                "",
                "declaration unreadable",
                "Failed: remote exception: timed out",
            ] {
                let error = catalogue_of(Ok(serde_json::json!({
                    "refused": "system-update-needed",
                    "detail": detail,
                })))
                .unwrap_err();
                assert_eq!(
                    error.refusal(),
                    Some(Refusal::SystemUpdateNeeded),
                    "{error}"
                );
                let (mut state, mut refresh) = refresh_with_bundled_mail();
                refresh.apply(&mut state, Response::Native(main_registry(), Err(error)));
                refresh.rebuild(&mut state);
                seen.push(state.catalogue_error);
            }
            assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
            let expired = catalogue_of(Ok(serde_json::json!({"refused": "expired"}))).unwrap_err();
            assert_eq!(expired.refusal(), Some(Refusal::Expired));
            for unknown in [
                serde_json::json!({"refused": "something new", "detail": "x"}),
                serde_json::json!({"rows": []}),
            ] {
                assert_eq!(catalogue_of(Ok(unknown)).unwrap_err().refusal(), None);
            }
            assert_eq!(
                catalogue_of(Ok(serde_json::json!({"entries": []}))).unwrap(),
                serde_json::json!([])
            );
            assert_eq!(
                catalogue_of(Err("no package cache".into())).unwrap_err(),
                CatalogueError::Failed("no package cache".into())
            );
        }

        phase an_expired_catalogue_is_reported_as_expired;
        {
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            let error = CatalogueError::Refused {
                refusal: Refusal::Expired,
                detail: "the repository's catalogue expired at 1792587897 (Unix time)".into(),
            };
            refresh.apply(&mut state, Response::Native(main_registry(), Err(error)));
            refresh.rebuild(&mut state);
            assert_eq!(state.expired_repositories, [MAIN]);
            // Reported in the Store's words (the visible copy is pinned by
            // app::tests::an_expired_catalogue_is_named_as_expired_not_as_unreachable),
            // never as the package cache's raw text.
            let reported = state.catalogue_error.as_deref().unwrap();
            assert!(!reported.contains("1792587897"), "{reported}");
            assert!(!reported.contains("Could not load"), "{reported}");

            // Any other failure is not called expired.
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            refresh.apply(
                &mut state,
                Response::Native(main_registry(), Err("error sending request".into())),
            );
            refresh.rebuild(&mut state);
            assert!(state.expired_repositories.is_empty());
        }
    }
}

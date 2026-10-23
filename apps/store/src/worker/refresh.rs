//! Independent, bounded catalogue reads on the worker's existing host connection.

use std::collections::{HashMap, HashSet, VecDeque};
use ui::time::Instant;

use app_host::client::{
    CatalogueError, FlatpakStatus, ListedPackage, PackageAbout, PendingRead, Refusal,
};
use package_store::InstalledPackage;

use super::{
    Done, Duration, LocaleContext, RegistryConfig, Remote, State, StoreItem, StoreSource,
    installed_flatpak_to_item, installed_flatpaks, installed_tilo_to_store_item, merge,
    refresh_flatpak, sync_authoritative_catalogue, tilo_to_store_item_on_channel,
};
use crate::catalogue::flathub::FlathubFeed;

const MAX_ACTIVE_READS: usize = 4;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(90);

enum Source {
    /// The catalogue the host last verified from a repository, read without
    /// the network so the repository's apps are described at once.
    LastVerified(RegistryConfig),
    Native(RegistryConfig),
    Flatpak(Remote),
}

enum Read {
    Status(PendingRead<FlatpakStatus>),
    Repositories(PendingRead<String>),
    Installed(PendingRead<Vec<ListedPackage>>),
    LastVerified(RegistryConfig, PendingRead<String, CatalogueError>),
    Native(RegistryConfig, PendingRead<String, CatalogueError>),
    Flatpak(Remote, PendingRead<String, CatalogueError>),
    Flathub(PendingRead<FlathubFeed>),
}

enum Response {
    Status(Result<FlatpakStatus, String>),
    Repositories(Result<String, String>),
    Installed(Result<Vec<ListedPackage>, String>),
    LastVerified(RegistryConfig, Result<String, CatalogueError>),
    Native(RegistryConfig, Result<String, CatalogueError>),
    Flatpak(Remote, Result<String, CatalogueError>),
    Flathub(Result<FlathubFeed, String>),
}

impl Read {
    fn poll(&mut self, host: &app_host::client::BlockingClient) -> Option<Response> {
        match self {
            Self::Status(read) => read.poll(host).map(Response::Status),
            Self::Repositories(read) => read.poll(host).map(Response::Repositories),
            Self::Installed(read) => read.poll(host).map(Response::Installed),
            Self::LastVerified(registry, read) => read
                .poll(host)
                .map(|result| Response::LastVerified(registry.clone(), result)),
            Self::Native(registry, read) => read
                .poll(host)
                .map(|result| Response::Native(registry.clone(), result)),
            Self::Flatpak(remote, read) => read
                .poll(host)
                .map(|result| Response::Flatpak(remote.clone(), result)),
            Self::Flathub(read) => read.poll(host).map(Response::Flathub),
        }
    }

    fn timed_out(&self) -> Response {
        let error = ui::i18n!("store-source-timeout");
        match self {
            Self::Status(_) => Response::Status(Err(error)),
            Self::Repositories(_) => Response::Repositories(Err(error)),
            Self::Installed(_) => Response::Installed(Err(error)),
            Self::LastVerified(registry, _) => {
                Response::LastVerified(registry.clone(), Err(timed_out(error)))
            }
            Self::Native(registry, _) => Response::Native(registry.clone(), Err(timed_out(error))),
            Self::Flatpak(remote, _) => Response::Flatpak(remote.clone(), Err(timed_out(error))),
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
    installed: Option<Vec<InstalledPackage>>,
    /// What each installed package's own manifest says about it, by id.
    about: HashMap<String, PackageAbout>,
    registries: Option<Vec<RegistryConfig>>,
    native: HashMap<String, Vec<crate::registry_client::VerifiedEntry>>,
    /// Repositories that answered this refresh; their catalogue replaces the
    /// last verified one and is never replaced by it.
    answered: HashSet<String>,
    /// Repositories whose catalogue this refresh found authentic but expired.
    expired: Vec<String>,
    flatpak: HashMap<String, Vec<StoreItem>>,
    /// Flathub's catalogue: the one the last refresh read until this one's
    /// read answers.
    feed: Option<FlathubFeed>,
    cached: Vec<StoreItem>,
    errors: Vec<(u8, String)>,
    deadline: Instant,
    retired: bool,
    locale: LocaleContext,
    pub(super) done: Option<Done>,
}

impl Refresh {
    pub(super) fn start(state: &State, done: Option<Done>) -> Self {
        Self::start_with_feed(state, done, None, None)
    }

    /// A refresh generation. `feed_read` is a Flathub read already under way
    /// (a restarted generation keeps it); `feed` one already answered.
    fn start_with_feed(
        state: &State,
        done: Option<Done>,
        feed_read: Option<ActiveRead>,
        feed: Option<FlathubFeed>,
    ) -> Self {
        let mut active = state.host.as_ref().map_or_else(Vec::new, |host| {
            vec![
                Read::Status(host.begin_flatpak_status()),
                Read::Repositories(host.begin_repositories()),
                Read::Installed(host.begin_list()),
            ]
            .into_iter()
            .map(|read| ActiveRead {
                read,
                started: ui::time::now(),
            })
            .collect()
        });
        let answered = feed.is_some();
        if let Some(read) = feed_read {
            active.push(read);
        } else if !answered && let Some(host) = state.host.as_ref() {
            // The kept catalogue answers at once while it is fresh; reading
            // Flathub's whole catalogue again runs in the background and
            // never blocks the other sources.
            let locale = crate::catalogue::desktop_locale();
            let force = state.force_refresh;
            active.push(ActiveRead {
                read: Read::Flathub(host.background_read(move || {
                    crate::catalogue::flathub::load_or_fetch(&locale, force)
                })),
                started: ui::time::now(),
            });
        }
        Self {
            active,
            queued: VecDeque::new(),
            installed: None,
            about: HashMap::new(),
            registries: None,
            native: HashMap::new(),
            answered: HashSet::new(),
            expired: Vec::new(),
            flatpak: HashMap::new(),
            feed: feed.or_else(|| state.flathub.clone()),
            cached: state.base_items.clone(),
            errors: if state.host.is_none() {
                vec![(0, "App service unavailable.".into())]
            } else {
                Vec::new()
            },
            deadline: ui::time::now() + REFRESH_TIMEOUT,
            retired: false,
            locale: LocaleContext::capture(),
            done,
        }
    }

    /// Drain the old bounded requests before starting a replacement generation.
    /// Cap'n Proto response cancellation does not promise server cancellation.
    pub(super) fn retire(&mut self) {
        self.retired = true;
        self.queued.clear();
    }

    pub(super) fn is_retired(&self) -> bool {
        self.retired
    }

    /// Replace read generation after a real mutation, retaining the original
    /// refresh invocation and its deadline. Old responses cannot merge later.
    pub(super) fn restart(self, state: &State) -> Self {
        let deadline = self.deadline;
        self.locale.apply();
        let feed_read = self
            .active
            .into_iter()
            .find(|active| matches!(active.read, Read::Flathub(_)));
        let feed = if feed_read.is_some() { None } else { self.feed };
        let mut refreshed = Self::start_with_feed(state, self.done, feed_read, feed);
        refreshed.deadline = deadline;
        refreshed
    }

    /// Whether a snapshot changed, and whether every source has settled.
    pub(super) fn poll(&mut self, state: &mut State) -> (bool, bool) {
        self.locale.apply();
        let Some(host) = state.host.as_ref() else {
            refresh_flatpak(state);
            self.rebuild(state);
            return (true, true);
        };
        let mut responses = Vec::new();
        for index in (0..self.active.len()).rev() {
            let Some(active) = self.active.get_mut(index) else {
                continue;
            };
            let response =
                if active.started.elapsed() >= READ_TIMEOUT || ui::time::now() >= self.deadline {
                    Some(active.read.timed_out())
                } else {
                    active.read.poll(host)
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
        let changed = !responses.is_empty();
        // Explicit browsing uses the same authoritative catalogue. Preserve
        // those discoveries when an independent source completes later.
        if changed {
            self.cached.clone_from(&state.base_items);
        }
        for response in responses {
            self.apply(state, response);
        }
        self.admit(state);
        if changed {
            self.rebuild(state);
        }
        (changed, self.active.is_empty() && self.queued.is_empty())
    }

    fn admit(&mut self, state: &State) {
        let Some(host) = state.host.as_ref() else {
            return;
        };
        while self.active.len() < MAX_ACTIVE_READS {
            let Some(source) = self.queued.pop_front() else {
                break;
            };
            if ui::time::now() >= self.deadline {
                self.errors.push((0, ui::i18n!("store-source-timeout")));
                continue;
            }
            let read = match source {
                Source::LastVerified(registry) => {
                    let read = host.begin_last_verified_catalogue(&registry.name);
                    Read::LastVerified(registry, read)
                }
                Source::Native(registry) => {
                    let read = host.begin_registry_catalogue(&registry.name);
                    Read::Native(registry, read)
                }
                Source::Flatpak(remote) => {
                    let read = host.begin_flatpak_catalogue(&remote.name, remote.user_managed);
                    Read::Flatpak(remote, read)
                }
            };
            self.active.push(ActiveRead {
                read,
                started: ui::time::now(),
            });
        }
    }

    fn apply(&mut self, state: &mut State, response: Response) {
        match response {
            Response::Status(Ok(status)) => {
                state.flatpak_error = None;
                state.flatpak_available = status.available;
                state.installed = installed_flatpaks(&status.installed);
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
            Response::Repositories(result) => self.repositories(state, result),
            Response::Installed(Ok(listed)) => {
                self.about = listed
                    .iter()
                    .map(|listed| (listed.package.id.clone(), listed.about.clone()))
                    .collect();
                self.installed = Some(listed.into_iter().map(|listed| listed.package).collect());
            }
            Response::Installed(Err(error)) => self
                .errors
                .push((0, format!("Could not read apps on this device: {error}"))),
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
                        crate::sources::remote_catalogue_items(&rows, &remote, &state.installed),
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

    fn repositories(&mut self, state: &mut State, result: Result<String, String>) {
        let parsed = result.and_then(|json| {
            serde_json::from_str::<crate::config::RegistriesFile>(&json)
                .map_err(|error| format!("unreadable repository list: {error}"))
        });
        match parsed {
            Ok(file) => {
                // The directory publishes from the list kept here instead of
                // asking the host again.
                state.registries.clone_from(&file.registries);
                // The stored catalogues are local reads: ask for them first,
                // so a slow repository is already described while it answers.
                let enabled = || file.registries.iter().filter(|registry| registry.enabled);
                self.queued
                    .extend(enabled().cloned().map(Source::LastVerified));
                self.queued.extend(enabled().cloned().map(Source::Native));
                self.registries = Some(file.registries);
            }
            Err(error) => self
                .errors
                .push((0, format!("Could not read app repositories: {error}"))),
        }
    }

    /// The host's last verified catalogue stands in until the repository
    /// answers. Without one (none accepted yet, expired, or a host that
    /// predates it) the fresh read alone decides, so this reports nothing.
    fn last_verified(&mut self, registry: RegistryConfig, result: Result<String, CatalogueError>) {
        if self.answered.contains(&registry.name) {
            return;
        }
        if let Ok(entries) = result.map_err(|error| error.to_string()).and_then(|json| {
            serde_json::from_str(&json).map_err(|error: serde_json::Error| error.to_string())
        }) {
            self.native.insert(registry.name, entries);
        }
    }

    fn native_response(
        &mut self,
        registry: RegistryConfig,
        result: Result<String, CatalogueError>,
    ) {
        let entries = result.and_then(|json| {
            serde_json::from_str(&json).map_err(|error| {
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

    fn native_items(&self) -> Vec<StoreItem> {
        let installed = self.installed.as_deref().unwrap_or_default();
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
        if self.installed.is_some() {
            for item in &mut items {
                // What the signed manifest of the installed copy declares is
                // what the app may do here; a registry entry does not say.
                if let Some(about) = self.about.get(&item.id) {
                    item.listing.capabilities.clone_from(&about.capabilities);
                    if item.tagline.trim().is_empty() {
                        item.tagline = about
                            .tagline_for(&crate::sources::preferred_locale())
                            .to_owned();
                    }
                }
                item.installed = installed.iter().find(|entry| entry.id == item.id).map_or(
                    crate::InstalledState::Not,
                    |entry| crate::InstalledState::Installed {
                        version: entry.version.clone(),
                        available: item
                            .versions
                            .first()
                            .map_or_else(String::new, |version| version.version.clone()),
                        pinned: entry.pinned,
                        user_managed: entry.user_managed(),
                    },
                );
            }
            for entry in installed {
                if !items.iter().any(|item| item.id == entry.id) {
                    items.push(installed_tilo_to_store_item(
                        entry,
                        self.about.get(&entry.id),
                    ));
                }
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
        for (id, version) in &state.installed {
            if !items.iter().any(|item| item.id == *id) {
                items.push(installed_flatpak_to_item(id, version));
            }
        }
        if let Some(reference) = &state.opened_ref {
            // Opening a reference is an explicit source/branch choice. A
            // catalogue response must not silently install a different offer.
            items.retain(|item| item.id != reference.app_id);
            let version = state
                .installed
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
                .installed
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
        state.base_items = merge(self.native_items(), self.flatpak_items(state));
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

/// A catalogue read the Store stopped waiting for: timed out, as typed as
/// the host's own deadline.
fn timed_out(detail: String) -> CatalogueError {
    CatalogueError::Refused {
        refusal: Refusal::TimedOut,
        detail,
    }
}

/// What the Store says when the repository `name` could not be read.
///
/// Decided by the host's typed refusal alone, never by the text of an error:
/// an expired catalogue is the repository's to renew; while the system's
/// interface declaration cannot be read the system needs an update, and
/// saying so is the way out, where transport details would read as an
/// unreachable repository.
fn native_error(name: &str, error: &CatalogueError) -> String {
    match error.refusal() {
        Some(Refusal::Expired) => ui::i18n!("store-repository-expired", "name" => name.to_owned()),
        Some(Refusal::SystemUpdateNeeded) => {
            ui::i18n!("store-system-update-needed", "name" => name.to_owned())
        }
        Some(Refusal::TimedOut) => ui::i18n!("store-repository-timeout", "name" => name.to_owned()),
        Some(Refusal::DeviceRepositoryStale) | None => format!("Could not load {name}: {error}"),
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

    /// The bundled Mail app as app-host lists it: the system store row and
    /// its signed manifest's name and category.
    fn bundled_mail() -> ListedPackage {
        let mut package = crate::sources::tests::installed_package(
            "so.tilo.mail",
            "0.4.0",
            "ab".repeat(32),
            false,
            false,
        );
        package.payload_bytes = 2_400_000;
        ListedPackage {
            package,
            about: PackageAbout {
                name: "Mail".into(),
                names: std::collections::BTreeMap::new(),
                category: Some("Office".into()),
                ..PackageAbout::default()
            },
        }
    }

    /// A verified catalogue listing Mail, as the host serialises one.
    fn catalogue_with_mail(version: &str) -> String {
        let mut entry = crate::sources::tests::tilo_entry("so.tilo.mail", "Mail", version, [3; 32]);
        entry.description = "Read and write email.".into();
        serde_json::to_string(&[crate::sources::tests::verified(entry)]).unwrap()
    }

    /// A refresh that has read this device's apps and repositories.
    fn refresh_with_bundled_mail() -> (State, Refresh) {
        let mut state = super::super::tests::state_with_items(
            Vec::new(),
            crate::search::StoreDirectory::default(),
        );
        let mut refresh = Refresh::start(&state, None);
        refresh.apply(
            &mut state,
            Response::Repositories(Ok(serde_json::json!({
                "registry": [{
                    "name": MAIN, "base_url": "https://apps.tilo.so",
                    "owner_pubkey_hex": "11".repeat(32), "enabled": true
                }]
            })
            .to_string())),
        );
        refresh.apply(&mut state, Response::Installed(Ok(vec![bundled_mail()])));
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
        let mut app = crate::StoreApp::new();
        app.set_items(state.base_items.clone());
        app.set_unanswered_repositories(state.unanswered_repositories.clone());
        app.set_expired_repositories(state.expired_repositories.clone());
        app.admit(crate::StoreEffect::ShowDetails {
            app: "so.tilo.mail".into(),
        })
        .unwrap();
        app.inspect_catalogue(Some("so.tilo.mail")).unwrap()
    }

    #[test]
    fn refreshing_preserves_the_opened_reference_over_catalogue_offers() {
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
        let mut refresh = Refresh::start(&state, None);
        // Responses are supplied below rather than read through a host connection.
        refresh.errors.clear();
        let record = serde_json::from_value(serde_json::json!({
            "flatpakAppId": "org.example.Editor", "name": "Editor",
            "currentReleaseVersion": "1.0"
        }))
        .unwrap();
        refresh.apply(
            &mut state,
            Response::Flathub(Ok(FlathubFeed::from_records(
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

    #[test]
    fn refreshing_a_remote_replaces_its_cached_detail_for_installation() {
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
            let mut refresh = Refresh::start(&state, None);
            // Responses are supplied below rather than read through a host connection.
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

    #[test]
    fn a_bundled_app_with_no_reachable_catalogue_shows_its_manifest_details() {
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

    #[test]
    fn a_failed_refresh_keeps_the_last_verified_catalogue() {
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

    /// When the app host cannot read the system's interface declaration it
    /// refuses the repository; the Store says the device needs a system
    /// update, naming the repository, as visibly as an outage and without
    /// the RPC's internals.
    #[test]
    fn a_repository_the_system_is_too_old_to_read_says_the_system_needs_an_update() {
        crate::resource::STORE_CATALOG.ensure();
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
        // The fixture has no host connection, which reports its own line.
        let says = ui::i18n::I18nManager::format(
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

    /// The Store's state follows the host's typed refusal alone: the same
    /// refusal with any wording of its detail, or with none, reads the same,
    /// and a failure whose text merely mentions a refusal is not one.
    #[test]
    fn a_refusal_reads_the_same_whatever_its_detail_says() {
        crate::resource::STORE_CATALOG.ensure();
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
            let says = ui::i18n::I18nManager::format(
                refused,
                &[("name", ui::i18n::ArgValue::Text(MAIN.into()))],
            );
            assert!(!error.lines().any(|line| line == says), "{error}");
        }
    }

    /// A host that refuses every catalogue read with one refusal and the
    /// detail it is given.
    struct RefusingHost(app_host::client::WireCatalogueRefusal, &'static str);
    impl app_host::app_host_capnp::app_host::Server for RefusingHost {
        fn registry_catalogue(
            self: capnp::capability::Rc<Self>,
            _params: app_host::app_host_capnp::app_host::RegistryCatalogueParams,
            mut results: app_host::app_host_capnp::app_host::RegistryCatalogueResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let mut out = results.get();
            out.set_refusal(self.0);
            out.set_detail(self.1);
            std::future::ready(Ok(()))
        }
    }

    /// The refusal crosses the wire typed: whatever the host's detail says,
    /// the Store's pending read gets the refusal, and the same state follows.
    #[test]
    fn a_refusal_crosses_the_wire_typed_whatever_its_detail_says() {
        crate::resource::STORE_CATALOG.ensure();
        let mut seen = Vec::new();
        for detail in [
            "",
            "declaration unreadable",
            "Failed: remote exception: timed out",
        ] {
            let (service, connection) = app_host::testing::ConnectedService::start(RefusingHost(
                app_host::client::WireCatalogueRefusal::SystemUpdateNeeded,
                detail,
            ))
            .unwrap();
            let client = app_host::client::BlockingClient::from_connected_fd(connection).unwrap();
            let error = client.registry_catalogue(MAIN).unwrap_err();
            assert_eq!(
                error.refusal(),
                Some(Refusal::SystemUpdateNeeded),
                "{error}"
            );
            let (mut state, mut refresh) = refresh_with_bundled_mail();
            refresh.apply(&mut state, Response::Native(main_registry(), Err(error)));
            refresh.rebuild(&mut state);
            seen.push(state.catalogue_error);
            drop(client);
            service.shutdown().unwrap();
        }
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    }

    #[test]
    fn an_expired_catalogue_is_reported_as_expired() {
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
        // never as the host's raw text.
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

    #[test]
    fn a_verified_catalogue_fills_the_details_and_outranks_the_stored_one() {
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

    #[test]
    fn installed_flatpak_survives_native_name_collision_and_remains_selected() {
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
        let mut refresh = Refresh::start(&state, None);
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
        let mut app = crate::StoreApp::new();
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

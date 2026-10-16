//! Independent, bounded catalogue reads on the worker's existing host connection.

use std::collections::{HashMap, HashSet, VecDeque};
use ui::time::Instant;

use app_host::client::{FlatpakStatus, ListedPackage, PackageAbout, PendingRead};
use package_store::InstalledPackage;

use super::{
    Done, Duration, LocaleContext, RegistriesFile, RegistryConfig, Remote, State, StoreItem,
    StoreSource, flathub_record_to_item, installed_flatpak_to_item, installed_flatpaks,
    installed_tilo_to_store_item, merge, refresh_flatpak, sync_authoritative_catalogue,
    tilo_to_store_item_on_channel,
};

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
    LastVerified(RegistryConfig, PendingRead<String>),
    Native(RegistryConfig, PendingRead<String>),
    Flatpak(Remote, PendingRead<String>),
    Popular(PendingRead<Vec<crate::flathub_client::AppStreamRecord>>),
}

enum Response {
    Status(Result<FlatpakStatus, String>),
    Repositories(Result<String, String>),
    Installed(Result<Vec<ListedPackage>, String>),
    LastVerified(RegistryConfig, Result<String, String>),
    Native(RegistryConfig, Result<String, String>),
    Flatpak(Remote, Result<String, String>),
    Popular(Result<Vec<crate::flathub_client::AppStreamRecord>, String>),
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
            Self::Popular(read) => read.poll(host).map(Response::Popular),
        }
    }

    fn timed_out(&self) -> Response {
        let error = ui::i18n!("store-source-timeout");
        match self {
            Self::Status(_) => Response::Status(Err(error)),
            Self::Repositories(_) => Response::Repositories(Err(error)),
            Self::Installed(_) => Response::Installed(Err(error)),
            Self::LastVerified(registry, _) => Response::LastVerified(registry.clone(), Err(error)),
            Self::Native(registry, _) => Response::Native(registry.clone(), Err(error)),
            Self::Flatpak(remote, _) => Response::Flatpak(remote.clone(), Err(error)),
            Self::Popular(_) => Response::Popular(Err(error)),
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
    flatpak: HashMap<String, Vec<StoreItem>>,
    popular: Option<Vec<crate::flathub_client::AppStreamRecord>>,
    cached: Vec<StoreItem>,
    errors: Vec<(u8, String)>,
    deadline: Instant,
    retired: bool,
    locale: LocaleContext,
    pub(super) done: Option<Done>,
}

impl Refresh {
    pub(super) fn start(state: &State, done: Option<Done>) -> Self {
        Self::start_with_popular(state, done, None, None)
    }

    fn start_with_popular(
        state: &State,
        done: Option<Done>,
        popular: Option<ActiveRead>,
        records: Option<Vec<crate::flathub_client::AppStreamRecord>>,
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
        if let Some(read) = popular {
            active.push(read);
        } else if records.is_none()
            && let Some(host) = state.host.as_ref()
        {
            active.push(ActiveRead {
                read: Read::Popular(host.background_read(|| {
                    crate::flathub_client::fetch_popular(200).map_err(|error| error.to_string())
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
            flatpak: HashMap::new(),
            popular: records,
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
        let popular = self
            .active
            .into_iter()
            .find(|active| matches!(active.read, Read::Popular(_)));
        let records = self.popular.or_else(|| popular.is_none().then(Vec::new));
        let mut refreshed = Self::start_with_popular(state, self.done, popular, records);
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
                if let Response::Popular(Ok(records)) = response {
                    self.popular = Some(records);
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
            Response::Repositories(result) => self.repositories(result),
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
            Response::Popular(Ok(records)) => self.popular = Some(records),
            Response::Popular(Err(error)) => self
                .errors
                .push((1, format!("Could not load Flathub: {error}"))),
        }
    }

    fn repositories(&mut self, result: Result<String, String>) {
        let parsed = result.and_then(|json| {
            serde_json::from_str::<RegistriesFile>(&json)
                .map_err(|error| format!("unreadable repository list: {error}"))
        });
        match parsed {
            Ok(file) => {
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
    fn last_verified(&mut self, registry: RegistryConfig, result: Result<String, String>) {
        if self.answered.contains(&registry.name) {
            return;
        }
        if let Ok(entries) = result.and_then(|json| {
            serde_json::from_str(&json).map_err(|error: serde_json::Error| error.to_string())
        }) {
            self.native.insert(registry.name, entries);
        }
    }

    fn native_response(&mut self, registry: RegistryConfig, result: Result<String, String>) {
        let entries = result.and_then(|json| {
            serde_json::from_str(&json)
                .map_err(|error| format!("Invalid verified catalogue: {error}"))
        });
        match entries {
            Ok(entries) => {
                self.answered.insert(registry.name.clone());
                self.native.insert(registry.name, entries);
            }
            Err(error) => self
                .errors
                .push((0, format!("Could not load {}: {error}", registry.name))),
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
        let mut items = self.popular.as_ref().map_or_else(Vec::new, |records| {
            records
                .iter()
                .map(|record| flathub_record_to_item(record, &state.installed))
                .collect()
        });
        for remote in state.remotes.iter().filter(|remote| !remote.disabled) {
            if let Some(rows) = self.flatpak.get(&remote.resource_id()) {
                for item in rows {
                    if !items.iter().any(|existing| existing.id == item.id) {
                        items.push(item.clone());
                    }
                }
            }
        }
        for item in self
            .cached
            .iter()
            .filter(|item| !matches!(item.source, StoreSource::Tilo { .. }))
        {
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
        if let Some(reference) = &state.opened_ref
            && !items.iter().any(|item| item.id == reference.app_id)
        {
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
        // Pending or failed alike: what these list is not current yet.
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
        state.searches.clear();
        state.details.clear();
        sync_authoritative_catalogue(state);
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
        app.admit(crate::StoreEffect::ShowDetails {
            app: "so.tilo.mail".into(),
        })
        .unwrap();
        app.inspect_catalogue(Some("so.tilo.mail")).unwrap()
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
        // Still within its validity, the verified catalogue's update stands.
        assert!(item.installed.update_available());
        assert!(details(&state)["detail"].get("notice").is_none());
        // The outage itself is still reported.
        assert!(
            state
                .catalogue_error
                .as_deref()
                .is_some_and(|error| error.contains(MAIN))
        );
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
        app.admit(crate::StoreEffect::SetSource {
            source: "On device".into(),
        })
        .unwrap();
        assert!(
            app.visible_items()
                .iter()
                .any(|item| item.id == "org.example.Editor")
        );
    }
}

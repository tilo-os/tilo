//! One account decision at a time. All state and prepared credentials stay
//! device-local. Rendering and page entry never open the browser or sign in.

mod batch;
mod icloud;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use zeroize::Zeroizing;

use ui::icons::Icon;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    Button, ButtonRole, Container, Field, IconWidget, LabelPlacement, ListRow, Text, TextInput,
    TextInputKind,
};

use super::super::account_transfer::{
    AccountActions, AccountJobState, Candidate, ConnectionError, ConnectionProvider, DavConnection,
    Inventory, Provider, SourceRoots, TiloCandidate,
};
use super::super::strings::{t, t_args};
use crate::page;
use imager_backend::account_sources::Service;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AccountRequest {
    Source(String),
    Remove(String),
    Provider(String),
    AddAccount,
    ReviewSaved,
    ImportSaved,
    SkipSaved,
    IncludeSaved(String),
    ExcludeSaved(String),
    Attention(String),
    Confirm,
    SubmitCode,
    Cancel,
    Retry,
    Reconnect,
    Back,
    Overview,
    OpenAgain,
    NextcloudBrowser,
    NextcloudAppPassword,
}

impl AccountRequest {
    pub fn from_parts(step: &str, selection: Option<String>) -> Result<Self, &'static str> {
        match (step, selection) {
            ("choose-source", Some(id)) => Ok(Self::Source(id)),
            ("remove-prepared", Some(id)) => Ok(Self::Remove(id)),
            ("choose-provider", Some(id)) => Ok(Self::Provider(id)),
            ("add-account", None) => Ok(Self::AddAccount),
            ("review-saved", None) => Ok(Self::ReviewSaved),
            ("import-saved", None) => Ok(Self::ImportSaved),
            ("skip-saved", None) => Ok(Self::SkipSaved),
            ("include-saved", Some(id)) => Ok(Self::IncludeSaved(id)),
            ("exclude-saved", Some(id)) => Ok(Self::ExcludeSaved(id)),
            ("attention", Some(id)) => Ok(Self::Attention(id)),
            ("confirm", None) => Ok(Self::Confirm),
            ("submit-code", None) => Ok(Self::SubmitCode),
            ("cancel", None) => Ok(Self::Cancel),
            ("retry", None) => Ok(Self::Retry),
            ("reconnect", None) => Ok(Self::Reconnect),
            ("back", None) => Ok(Self::Back),
            ("overview", None) => Ok(Self::Overview),
            ("open-again", None) => Ok(Self::OpenAgain),
            ("nextcloud-browser", None) => Ok(Self::NextcloudBrowser),
            ("nextcloud-app-password", None) => Ok(Self::NextcloudAppPassword),
            _ => Err("installer-accounts-invalid-action"),
        }
    }
    pub fn parts(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Source(id) => ("choose-source", Some(id.clone())),
            Self::Remove(id) => ("remove-prepared", Some(id.clone())),
            Self::Provider(id) => ("choose-provider", Some(id.clone())),
            Self::AddAccount => ("add-account", None),
            Self::ReviewSaved => ("review-saved", None),
            Self::ImportSaved => ("import-saved", None),
            Self::SkipSaved => ("skip-saved", None),
            Self::IncludeSaved(id) => ("include-saved", Some(id.clone())),
            Self::ExcludeSaved(id) => ("exclude-saved", Some(id.clone())),
            Self::Attention(id) => ("attention", Some(id.clone())),
            Self::Confirm => ("confirm", None),
            Self::SubmitCode => ("submit-code", None),
            Self::Cancel => ("cancel", None),
            Self::Retry => ("retry", None),
            Self::Reconnect => ("reconnect", None),
            Self::Back => ("back", None),
            Self::Overview => ("overview", None),
            Self::OpenAgain => ("open-again", None),
            Self::NextcloudBrowser => ("nextcloud-browser", None),
            Self::NextcloudAppPassword => ("nextcloud-app-password", None),
        }
    }
}

pub(super) trait BrowserOpener: Send + Sync {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel;
}
struct PortalOpener;
impl BrowserOpener for PortalOpener {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        ui::portal::send_open_uri(url)
    }
}

#[derive(Clone)]
enum Selection {
    ICloud,
    Prepared(PreparedEntry),
    Saved(Candidate),
    Tilo(TiloCandidate),
    NextcloudBrowser {
        candidate: Option<Candidate>,
    },
    Dav {
        nextcloud: bool,
        candidate: Option<Candidate>,
    },
    OAuth {
        provider: ConnectionProvider,
        candidate: Option<Candidate>,
    },
}
impl Selection {
    fn source_id(&self) -> Option<&str> {
        match self {
            Self::Prepared(entry) => entry.source_id.as_deref(),
            Self::Tilo(candidate) => Some(&candidate.source_id),
            Self::Saved(candidate)
            | Self::NextcloudBrowser {
                candidate: Some(candidate),
            }
            | Self::Dav {
                candidate: Some(candidate),
                ..
            }
            | Self::OAuth {
                candidate: Some(candidate),
                ..
            } => Some(&candidate.id),
            Self::ICloud
            | Self::NextcloudBrowser { candidate: None }
            | Self::OAuth {
                candidate: None, ..
            }
            | Self::Dav {
                candidate: None, ..
            } => None,
        }
    }
    fn label(&self) -> String {
        match self {
            Self::ICloud => t("installer-accounts-icloud"),
            Self::Prepared(entry) => entry.label.clone(),
            Self::Saved(candidate) => candidate_label(candidate),
            Self::Tilo(candidate) => tilo_candidate_label(candidate),
            Self::NextcloudBrowser { candidate } => candidate
                .as_ref()
                .map_or_else(|| t("installer-accounts-nextcloud"), candidate_label),
            Self::Dav {
                nextcloud,
                candidate,
            } => candidate.as_ref().map_or_else(
                || {
                    t(if *nextcloud {
                        "installer-accounts-nextcloud"
                    } else {
                        "installer-accounts-webdav"
                    })
                },
                candidate_label,
            ),
            Self::OAuth {
                provider,
                candidate,
            } => candidate
                .as_ref()
                .map_or_else(|| provider_label(*provider), candidate_label),
        }
    }
}

pub(in crate::setup) use imager_backend::account_checkpoint::PreparedAccounts;
use imager_backend::account_checkpoint::PreparedEntry;
struct Work {
    selected: Selection,
    job: String,
    needs_code: bool,
    url: Option<String>,
    launch: Option<ui::portal::LaunchChannel>,
    cancelling: bool,
    browser_failed: bool,
    sign_in: bool,
    return_overview: bool,
}
enum Step {
    Overview,
    ReviewSaved,
    Providers,
    SpotifyPurpose(Option<Candidate>),
    ICloud(Box<icloud::ICloud>),
    Confirm(Selection),
    ConfirmRemoval(PreparedEntry),
    NextcloudMethod(Selection),
    Dav {
        selected: Selection,
        field: usize,
    },
    Working(Work),
    PastedCode(Work),
    Removing {
        selected: Selection,
        job: String,
        account_id: String,
    },
    Done(Selection),
    Failed {
        selected: Selection,
        message: &'static str,
        retry_cleanup: Option<String>,
    },
}

/// Which configured accounts a source row owns. The source itself is an app
/// identity (GNOME Online Accounts, Nextcloud, rclone), never a generic account
/// form: each account and each service it turns on is its own decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SourceAccountScope {
    Gnome,
    Nextcloud,
    Rclone,
    Application(String),
    Tilo,
}

impl SourceAccountScope {
    pub(super) fn id(&self) -> String {
        match self {
            Self::Gnome => "goa".into(),
            Self::Nextcloud => "accounts|nextcloud".into(),
            Self::Rclone => "accounts|rclone".into(),
            Self::Application(label) => {
                format!("accounts|application|{}", super::your_data::slug(label))
            }
            Self::Tilo => "accounts|tilo".into(),
        }
    }

    pub(super) fn parse(source: &str) -> Option<Self> {
        match source {
            "goa" => Some(Self::Gnome),
            "accounts|nextcloud" => Some(Self::Nextcloud),
            "accounts|rclone" => Some(Self::Rclone),
            "accounts|tilo" => Some(Self::Tilo),
            _ => source
                .strip_prefix("accounts|application|")
                .map(|label| Self::Application(label.into())),
        }
    }

    fn includes(&self, candidate: &Candidate) -> bool {
        match self {
            Self::Gnome => candidate.origin == super::super::account_transfer::Origin::Gnome,
            Self::Nextcloud => {
                candidate.origin == super::super::account_transfer::Origin::Nextcloud
            }
            Self::Rclone => candidate.origin == super::super::account_transfer::Origin::Rclone,
            Self::Application(label) => {
                candidate.origin == super::super::account_transfer::Origin::Application
                    && super::your_data::slug(&candidate.label) == *label
            }
            Self::Tilo => false,
        }
    }
}

/// The person's per-account, per-service choice over the configured accounts
/// discovered under the consented source roots. An empty selection means every
/// discovered service is included, so a fresh review starts with all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SourceAccountSelection {
    /// Services the person turned off, by opaque discovery identity. A missing
    /// identity or service is included.
    excluded: BTreeMap<String, BTreeSet<Service>>,
    /// Whether the person edited this review. Until then an older checkpoint's
    /// exact prepared accounts stay authoritative.
    touched: bool,
}

/// The services a configured source actually offers. GNOME Online Accounts and
/// Nextcloud report them; a file remote (rclone, a granted Windows OneDrive
/// root) enables Files even though its configuration has no service list. A
/// source that names no service and is not a file remote keeps an empty list
/// rather than an invented one.
pub(super) fn source_account_services(candidate: &Candidate) -> Vec<Service> {
    use super::super::account_transfer::Provider;
    if !candidate.services.is_empty() {
        return candidate.services.clone();
    }
    if matches!(
        candidate.provider,
        Provider::Google
            | Provider::Microsoft
            | Provider::Dropbox
            | Provider::Nextcloud
            | Provider::WebDav
    ) {
        return vec![Service::Files];
    }
    Vec::new()
}

impl SourceAccountSelection {
    pub(super) fn services(&self, candidate: &Candidate) -> Vec<Service> {
        let excluded = self.excluded.get(&candidate.id);
        source_account_services(candidate)
            .into_iter()
            .filter(|service| !excluded.is_some_and(|excluded| excluded.contains(service)))
            .collect()
    }

    pub(super) fn includes_service(&self, candidate: &Candidate, service: Service) -> bool {
        source_account_services(candidate).contains(&service)
            && !self
                .excluded
                .get(&candidate.id)
                .is_some_and(|excluded| excluded.contains(&service))
    }

    fn set_account(&mut self, candidate: &Candidate, included: bool) {
        self.touched = true;
        let services = source_account_services(candidate);
        if included {
            self.excluded.remove(&candidate.id);
        } else if !services.is_empty() {
            self.excluded
                .insert(candidate.id.clone(), services.into_iter().collect());
        }
    }

    fn set_service(&mut self, candidate: &Candidate, service: Service, included: bool) {
        self.touched = true;
        if included {
            if let Some(excluded) = self.excluded.get_mut(&candidate.id) {
                excluded.remove(&service);
                if excluded.is_empty() {
                    self.excluded.remove(&candidate.id);
                }
            }
        } else {
            self.excluded
                .entry(candidate.id.clone())
                .or_default()
                .insert(service);
        }
    }

    /// Drop identities the current discovery no longer has and services the
    /// source stopped enabling, so an old scope cannot survive a changed source.
    fn retain(&mut self, inventory: &Inventory) {
        let current = inventory
            .candidates
            .iter()
            .map(|candidate| (&candidate.id, source_account_services(candidate)))
            .collect::<BTreeMap<_, _>>();
        self.excluded.retain(|id, excluded| {
            let Some(services) = current.get(id) else {
                return false;
            };
            excluded.retain(|service| services.contains(service));
            !excluded.is_empty()
        });
    }
}

pub(super) struct Accounts {
    inventory: Inventory,
    source_roots: Option<SourceRoots>,
    actions: AccountActions,
    stage: Option<Arc<tempfile::TempDir>>,
    prepared: Vec<PreparedEntry>,
    reviewed_empty: bool,
    saved: batch::SavedConnections,
    checkpoint_worker: Option<checkpoint::Worker>,
    step: Step,
    opener: Arc<dyn BrowserOpener>,
    dav_fields: [ui::LocalState<String>; 3],
    source_accounts: SourceAccountSelection,
}
impl Default for Accounts {
    fn default() -> Self {
        Self {
            inventory: Inventory::default(),
            source_roots: None,
            actions: AccountActions::default(),
            stage: None,
            prepared: Vec::new(),
            reviewed_empty: false,
            saved: batch::SavedConnections::default(),
            checkpoint_worker: None,
            step: Step::Overview,
            opener: Arc::new(PortalOpener),
            dav_fields: std::array::from_fn(|_| ui::LocalState::new(String::new())),
            source_accounts: SourceAccountSelection::default(),
        }
    }
}

impl Accounts {
    pub(super) fn source_account_selection(&self) -> &SourceAccountSelection {
        &self.source_accounts
    }

    pub(super) fn source_account_selection_snapshot(&self) -> SourceAccountSelection {
        self.source_accounts.clone()
    }

    pub(super) fn restore_source_account_selection(&mut self, previous: SourceAccountSelection) {
        self.source_accounts = previous;
    }

    pub(super) fn set_source_account(
        &mut self,
        inventory: &Inventory,
        id: &str,
        included: bool,
    ) -> Result<bool, &'static str> {
        let candidate = inventory
            .candidates
            .iter()
            .find(|candidate| candidate.id == id && !source_account_services(candidate).is_empty())
            .ok_or("installer-your-data-changed")?;
        let before = self.source_accounts.clone();
        self.source_accounts.set_account(candidate, included);
        Ok(before != self.source_accounts)
    }

    pub(super) fn set_source_account_service(
        &mut self,
        inventory: &Inventory,
        id: &str,
        service: Service,
        included: bool,
    ) -> Result<bool, &'static str> {
        let candidate = inventory
            .candidates
            .iter()
            .find(|candidate| {
                candidate.id == id && source_account_services(candidate).contains(&service)
            })
            .ok_or("installer-your-data-changed")?;
        let before = self.source_accounts.clone();
        self.source_accounts
            .set_service(candidate, service, included);
        Ok(before != self.source_accounts)
    }

    pub(super) fn set_source_accounts(
        &mut self,
        inventory: &Inventory,
        scope: &SourceAccountScope,
        included: bool,
    ) -> Result<bool, &'static str> {
        let mut matched = false;
        let before = self.source_accounts.clone();
        for candidate in inventory.candidates.iter().filter(|candidate| {
            scope.includes(candidate) && !source_account_services(candidate).is_empty()
        }) {
            matched = true;
            self.source_accounts.set_account(candidate, included);
        }
        if !matched && !matches!(scope, SourceAccountScope::Gnome) {
            return Err("installer-your-data-changed");
        }
        Ok(before != self.source_accounts)
    }

    pub(super) fn set_all_source_accounts(
        &mut self,
        inventory: &Inventory,
        included: bool,
    ) -> bool {
        let before = self.source_accounts.clone();
        self.source_accounts.touched = true;
        if included {
            self.source_accounts.excluded.clear();
        } else {
            for candidate in inventory
                .candidates
                .iter()
                .filter(|candidate| !source_account_services(candidate).is_empty())
            {
                self.source_accounts.set_account(candidate, false);
            }
        }
        before != self.source_accounts
    }

    pub fn update_sources(
        &mut self,
        roots: SourceRoots,
        inventory: Inventory,
    ) -> Result<(), &'static str> {
        if self.busy() || matches!(self.step, Step::ReviewSaved) {
            return Err("installer-accounts-busy");
        }
        if !self.prepared.is_empty() {
            if self
                .source_roots
                .as_ref()
                .is_some_and(|source| source.home != roots.home || source.config != roots.config)
            {
                return Err("installer-accounts-sources-changed");
            }
            if let Some(stage) = &self.stage {
                self.actions
                    .validate_destination(stage.path())
                    .map_err(|_| "installer-accounts-sources-changed")?;
            }
        }
        self.actions
            .update_sources(roots.clone(), inventory.clone())
            .map_err(|_| "installer-accounts-sources-changed")?;
        self.source_accounts.retain(&inventory);
        self.source_roots = Some(roots);
        self.inventory = inventory;
        Ok(())
    }
    pub fn busy(&mut self) -> bool {
        if self.checkpoint_worker.is_some() {
            return true;
        }
        matches!(
            self.step,
            Step::Working(_)
                | Step::Removing { .. }
                | Step::Failed {
                    retry_cleanup: Some(_),
                    ..
                }
        ) || self.saved.running()
            || matches!(&self.step, Step::ICloud(flow) if flow.active())
            || self.actions.busy()
    }
    pub fn active(&self) -> bool {
        if self.checkpoint_worker.is_some() {
            return true;
        }
        self.saved.running()
            || matches!(self.step, Step::Working(_) | Step::Removing { .. })
            || matches!(&self.step, Step::ICloud(flow) if flow.active())
    }
    pub fn can_continue(&self) -> bool {
        if self.checkpoint_worker.is_some() {
            return false;
        }
        matches!(self.step, Step::Overview | Step::Done(_))
            && !self.saved.running()
            && (self.saved.reviewed || self.pending_saved().is_empty())
    }
    pub fn at_overview(&self) -> bool {
        matches!(self.step, Step::Overview) && !self.saved.running()
    }
    pub fn has_prepared_connections(&self) -> bool {
        !self.prepared.is_empty()
    }
    /// The GOA source accounts the person selected, with exactly the services
    /// they left on. Only opaque discovery ids and enabled services travel;
    /// credentials stay in their source until the destination revalidates it.
    pub(in crate::setup::journey) fn selected_source_accounts(
        &self,
        inventory: &Inventory,
    ) -> Vec<imager_backend::setup::deferred_handoff::SelectedAccount> {
        let prepared = self
            .prepared
            .iter()
            .filter_map(|entry| entry.source_id.as_deref())
            .collect::<BTreeSet<_>>();
        let checkpoint = !self.source_accounts.touched && !prepared.is_empty();
        let mut selected = BTreeMap::new();
        for candidate in inventory.candidates.iter().filter(|candidate| {
            candidate.origin == super::super::account_transfer::Origin::Gnome
                && (!checkpoint || prepared.contains(candidate.id.as_str()))
        }) {
            let services = self.source_accounts.services(candidate);
            let Some(identity) = candidate
                .identity
                .clone()
                .filter(|identity| !identity.is_empty())
            else {
                continue;
            };
            if services.is_empty() {
                continue;
            }
            selected.insert(
                candidate.id.clone(),
                imager_backend::setup::deferred_handoff::SelectedAccount {
                    source_id: candidate.id.clone(),
                    provider: candidate.provider,
                    identity,
                    label: candidate.label.clone(),
                    services,
                },
            );
        }
        selected.into_values().collect()
    }
    pub fn prepared_handle(&self) -> Option<PreparedAccounts> {
        if (self.prepared.is_empty() && !self.reviewed_empty) || !self.can_continue() {
            return None;
        }
        self.stage.as_ref().map(|stage| {
            self.actions
                .prepared_accounts(stage.clone(), self.prepared.clone())
        })
    }
    pub(super) fn navigation_step(&self) -> &'static str {
        match &self.step {
            Step::Overview if self.saved.running() => "waiting",
            Step::Overview => "overview",
            Step::ReviewSaved => "review-saved",
            Step::Providers => "providers",
            Step::SpotifyPurpose(_) => "spotify-purpose",
            Step::ICloud(flow) => flow.state(),
            Step::Confirm(_) => "confirm",
            Step::ConfirmRemoval(_) => "confirm-removal",
            Step::NextcloudMethod(_) => "nextcloud-method",
            Step::Dav { field: 0, .. } => "dav-server",
            Step::Dav { field: 1, .. } => "dav-user",
            Step::Dav { .. } => "dav-password",
            Step::Working(_) | Step::Removing { .. } => "waiting",
            Step::PastedCode(_) => "awaiting-code",
            Step::Done(_) => "done",
            Step::Failed { .. } => "failed",
        }
    }
    pub fn projection(&self) -> serde_json::Value {
        let state = match &self.step {
            Step::Overview if self.saved.running() => "waiting",
            Step::Overview => "overview",
            Step::ReviewSaved => "review-saved",
            Step::Providers => "providers",
            Step::SpotifyPurpose(_) => "spotify-purpose",
            Step::ICloud(flow) => flow.state(),
            Step::Dav { .. } => "dav-details",
            Step::NextcloudMethod(_) => "nextcloud-method",
            Step::Confirm(_) => "confirm",
            Step::ConfirmRemoval(_) => "confirm-removal",
            Step::Working(work) if work.cancelling => "cancelling",
            Step::Working(_) => "waiting",
            Step::PastedCode(_) => "awaiting-code",
            Step::Removing { .. } => "cancelling",
            Step::Done(Selection::Dav { .. }) => "saved",
            Step::Done(_) => "connected",
            Step::Failed { .. } => "failed",
        };
        serde_json::json!({ "state": state, "prepared_accounts": self.prepared.len(),
            "can_continue": self.can_continue(),
            "source_review": self.source_review_keys().iter().map(|key| t(key)).collect::<Vec<_>>() })
    }
    fn stage_home(&mut self) -> Result<PathBuf, &'static str> {
        if self.stage.is_none() {
            self.stage = Some(
                self.actions
                    .create_staging()
                    .map_err(|_| "installer-accounts-save-failed")?,
            );
        }
        self.stage
            .as_ref()
            .map(|stage| stage.path().to_path_buf())
            .ok_or("installer-accounts-save-failed")
    }
    fn begin_dav(&mut self, selected: Selection) {
        let candidate = match &selected {
            Selection::Dav { candidate, .. } => candidate.as_ref(),
            _ => None,
        };
        self.dav_fields[0].set(candidate.and_then(|c| c.server.clone()).unwrap_or_default());
        self.dav_fields[1].set(
            candidate
                .and_then(|c| c.identity.clone())
                .unwrap_or_default(),
        );
        self.dav_fields[2].set(String::new());
        self.step = Step::Dav { selected, field: 0 };
    }
    pub fn set_dav_field(
        &mut self,
        field: &str,
        value: zeroize::Zeroizing<String>,
    ) -> Result<(), &'static str> {
        if let Step::ICloud(flow) = &mut self.step {
            return flow.edit(field, value);
        }
        if field == "oauth-code" && matches!(self.step, Step::PastedCode(_)) && value.len() <= 4096
        {
            self.dav_fields[2].set(value.to_string());
            return Ok(());
        }
        let index = match field {
            "server" => 0,
            "login" => 1,
            "password" => 2,
            _ => return Err("installer-accounts-invalid-action"),
        };
        if !matches!(self.step, Step::Dav { field, .. } if field == index) || value.len() > 4096 {
            return Err("installer-accounts-invalid-action");
        }
        self.dav_fields[index].set(value.to_string());
        Ok(())
    }
    pub fn apply(&mut self, request: AccountRequest) -> Result<(), &'static str> {
        if self.checkpoint_worker.is_some() {
            return Err("installer-accounts-busy");
        }

        if self.apply_saved_request(&request)? {
            return Ok(());
        }
        if let Step::ICloud(flow) = &mut self.step {
            match request {
                AccountRequest::Confirm => return flow.confirm(),
                AccountRequest::Back => {
                    if flow.back() {
                        self.step = Step::Overview;
                    }
                    return Ok(());
                }
                AccountRequest::Cancel | AccountRequest::Overview => {
                    flow.cancel();
                    if !flow.active() {
                        self.step = Step::Overview;
                    }
                    return Ok(());
                }
                _ => return Err("installer-accounts-invalid-action"),
            }
        }
        match request {
            AccountRequest::Remove(id) if matches!(self.step, Step::Overview | Step::Done(_)) => {
                let entry = self
                    .prepared
                    .iter()
                    .find(|entry| entry.id == id)
                    .cloned()
                    .ok_or("installer-accounts-invalid-action")?;
                self.step = Step::ConfirmRemoval(entry);
            }
            AccountRequest::Confirm if matches!(self.step, Step::ConfirmRemoval(_)) => {
                let Step::ConfirmRemoval(entry) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                let entry = entry.clone();
                self.remove_prepared(Selection::Prepared(entry.clone()), entry.id);
            }
            AccountRequest::Source(id) if matches!(self.step, Step::Overview) => {
                if self
                    .prepared
                    .iter()
                    .any(|entry| entry.source_id.as_deref() == Some(id.as_str()))
                {
                    return Err("installer-accounts-already-included");
                }
                if let Some(candidate) = self
                    .inventory
                    .tilo_accounts
                    .iter()
                    .find(|c| c.source_id == id)
                {
                    self.step = Step::Confirm(Selection::Tilo(candidate.clone()));
                    return Ok(());
                }
                let candidate = self
                    .inventory
                    .candidates
                    .iter()
                    .find(|c| c.id == id)
                    .cloned()
                    .ok_or("installer-accounts-invalid-action")?;
                if candidate.provider == Provider::Spotify && !candidate.can_import_credentials {
                    if !self.spotify_connection_available() {
                        return Err("installer-accounts-unavailable");
                    }
                    self.step = Step::SpotifyPurpose(Some(candidate));
                    return Ok(());
                }
                let selected = if candidate.can_import_credentials {
                    Selection::Saved(candidate)
                } else if matches!(candidate.provider, Provider::Nextcloud | Provider::WebDav) {
                    Selection::Dav {
                        nextcloud: candidate.provider == Provider::Nextcloud,
                        candidate: Some(candidate),
                    }
                } else {
                    let provider =
                        candidate_provider(&candidate).ok_or("installer-accounts-profile-only")?;
                    self.require_connection(provider)?;
                    Selection::OAuth {
                        provider,
                        candidate: Some(candidate),
                    }
                };
                if matches!(selected, Selection::Dav { .. }) {
                    self.begin_dav(selected);
                } else {
                    self.step = Step::Confirm(selected);
                }
            }
            AccountRequest::AddAccount if matches!(self.step, Step::Overview | Step::Done(_)) => {
                self.step = Step::Providers;
            }
            AccountRequest::Provider(id)
                if matches!(
                    self.step,
                    Step::Overview | Step::Providers | Step::SpotifyPurpose(_)
                ) =>
            {
                let spotify_candidate = match &self.step {
                    Step::SpotifyPurpose(candidate) => {
                        if !matches!(id.as_str(), "spotify" | "spotify-streaming") {
                            return Err("installer-accounts-invalid-action");
                        }
                        candidate.clone()
                    }
                    _ => None,
                };
                if id == "spotify-options" {
                    if !self
                        .actions
                        .connection_available(ConnectionProvider::Spotify)
                        && !self
                            .actions
                            .connection_available(ConnectionProvider::SpotifyStreaming)
                    {
                        return Err("installer-accounts-unavailable");
                    }
                    self.step = Step::SpotifyPurpose(None);
                    return Ok(());
                }
                if id == "icloud" {
                    let home = self.stage_home()?;
                    let connector = self
                        .actions
                        .connector(&home)
                        .map_err(|_| "installer-accounts-save-failed")?;
                    if !connector.icloud_available() {
                        return Err("installer-accounts-unavailable");
                    }
                    self.step = Step::ICloud(Box::new(icloud::ICloud::new(
                        connector,
                        self.stage.as_ref().unwrap().clone(),
                    )));
                    return Ok(());
                }
                if matches!(id.as_str(), "webdav" | "nextcloud") {
                    self.begin_dav(Selection::Dav {
                        nextcloud: id == "nextcloud",
                        candidate: None,
                    });
                    return Ok(());
                }
                let provider =
                    ConnectionProvider::from_id(&id).ok_or("installer-accounts-invalid-action")?;
                self.require_connection(provider)?;
                self.step = Step::Confirm(Selection::OAuth {
                    provider,
                    candidate: spotify_candidate,
                });
            }
            AccountRequest::Confirm if matches!(self.step, Step::Dav { .. }) => {
                let Step::Dav { selected, field } = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                let selected = selected.clone();
                let field = *field;
                let value = self.dav_fields[field].get();
                if value.is_empty() || value.len() > 4096 {
                    return Err("installer-accounts-dav-required");
                }
                if field == 1 && value.len() > 256 {
                    return Err("installer-accounts-dav-required");
                }
                if field == 0 {
                    let server = imager_backend::account_dav_readiness::normalize_server(&value)
                        .map_err(|_| "installer-accounts-dav-server-invalid")?;
                    self.dav_fields[0].set(server);
                }
                if field == 0
                    && matches!(
                        selected,
                        Selection::Dav {
                            nextcloud: true,
                            ..
                        }
                    )
                {
                    self.step = Step::NextcloudMethod(selected);
                } else if field < 2 {
                    self.step = Step::Dav {
                        selected,
                        field: field + 1,
                    };
                } else {
                    self.start(selected);
                }
            }
            AccountRequest::NextcloudBrowser if matches!(self.step, Step::NextcloudMethod(_)) => {
                let Step::NextcloudMethod(Selection::Dav { candidate, .. }) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                self.start(Selection::NextcloudBrowser {
                    candidate: candidate.clone(),
                });
            }
            AccountRequest::NextcloudAppPassword
                if matches!(self.step, Step::NextcloudMethod(_)) =>
            {
                let Step::NextcloudMethod(selected) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                self.step = Step::Dav {
                    selected: selected.clone(),
                    field: 1,
                };
            }
            AccountRequest::Back if matches!(self.step, Step::NextcloudMethod(_)) => {
                let Step::NextcloudMethod(selected) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                self.step = Step::Dav {
                    selected: selected.clone(),
                    field: 0,
                };
            }
            AccountRequest::Confirm if matches!(self.step, Step::Confirm(_)) => {
                let Step::Confirm(selected) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                if let Selection::OAuth { provider, .. } = selected {
                    self.require_connection(*provider)?;
                }
                if matches!(selected, Selection::Saved(_) | Selection::Tilo(_)) {
                    self.saved.reviewed = true;
                }
                self.start(selected.clone());
            }
            AccountRequest::Reconnect if matches!(self.step, Step::Failed { .. }) => {
                let Step::Failed {
                    selected, message, ..
                } = &self.step
                else {
                    return Err("installer-accounts-invalid-action");
                };
                if *message == "installer-accounts-tilo-reconnect" {
                    if !matches!(selected, Selection::Tilo(_)) {
                        return Err("installer-accounts-invalid-action");
                    }
                    self.step = Step::Providers;
                } else if *message == "installer-accounts-nextcloud-source-reconnect" {
                    let Selection::Saved(candidate) = selected else {
                        return Err("installer-accounts-invalid-action");
                    };
                    self.begin_dav(Selection::Dav {
                        nextcloud: true,
                        candidate: Some(candidate.clone()),
                    });
                } else if matches!(
                    *message,
                    "installer-accounts-rclone-source-reconnect"
                        | "installer-accounts-goa-source-reconnect"
                ) {
                    let provider = saved_cloud_provider(selected)
                        .ok_or("installer-accounts-invalid-action")?;
                    self.require_connection(provider)?;
                    let Selection::Saved(candidate) = selected else {
                        return Err("installer-accounts-invalid-action");
                    };
                    self.step = Step::Confirm(Selection::OAuth {
                        provider,
                        candidate: Some(candidate.clone()),
                    });
                } else {
                    return Err("installer-accounts-invalid-action");
                }
            }
            AccountRequest::Retry if matches!(self.step, Step::Failed { .. }) => {
                let Step::Failed {
                    selected,
                    retry_cleanup,
                    ..
                } = &self.step
                else {
                    return Err("installer-accounts-invalid-action");
                };
                if matches!(&self.step, Step::Failed { message, .. }
                    if matches!(*message, "installer-accounts-nextcloud-source-reconnect" | "installer-accounts-rclone-source-reconnect" | "installer-accounts-goa-source-reconnect" | "installer-accounts-tilo-reconnect"))
                {
                    return Err("installer-accounts-invalid-action");
                }
                let selected = selected.clone();
                if matches!(selected, Selection::Dav { .. }) && retry_cleanup.is_none() {
                    self.step = Step::Dav { selected, field: 2 };
                    return Ok(());
                }
                if let Some(account_id) = retry_cleanup.clone() {
                    self.remove_prepared(selected, account_id);
                } else {
                    self.start(selected);
                }
            }
            AccountRequest::Back if matches!(self.step, Step::Dav { .. }) => {
                let Step::Dav { selected, field } = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                let selected = selected.clone();
                let field = *field;
                self.dav_fields[2].set(String::new());
                self.step = if field == 0 {
                    if selected.source_id().is_some() {
                        Step::Overview
                    } else {
                        Step::Providers
                    }
                } else {
                    Step::Dav {
                        selected,
                        field: field - 1,
                    }
                };
            }
            AccountRequest::Back if matches!(self.step, Step::Working(_)) => {
                let Step::Working(work) = &mut self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                if work.sign_in {
                    self.actions.cancel_job(&work.job);
                    work.cancelling = true;
                } else {
                    work.return_overview = true;
                }
            }
            AccountRequest::SubmitCode if matches!(self.step, Step::PastedCode(_)) => {
                let code = zeroize::Zeroizing::new(self.dav_fields[2].get());
                let Step::PastedCode(work) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                let job = self
                    .actions
                    .complete_with_code(&work.job, code)
                    .map_err(|_| "installer-accounts-code-invalid")?;
                self.dav_fields[2].set(String::new());
                let Step::PastedCode(mut work) = std::mem::replace(&mut self.step, Step::Overview)
                else {
                    return Err("installer-accounts-invalid-action");
                };
                work.job = job;
                work.needs_code = false;
                self.step = Step::Working(work);
            }
            AccountRequest::Back | AccountRequest::Cancel
                if matches!(self.step, Step::PastedCode(_)) =>
            {
                let Step::PastedCode(work) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                self.actions.cancel(&work.job);
                self.dav_fields[2].set(String::new());
                self.step = Step::Overview;
            }
            AccountRequest::Back if matches!(self.step, Step::Removing { .. }) => {}
            AccountRequest::Back if matches!(self.step, Step::SpotifyPurpose(_)) => {
                self.step = if matches!(self.step, Step::SpotifyPurpose(Some(_))) {
                    Step::Overview
                } else {
                    Step::Providers
                };
            }
            AccountRequest::Back
                if matches!(
                    self.step,
                    Step::Confirm(Selection::OAuth {
                        provider: ConnectionProvider::Spotify
                            | ConnectionProvider::SpotifyStreaming,
                        ..
                    })
                ) =>
            {
                let Step::Confirm(Selection::OAuth { candidate, .. }) = &self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                self.step = Step::SpotifyPurpose(candidate.clone());
            }
            AccountRequest::Back
                if matches!(
                    self.step,
                    Step::Confirm(Selection::OAuth {
                        candidate: None,
                        ..
                    })
                ) =>
            {
                self.step = Step::Providers;
            }
            AccountRequest::Back | AccountRequest::Overview if !self.busy() => {
                self.dav_fields[2].set(String::new());
                self.step = Step::Overview
            }
            AccountRequest::Cancel => {
                let Step::Working(work) = &mut self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                if work.sign_in {
                    self.actions.cancel_job(&work.job);
                    work.cancelling = true;
                } else {
                    return Err("installer-accounts-busy");
                }
            }
            AccountRequest::OpenAgain => {
                let (Step::Working(work) | Step::PastedCode(work)) = &mut self.step else {
                    return Err("installer-accounts-invalid-action");
                };
                if work.cancelling {
                    return Err("installer-accounts-busy");
                }
                let url = work
                    .url
                    .as_ref()
                    .ok_or("installer-accounts-invalid-action")?;
                work.launch = Some(self.opener.open(url));
                work.browser_failed = false;
            }
            _ => return Err("installer-accounts-invalid-action"),
        }
        Ok(())
    }
    fn start(&mut self, selected: Selection) {
        let outcome = self.start_work(&selected);
        self.step = match outcome {
            Ok(work) if work.needs_code => {
                self.dav_fields[2].set(String::new());
                Step::PastedCode(work)
            }
            Ok(work) => Step::Working(work),
            Err(message) => Step::Failed {
                selected,
                message,
                retry_cleanup: None,
            },
        };
    }
    fn start_work(&mut self, selected: &Selection) -> Result<Work, &'static str> {
        let home = self.stage_home()?;
        match selected {
            Selection::ICloud | Selection::Prepared(_) => Err("installer-accounts-invalid-action"),
            Selection::Tilo(candidate) => {
                let job = self
                    .actions
                    .import_reviewed_tilo(&candidate.source_id, &home)
                    .map_err(|_| "installer-accounts-import-failed")?;
                Ok(Work {
                    selected: selected.clone(),
                    job,
                    needs_code: false,
                    url: None,
                    launch: None,
                    cancelling: false,
                    browser_failed: false,
                    sign_in: false,
                    return_overview: false,
                })
            }
            Selection::Saved(candidate) => {
                let job = self
                    .actions
                    .import_selected(&candidate.id, &home)
                    .map_err(|_| "installer-accounts-import-failed")?;
                Ok(Work {
                    selected: selected.clone(),
                    job,
                    needs_code: false,
                    url: None,
                    launch: None,
                    cancelling: false,
                    browser_failed: false,
                    sign_in: false,
                    return_overview: false,
                })
            }
            Selection::NextcloudBrowser { .. } => {
                let job = self
                    .actions
                    .connect_nextcloud_browser(&home, &self.dav_fields[0].get(), &selected.label())
                    .map_err(|_| "installer-accounts-connect-failed")?;
                Ok(Work {
                    selected: selected.clone(),
                    job,
                    needs_code: false,
                    url: None,
                    launch: None,
                    cancelling: false,
                    browser_failed: false,
                    sign_in: true,
                    return_overview: false,
                })
            }
            Selection::Dav { nextcloud, .. } => {
                let password = zeroize::Zeroizing::new(self.dav_fields[2].get());
                self.dav_fields[2].set(String::new());
                let job = self
                    .actions
                    .connect_dav(
                        &home,
                        DavConnection {
                            nextcloud: *nextcloud,
                            label: selected.label(),
                            server: self.dav_fields[0].get(),
                            username: self.dav_fields[1].get(),
                            password,
                        },
                    )
                    .map_err(|_| "installer-accounts-save-failed")?;
                Ok(Work {
                    selected: selected.clone(),
                    job,
                    needs_code: false,
                    url: None,
                    launch: None,
                    cancelling: false,
                    browser_failed: false,
                    sign_in: false,
                    return_overview: false,
                })
            }
            Selection::OAuth {
                provider,
                candidate,
            } => self.start_oauth(selected, *provider, candidate.as_ref(), &home),
        }
    }
    fn start_oauth(
        &mut self,
        selected: &Selection,
        provider: ConnectionProvider,
        candidate: Option<&Candidate>,
        home: &Path,
    ) -> Result<Work, &'static str> {
        if !self.actions.connection_available(provider) {
            return Err("installer-accounts-unavailable");
        }
        let link = self
            .actions
            .begin(provider, candidate.map(|c| c.id.as_str()), home)
            .map_err(|_| "installer-accounts-connect-failed")?;
        let launch = self.opener.open(&link.url);
        let job = if link.needs_pasted_code {
            link.connection_id.clone()
        } else {
            let Ok(job) = self.actions.complete(&link.connection_id) else {
                self.actions.cancel(&link.connection_id);
                return Err("installer-accounts-connect-failed");
            };
            job
        };
        Ok(Work {
            selected: selected.clone(),
            job,
            needs_code: link.needs_pasted_code,
            url: Some(link.url),
            launch: Some(launch),
            cancelling: false,
            browser_failed: false,
            sign_in: true,
            return_overview: false,
        })
    }
    pub fn poll(&mut self) -> bool {
        if self.poll_saved_queue() {
            return true;
        }
        if let Step::ICloud(flow) = &mut self.step {
            return match flow.poll() {
                None => false,
                Some(Err(())) => {
                    self.step = Step::Overview;
                    true
                }
                Some(Ok(None)) => true,
                Some(Ok(Some(id))) => {
                    self.prepared.push(PreparedEntry {
                        id: id.as_str().to_owned(),
                        source_id: None,
                        label: t("installer-accounts-icloud"),
                        revoke_on_remove: true,
                    });
                    self.step = Step::Done(Selection::ICloud);
                    true
                }
            };
        }
        if let Step::Removing {
            selected,
            job,
            account_id,
        } = &self.step
        {
            let Some(receipt) = self
                .actions
                .status()
                .into_iter()
                .find(|receipt| receipt.id == *job)
            else {
                return false;
            };
            match receipt.state {
                AccountJobState::Running => return false,
                AccountJobState::Completed { .. } => {
                    self.actions.acknowledge_removed(account_id);
                    self.prepared.retain(|entry| entry.id != *account_id);
                    self.reviewed_empty = self.prepared.is_empty();
                    self.step = Step::Overview;
                }
                AccountJobState::Failed { .. } => {
                    self.step = Step::Failed {
                        selected: selected.clone(),
                        message: "installer-accounts-remove-failed",
                        retry_cleanup: Some(account_id.clone()),
                    }
                }
            }
            return true;
        }
        let Step::Working(work) = &mut self.step else {
            return false;
        };
        let mut changed = false;
        if work.sign_in
            && work.url.is_none()
            && let Some(url) = self.actions.take_nextcloud_url(&work.job)
        {
            if !work.cancelling {
                work.launch = Some(self.opener.open(&url));
            }
            work.url = Some(url);
            changed = true;
        }
        if let Some(launch) = &work.launch {
            match launch.try_recv() {
                Ok(ui::portal::LaunchOutcome::Opened) => {
                    work.launch = None;
                    changed = true;
                }
                Ok(ui::portal::LaunchOutcome::Failed(_))
                | Err(mpsc::TryRecvError::Disconnected) => {
                    work.launch = None;
                    work.browser_failed = true;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let Some(job) = self
            .actions
            .status()
            .into_iter()
            .find(|job| job.id == work.job)
        else {
            self.step = Step::Failed {
                selected: work.selected.clone(),
                message: "installer-accounts-connect-failed",
                retry_cleanup: None,
            };
            self.collect_saved_failure();
            return true;
        };
        match job.state {
            AccountJobState::Running => changed,
            AccountJobState::Failed { reason } => {
                self.step = if work.cancelling {
                    Step::Overview
                } else {
                    Step::Failed {
                        selected: work.selected.clone(),
                        message: if matches!(work.selected, Selection::Tilo(_))
                            && reason == ConnectionError::CredentialUnavailable
                        {
                            "installer-accounts-tilo-reconnect"
                        } else if matches!(work.selected, Selection::Tilo(_))
                            && reason == ConnectionError::AccountMissing
                        {
                            "installer-accounts-tilo-changed"
                        } else if matches!(&work.selected, Selection::Saved(candidate)
                            if candidate.origin == super::super::account_transfer::Origin::Nextcloud)
                            && reason == ConnectionError::CredentialUnavailable
                        {
                            "installer-accounts-nextcloud-source-reconnect"
                        } else if saved_cloud_provider(&work.selected).is_some()
                            && matches!(
                                reason,
                                ConnectionError::CredentialUnavailable
                                    | ConnectionError::SignInFailed
                                    | ConnectionError::SignInUnavailable
                            )
                        {
                            if matches!(&work.selected, Selection::Saved(candidate) if candidate.origin == super::super::account_transfer::Origin::Gnome)
                            {
                                "installer-accounts-goa-source-reconnect"
                            } else {
                                "installer-accounts-rclone-source-reconnect"
                            }
                        } else {
                            connection_message(reason)
                        },
                        retry_cleanup: None,
                    }
                };
                self.collect_saved_failure();
                true
            }
            AccountJobState::Completed { account_id } => {
                if work.cancelling {
                    let selected = work.selected.clone();
                    self.remove_prepared(selected, account_id);
                } else {
                    if let Some(id) = work.selected.source_id() {
                        self.saved.attention.remove(id);
                    }
                    if !self.prepared.iter().any(|entry| entry.id == account_id) {
                        self.prepared.push(PreparedEntry {
                            id: account_id,
                            source_id: work.selected.source_id().map(str::to_owned),
                            label: work.selected.label(),
                            revoke_on_remove: matches!(
                                work.selected,
                                Selection::OAuth { .. } | Selection::NextcloudBrowser { .. }
                            ),
                        });
                    }
                    self.actions.acknowledge_prepared(&work.job);
                    self.step = if self.saved.running() || work.return_overview {
                        Step::Overview
                    } else {
                        Step::Done(work.selected.clone())
                    };
                }
                true
            }
        }
    }
    fn remove_prepared(&mut self, selected: Selection, account_id: String) {
        let outcome = self
            .stage
            .as_ref()
            .ok_or("installer-accounts-save-failed")
            .and_then(|stage| {
                let result = if matches!(&selected, Selection::Prepared(entry) if !entry.revoke_on_remove) {
                    self.actions.forget_prepared(&account_id, stage.path())
                } else {
                    self.actions.disconnect_prepared(&account_id, stage.path())
                };
                result.map_err(|_| "installer-accounts-remove-failed")
            });
        self.step = match outcome {
            Ok(job) => Step::Removing {
                selected,
                job,
                account_id,
            },
            Err(message) => Step::Failed {
                selected,
                message,
                retry_cleanup: Some(account_id),
            },
        };
    }

    fn spotify_connection_available(&self) -> bool {
        self.actions
            .connection_available(ConnectionProvider::Spotify)
            || self
                .actions
                .connection_available(ConnectionProvider::SpotifyStreaming)
    }

    fn require_connection(&self, provider: ConnectionProvider) -> Result<(), &'static str> {
        if self.actions.connection_available(provider) {
            Ok(())
        } else {
            Err("installer-accounts-unavailable")
        }
    }

    pub fn heading(&self) -> String {
        if self.saved.running() {
            return t("installer-accounts-bulk-heading");
        }
        match &self.step {
            Step::Overview => t("installer-accounts-heading"),
            Step::ReviewSaved => t("installer-accounts-review-saved-heading"),
            Step::Providers => t("installer-accounts-provider-heading"),
            Step::SpotifyPurpose(_) => t("installer-accounts-spotify-purpose-heading"),
            Step::ICloud(flow) => flow.heading(),
            Step::ConfirmRemoval(entry) => {
                service_copy("installer-accounts-remove-heading", &entry.label)
            }
            Step::NextcloudMethod(_) => t("installer-accounts-nextcloud-method-heading"),
            Step::Dav { field, .. } => t([
                "installer-accounts-dav-server-heading",
                "installer-accounts-dav-login-heading",
                "installer-accounts-dav-password-heading",
            ][*field]),
            Step::Confirm(Selection::Saved(_) | Selection::Tilo(_)) => {
                t("installer-accounts-import-heading")
            }
            Step::Confirm(selected) => {
                service_copy("installer-accounts-connect-heading", &selected.label())
            }
            Step::PastedCode(_) => t("installer-accounts-code-heading"),
            Step::Working(work) if work.cancelling => t("installer-accounts-cancelling-heading"),
            Step::Working(work) if work.sign_in && work.url.is_none() => {
                t("installer-accounts-starting-heading")
            }
            Step::Working(work) if work.url.is_none() => t("installer-accounts-importing-heading"),
            Step::Working(_) => t("installer-accounts-waiting-heading"),
            Step::Removing { .. } => t("installer-accounts-cancelling-heading"),
            Step::Done(Selection::Saved(_) | Selection::Tilo(_)) => {
                t("installer-accounts-import-done-heading")
            }
            Step::Done(Selection::Dav { .. }) => t("installer-accounts-dav-saved-heading"),
            Step::Done(Selection::OAuth {
                provider: ConnectionProvider::Spotify,
                ..
            }) => t("installer-accounts-spotify-done-heading"),
            Step::Done(Selection::OAuth {
                provider: ConnectionProvider::SpotifyStreaming,
                ..
            }) => t("installer-accounts-spotify-playback-done-heading"),
            Step::Done(_) => t("installer-accounts-done-heading"),
            Step::Failed { .. } => t("installer-accounts-failed-heading"),
        }
    }
    pub fn subheading(&self) -> String {
        if self.saved.running() {
            return t("installer-accounts-bulk-intro");
        }
        match &self.step {
            Step::Overview => t("installer-accounts-intro"),
            Step::ReviewSaved => t("installer-accounts-review-saved-intro"),
            Step::Providers | Step::SpotifyPurpose(None) => String::new(),
            Step::SpotifyPurpose(Some(_)) => t("installer-accounts-spotify-source-reconnect"),
            Step::ICloud(flow) => flow.intro(),
            Step::ConfirmRemoval(_) => t("installer-accounts-remove-intro"),
            Step::NextcloudMethod(_) => t("installer-accounts-nextcloud-method-intro"),
            Step::Dav { field, .. } => t([
                "installer-accounts-dav-server-intro",
                "installer-accounts-dav-login-intro",
                "installer-accounts-dav-password-intro",
            ][*field]),
            Step::Confirm(Selection::Tilo(_)) => t("installer-accounts-tilo-import-intro"),
            Step::Confirm(Selection::Saved(_)) => t("installer-accounts-import-intro"),
            Step::Confirm(Selection::OAuth {
                provider: ConnectionProvider::Spotify,
                ..
            }) => t("installer-accounts-spotify-intro"),
            Step::Confirm(Selection::OAuth {
                provider: ConnectionProvider::SpotifyStreaming,
                ..
            }) => t("installer-accounts-spotify-playback-intro"),
            Step::Confirm(_) => t("installer-accounts-cloud-intro"),
            Step::PastedCode(_) => t("installer-accounts-code-intro"),
            Step::Working(work) if work.cancelling => t("installer-accounts-cancelling-intro"),
            Step::Working(work) if work.sign_in && work.url.is_none() => {
                t("installer-accounts-starting-intro")
            }
            Step::Working(work) if work.url.is_none() => String::new(),
            Step::Working(_) => t("installer-accounts-waiting-intro"),
            Step::Removing { .. } => t("installer-accounts-cancelling-intro"),
            Step::Done(Selection::Saved(_) | Selection::Tilo(_)) => {
                t("installer-accounts-import-done-intro")
            }
            Step::Done(Selection::Dav { .. }) => t("installer-accounts-dav-saved-intro"),
            Step::Done(Selection::OAuth {
                provider: ConnectionProvider::Spotify,
                ..
            }) => t("installer-accounts-spotify-done-intro"),
            Step::Done(Selection::OAuth {
                provider: ConnectionProvider::SpotifyStreaming,
                ..
            }) => t("installer-accounts-spotify-playback-done-intro"),
            Step::Done(selected) => {
                service_copy("installer-accounts-done-intro", &selected.label())
            }
            Step::Failed { message, .. } => t(message),
        }
    }
    pub fn navigation_action(&self) -> Option<(&'static str, &'static str, AccountRequest)> {
        if let Step::Dav { field, .. } = self.step {
            return Some((
                "installer.account.dav.next",
                if field == 2 {
                    "installer-accounts-dav-save"
                } else {
                    "installer-accounts-continue"
                },
                AccountRequest::Confirm,
            ));
        }
        if matches!(self.step, Step::Overview)
            && !self.saved.reviewed
            && self.saved.require_review
            && !self.pending_saved().is_empty()
        {
            return Some((
                "installer.account.review-saved",
                "installer-accounts-review-saved",
                AccountRequest::ReviewSaved,
            ));
        }
        if matches!(self.step, Step::ReviewSaved)
            || (matches!(self.step, Step::Overview)
                && !self.saved.reviewed
                && !self.pending_saved().is_empty())
        {
            return Some((
                "installer.account.import-saved",
                "installer-accounts-import-saved",
                AccountRequest::ImportSaved,
            ));
        }
        let Step::Confirm(selected) = &self.step else {
            return None;
        };
        Some((
            "installer.account.confirm",
            if matches!(selected, Selection::Saved(_) | Selection::Tilo(_)) {
                "installer-accounts-import"
            } else {
                "installer-accounts-connect"
            },
            AccountRequest::Confirm,
        ))
    }

    // Native client configuration identifies an account, not the state of its sync queue.
    // Keep this advice separate from authentication and migration destination choices.
    fn source_review_keys(&self) -> Vec<&'static str> {
        {
            use super::super::account_transfer::Origin;
            let native = |candidate: &Candidate| candidate.origin == Origin::Nextcloud;
            let selected_native = |selection: &Selection| match selection {
                Selection::Saved(candidate)
                | Selection::NextcloudBrowser {
                    candidate: Some(candidate),
                }
                | Selection::Dav {
                    candidate: Some(candidate),
                    ..
                } => native(candidate),
                _ => false,
            };
            let detailed = match &self.step {
                Step::ReviewSaved => self.inventory.candidates.iter().any(native),
                Step::NextcloudMethod(selected)
                | Step::Confirm(selected)
                | Step::Dav { selected, .. }
                | Step::Done(selected)
                | Step::Failed { selected, .. } => selected_native(selected),
                _ => false,
            };
            if detailed {
                return vec![
                    "installer-accounts-nextcloud-linux-files",
                    "installer-accounts-nextcloud-linux-local-changes",
                    "installer-accounts-nextcloud-linux-dual-boot",
                    "installer-accounts-nextcloud-linux-calendar",
                ];
            }
            if matches!(self.step, Step::Overview) && self.inventory.candidates.iter().any(native) {
                return vec!["installer-accounts-nextcloud-linux-summary"];
            }
        }
        Vec::new()
    }

    pub fn view(
        &self,
        dispatch: Rc<dyn Fn(AccountRequest)>,
        edit: Rc<dyn Fn(&str, String)>,
    ) -> Container {
        let mut body = page::stack().with_rhythm(Rhythm::Sibling);
        for key in self.source_review_keys() {
            body = body.push(Box::new(Text::new(&t(key))));
        }
        match &self.step {
            Step::ICloud(flow) => {
                body = body.push(flow.view(dispatch.clone(), edit));
            }
            Step::ConfirmRemoval(_) => {
                body = body
                    .push(Box::new(
                        action_button(
                            "installer.account.remove.confirm",
                            "installer-accounts-remove-confirm",
                            AccountRequest::Confirm,
                            dispatch.clone(),
                        )
                        .role(ButtonRole::Primary),
                    ))
                    .push(Box::new(action_button(
                        "installer.account.remove.keep",
                        "installer-accounts-remove-keep",
                        AccountRequest::Back,
                        dispatch.clone(),
                    )));
            }
            Step::ReviewSaved => {
                body = self.saved_review_view(body, dispatch.clone());
            }
            Step::Overview if self.saved.running() => {
                body = body.push(Box::new(Text::new(&t("installer-accounts-bulk-working"))));
            }
            Step::Overview => {
                body = self.saved_overview_view(body, dispatch.clone());
                for entry in &self.prepared {
                    let choose = dispatch.clone();
                    let id = entry.id.clone();
                    body = body.push(Box::new(
                        ListRow::new(
                            format!("installer.account.ready.{}", entry.id),
                            &entry.label,
                        )
                        .secondary(t("installer-accounts-included"))
                        .icon(Icon::CircleCheck)
                        .on_activate(move || choose(AccountRequest::Remove(id.clone()))),
                    ));
                }
                for candidate in self.inventory.candidates.iter().filter(|c| {
                    !self.saved.attention.contains_key(&c.id)
                        && !c.can_import_credentials
                        && can_connect_candidate(c)
                        && !self
                            .prepared
                            .iter()
                            .any(|entry| entry.source_id.as_deref() == Some(c.id.as_str()))
                }) {
                    let available = if candidate.provider == Provider::Spotify {
                        self.spotify_connection_available()
                    } else {
                        candidate_provider(candidate)
                            .is_none_or(|provider| self.actions.connection_available(provider))
                    };
                    let message = if !available {
                        "installer-accounts-unavailable"
                    } else if candidate.can_import_credentials {
                        "installer-accounts-saved"
                    } else if candidate.provider == Provider::Spotify {
                        "installer-accounts-spotify-source-sign-in"
                    } else if candidate.provider == Provider::Nextcloud {
                        "installer-accounts-nextcloud-add"
                    } else if candidate.provider == Provider::WebDav {
                        "installer-accounts-app-password-needed"
                    } else {
                        "installer-accounts-sign-in"
                    };
                    let choose = dispatch.clone();
                    let id = candidate.id.clone();
                    body = body.push(Box::new(
                        ListRow::new(
                            format!("installer.account.source.{}", candidate.id),
                            candidate_label(candidate),
                        )
                        .secondary(t(message))
                        .with_enabled(available)
                        .icon(Icon::Cloud)
                        .on_activate(move || choose(AccountRequest::Source(id.clone()))),
                    ));
                }
                body = body.push(Box::new(action_button(
                    "installer.account.add",
                    if self.prepared.is_empty() {
                        "installer-accounts-add"
                    } else {
                        "installer-accounts-another"
                    },
                    AccountRequest::AddAccount,
                    dispatch.clone(),
                )));
            }
            Step::Providers => {
                for (id, provider) in [
                    ("google-drive", ConnectionProvider::GoogleDrive),
                    ("onedrive", ConnectionProvider::OneDrive),
                    ("dropbox", ConnectionProvider::Dropbox),
                ] {
                    if !self.actions.connection_available(provider) {
                        continue;
                    }
                    let choose = dispatch.clone();
                    body = body.push(Box::new(
                        ListRow::new(
                            format!("installer.account.provider.{id}"),
                            provider_label(provider),
                        )
                        .icon(Icon::Cloud)
                        .secondary(t("installer-accounts-add"))
                        .on_activate(move || choose(AccountRequest::Provider(id.into()))),
                    ));
                }
                if self
                    .actions
                    .connection_available(ConnectionProvider::Spotify)
                    || self
                        .actions
                        .connection_available(ConnectionProvider::SpotifyStreaming)
                {
                    let choose = dispatch.clone();
                    body = body.push(Box::new(
                        ListRow::new(
                            "installer.account.provider.spotify",
                            t("installer-accounts-spotify"),
                        )
                        .icon(Icon::Music)
                        .secondary(t("installer-accounts-add"))
                        .on_activate(move || {
                            choose(AccountRequest::Provider("spotify-options".into()))
                        }),
                    ));
                }
                for (id, label) in [
                    ("nextcloud", "installer-accounts-nextcloud"),
                    ("webdav", "installer-accounts-webdav"),
                    ("icloud", "installer-accounts-icloud"),
                ] {
                    let choose = dispatch.clone();
                    body = body.push(Box::new(
                        ListRow::new(format!("installer.account.provider.{id}"), t(label))
                            .icon(Icon::Cloud)
                            .secondary(t(if id == "nextcloud" {
                                "installer-accounts-nextcloud-add"
                            } else if id == "icloud" {
                                "installer-accounts-icloud-add"
                            } else {
                                "installer-accounts-app-password-needed"
                            }))
                            .on_activate(move || choose(AccountRequest::Provider(id.to_string()))),
                    ));
                }
            }
            Step::NextcloudMethod(_) => {
                body = body.push(Box::new(
                    action_button(
                        "installer.account.nextcloud.browser",
                        "installer-accounts-nextcloud-browser",
                        AccountRequest::NextcloudBrowser,
                        dispatch.clone(),
                    )
                    .role(ButtonRole::Primary),
                ));
                body = body.push(Box::new(
                    action_button(
                        "installer.account.nextcloud.app-password",
                        "installer-accounts-nextcloud-app-password",
                        AccountRequest::NextcloudAppPassword,
                        dispatch.clone(),
                    )
                    .role(ButtonRole::Plain),
                ));
            }
            Step::Dav { field, .. } => {
                let submit = dispatch.clone();
                let (key, label, kind) = match *field {
                    0 => (
                        "server",
                        "installer-accounts-dav-server-label",
                        TextInputKind::Url,
                    ),
                    1 => (
                        "login",
                        "installer-accounts-dav-login-label",
                        TextInputKind::Username,
                    ),
                    _ => (
                        "password",
                        "installer-accounts-dav-password-label",
                        TextInputKind::Password,
                    ),
                };
                body = body.push(Box::new(
                    Field::new(
                        t(label),
                        TextInput::from_state(&self.dav_fields[*field])
                            .with_id(&format!("installer.account.dav.{key}"))
                            .with_kind(kind)
                            .with_secret(*field == 2)
                            .on_change(move |value| edit(key, value.to_string()))
                            .on_submit(move |_| submit(AccountRequest::Confirm)),
                    )
                    .with_label_placement(Some(LabelPlacement::Above)),
                ));
            }
            Step::SpotifyPurpose(_) => {
                for (id, provider, label, detail) in [
                    (
                        "spotify",
                        ConnectionProvider::Spotify,
                        "installer-accounts-spotify-library-choice",
                        "installer-accounts-spotify-library-choice-intro",
                    ),
                    (
                        "spotify-streaming",
                        ConnectionProvider::SpotifyStreaming,
                        "installer-accounts-spotify-playback-choice",
                        "installer-accounts-spotify-playback-choice-intro",
                    ),
                ] {
                    if !self.actions.connection_available(provider) {
                        continue;
                    }
                    let choose = dispatch.clone();
                    body = body.push(Box::new(
                        ListRow::new(format!("installer.account.spotify.{id}"), t(label))
                            .icon(Icon::Music)
                            .secondary(t(detail))
                            .on_activate(move || choose(AccountRequest::Provider(id.into()))),
                    ));
                }
            }
            Step::Confirm(selected) => {
                body = body.push(Box::new(
                    ListRow::new("installer.account.selected", selected.label())
                        .icon(Icon::Cloud)
                        .secondary(t(
                            if matches!(selected, Selection::Saved(_) | Selection::Tilo(_)) {
                                "installer-accounts-saved"
                            } else {
                                "installer-accounts-sign-in"
                            },
                        )),
                ));
                if let Selection::OAuth { provider, .. } = selected {
                    let permission = match provider {
                        ConnectionProvider::Spotify => "installer-accounts-spotify-permission",
                        ConnectionProvider::SpotifyStreaming => {
                            "installer-accounts-spotify-playback-permission"
                        }
                        _ => "installer-accounts-files-permission",
                    };
                    body = body.push(Box::new(Text::new(&t(permission))));
                }
            }
            Step::PastedCode(_) => {
                let edit = edit.clone();
                let submit = dispatch.clone();
                body = body
                    .push(Box::new(
                        Field::new(
                            t("installer-accounts-code-label"),
                            TextInput::from_state(&self.dav_fields[2])
                                .with_id("installer.account.oauth-code")
                                .with_kind(TextInputKind::Secret)
                                .with_secret(true)
                                .on_change(move |value| edit("oauth-code", value.to_string()))
                                .on_submit(move |_| submit(AccountRequest::SubmitCode)),
                        )
                        .with_label_placement(Some(LabelPlacement::Above)),
                    ))
                    .push(Box::new(
                        action_button(
                            "installer.account.submit-code",
                            "installer-accounts-code-submit",
                            AccountRequest::SubmitCode,
                            dispatch.clone(),
                        )
                        .role(ButtonRole::Primary),
                    ))
                    .push(Box::new(action_button(
                        "installer.account.open-again",
                        "installer-accounts-open-again",
                        AccountRequest::OpenAgain,
                        dispatch.clone(),
                    )))
                    .push(Box::new(action_button(
                        "installer.account.cancel",
                        "installer-accounts-cancel",
                        AccountRequest::Cancel,
                        dispatch.clone(),
                    )));
            }
            Step::Working(work) => {
                body = body.push(Box::new(
                    Text::new(&work.selected.label()).with_role(TextRole::Title),
                ));
                if work.browser_failed {
                    body = body.push(Box::new(Text::new(&t("installer-accounts-browser-failed"))));
                }
                if work.url.is_some() && !work.cancelling {
                    body = body.push(Box::new(action_button(
                        "installer.account.open-again",
                        "installer-accounts-open-again",
                        AccountRequest::OpenAgain,
                        dispatch.clone(),
                    )));
                }
                if work.sign_in && !work.cancelling {
                    body = body.push(Box::new(action_button(
                        "installer.account.cancel",
                        "installer-accounts-cancel",
                        AccountRequest::Cancel,
                        dispatch.clone(),
                    )));
                }
            }
            Step::Removing { .. } => {}
            Step::Done(_) => {
                body = body.push(Box::new(
                    IconWidget::new(Icon::CircleCheck)
                        .sized(ui::widget::IconSize::Large)
                        .accent(),
                ));
                body = body.push(Box::new(action_button(
                    "installer.account.another",
                    "installer-accounts-another",
                    AccountRequest::AddAccount,
                    dispatch.clone(),
                )));
            }
            Step::Failed {
                retry_cleanup,
                message,
                selected,
            } => {
                if matches!(
                    *message,
                    "installer-accounts-nextcloud-source-reconnect"
                        | "installer-accounts-rclone-source-reconnect"
                        | "installer-accounts-goa-source-reconnect"
                        | "installer-accounts-tilo-reconnect"
                ) {
                    let available = saved_cloud_provider(selected)
                        .is_none_or(|provider| self.actions.connection_available(provider));
                    if !available {
                        body = body.push(Box::new(Text::new(&t("installer-accounts-unavailable"))));
                    }
                    body = body.push(Box::new(
                        action_button(
                            "installer.account.reconnect",
                            "installer-accounts-reconnect",
                            AccountRequest::Reconnect,
                            dispatch.clone(),
                        )
                        .role(ButtonRole::Primary)
                        .with_enabled(available),
                    ));
                } else if *message != "installer-accounts-tilo-reconnect" {
                    body = body.push(Box::new(
                        action_button(
                            "installer.account.retry",
                            "installer-accounts-retry",
                            AccountRequest::Retry,
                            dispatch.clone(),
                        )
                        .role(ButtonRole::Primary),
                    ));
                }
                if retry_cleanup.is_none() {
                    body = body.push(Box::new(action_button(
                        "installer.account.skip",
                        "installer-accounts-skip",
                        AccountRequest::Overview,
                        dispatch.clone(),
                    )));
                }
            }
        }
        Container::new()
            .with_no_padding()
            .with_child(Box::new(body))
    }
}

impl Drop for Accounts {
    fn drop(&mut self) {
        self.dav_fields[2].set(String::new());
        if let Step::PastedCode(work) = &self.step {
            self.actions.cancel(&work.job);
        }
        if let Step::Working(work) = &self.step
            && work.sign_in
        {
            self.actions.cancel_job(&work.job);
        }
    }
}

fn action_button(
    id: &str,
    key: &str,
    action: AccountRequest,
    dispatch: Rc<dyn Fn(AccountRequest)>,
) -> Button {
    Button::new(id, &t(key)).on_effect(move || dispatch(action.clone()))
}
fn service_copy(key: &str, service: &str) -> String {
    t_args(key, &[("service", service.into())])
}
fn provider_label(provider: ConnectionProvider) -> String {
    t(match provider {
        ConnectionProvider::GoogleDrive => "installer-accounts-google-drive",
        ConnectionProvider::OneDrive => "installer-accounts-onedrive",
        ConnectionProvider::Dropbox => "installer-accounts-dropbox",
        ConnectionProvider::Spotify => "installer-accounts-spotify-library",
        ConnectionProvider::SpotifyStreaming => "installer-accounts-spotify-playback",
    })
}
pub(super) fn can_connect_candidate(candidate: &Candidate) -> bool {
    candidate_provider(candidate).is_some()
        || matches!(candidate.provider, Provider::Nextcloud | Provider::WebDav)
}

fn saved_cloud_provider(selected: &Selection) -> Option<ConnectionProvider> {
    let Selection::Saved(candidate) = selected else {
        return None;
    };
    if !matches!(
        candidate.origin,
        super::super::account_transfer::Origin::Rclone
            | super::super::account_transfer::Origin::Gnome
    ) {
        return None;
    }
    match candidate.provider {
        Provider::Google => Some(ConnectionProvider::GoogleDrive),
        Provider::Microsoft => Some(ConnectionProvider::OneDrive),
        Provider::Dropbox => Some(ConnectionProvider::Dropbox),
        _ => None,
    }
}

fn candidate_provider(candidate: &Candidate) -> Option<ConnectionProvider> {
    match candidate.provider {
        Provider::Google => Some(ConnectionProvider::GoogleDrive),
        Provider::Microsoft => Some(ConnectionProvider::OneDrive),
        Provider::Dropbox => Some(ConnectionProvider::Dropbox),
        Provider::Spotify => Some(ConnectionProvider::Spotify),
        _ => None,
    }
}
pub(super) fn candidate_label(candidate: &Candidate) -> String {
    let service = match candidate.provider {
        Provider::Nextcloud => t("installer-accounts-nextcloud"),
        Provider::WebDav => t("installer-accounts-webdav"),
        Provider::Spotify => t("installer-accounts-spotify"),
        _ => match candidate_provider(candidate) {
            Some(provider) => provider_label(provider),
            None => return candidate.label.clone(),
        },
    };
    let account = match candidate
        .identity
        .as_deref()
        .filter(|identity| !identity.is_empty())
    {
        Some(identity) if candidate.label == service || candidate.label.is_empty() => {
            identity.to_owned()
        }
        Some(identity) if identity != candidate.label => t_args(
            "installer-accounts-source-identity",
            &[
                ("account", candidate.label.clone().into()),
                ("identity", identity.into()),
            ],
        ),
        _ => candidate.label.clone(),
    };
    account_source_label(service, account)
}

pub(super) fn tilo_candidate_label(
    candidate: &super::super::account_transfer::TiloCandidate,
) -> String {
    let service = match candidate.type_id.as_str() {
        "google-drive" => "installer-accounts-google-drive",
        "onedrive" => "installer-accounts-onedrive",
        "dropbox" => "installer-accounts-dropbox",
        "icloud-drive" => "installer-accounts-icloud",
        "nextcloud" => "installer-accounts-nextcloud",
        "webdav" => "installer-accounts-webdav",
        "spotify" if candidate.account_id == "music.spotify.streaming" => {
            "installer-accounts-spotify-playback"
        }
        "spotify" => "installer-accounts-spotify-library",
        _ => return candidate.label.clone(),
    };
    account_source_label(t(service), candidate.label.clone())
}

fn account_source_label(service: String, account: String) -> String {
    if account.is_empty() || account == service {
        return service;
    }
    t_args(
        "installer-accounts-source-label",
        &[("service", service.into()), ("account", account.into())],
    )
}
fn connection_message(reason: ConnectionError) -> &'static str {
    match reason {
        ConnectionError::SignInUnavailable => "installer-accounts-unavailable",
        ConnectionError::StorageUnavailable => "installer-accounts-save-failed",
        ConnectionError::CredentialUnavailable => "installer-accounts-import-failed",
        ConnectionError::AlreadyPresent => "installer-accounts-conflict",
        ConnectionError::DavAuthenticationRefused => "installer-accounts-dav-auth-refused",
        ConnectionError::DavEndpointUnavailable => "installer-accounts-dav-endpoint-unavailable",
        ConnectionError::DavFilesUnavailable => "installer-accounts-dav-files-unavailable",
        _ => "installer-accounts-connect-failed",
    }
}

#[cfg(all(test, target_os = "linux"))]
mod source_review_tests;
#[cfg(test)]
mod tests;

#[path = "accounts_checkpoint.rs"]
pub(super) mod checkpoint;

impl Accounts {
    pub(super) fn checkpoint_selection(&self) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        if !self.can_continue() {
            return None;
        }
        let bytes = serde_json::to_vec(&self.prepared).ok()?;
        Some(Sha256::digest(bytes).into())
    }
    pub(super) fn checkpoint_save(
        &mut self,
        root: PathBuf,
        binding: String,
        group: [u8; 32],
        password: Zeroizing<Vec<u8>>,
    ) -> Result<(), &'static str> {
        if self.busy() || !self.can_continue() {
            return Err("installer-accounts-busy");
        }
        self.reviewed_empty = true;
        let handle = self.prepared_handle();
        if handle.is_none() && !self.prepared.is_empty() {
            return Err("installer-accounts-save-failed");
        }
        self.checkpoint_worker = Some(checkpoint::Worker::save(
            handle,
            self.actions.destination_snapshot(),
            root,
            binding,
            group,
            password,
        ));
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn checkpoint_restore(
        &mut self,
        root: PathBuf,
        receipt: checkpoint::Receipt,
        password: Zeroizing<Vec<u8>>,
    ) -> Result<(), &'static str> {
        if self.busy() || !self.prepared.is_empty() || self.source_roots.is_some() {
            return Err("installer-accounts-busy");
        }
        receipt
            .validate()
            .map_err(|_| "installer-accounts-save-failed")?;
        self.checkpoint_worker = Some(checkpoint::Worker::restore(root, receipt, password));
        Ok(())
    }
    pub(super) fn checkpoint_restore_ready(
        &mut self,
        ready: super::draft::ReadyDraft,
        password: Zeroizing<Vec<u8>>,
    ) -> Result<(), &'static str> {
        if self.busy() || !self.prepared.is_empty() || self.source_roots.is_some() {
            return Err("installer-accounts-busy");
        }
        if ready.account_checkpoint().is_none() {
            return Err("installer-accounts-import-failed");
        }
        self.checkpoint_worker = Some(checkpoint::Worker::restore_ready(ready, password));
        Ok(())
    }
    pub(super) fn checkpoint_poll(
        &mut self,
    ) -> Option<Result<Option<checkpoint::Receipt>, &'static str>> {
        let worker = self.checkpoint_worker.as_ref()?;
        let restoring = worker.restoring();
        let outcome = worker.poll()?;
        let worker = self.checkpoint_worker.take()?;
        let result = match outcome {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Err("installer-restart-changed")
            }
            Err(_) => Err(if restoring {
                "installer-accounts-checkpoint-unlock-failed"
            } else {
                "installer-accounts-save-failed"
            }),
            Ok(checkpoint::Completion::Saved(receipt, handle)) => {
                if self.stage.is_none() {
                    self.actions.retain_staging(handle.stage());
                    self.stage = Some(handle.stage());
                }
                Ok(Some(receipt))
            }
            Ok(checkpoint::Completion::Restored(handle, generation_guard)) => {
                self.stage = Some(handle.stage());
                self.actions = AccountActions::default();
                self.actions.retain_staging(handle.stage());
                self.prepared = handle.entries().to_vec();
                self.saved.reviewed = true;
                self.require_saved_review();
                self.reviewed_empty = true;
                self.step = Step::Overview;
                drop(generation_guard);
                Ok(None)
            }
        };
        drop(worker);
        Some(result)
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn checkpoint_fixture_for_journey(root: &Path) -> Accounts {
    tests::prepared_checkpoint_accounts(root)
}

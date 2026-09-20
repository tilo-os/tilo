//! Bounded device-local owner for Connected catalog and observation state.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use tilo_actions::ActionOutput;
use ui::folders::connected::{
    self, ConnectedService, ConnectionCursor, ConnectionError, ConnectionInfo, ConnectionLifecycle,
    ConnectionPage, Observer, Pending, Phase, Request, Side, Snapshot,
};

use super::connection_actions::{
    ConnectionOperation, ConnectionProjection, EndpointIssueProjection, ManagementPhase,
    ManagementProjection, SelectedProjection,
};
use super::strings;

const CATALOG_LIMIT: usize = connected::PAGE_LIMIT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    OpenManagement,
    BackToImports,
}

#[derive(Debug)]
pub struct ControllerInvocation {
    pub navigation: Option<Navigation>,
    pub output: ActionOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ObservedPhase {
    PendingNotCompared,
    PendingIncompleteObservation,
    PendingNativeRefused,
    PendingStructural,
    PendingRunLimit,
    Scanning,
    Syncing,
    AwaitingReview {
        entries: usize,
    },
    UpToDate,
    Conflict,
    Unavailable {
        side: &'static str,
        error: &'static str,
    },
    RecoveryRequired {
        error: &'static str,
    },
    Paused,
    Disconnecting,
    Disconnected,
    Detached,
}

impl ObservedPhase {
    fn code(&self) -> String {
        match self {
            Self::PendingNotCompared => "pending_not_compared".into(),
            Self::PendingIncompleteObservation => "pending_incomplete_observation".into(),
            Self::PendingNativeRefused => "pending_native_refused".into(),
            Self::PendingStructural => "pending_structural".into(),
            Self::PendingRunLimit => "pending_run_limit".into(),
            Self::Scanning => "scanning".into(),
            Self::Syncing => "syncing".into(),
            Self::AwaitingReview { .. } => "awaiting_review".into(),
            Self::UpToDate => "up_to_date".into(),
            Self::Conflict => "conflict".into(),
            Self::Unavailable { side, error } => format!("unavailable_{side}_{error}"),
            Self::RecoveryRequired { error } => format!("recovery_required_{error}"),
            Self::Paused => "paused".into(),
            Self::Disconnecting => "disconnecting".into(),
            Self::Disconnected => "disconnected".into(),
            Self::Detached => "detached".into(),
        }
    }

    const fn awaiting_entries(&self) -> Option<usize> {
        match self {
            Self::AwaitingReview { entries } => Some(*entries),
            _ => None,
        }
    }

    const fn prevents_switch(&self) -> bool {
        matches!(
            self,
            Self::Scanning
                | Self::Syncing
                | Self::AwaitingReview { .. }
                | Self::Conflict
                | Self::RecoveryRequired { .. }
                | Self::Disconnecting
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedIssue {
    side: &'static str,
    error: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedSnapshot {
    connection: ConnectionInfo,
    phase: ObservedPhase,
    verified_steps: u32,
    pending_operation: bool,
    endpoint_issue: Option<ObservedIssue>,
}

impl From<Snapshot> for ObservedSnapshot {
    fn from(snapshot: Snapshot) -> Self {
        Self {
            connection: snapshot.connection,
            phase: match snapshot.phase {
                Phase::Pending(Pending::NotCompared) => ObservedPhase::PendingNotCompared,
                Phase::Pending(Pending::IncompleteObservation) => {
                    ObservedPhase::PendingIncompleteObservation
                }
                Phase::Pending(Pending::NativeRefused) => ObservedPhase::PendingNativeRefused,
                Phase::Pending(Pending::Structural) => ObservedPhase::PendingStructural,
                Phase::Pending(Pending::RunLimit) => ObservedPhase::PendingRunLimit,
                Phase::Scanning => ObservedPhase::Scanning,
                Phase::Syncing => ObservedPhase::Syncing,
                Phase::AwaitingReview { entries, .. } => ObservedPhase::AwaitingReview { entries },
                Phase::UpToDate => ObservedPhase::UpToDate,
                Phase::Conflict => ObservedPhase::Conflict,
                Phase::Unavailable { side, error } => ObservedPhase::Unavailable {
                    side: side_code(side),
                    error: error_code(error),
                },
                Phase::RecoveryRequired(error) => ObservedPhase::RecoveryRequired {
                    error: error_code(error),
                },
                Phase::Paused => ObservedPhase::Paused,
                Phase::Disconnecting => ObservedPhase::Disconnecting,
                Phase::Disconnected => ObservedPhase::Disconnected,
                Phase::Detached => ObservedPhase::Detached,
            },
            verified_steps: snapshot.verified_steps,
            pending_operation: snapshot.pending_operation,
            endpoint_issue: snapshot.endpoint_issue.map(|issue| ObservedIssue {
                side: side_code(issue.side),
                error: error_code(issue.error),
            }),
        }
    }
}

fn side_code(side: Side) -> &'static str {
    match side {
        Side::Source => "source",
        Side::Destination => "destination",
    }
}

const fn error_code(error: ConnectionError) -> &'static str {
    match error {
        ConnectionError::UnsupportedHost => "unsupported_host",
        ConnectionError::Busy => "busy",
        ConnectionError::Closed => "closed",
        ConnectionError::LimitExceeded => "limit_exceeded",
        ConnectionError::StaleReview => "stale_review",
        ConnectionError::Disconnected => "disconnected",
        ConnectionError::IdentityChanged => "identity_changed",
        ConnectionError::Unavailable => "unavailable",
        ConnectionError::PermissionDenied => "permission_denied",
        ConnectionError::UnsafeVolume => "unsafe_volume",
        ConnectionError::ReadOnly => "read_only",
        ConnectionError::Unverified => "unverified",
        ConnectionError::Unstable => "unstable",
        ConnectionError::RecoveryRequired => "recovery_required",
        ConnectionError::RecoverySpace => "recovery_space",
        ConnectionError::CorruptStore => "corrupt_store",
        ConnectionError::Persistence => "persistence",
    }
}

fn service_error_text(error: ConnectionError) -> String {
    strings::t(match error {
        ConnectionError::UnsupportedHost => "imager-connected-reason-unsupported-host",
        ConnectionError::Busy => "imager-connected-reason-busy",
        ConnectionError::Closed => "imager-connected-reason-closed",
        ConnectionError::LimitExceeded => "imager-connected-reason-limit-exceeded",
        ConnectionError::StaleReview => "imager-connected-reason-stale-review",
        ConnectionError::Disconnected => "imager-connected-reason-disconnected",
        ConnectionError::IdentityChanged => "imager-connected-reason-identity-changed",
        ConnectionError::Unavailable => "imager-connected-reason-unavailable",
        ConnectionError::PermissionDenied => "imager-connected-reason-permission-denied",
        ConnectionError::UnsafeVolume => "imager-connected-reason-unsafe-volume",
        ConnectionError::ReadOnly => "imager-connected-reason-read-only",
        ConnectionError::Unverified => "imager-connected-reason-unverified",
        ConnectionError::Unstable => "imager-connected-reason-unstable",
        ConnectionError::RecoveryRequired => "imager-connected-reason-recovery-required",
        ConnectionError::RecoverySpace => "imager-connected-reason-recovery-space",
        ConnectionError::CorruptStore => "imager-connected-reason-corrupt-store",
        ConnectionError::Persistence => "imager-connected-reason-persistence",
    })
}

fn admission_error(reason: &str) -> String {
    strings::t(match reason {
        "request_pending" => "imager-connected-error-request-pending",
        "open_pending" => "imager-connected-error-open-pending",
        "switch_busy" => "imager-connected-error-switch-busy",
        "service_unavailable" => "imager-connected-error-service-unavailable",
        _ => "imager-connected-error-stale-route",
    })
}

const fn lifecycle_code(lifecycle: ConnectionLifecycle) -> &'static str {
    match lifecycle {
        ConnectionLifecycle::Active => "active",
        ConnectionLifecycle::Disconnecting => "disconnecting",
        ConnectionLifecycle::Disconnected => "disconnected",
    }
}

struct CatalogRow {
    alias: u64,
    info: ConnectionInfo,
}

enum PageCursor {
    Production(ConnectionCursor),
    Fixture(usize),
}

enum ServiceOwner {
    Production(ConnectedService),
    Fixture(FixtureBackend),
}

enum ObserverOwner {
    Production(Observer),
    Fixture(ObservedSnapshot),
}

enum OpenJob {
    Production(mpsc::Receiver<Result<ConnectedService, ConnectionError>>),
    Fixture {
        ready: bool,
        result: Option<Result<FixtureBackend, ConnectionError>>,
    },
}

enum PendingRequest {
    ProductionList {
        request: Request<ConnectionPage>,
        first: bool,
    },
    ProductionOpen {
        alias: u64,
        request: Request<Observer>,
    },
    FixtureList {
        result: Result<(Vec<ConnectionInfo>, Option<usize>), ConnectionError>,
        first: bool,
        ready: bool,
    },
    FixtureOpen {
        alias: u64,
        result: Result<ObservedSnapshot, ConnectionError>,
        ready: bool,
    },
}

#[derive(Clone)]
struct FixtureBackend {
    entries: Vec<ConnectionInfo>,
    observations: Vec<ObservedSnapshot>,
}

impl FixtureBackend {
    fn page(&self, offset: usize) -> (Vec<ConnectionInfo>, Option<usize>) {
        let end = offset.saturating_add(CATALOG_LIMIT).min(self.entries.len());
        let entries = self.entries.get(offset..end).unwrap_or_default().to_vec();
        let next = (end < self.entries.len()).then_some(end);
        (entries, next)
    }

    fn observation(&self, id: connected::ConnectionId) -> ObservedSnapshot {
        self.observations
            .iter()
            .find(|snapshot| snapshot.connection.id == id)
            .cloned()
            .unwrap_or_else(|| ObservedSnapshot {
                connection: self
                    .entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .cloned()
                    .expect("fixture catalog and observation ids stay aligned"),
                phase: ObservedPhase::PendingNotCompared,
                verified_steps: 0,
                pending_operation: false,
                endpoint_issue: None,
            })
    }
}

/// One retained catalog/settings owner. Native mutation admission is closed.
pub struct ConnectionController {
    generation: u64,
    next_alias: u64,
    opener: Option<OpenJob>,
    service: Option<ServiceOwner>,
    pending: Option<PendingRequest>,
    rows: Vec<CatalogRow>,
    next_cursor: Option<PageCursor>,
    selected_alias: Option<u64>,
    observer: Option<ObserverOwner>,
    selected: Option<ObservedSnapshot>,
    status_code: String,
    status_is_error: bool,
    structure_dirty: bool,
    #[cfg(test)]
    fixture_open: Option<FixtureBackend>,
    #[cfg(test)]
    next_submission_error: Option<ConnectionError>,
    #[cfg(test)]
    service_submissions: usize,
}

impl ConnectionController {
    #[must_use]
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            next_alias: 1,
            opener: None,
            service: None,
            pending: None,
            rows: Vec::new(),
            next_cursor: None,
            selected_alias: None,
            observer: None,
            selected: None,
            status_code: "not_open".into(),
            status_is_error: false,
            structure_dirty: true,
            #[cfg(test)]
            fixture_open: None,
            #[cfg(test)]
            next_submission_error: None,
            #[cfg(test)]
            service_submissions: 0,
        }
    }

    #[must_use]
    pub fn for_shot(scenario: &str) -> Self {
        let entries = fixture_connections();
        let observations = fixture_observations(&entries);
        let mut controller = Self::new(1);
        match scenario {
            "connected-unavailable" => {
                controller.status_code = "unsupported_host".into();
                controller.status_is_error = true;
            }
            "connected-loading" => {
                controller.opener = Some(OpenJob::Fixture {
                    ready: false,
                    result: None,
                });
                controller.status_code = "loading".into();
            }
            "connected-empty" => {
                controller.service = Some(ServiceOwner::Fixture(FixtureBackend {
                    entries: Vec::new(),
                    observations: Vec::new(),
                }));
                controller.status_code = "empty".into();
            }
            "connected-status" => {
                let selected = observations
                    .last()
                    .cloned()
                    .expect("status fixture has one observation");
                controller.rows = vec![CatalogRow {
                    alias: 1,
                    info: selected.connection.clone(),
                }];
                controller.next_alias = 2;
                controller.selected_alias = Some(1);
                controller.observer = Some(ObserverOwner::Fixture(selected.clone()));
                controller.selected = Some(selected.clone());
                controller.status_code = selected.phase.code();
                controller.service = Some(ServiceOwner::Fixture(FixtureBackend {
                    entries,
                    observations,
                }));
            }
            _ => {
                controller.rows = entries
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, info)| CatalogRow {
                        alias: u64::try_from(index).unwrap_or(0).saturating_add(1),
                        info,
                    })
                    .collect();
                controller.next_alias = u64::try_from(controller.rows.len())
                    .unwrap_or(0)
                    .saturating_add(1);
                controller.status_code = "catalog".into();
                controller.service = Some(ServiceOwner::Fixture(FixtureBackend {
                    entries,
                    observations,
                }));
            }
        }
        controller
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
        self.structure_dirty = true;
    }

    #[cfg(test)]
    fn controlled_open(
        generation: u64,
        entries: Vec<ConnectionInfo>,
        observations: Vec<ObservedSnapshot>,
    ) -> Self {
        let mut controller = Self::new(generation);
        controller.fixture_open = Some(FixtureBackend {
            entries,
            observations,
        });
        controller
    }

    #[cfg(test)]
    pub(super) fn controlled_fixture_open(generation: u64) -> Self {
        let entries = fixture_connections();
        let observations = fixture_observations(&entries);
        Self::controlled_open(generation, entries, observations)
    }

    #[cfg(test)]
    pub(super) fn controlled_many_catalog() -> Self {
        let entries = fixture_many_connections();
        let observations = fixture_observations(&entries);
        let mut controller = Self::controlled_open(1, entries, observations);
        controller
            .invoke(ConnectionOperation::OpenManagement { generation: 1 })
            .expect("controlled fixture opens");
        controller.deliver_opener();
        assert!(controller.poll());
        assert!(controller.poll());
        controller
    }

    #[cfg(test)]
    fn deliver_opener(&mut self) {
        if let Some(OpenJob::Fixture { ready, .. }) = self.opener.as_mut() {
            *ready = true;
        }
    }

    #[cfg(test)]
    pub(super) fn hold_pending(&mut self) {
        match self.pending.as_mut() {
            Some(
                PendingRequest::FixtureList { ready, .. }
                | PendingRequest::FixtureOpen { ready, .. },
            ) => *ready = false,
            _ => panic!("fixture request must be pending"),
        }
    }

    #[cfg(test)]
    pub(super) fn deliver_pending_error(&mut self, error: ConnectionError) {
        match self.pending.as_mut() {
            Some(PendingRequest::FixtureList { result, ready, .. }) => {
                *result = Err(error);
                *ready = true;
            }
            Some(PendingRequest::FixtureOpen { result, ready, .. }) => {
                *result = Err(error);
                *ready = true;
            }
            _ => panic!("fixture request must be pending"),
        }
    }

    #[cfg(test)]
    pub(super) fn inject_submission_error(&mut self, error: ConnectionError) {
        self.next_submission_error = Some(error);
    }

    #[cfg(test)]
    pub(super) const fn service_submission_count(&self) -> usize {
        self.service_submissions
    }

    #[cfg(test)]
    pub(super) const fn opener_pending(&self) -> bool {
        self.opener.is_some()
    }

    fn advance_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.structure_dirty = true;
    }

    fn start_open(&mut self) -> Result<(), String> {
        if self.opener.is_some() {
            return Err(strings::t("imager-connected-error-open-pending"));
        }
        if self.service.is_some() {
            return Ok(());
        }
        #[cfg(test)]
        if let Some(service) = self.fixture_open.clone() {
            self.opener = Some(OpenJob::Fixture {
                ready: false,
                result: Some(Ok(service)),
            });
            self.status_code = "loading".into();
            self.status_is_error = false;
            self.structure_dirty = true;
            return Ok(());
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("imager-connected-open".into())
            .spawn(move || {
                let _ = sender.send(ConnectedService::open_default());
            })
            .map_err(|error| {
                strings::t_args(
                    "imager-connected-error-open-worker",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.opener = Some(OpenJob::Production(receiver));
        self.status_code = "loading".into();
        self.status_is_error = false;
        self.structure_dirty = true;
        Ok(())
    }

    /// Invoke one already decoded operation. Kind validation belongs to the owning widget.
    ///
    /// # Errors
    /// Refuses stale aliases/routes, pending work and every native Effect.
    pub fn invoke(
        &mut self,
        operation: ConnectionOperation,
    ) -> Result<ControllerInvocation, String> {
        if operation.generation() != self.generation {
            return Err(strings::t("imager-connected-error-stale-route"));
        }
        let navigation = match operation {
            ConnectionOperation::OpenManagement { .. } => {
                self.start_open()?;
                self.advance_generation();
                Some(Navigation::OpenManagement)
            }
            ConnectionOperation::BackToImports { .. } => {
                self.advance_generation();
                Some(Navigation::BackToImports)
            }
            ConnectionOperation::Observe { .. } => None,
            ConnectionOperation::ReloadConnections { .. }
            | ConnectionOperation::FirstConnections { .. } => {
                self.submit_list(true)?;
                None
            }
            ConnectionOperation::NextConnections { .. } => {
                self.submit_list(false)?;
                None
            }
            ConnectionOperation::OpenSettings { alias, .. } => {
                self.submit_open(alias)?;
                None
            }
            ConnectionOperation::SyncNow { .. }
            | ConnectionOperation::Pause { .. }
            | ConnectionOperation::Disconnect { .. }
            | ConnectionOperation::AcceptInitial { .. }
            | ConnectionOperation::Restore { .. }
            | ConnectionOperation::Replace { .. } => {
                return Err(strings::t(
                    "imager-connected-error-native-capability-closed",
                ));
            }
        };
        Ok(ControllerInvocation {
            navigation,
            output: ActionOutput {
                value: Some(self.projection().value()),
                resources: Vec::new(),
            },
        })
    }

    fn submit_list(&mut self, first: bool) -> Result<(), String> {
        if let Some(reason) = self.navigation_block() {
            return Err(admission_error(reason));
        }
        if !first && (self.service.is_none() || self.next_cursor.is_none()) {
            return Err(strings::t("imager-connected-error-next-unavailable"));
        }
        if self.service.is_none() {
            self.start_open()?;
            return Ok(());
        }
        let cursor = if first {
            None
        } else {
            Some(
                self.next_cursor
                    .take()
                    .ok_or_else(|| strings::t("imager-connected-error-next-unavailable"))?,
            )
        };
        match (self.service.as_ref(), cursor.as_ref()) {
            (Some(ServiceOwner::Production(_)), Some(PageCursor::Fixture(_)))
            | (Some(ServiceOwner::Fixture(_)), Some(PageCursor::Production(_))) => {
                return Err(strings::t("imager-connected-error-stale-page"));
            }
            _ => {}
        }
        #[cfg(test)]
        {
            self.service_submissions = self.service_submissions.saturating_add(1);
        }
        #[cfg(test)]
        if let Some(error) = self.next_submission_error.take() {
            return Err(self.record_error(error));
        }
        let pending = match self.service.as_mut().expect("service checked above") {
            ServiceOwner::Production(service) => {
                let cursor = match cursor {
                    Some(PageCursor::Production(cursor)) => Some(cursor),
                    Some(PageCursor::Fixture(_)) => unreachable!("cursor prevalidated"),
                    None => None,
                };
                let request = service
                    .list(cursor, CATALOG_LIMIT)
                    .map_err(|error| self.record_error(error))?;
                PendingRequest::ProductionList { request, first }
            }
            ServiceOwner::Fixture(service) => {
                let offset = match cursor {
                    Some(PageCursor::Fixture(offset)) => offset,
                    Some(PageCursor::Production(_)) => unreachable!("cursor prevalidated"),
                    None => 0,
                };
                let (entries, next) = service.page(offset);
                PendingRequest::FixtureList {
                    result: Ok((entries, next)),
                    first,
                    ready: true,
                }
            }
        };
        self.pending = Some(pending);
        self.status_code = "loading_catalog".into();
        self.status_is_error = false;
        self.structure_dirty = true;
        Ok(())
    }

    fn submit_open(&mut self, alias: u64) -> Result<(), String> {
        if let Some(reason) = self.open_block(alias) {
            return Err(admission_error(reason));
        }
        let info = self
            .rows
            .iter()
            .find(|row| row.alias == alias)
            .map(|row| row.info.clone())
            .ok_or_else(|| strings::t("imager-connected-error-stale-connection"))?;
        #[cfg(test)]
        {
            self.service_submissions = self.service_submissions.saturating_add(1);
        }
        let pending = match self.service.as_mut() {
            Some(ServiceOwner::Production(service)) => PendingRequest::ProductionOpen {
                alias,
                request: service
                    .reopen(info.id)
                    .map_err(|error| self.record_error(error))?,
            },
            Some(ServiceOwner::Fixture(service)) => PendingRequest::FixtureOpen {
                alias,
                result: Ok(service.observation(info.id)),
                ready: true,
            },
            None => return Err(strings::t("imager-connected-error-service-unavailable")),
        };
        self.pending = Some(pending);
        self.status_code = "opening_settings".into();
        self.status_is_error = false;
        self.structure_dirty = true;
        Ok(())
    }

    fn record_error(&mut self, error: ConnectionError) -> String {
        let code = error_code(error);
        if matches!(error, ConnectionError::Closed) {
            self.service = None;
            self.pending = None;
            self.observer = None;
            self.selected = None;
            self.selected_alias = None;
            self.next_cursor = None;
        }
        self.status_code = code.into();
        self.status_is_error = true;
        self.structure_dirty = true;
        strings::t_args(
            "imager-connected-error-service",
            &[("error", service_error_text(error).into())],
        )
    }

    fn selected_prevents_switch(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|selected| selected.pending_operation || selected.phase.prevents_switch())
    }

    fn navigation_block(&self) -> Option<&'static str> {
        if self.pending.is_some() {
            Some("request_pending")
        } else if self.opener.is_some() {
            Some("open_pending")
        } else if self.selected_prevents_switch() {
            Some("switch_busy")
        } else {
            None
        }
    }

    fn open_block(&self, alias: u64) -> Option<&'static str> {
        if self.pending.is_some() {
            Some("request_pending")
        } else if self.opener.is_some() {
            Some("open_pending")
        } else if self.service.is_none() {
            Some("service_unavailable")
        } else if self.selected_alias != Some(alias) && self.selected_prevents_switch() {
            Some("switch_busy")
        } else {
            None
        }
    }

    fn apply_page(&mut self, entries: Vec<ConnectionInfo>, next: Option<PageCursor>, _first: bool) {
        let old_rows = std::mem::take(&mut self.rows);
        self.rows = entries
            .into_iter()
            .map(|info| {
                let alias = old_rows
                    .iter()
                    .find(|old| old.info.id == info.id && old.info.revision == info.revision)
                    .map_or_else(
                        || {
                            let alias = self.next_alias;
                            self.next_alias = self.next_alias.wrapping_add(1).max(1);
                            alias
                        },
                        |old| old.alias,
                    );
                CatalogRow { alias, info }
            })
            .collect();
        self.next_cursor = next;
        if self
            .selected_alias
            .is_some_and(|alias| !self.rows.iter().any(|row| row.alias == alias))
        {
            self.selected_alias = None;
            self.observer = None;
            self.selected = None;
        }
        self.status_code = if self.rows.is_empty() {
            "empty"
        } else {
            "catalog"
        }
        .into();
        self.status_is_error = false;
        self.advance_generation();
    }

    fn apply_observer(&mut self, alias: u64, owner: ObserverOwner) {
        let snapshot = match &owner {
            ObserverOwner::Production(observer) => observer.snapshot().into(),
            ObserverOwner::Fixture(snapshot) => snapshot.clone(),
        };
        self.selected_alias = Some(alias);
        self.status_code = snapshot.phase.code();
        self.status_is_error = matches!(
            snapshot.phase,
            ObservedPhase::Unavailable { .. }
                | ObservedPhase::RecoveryRequired { .. }
                | ObservedPhase::Detached
        );
        self.selected = Some(snapshot);
        self.observer = Some(owner);
        self.advance_generation();
    }

    /// Poll at most one completion and coalesce the current observer snapshot.
    #[must_use]
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut consumed_completion = false;
        if let Some(opener) = self.opener.take() {
            match opener {
                OpenJob::Fixture {
                    ready: false,
                    result,
                } => {
                    self.opener = Some(OpenJob::Fixture {
                        ready: false,
                        result,
                    });
                }
                OpenJob::Fixture {
                    ready: true,
                    mut result,
                } => {
                    consumed_completion = true;
                    match result.take().unwrap_or(Err(ConnectionError::Closed)) {
                        Ok(service) => {
                            self.service = Some(ServiceOwner::Fixture(service));
                            self.status_code = "loading_catalog".into();
                            changed = true;
                            if let Err(error) = self.submit_list(true) {
                                self.status_code = "list_submission_failed".into();
                                self.status_is_error = true;
                                let _ = error;
                            }
                        }
                        Err(error) => {
                            let _ = self.record_error(error);
                            changed = true;
                        }
                    }
                }
                OpenJob::Production(receiver) => match receiver.try_recv() {
                    Ok(Ok(service)) => {
                        consumed_completion = true;
                        self.service = Some(ServiceOwner::Production(service));
                        self.status_code = "loading_catalog".into();
                        changed = true;
                        if let Err(error) = self.submit_list(true) {
                            self.status_code = "list_submission_failed".into();
                            self.status_is_error = true;
                            let _ = error;
                        }
                    }
                    Ok(Err(error)) => {
                        consumed_completion = true;
                        let _ = self.record_error(error);
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        self.opener = Some(OpenJob::Production(receiver));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        consumed_completion = true;
                        let _ = self.record_error(ConnectionError::Closed);
                        changed = true;
                    }
                },
            }
        }

        if !consumed_completion && let Some(pending) = self.pending.take() {
            match pending {
                PendingRequest::ProductionList { request, first } => match request.try_take() {
                    Ok(Some(page)) => {
                        self.apply_page(page.entries, page.next.map(PageCursor::Production), first);
                        changed = true;
                    }
                    Ok(None) => {
                        self.pending = Some(PendingRequest::ProductionList { request, first });
                    }
                    Err(error) => {
                        let _ = self.record_error(error);
                        changed = true;
                    }
                },
                PendingRequest::ProductionOpen { alias, request } => match request.try_take() {
                    Ok(Some(observer)) => {
                        self.apply_observer(alias, ObserverOwner::Production(observer));
                        changed = true;
                    }
                    Ok(None) => {
                        self.pending = Some(PendingRequest::ProductionOpen { alias, request });
                    }
                    Err(error) => {
                        let _ = self.record_error(error);
                        changed = true;
                    }
                },
                PendingRequest::FixtureList {
                    result,
                    first,
                    ready: false,
                } => {
                    self.pending = Some(PendingRequest::FixtureList {
                        result,
                        first,
                        ready: false,
                    });
                }
                PendingRequest::FixtureList {
                    result,
                    first,
                    ready: true,
                } => {
                    match result {
                        Ok((entries, next)) => {
                            self.apply_page(entries, next.map(PageCursor::Fixture), first)
                        }
                        Err(error) => {
                            let _ = self.record_error(error);
                        }
                    }
                    changed = true;
                }
                PendingRequest::FixtureOpen {
                    alias,
                    result,
                    ready: false,
                } => {
                    self.pending = Some(PendingRequest::FixtureOpen {
                        alias,
                        result,
                        ready: false,
                    });
                }
                PendingRequest::FixtureOpen {
                    alias,
                    result,
                    ready: true,
                } => {
                    match result {
                        Ok(snapshot) => {
                            self.apply_observer(alias, ObserverOwner::Fixture(snapshot))
                        }
                        Err(error) => {
                            let _ = self.record_error(error);
                        }
                    }
                    changed = true;
                }
            }
        }

        if self.pending.is_none()
            && let Some(owner) = &self.observer
        {
            let snapshot = match owner {
                ObserverOwner::Production(observer) => observer.snapshot().into(),
                ObserverOwner::Fixture(snapshot) => snapshot.clone(),
            };
            if self.selected.as_ref() != Some(&snapshot) {
                self.status_code = snapshot.phase.code();
                self.status_is_error = matches!(
                    snapshot.phase,
                    ObservedPhase::Unavailable { .. }
                        | ObservedPhase::RecoveryRequired { .. }
                        | ObservedPhase::Detached
                );
                self.selected = Some(snapshot.clone());
                changed = true;
            }
            if matches!(snapshot.phase, ObservedPhase::Detached) {
                self.observer = None;
                self.next_cursor = None;
                self.selected = None;
                self.selected_alias = None;
            }
        }
        self.structure_dirty |= changed;
        changed
    }

    #[must_use]
    pub fn projection(&self) -> ManagementProjection {
        let navigation_unavailable_reason = self.navigation_block().map(str::to_string);
        let phase = if self.selected.is_some() {
            ManagementPhase::Settings
        } else if !self.rows.is_empty() {
            ManagementPhase::Catalog
        } else if self.opener.is_some() || self.pending.is_some() {
            ManagementPhase::Loading
        } else if self.status_is_error || (self.service.is_none() && self.status_code != "not_open")
        {
            ManagementPhase::Unavailable
        } else if self.rows.is_empty() && self.service.is_some() {
            ManagementPhase::Empty
        } else {
            ManagementPhase::Loading
        };
        ManagementProjection {
            generation: self.generation,
            phase,
            status_code: self.status_code.clone(),
            status_is_error: self.status_is_error,
            rows: self
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| ConnectionProjection {
                    alias: row.alias,
                    number: index.saturating_add(1),
                    revision: row.info.revision,
                    lifecycle: lifecycle_code(row.info.lifecycle).into(),
                    has_baseline: row.info.has_baseline,
                    selected: self.selected_alias == Some(row.alias),
                    open_settings_available: self.open_block(row.alias).is_none(),
                    open_settings_unavailable_reason: self
                        .open_block(row.alias)
                        .map(str::to_string),
                })
                .collect(),
            selected: self
                .selected_alias
                .zip(self.selected.as_ref())
                .map(|(alias, selected)| SelectedProjection {
                    alias,
                    phase: selected.phase.code(),
                    verified_steps: selected.verified_steps,
                    pending_operation: selected.pending_operation,
                    endpoint_issue: selected.endpoint_issue.as_ref().map(|issue| {
                        EndpointIssueProjection {
                            side: issue.side.into(),
                            error: issue.error.into(),
                        }
                    }),
                    awaiting_entries: selected.phase.awaiting_entries(),
                }),
            reload_available: navigation_unavailable_reason.is_none(),
            next_available: navigation_unavailable_reason.is_none() && self.next_cursor.is_some(),
            first_available: navigation_unavailable_reason.is_none(),
            navigation_unavailable_reason,
        }
    }

    #[must_use]
    pub fn selected_paths(&self) -> Option<(&Path, &Path)> {
        self.selected.as_ref().map(|selected| {
            (
                selected.connection.source.as_path(),
                selected.connection.destination.as_path(),
            )
        })
    }

    #[must_use]
    pub fn needs_poll(&self) -> bool {
        self.opener.is_some() || self.pending.is_some() || self.observer.is_some()
    }

    pub fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }
}

fn fixture_connection(number: u8, source: &str, destination: &str) -> ConnectionInfo {
    ConnectionInfo {
        id: connected::ConnectionId([number; 16]),
        source: PathBuf::from(source),
        destination: PathBuf::from(destination),
        revision: u64::from(number),
        has_baseline: number != 1,
        lifecycle: ConnectionLifecycle::Active,
        deletion_retention_seconds: connected::DELETION_RETENTION_SECONDS,
    }
}

fn fixture_connections() -> Vec<ConnectionInfo> {
    vec![
        fixture_connection(
            1,
            "/home/alex/Documents",
            "/run/media/alex/Tilo files/Documents",
        ),
        fixture_connection(
            2,
            "/home/alex/Pictures and a very long local folder name",
            "/run/media/alex/Tilo files/Pictures and a very long local folder name",
        ),
        fixture_connection(
            3,
            "/home/alex/Projects",
            "/run/media/alex/Tilo files/Projects",
        ),
    ]
}

#[cfg(test)]
fn fixture_many_connections() -> Vec<ConnectionInfo> {
    (0..=CATALOG_LIMIT)
        .map(|index| {
            let number = u8::try_from(index % 250 + 1).expect("fixture number fits");
            fixture_connection(
                number,
                &format!("/source/{index}"),
                &format!("/destination/{index}"),
            )
        })
        .collect()
}

fn fixture_observations(entries: &[ConnectionInfo]) -> Vec<ObservedSnapshot> {
    entries
        .iter()
        .enumerate()
        .map(|(index, connection)| ObservedSnapshot {
            connection: connection.clone(),
            phase: if index == 2 {
                ObservedPhase::RecoveryRequired {
                    error: "recovery_space",
                }
            } else if index == 1 {
                ObservedPhase::Scanning
            } else {
                ObservedPhase::AwaitingReview { entries: 17 }
            },
            verified_steps: u32::try_from(index).unwrap_or(0).saturating_add(1),
            pending_operation: index == 1,
            endpoint_issue: (index == 2).then_some(ObservedIssue {
                side: "destination",
                error: "permission_denied",
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::connection_actions::{ConnectionOperation, ManagementPhase};

    #[test]
    fn shot_catalog_opens_selected_settings_without_exposing_paths_in_projection() {
        let mut controller = ConnectionController::for_shot("connected-catalog");
        let catalog = controller.projection();
        assert_eq!(catalog.phase, ManagementPhase::Catalog);
        assert_eq!(catalog.rows.len(), 3);
        let alias = catalog.rows[1].alias;

        controller
            .invoke(ConnectionOperation::OpenSettings {
                generation: catalog.generation,
                alias,
            })
            .unwrap();
        assert!(controller.poll());

        let settings = controller.projection();
        assert_eq!(settings.phase, ManagementPhase::Settings);
        assert_eq!(settings.selected.as_ref().unwrap().alias, alias);
        let value = settings.value().to_string();
        assert!(!value.contains("/home/alex"));
        let paths = controller.selected_paths().unwrap();
        assert!(paths.0.to_string_lossy().contains("/home/alex"));
    }

    #[test]
    fn every_native_effect_refuses_before_backend_submission() {
        let mut controller = ConnectionController::for_shot("connected-status");
        let state = controller.projection();
        let alias = state.selected.as_ref().unwrap().alias;
        for operation in [
            ConnectionOperation::SyncNow {
                generation: state.generation,
                alias,
            },
            ConnectionOperation::Pause {
                generation: state.generation,
                alias,
            },
            ConnectionOperation::Disconnect {
                generation: state.generation,
                alias,
            },
            ConnectionOperation::AcceptInitial {
                generation: state.generation,
                alias,
            },
            ConnectionOperation::Restore {
                generation: state.generation,
                alias,
            },
            ConnectionOperation::Replace {
                generation: state.generation,
                alias,
            },
        ] {
            let error = controller.invoke(operation).unwrap_err();
            assert!(error.contains("unavailable"), "{error}");
        }
    }

    #[test]
    fn truthful_phase_codes_keep_recovery_and_endpoint_issue_together() {
        let cases = [
            (ObservedPhase::PendingNotCompared, "pending_not_compared"),
            (
                ObservedPhase::PendingIncompleteObservation,
                "pending_incomplete_observation",
            ),
            (
                ObservedPhase::PendingNativeRefused,
                "pending_native_refused",
            ),
            (ObservedPhase::PendingStructural, "pending_structural"),
            (ObservedPhase::PendingRunLimit, "pending_run_limit"),
            (ObservedPhase::Scanning, "scanning"),
            (ObservedPhase::Syncing, "syncing"),
            (
                ObservedPhase::AwaitingReview { entries: 17 },
                "awaiting_review",
            ),
            (ObservedPhase::UpToDate, "up_to_date"),
            (ObservedPhase::Conflict, "conflict"),
            (
                ObservedPhase::Unavailable {
                    side: "source",
                    error: "read_only",
                },
                "unavailable_source_read_only",
            ),
            (
                ObservedPhase::RecoveryRequired {
                    error: "recovery_space",
                },
                "recovery_required_recovery_space",
            ),
            (ObservedPhase::Paused, "paused"),
            (ObservedPhase::Disconnecting, "disconnecting"),
            (ObservedPhase::Disconnected, "disconnected"),
            (ObservedPhase::Detached, "detached"),
        ];
        for (phase, expected) in cases {
            assert_eq!(phase.code(), expected);
        }

        let controller = ConnectionController::for_shot("connected-status");
        let projection = controller.projection();
        assert_eq!(
            projection.rows.len(),
            1,
            "status shot keeps its selected row visible"
        );
        let selected = projection.selected.unwrap();
        assert_eq!(selected.phase, "recovery_required_recovery_space");
        assert_eq!(selected.endpoint_issue.unwrap().side, "destination");
        assert_eq!(selected.verified_steps, 3);
    }

    #[test]
    fn retained_generation_and_missing_aliases_fail_closed() {
        let mut controller = ConnectionController::for_shot("connected-catalog");
        let old = controller.projection();
        controller
            .invoke(ConnectionOperation::FirstConnections {
                generation: old.generation,
            })
            .unwrap();
        assert!(controller.poll());
        let current = controller.projection();
        assert_ne!(current.generation, old.generation);
        assert!(
            controller
                .invoke(ConnectionOperation::Observe {
                    generation: old.generation
                })
                .is_err()
        );
        assert!(
            controller
                .invoke(ConnectionOperation::OpenSettings {
                    generation: current.generation,
                    alias: u64::MAX,
                })
                .is_err()
        );
    }

    #[test]
    fn retained_catalog_stays_visible_during_refresh_and_ordinary_errors() {
        let mut controller = ConnectionController::for_shot("connected-catalog");
        let catalog = controller.projection();
        controller
            .invoke(ConnectionOperation::ReloadConnections {
                generation: catalog.generation,
            })
            .unwrap();
        let refreshing = controller.projection();
        assert_eq!(refreshing.phase, ManagementPhase::Catalog);
        assert_eq!(refreshing.rows.len(), catalog.rows.len());
        assert!(!refreshing.reload_available);
        let resource = crate::setup::connection_actions::resource(&refreshing);
        let reload = resource
            .actions
            .iter()
            .find(|action| action.id == "reload-connections")
            .unwrap();
        assert!(!reload.contract.availability.is_available());

        assert!(controller.poll());
        let _ = controller.record_error(ConnectionError::Busy);
        let failed = controller.projection();
        assert_eq!(failed.phase, ManagementPhase::Catalog);
        assert_eq!(failed.rows.len(), catalog.rows.len());
        assert!(failed.status_is_error);
        assert!(failed.reload_available);
    }

    #[test]
    fn published_navigation_contracts_follow_pending_and_protected_selection_admission() {
        let mut controller = ConnectionController::for_shot("connected-catalog");
        let catalog = controller.projection();
        controller
            .invoke(ConnectionOperation::ReloadConnections {
                generation: catalog.generation,
            })
            .unwrap();
        let pending = crate::setup::connection_actions::resource(&controller.projection());
        assert!(pending.actions.iter().all(|action| {
            !matches!(
                action.id.as_str(),
                "reload-connections" | "first-connections" | "next-connections"
            ) || !action.contract.availability.is_available()
        }));
        assert!(pending.children.iter().all(|row| {
            !row.actions
                .iter()
                .find(|action| action.id == "open-settings")
                .unwrap()
                .contract
                .availability
                .is_available()
        }));

        assert!(controller.poll());
        let catalog = controller.projection();
        let alias = catalog.rows[1].alias;
        controller
            .invoke(ConnectionOperation::OpenSettings {
                generation: catalog.generation,
                alias,
            })
            .unwrap();
        assert!(controller.poll());
        let protected = crate::setup::connection_actions::resource(&controller.projection());
        for action_id in [
            "reload-connections",
            "first-connections",
            "next-connections",
        ] {
            let action = protected
                .actions
                .iter()
                .find(|action| action.id == action_id)
                .unwrap();
            assert!(!action.contract.availability.is_available(), "{action_id}");
        }
        let other = protected
            .children
            .iter()
            .find(|row| !row.id.0.ends_with(&format!(".{alias}")))
            .unwrap();
        assert!(
            !other
                .actions
                .iter()
                .find(|action| action.id == "open-settings")
                .unwrap()
                .contract
                .availability
                .is_available()
        );
    }

    #[test]
    fn controlled_opener_and_request_delivery_consume_only_one_completion_per_poll() {
        let entries = fixture_connections();
        let observations = fixture_observations(&entries);
        let mut controller = ConnectionController::controlled_open(4, entries, observations);
        controller
            .invoke(ConnectionOperation::OpenManagement { generation: 4 })
            .unwrap();
        assert!(!controller.poll());
        controller.deliver_opener();
        assert!(controller.poll());
        assert!(
            controller.pending.is_some(),
            "the newly submitted list remains pending"
        );
        assert_eq!(controller.service_submission_count(), 1);
        assert!(controller.projection().rows.is_empty());
        assert!(controller.poll());
        assert_eq!(controller.projection().rows.len(), 3);
    }

    #[test]
    fn failed_next_consumes_cursor_retains_page_and_localizes_submission_and_terminal_errors() {
        let entries = fixture_many_connections();
        let observations = fixture_observations(&entries);
        let mut controller = ConnectionController::controlled_open(1, entries, observations);
        controller
            .invoke(ConnectionOperation::OpenManagement { generation: 1 })
            .unwrap();
        controller.deliver_opener();
        assert!(controller.poll());
        assert!(controller.poll());
        let first = controller.projection();
        assert_eq!(first.rows.len(), CATALOG_LIMIT);
        assert!(first.next_available);

        controller.next_submission_error = Some(ConnectionError::PermissionDenied);
        let error = controller
            .invoke(ConnectionOperation::NextConnections {
                generation: first.generation,
            })
            .unwrap_err();
        assert!(error.contains("permissions"), "{error}");
        assert!(!error.contains("permission_denied"));
        let failed = controller.projection();
        assert_eq!(failed.rows.len(), CATALOG_LIMIT);
        assert!(!failed.next_available);
        assert!(failed.first_available && failed.reload_available);

        controller
            .invoke(ConnectionOperation::FirstConnections {
                generation: failed.generation,
            })
            .unwrap();
        controller.hold_pending();
        assert!(!controller.poll());
        controller.deliver_pending_error(ConnectionError::Busy);
        assert!(controller.poll());
        let terminal = controller.projection();
        assert_eq!(terminal.rows.len(), CATALOG_LIMIT);
        assert_eq!(terminal.status_code, "busy");
        assert!(terminal.status_is_error);
        assert_eq!(controller.service_submission_count(), 3);
    }

    #[test]
    fn protected_selection_refuses_page_replacement_and_detached_and_closed_are_terminal() {
        let mut controller = ConnectionController::for_shot("connected-catalog");
        let state = controller.projection();
        let alias = state.rows[1].alias;
        controller
            .invoke(ConnectionOperation::OpenSettings {
                generation: state.generation,
                alias,
            })
            .unwrap();
        assert!(controller.poll());
        let selected = controller.projection();
        assert!(
            controller
                .invoke(ConnectionOperation::FirstConnections {
                    generation: selected.generation
                })
                .unwrap_err()
                .contains("Finish")
        );
        assert_eq!(
            controller.projection().selected.as_ref().unwrap().alias,
            alias
        );

        if let Some(ObserverOwner::Fixture(snapshot)) = controller.observer.as_mut() {
            snapshot.phase = ObservedPhase::Detached;
        }
        assert!(controller.poll());
        assert!(controller.projection().selected.is_none());

        let current = controller.projection();
        controller
            .invoke(ConnectionOperation::FirstConnections {
                generation: current.generation,
            })
            .unwrap();
        controller.deliver_pending_error(ConnectionError::Closed);
        assert!(controller.poll());
        assert!(controller.service.is_none());
        assert_eq!(controller.projection().status_code, "closed");
    }
}

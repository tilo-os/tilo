//! First-use operations on the same registry and reducer as the rendered ceremony.
use super::{
    AcknowledgementKind, KEY_BACK, KEY_BACKUP_CONTINUE, KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BEGIN,
    KEY_CHOOSE_NEARBY, KEY_CHOOSE_RECOVERY_KEY, KEY_CHOOSE_RESTORE, KEY_CODECS_CONTINUE,
    KEY_CODECS_INSTALL, KEY_CODECS_SKIP, KEY_CONNECT_CANCEL, KEY_CREATE, KEY_EXISTING, KEY_FINISH,
    KEY_TOUR_BACK, KEY_TOUR_NEXT, KEY_TOUR_SKIP, KEY_TOUR_START, KEY_UNDERSTOOD, OnboardingView,
    PressedActions, TourPage,
};
use crate::onboarding::{Screen, intro_title};
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{
    ActionReceipt, InvocationDisposition, ResourceKind, SearchContext, SearchProvider, SearchResult,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::rc::Weak;
use zeroize::Zeroizing;

#[derive(Default)]
struct Acknowledgements {
    protection: bool,
    skip_backup: bool,
}

#[derive(Default)]
enum CodecCompletion {
    #[default]
    Idle,
    Running,
    Succeeded,
    Failed,
}

impl CodecCompletion {
    fn from_view(view: &OnboardingView) -> Self {
        if view.codec_running.is_some() {
            Self::Running
        } else {
            match view.codec_worker_succeeded {
                Some(true)
                    if view.codec_status.state == os_actions::codec_pack::PackState::Installed =>
                {
                    Self::Succeeded
                }
                Some(_) => Self::Failed,
                None => Self::Idle,
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Snapshot {
    pub(super) revision: u64,
    pub(super) phase: &'static str,
    pub(super) operations: Vec<(&'static str, &'static str)>,
    acknowledgements: Acknowledgements,
    pub(super) failed: bool,
    tour_page: Option<TourPage>,
    nearby_active: bool,
    codec_completion: CodecCompletion,
    prepared: Option<&'static str>,
    identity_ready: bool,
    migration: Option<serde_json::Value>,
    backup: Option<serde_json::Value>,
}

impl Snapshot {
    pub(super) fn from_view(view: &OnboardingView) -> Self {
        let prepared = view.prepared_setup_status();
        let migration = view.migration_status();
        let backup = view.backup_status();
        let (phase, mut operations) = if view.finished {
            ("finished", vec![])
        } else if let Some(status) = prepared {
            (
                "prepared",
                if status == "ready" {
                    vec![("prepared.import", "onboarding-prepared-title")]
                } else {
                    vec![]
                },
            )
        } else if let Some(status) = migration
            .as_ref()
            .filter(|status| status["deferred"] != true)
        {
            ("migration", migration_operations(status))
        } else if let Some(page) = view.tour_page {
            let mut ops = vec![
                (KEY_TOUR_BACK, "onboarding-back"),
                (KEY_TOUR_SKIP, "onboarding-tour-skip"),
            ];
            ops.push(if page == TourPage::Start {
                (KEY_FINISH, "onboarding-finish")
            } else {
                (KEY_TOUR_NEXT, "onboarding-continue")
            });
            ("tour", ops)
        } else if view.codec_page {
            let operations = if view.codec_choice().is_some() {
                vec![
                    (KEY_CODECS_INSTALL, "onboarding-codecs-install"),
                    (KEY_CODECS_SKIP, "onboarding-codecs-skip"),
                ]
            } else {
                vec![(KEY_CODECS_CONTINUE, "onboarding-continue")]
            };
            ("codecs", operations)
        } else {
            ceremony_operations(view)
        };
        if !view.finished
            && migration
                .as_ref()
                .is_some_and(|status| status["deferred"] == true)
        {
            operations.push(("migration.resume", "onboarding-migration-title"));
        }
        if backup.is_some() {
            operations.push(("backup.continue", "onboarding-continue"));
            if backup
                .as_ref()
                .is_some_and(|status| status["can_save"] == true)
            {
                operations.push(("backup.save", "onboarding-backup-save-action"));
            }
        }
        Self {
            revision: view.semantic.borrow().revision.wrapping_add(1),
            phase,
            operations,
            acknowledgements: Acknowledgements {
                protection: view.flow.acknowledged(),
                skip_backup: view.backup_skip_acknowledged,
            },
            failed: view.error.is_some() || view.progress_error.is_some(),
            tour_page: view.tour_page,
            nearby_active: view.nearby_active(),
            codec_completion: CodecCompletion::from_view(view),
            prepared,
            identity_ready: view.flow.is_done(),
            migration,
            backup,
        }
    }

    fn value(&self) -> serde_json::Value {
        serde_json::json!({"phase": self.phase, "protection_acknowledged": self.acknowledgements.protection,
            "backup_skip_acknowledged":self.acknowledgements.skip_backup, "failed":self.failed,
            "prepared":self.prepared, "migration":self.migration, "backup":self.backup,
            "operations":self.operations.iter().map(|(name,_)| *name).collect::<Vec<_>>()})
    }
}

/// The actual view owns this observation. It conveys only ceremony activity;
/// native `control_session_decision` still independently authenticates root delivery.
pub(super) fn authentication_first_use_scope(view: &OnboardingView) -> Rc<dyn Fn() -> bool> {
    view.publish_semantic_state();
    let snapshot = Rc::downgrade(&view.semantic);
    Rc::new(move || {
        snapshot.upgrade().is_some_and(|snapshot| {
            matches!(
                snapshot.borrow().phase,
                "intro"
                    | "protection"
                    | "existing"
                    | "nearby"
                    | "restore"
                    | "prepared"
                    | "migration"
                    | "backup"
                    | "codecs"
                    | "ready"
                    | "tour"
            )
        })
    })
}

fn ceremony_operations(view: &OnboardingView) -> (&'static str, Vec<(&'static str, &'static str)>) {
    match view.flow.screen() {
        Screen::Intro => (
            "intro",
            vec![
                (KEY_BEGIN, "onboarding-continue"),
                (KEY_EXISTING, "onboarding-existing-title"),
            ],
        ),
        Screen::NoRecovery => {
            let mut ops = vec![
                (KEY_UNDERSTOOD, "onboarding-acknowledgement"),
                (KEY_BACK, "onboarding-back"),
            ];
            if view.flow.can_create() {
                ops.push((KEY_CREATE, "onboarding-create-local-identity"));
            }
            ("protection", ops)
        }
        Screen::Done if view.identity_ready_page => (
            "ready",
            vec![
                (KEY_TOUR_START, "onboarding-tour-take"),
                (KEY_FINISH, "onboarding-finish"),
            ],
        ),
        Screen::Done => {
            let mut ops = if view.portable_backup_only {
                vec![]
            } else {
                vec![(
                    KEY_BACKUP_SKIP_ACKNOWLEDGED,
                    "onboarding-backup-skip-acknowledgement",
                )]
            };
            if view.backup_was_verified
                || (!view.portable_backup_only && view.backup_skip_acknowledged)
            {
                ops.push((KEY_BACKUP_CONTINUE, "onboarding-continue"));
            }
            ("backup", ops)
        }
        Screen::Existing => (
            "existing",
            vec![
                (KEY_CHOOSE_NEARBY, "onboarding-choice-nearby"),
                (KEY_CHOOSE_RECOVERY_KEY, "onboarding-choice-recovery-key"),
                (KEY_CHOOSE_RESTORE, "onboarding-choice-restore"),
            ],
        ),
        Screen::Nearby => {
            let mut ops = vec![(KEY_BACK, "onboarding-back")];
            if view.nearby_active() {
                ops.push((KEY_CONNECT_CANCEL, "onboarding-cancel"));
            }
            ("nearby", ops)
        }
        Screen::Restore => ("restore", vec![(KEY_BACK, "onboarding-back")]),
        Screen::RecoveryKey => ("recovery-key", vec![(KEY_BACK, "onboarding-back")]),
    }
}

pub(super) struct Provider {
    snapshot: Weak<RefCell<Snapshot>>,
    ranker: Rc<ui::search::ListIndex>,
    pressed: PressedActions,
    acknowledgements: Rc<RefCell<Vec<(AcknowledgementKind, bool)>>>,
    requests: Rc<RefCell<Vec<Request>>>,
}

fn authorized(chrome: &crate::shell::chrome_state::ChromeState) -> bool {
    !chrome.session_locked() && crate::shell::overlay_host::is_active_workspace_owner(chrome)
}

impl Provider {
    pub(super) fn new(view: &OnboardingView) -> Self {
        Self {
            snapshot: Rc::downgrade(&view.semantic),
            ranker: Rc::default(),
            pressed: view.pressed.clone(),
            acknowledgements: view.acknowledgements.clone(),
            requests: view.semantic_requests.clone(),
        }
    }
}

fn unavailable() -> ActionError {
    ActionError::Explained(t("onboarding-semantic-unavailable"))
}

impl Provider {
    fn row(
        &self,
        _ctx: &SearchContext<'_>,
        phase: &'static str,
        operation: &'static str,
        label: &'static str,
    ) -> SearchResult {
        let title = t(label);
        let params = if queued_operation(operation) {
            self.snapshot.upgrade().map_or_else(Vec::new, |snapshot| {
                request_params(&snapshot.borrow(), operation)
            })
        } else {
            acknowledgement(operation).map_or_else(Vec::new, |_| {
                vec![ActionParam::required_owned(
                    "checked",
                    ParamType::Boolean,
                    title.clone(),
                )]
            })
        };
        let offered_tour = self
            .snapshot
            .upgrade()
            .and_then(|snapshot| snapshot.borrow().tour_page);
        let provider = Self {
            snapshot: self.snapshot.clone(),
            ranker: self.ranker.clone(),
            pressed: self.pressed.clone(),
            acknowledgements: self.acknowledgements.clone(),
            requests: self.requests.clone(),
        };
        SearchResult {
            facts: Vec::new(),
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: if operation == "inspect" {
                tilo_actions::OperationKind::View
            } else {
                tilo_actions::OperationKind::Effect
            },
            provider_id: self.id().into(),
            id: operation.into(),
            title,
            subtitle: None,
            badge: Some(self.display_name()),
            score: 0,
            params,
            contract: Some(tilo_actions::ActionContract {
                // Setting up the device's identity reaches outside any
                // document; only the local owner sees it (search visibility).
                kind: if operation == "inspect" {
                    tilo_actions::ActionKind::Read
                } else {
                    tilo_actions::ActionKind::Outside
                },
                retry: tilo_actions::RetryPolicy::AtMostOnce,
                result: tilo_actions::ResultSpec::Json {
                    schema: serde_json::json!({"type":"object"}),
                },
                ..Default::default()
            }),
            on_open: Rc::new(|_, _, _, _| Err(unavailable())),
            on_invoke: None,
            on_begin_invoke: Some(Rc::new(move |_, _, chrome, args| {
                provider.begin(chrome, operation, phase, offered_tour, args)
            })),
        }
    }

    fn begin(
        &self,
        chrome: &crate::shell::chrome_state::Chrome,
        operation: &'static str,
        phase: &str,
        offered_tour: Option<TourPage>,
        args: &ActionArgs,
    ) -> Result<InvocationDisposition, ActionError> {
        if !authorized(&chrome.borrow()) {
            return Err(unavailable());
        }
        let current = self.snapshot.upgrade().ok_or_else(unavailable)?;
        let state = current.borrow();
        if operation == "inspect" {
            return Ok(InvocationDisposition::Accepted(
                ActionReceipt::completed_with(state.value()),
            ));
        }
        if state.tour_page != offered_tour
            || state.phase != phase
            || !state.operations.iter().any(|(name, _)| *name == operation)
        {
            return Err(unavailable());
        }
        if queued_operation(operation) {
            return self.enqueue(&current, &state, operation, args);
        }
        let revision = state.revision;
        let tour_page = state.tour_page;
        let kind = acknowledgement(operation);
        let checked = if let Some(kind) = kind {
            let checked = args.boolean("checked").ok_or_else(unavailable)?;
            self.acknowledgements.borrow_mut().push((kind, checked));
            Some(checked)
        } else {
            self.pressed.press(operation);
            None
        };
        drop(state);
        Ok(InvocationDisposition::Pending(Rc::new(move || {
            poll(&current, revision, operation, tour_page, checked)
        })))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Queued,
    Applied,
    Refused,
}

/// Private reducer input, never a serialized credential or authority claim.
pub(super) struct Request {
    revision: u64,
    phase: &'static str,
    operation: &'static str,
    source_operation: Option<String>,
    migration_identity: Option<serde_json::Value>,
    password: Option<Zeroizing<String>>,
    delivered: Rc<RefCell<Delivery>>,
    backup: Option<BackupRequest>,
    acknowledge_without_backup: bool,
}

struct BackupRequest {
    destination: Zeroizing<String>,
    passphrase: Zeroizing<String>,
    confirmation: Zeroizing<String>,
}

fn queued_operation(operation: &str) -> bool {
    operation == "prepared.import"
        || operation.starts_with("migration.")
        || matches!(operation, "backup.save" | "backup.continue")
}

fn migration_operations(status: &serde_json::Value) -> Vec<(&'static str, &'static str)> {
    [
        ("migration.review", "onboarding-migration-review"),
        ("migration.start", "onboarding-migration-start"),
        ("migration.adopt", "onboarding-migration-adopt"),
        ("migration.retry", "onboarding-migration-retry"),
        (
            "migration.cleanup-resume",
            "onboarding-migration-cleanup-resume",
        ),
        ("migration.cancel", "onboarding-migration-cancel"),
        ("migration.continue", "onboarding-continue"),
        ("migration.later", "onboarding-migration-later"),
        ("migration.resume", "onboarding-migration-title"),
    ]
    .into_iter()
    .filter(|(op, _)| migration_available(status, op))
    .map(|(op, label)| {
        let label = match op {
            "migration.start" if status["browser"] == true => "onboarding-browser-start",
            "migration.start" if status["portable_applications"] == true => {
                "onboarding-portable-applications-start"
            }
            "migration.start" if status["retained_access"] == true => {
                "onboarding-existing-files-prepare-apps"
            }
            "migration.continue" if status["retained_access"] == true => {
                "onboarding-existing-files-use"
            }
            _ => label,
        };
        (op, label)
    })
    .collect()
}

fn migration_available(status: &serde_json::Value, operation: &str) -> bool {
    let state = status["state"].as_str().unwrap_or("unavailable");
    let cleanup_pending =
        status["transfer_complete"] == true && status["cleanup_state"] != "finished";
    match operation {
        "migration.review" => {
            status["can_review"] == true
                && matches!(state, "pending" | "unavailable" | "failed" | "cancelled")
        }
        "migration.start" => state == "reviewed",
        "migration.adopt" => state == "ready-to-adopt",
        "migration.retry" => matches!(state, "failed" | "cancelled" | "incomplete"),
        "migration.cleanup-resume" => {
            status["transfer_complete"] == true && status["cleanup_state"] == "pending"
        }
        "migration.cancel" => {
            matches!(
                state,
                "copying"
                    | "restoring-applications"
                    | "adopting"
                    | "activating-browser"
                    | "reviewing"
            ) || (status["original_copy"] == true
                && matches!(
                    state,
                    "verifying" | "retirement-pending" | "releasing-applications"
                ))
        }
        "migration.continue" => {
            !cleanup_pending
                && (state == "existing-files-available"
                    || (status["retained_access"] != true
                        && matches!(state, "complete" | "incomplete")))
        }
        "migration.later" => {
            !cleanup_pending
                && matches!(
                    state,
                    "pending"
                        | "reviewed"
                        | "ready-to-adopt"
                        | "failed"
                        | "cancelled"
                        | "complete"
                        | "incomplete"
                        | "existing-files-available"
                )
        }
        "migration.resume" => state == "deferred",
        _ => false,
    }
}

fn request_params(snapshot: &Snapshot, operation: &str) -> Vec<ActionParam> {
    if operation == "backup.save" {
        return ["destination", "passphrase", "confirmation"]
            .into_iter()
            .map(|name| {
                ActionParam::required_owned(
                    name,
                    ParamType::Secret,
                    t("onboarding-backup-save-action"),
                )
            })
            .collect();
    }
    if operation == "backup.continue" {
        return vec![ActionParam::optional_owned(
            "acknowledge_without_backup",
            ParamType::Boolean,
            t("onboarding-backup-skip-acknowledgement"),
        )];
    }
    if operation == "prepared.import" {
        return vec![ActionParam::required_owned(
            "password",
            ParamType::Secret,
            t("onboarding-prepared-password"),
        )];
    }
    let Some(migration) = snapshot.migration.as_ref() else {
        return vec![];
    };
    let portable = migration["portable_home"] == true
        || migration["portable_applications"] == true
        || migration["browser"] == true;
    if portable
        && matches!(
            operation,
            "migration.review" | "migration.retry" | "migration.adopt" | "migration.start"
        )
    {
        let label = t("onboarding-prepared-password");
        let mut params = vec![if migration["requires_password"] == true {
            ActionParam::required_owned("password", ParamType::Secret, label)
        } else {
            ActionParam::optional_owned("password", ParamType::Secret, label)
        }];
        if operation == "migration.review" {
            params.push(ActionParam::optional_owned(
                "operation",
                ParamType::Text,
                t("onboarding-migration-review"),
            ));
        }
        return params;
    }
    if operation == "migration.review" {
        vec![ActionParam::optional_owned(
            "operation",
            ParamType::Text,
            t("onboarding-migration-review"),
        )]
    } else {
        vec![]
    }
}

fn migration_identity(value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"operations":value["operations"], "selected":value["selected"],
        "setup_binding":value["setup_binding"], "portable_home":value["portable_home"],
        "portable_applications":value["portable_applications"], "browser":value["browser"], "retained_access":value["retained_access"]})
}

impl Provider {
    fn enqueue(
        &self,
        current: &Rc<RefCell<Snapshot>>,
        state: &Snapshot,
        operation: &'static str,
        args: &ActionArgs,
    ) -> Result<InvocationDisposition, ActionError> {
        if !self.requests.borrow().is_empty() {
            return Err(unavailable());
        }
        let params = request_params(state, operation);
        let password = args.text("password");
        if password.is_some_and(|text| text.is_empty() || text.len() > 4096 || text.contains('\0'))
            || (params
                .iter()
                .any(|param| param.name == "password" && param.required)
                && password.is_none())
        {
            return Err(unavailable());
        }
        if let Some(selected) = args.text("operation") {
            let offered = state
                .migration
                .as_ref()
                .and_then(|status| status["operations"].as_array());
            if !offered.is_some_and(|operations| {
                operations
                    .iter()
                    .any(|value| value.as_str() == Some(selected))
            }) {
                return Err(unavailable());
            }
        }
        let backup = if operation == "backup.save" {
            let read = |name| {
                args.text(name)
                    .filter(|value| {
                        !value.is_empty() && value.len() <= 4096 && !value.contains('\0')
                    })
                    .map(|value| Zeroizing::new(value.to_owned()))
                    .ok_or_else(unavailable)
            };
            Some(BackupRequest {
                destination: read("destination")?,
                passphrase: read("passphrase")?,
                confirmation: read("confirmation")?,
            })
        } else {
            None
        };
        let delivered = Rc::new(RefCell::new(Delivery::Queued));
        self.requests.borrow_mut().push(Request {
            revision: state.revision,
            phase: state.phase,
            operation,
            source_operation: args
                .text("operation")
                .or_else(|| {
                    state.migration.as_ref().and_then(|value| {
                        value["selected"]
                            .as_str()
                            .or_else(|| value["operations"].as_array()?.first()?.as_str())
                    })
                })
                .map(str::to_owned),
            migration_identity: state.migration.as_ref().map(migration_identity),
            password: password.map(|value| Zeroizing::new(value.to_owned())),
            delivered: delivered.clone(),
            backup,
            acknowledge_without_backup: args.boolean("acknowledge_without_backup").unwrap_or(false),
        });
        let snapshot = current.clone();
        let revision = state.revision;
        Ok(InvocationDisposition::Pending(Rc::new(move || {
            poll_queued(&snapshot, revision, &delivered, operation)
        })))
    }
}

fn poll_queued(
    snapshot: &RefCell<Snapshot>,
    revision: u64,
    delivered: &RefCell<Delivery>,
    operation: &str,
) -> Result<Option<ActionReceipt>, ActionError> {
    match *delivered.borrow() {
        Delivery::Queued => return Ok(None),
        Delivery::Refused => return Err(unavailable()),
        Delivery::Applied => {}
    }
    let state = snapshot.borrow();
    if state.phase == "closed" {
        return Err(unavailable());
    }
    if state.revision == revision {
        return Ok(None);
    }
    if operation == "backup.continue" && state.backup.is_some() {
        return Ok(None);
    }
    if operation == "prepared.import" {
        match state.prepared {
            Some("busy") => return Ok(None),
            Some(_) => return Err(ActionError::Explained(t("onboarding-semantic-failed"))),
            None if state.identity_ready => {}
            None => return Err(unavailable()),
        }
    }
    // Migration initiation is observed reducer delivery, not a claim
    // that its worker copied, adopted or retired any source. The exact
    // status accompanies the receipt, and subsequent steps stay gated.
    Ok(Some(ActionReceipt::completed_with(state.value())))
}

/// Run after widget sync and before the normal prepared/migration reducer.
/// A queued old view cannot acquire authority from a later frame's state.
pub(super) fn drain(view: &mut OnboardingView) {
    let requests = std::mem::take(&mut *view.semantic_requests.borrow_mut());
    for request in requests {
        let current = view.semantic.borrow();
        let valid = current.revision == request.revision && current.phase == request.phase;
        drop(current);
        let observed = Snapshot::from_view(view);
        let same_operation =
            observed.migration.as_ref().map(migration_identity) == request.migration_identity;
        if !valid
            || observed.phase != request.phase
            || !same_operation
            || !observed
                .operations
                .iter()
                .any(|(name, _)| *name == request.operation)
        {
            *request.delivered.borrow_mut() = Delivery::Refused;
            continue;
        }
        let result = if request.operation == "backup.save" {
            request.backup.as_ref().ok_or(()).and_then(|backup| {
                view.submit_backup(
                    &backup.destination,
                    backup.passphrase.as_bytes(),
                    backup.confirmation.as_bytes(),
                )
            })
        } else if request.operation == "backup.continue" {
            view.continue_backup(request.acknowledge_without_backup)
        } else if request.operation == "prepared.import" {
            request
                .password
                .as_ref()
                .ok_or(())
                .and_then(|password| view.submit_prepared_setup_secret(password.as_bytes()))
        } else if let Some(op) = request.operation.strip_prefix("migration.") {
            request
                .password
                .as_ref()
                .map_or(Ok(()), |password| {
                    view.submit_migration_secret(password.as_bytes())
                })
                .and_then(|()| view.invoke_migration(op, request.source_operation.as_deref()))
        } else {
            Err(())
        };
        *request.delivered.borrow_mut() = if result.is_ok() {
            Delivery::Applied
        } else {
            Delivery::Refused
        };
    }
}

fn acknowledgement(operation: &str) -> Option<AcknowledgementKind> {
    match operation {
        KEY_UNDERSTOOD => Some(AcknowledgementKind::IdentityProtection),
        KEY_BACKUP_SKIP_ACKNOWLEDGED => Some(AcknowledgementKind::SkipBackup),
        _ => None,
    }
}

fn poll(
    current: &RefCell<Snapshot>,
    revision: u64,
    operation: &str,
    previous_tour: Option<TourPage>,
    checked: Option<bool>,
) -> Result<Option<ActionReceipt>, ActionError> {
    let state = current.borrow();
    if state.revision == revision {
        return Ok(None);
    }
    if (state.failed && checked.is_none()) || state.phase == "closed" {
        return Err(ActionError::Explained(t("onboarding-semantic-failed")));
    }
    if operation == KEY_CODECS_INSTALL {
        if matches!(state.codec_completion, CodecCompletion::Running) {
            return Ok(None);
        }
        if !matches!(state.codec_completion, CodecCompletion::Succeeded) {
            return Err(ActionError::Explained(t("onboarding-semantic-failed")));
        }
    } else if !effect_observed(&state, operation, previous_tour, checked) {
        // The normal reducer ran, but did not perform this requested effect.
        // A later unrelated frame cannot turn a stale/no-op action into success.
        return Err(unavailable());
    }
    Ok(Some(ActionReceipt::completed_with(state.value())))
}

fn effect_observed(
    state: &Snapshot,
    operation: &str,
    previous_tour: Option<TourPage>,
    checked: Option<bool>,
) -> bool {
    match operation {
        KEY_UNDERSTOOD => checked == Some(state.acknowledgements.protection),
        KEY_BACKUP_SKIP_ACKNOWLEDGED => checked == Some(state.acknowledgements.skip_backup),
        KEY_BEGIN => state.phase == "protection",
        KEY_EXISTING => state.phase == "existing",
        KEY_CHOOSE_RESTORE => state.phase == "restore",
        KEY_CHOOSE_RECOVERY_KEY => state.phase == "recovery-key",
        KEY_CHOOSE_NEARBY => state.phase == "nearby" && state.nearby_active,
        KEY_CONNECT_CANCEL => state.phase == "nearby" && !state.nearby_active,
        KEY_CREATE => state.phase == "backup",
        KEY_BACKUP_CONTINUE => matches!(state.phase, "ready" | "codecs" | "finished"),
        KEY_CODECS_SKIP | KEY_CODECS_CONTINUE => state.phase == "ready",
        KEY_TOUR_START => state.tour_page == Some(TourPage::Navigate),
        KEY_TOUR_NEXT => {
            state.tour_page
                == match previous_tour {
                    Some(TourPage::Navigate) => Some(TourPage::Organise),
                    Some(TourPage::Organise) => Some(TourPage::Collaborate),
                    Some(TourPage::Collaborate) => Some(TourPage::Start),
                    _ => return false,
                }
        }
        KEY_TOUR_BACK => match previous_tour {
            Some(TourPage::Navigate) => matches!(state.phase, "ready" | "finished"),
            Some(TourPage::Organise) => state.tour_page == Some(TourPage::Navigate),
            Some(TourPage::Collaborate) => state.tour_page == Some(TourPage::Organise),
            Some(TourPage::Start) => state.tour_page == Some(TourPage::Collaborate),
            None => false,
        },
        KEY_TOUR_SKIP | KEY_FINISH => state.phase == "finished",
        KEY_BACK => matches!(state.phase, "intro" | "existing"),
        _ => false,
    }
}

impl SearchProvider for Provider {
    fn id(&self) -> &'static str {
        "onboarding"
    }
    fn name(&self) -> &'static str {
        "Identity setup"
    }
    fn display_name(&self) -> String {
        intro_title()
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !authorized(ctx.chrome) || !ctx.wanted.contains(&ResourceKind::Action) {
            return vec![];
        }
        let Some(snapshot) = self.snapshot.upgrade() else {
            return vec![];
        };
        let current = snapshot.borrow();
        if matches!(current.phase, "finished" | "closed") {
            return vec![];
        }
        let phase = current.phase;
        let mut operations = current.operations.clone();
        operations.insert(0, ("inspect", "onboarding-intro-title"));
        let rows: Vec<_> = operations
            .into_iter()
            .map(|(operation, label)| self.row(&ctx, phase, operation, label))
            .collect();
        let fields = rows
            .iter()
            .map(|row| {
                vec![
                    (row.title.clone(), ui::search::FieldWeight::TITLE),
                    (row.id.clone(), ui::search::FieldWeight::IDENTITY),
                    (self.id().into(), ui::search::FieldWeight::IDENTITY),
                ]
            })
            .collect::<Vec<_>>();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle, rows, &fields)
    }
}

/// Test adapter over the production provider, binder, admission and reducer.
/// `accepted` reports an actual pending ticket; it never invents a completion.
#[cfg(test)]
pub(super) fn test_dispatch(
    bytes: &[u8],
    view: &mut OnboardingView,
    _unused: Option<()>,
) -> String {
    use serde_json::json;
    let refusal = |code: &str| json!({"err":{"code":code}}).to_string();
    let Ok(request) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return refusal("invalid_arguments");
    };
    let chrome = test_owner_chrome();
    view.publish_semantic_state();
    let provider = Provider::new(view);
    let mut doc = automerge::AutoCommit::new();
    let rows = provider.query(SearchContext {
        query_scope: None,
        include_programmatic: true,
        needle: "",
        wanted: &[ResourceKind::Action],
        chrome: &chrome.borrow(),
        doc: &doc,
        doc_root: &automerge::ROOT,
    });
    let scope = {
        let snapshot = view.semantic.borrow();
        format!("{}:{:?}", snapshot.phase, snapshot.tour_page)
    };
    let handle = |row: &SearchResult| {
        if row.id.starts_with("onboarding.") {
            format!("{}@{scope}", row.id)
        } else {
            row.id.clone()
        }
    };
    if request["op"] == "actions_enumerate" {
        return json!({"answer":{"actions":rows.iter().map(|row| json!({"provider":row.provider_id,"id":handle(row),"operation":row.id,"params":row.params})).collect::<Vec<_>>()}}).to_string();
    }
    let selected = if request["op"] == "actions_status" {
        rows.iter().find(|row| row.id == "inspect")
    } else if request["op"] == "actions_invoke" && request["provider"] == "onboarding" {
        rows.iter()
            .find(|row| request["id"].as_str() == Some(handle(row).as_str()))
    } else {
        None
    };
    let Some(row) = selected else {
        return refusal("action_failed");
    };
    let empty = serde_json::Map::new();
    let supplied = if request["op"] == "actions_status" {
        &empty
    } else {
        let Some(args) = request["args"].as_object() else {
            return refusal("invalid_arguments");
        };
        args
    };
    let Ok(args) = ActionArgs::bind_json(&row.params, supplied) else {
        return refusal("invalid_arguments");
    };
    let Some(begin) = row.on_begin_invoke.as_ref() else {
        return refusal("action_failed");
    };
    let disposition = match begin(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &chrome,
        &args,
    ) {
        Ok(disposition) => disposition,
        Err(error) => return refusal(error.code()),
    };
    if let InvocationDisposition::Accepted(receipt) = &disposition
        && request["op"] == "actions_status"
    {
        if let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &receipt.update {
            return json!({"answer":output.value}).to_string();
        }
        return refusal("action_failed");
    }
    drain(view);
    view.publish_semantic_state();
    match disposition {
        InvocationDisposition::Pending(_) if !queued_operation(&row.id) => {
            serde_json::json!({"answer":{"status":"accepted","pending":true}}).to_string()
        }
        InvocationDisposition::Pending(poll) => match poll() {
            Err(error) => refusal(error.code()),
            Ok(receipt) => json!({"answer":{"status":"accepted","pending":receipt.is_none(),"receipt":receipt}}).to_string(),
        },
        InvocationDisposition::Accepted(receipt) => json!({"answer":{"status":"accepted","receipt":receipt}}).to_string(),
    }
}

#[cfg(test)]
fn test_owner_chrome() -> crate::shell::chrome_state::Chrome {
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{
        FreshId as _, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "First-use semantic fixture");
    workspace.set_owner(crate::shell::state::local_user_id_for_room(
        &workspace.room(),
    ));
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    ChromeState::new_with_handle(WorkspaceTreeHandle::new(tree))
}

/// Observe the live first-use provider through its real read action. A finished
/// or closed view has no published action, rather than a parallel pending flag.
#[cfg(test)]
pub(super) fn test_observe(view: &OnboardingView) -> Option<serde_json::Value> {
    view.publish_semantic_state();
    let provider = Provider::new(view);
    let chrome = test_owner_chrome();
    let mut doc = automerge::AutoCommit::new();
    let rows = provider.query(SearchContext {
        query_scope: None,
        include_programmatic: true,
        needle: "",
        wanted: &[ResourceKind::Action],
        chrome: &chrome.borrow(),
        doc: &doc,
        doc_root: &automerge::ROOT,
    });
    let inspect = rows.iter().find(|row| row.id == "inspect")?;
    let begin = inspect.on_begin_invoke.as_ref()?;
    let disposition = begin(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &chrome,
        &ActionArgs::none(),
    )
    .expect("published owner inspect action");
    match disposition {
        InvocationDisposition::Accepted(receipt) => match receipt.update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => output.value,
            _ => panic!("owner inspect must report its actual completed observation"),
        },
        InvocationDisposition::Pending(_) => panic!("inspect has no asynchronous side effect"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::action_api::ArgValue;

    #[test]
    fn prepared_import_requires_typed_secret_and_never_exposes_it_in_snapshot() {
        let snapshot = Snapshot {
            prepared: Some("ready"),
            ..Default::default()
        };
        let params = request_params(&snapshot, "prepared.import");
        assert_eq!(params[0].ty, ParamType::Secret);
        assert!(
            ActionArgs::bind(
                &params,
                [("password".into(), ArgValue::Text("private words".into()))]
            )
            .is_err()
        );
        let args = ActionArgs::bind(
            &params,
            [("password".into(), ArgValue::Secret("private words".into()))],
        )
        .expect("secret bound by canonical binder");
        assert_eq!(args.text("password"), Some("private words"));
        assert!(!snapshot.value().to_string().contains("private words"));
    }

    #[test]
    fn retained_continue_is_absent_until_apps_and_source_cleanup_finish() {
        let mut status = serde_json::json!({"state":"existing-files-available", "retained_access":true, "transfer_complete":true, "cleanup_state":"pending"});
        let offered = |status: &serde_json::Value, op: &str| {
            migration_operations(status)
                .iter()
                .any(|(name, _)| *name == op)
        };
        assert!(offered(&status, "migration.cleanup-resume"));
        assert!(!offered(&status, "migration.continue"));
        assert!(!offered(&status, "migration.later"));
        status["cleanup_state"] = "running".into();
        assert!(!offered(&status, "migration.continue"));
        status["cleanup_state"] = "finished".into();
        assert!(offered(&status, "migration.continue"));
        status["state"] = "complete".into();
        assert!(
            !offered(&status, "migration.continue"),
            "retained copy completion does not establish actual writable Files availability"
        );
    }

    #[test]
    fn actual_first_use_scope_closes_even_when_a_pending_ticket_keeps_snapshot_alive() {
        let view = OnboardingView::with_minter(Box::new(|| Err("no identity requested".into())));
        let scope = authentication_first_use_scope(&view);
        let pending_snapshot = view.semantic.clone();
        assert!(scope());
        drop(view);
        assert_eq!(pending_snapshot.borrow().phase, "closed");
        assert!(
            !scope(),
            "held old action state cannot continue first-use root-delivery eligibility"
        );
    }

    #[test]
    fn actual_view_refuses_stale_prepared_secret_without_touching_reducer() {
        let directory = tempfile::TempDir::new().expect("isolated first-use identity directory");
        let mut view = OnboardingView::isolated(directory.path().to_path_buf(), false);
        view.publish_semantic_state();
        let revision = view.semantic.borrow().revision;
        let phase = view.semantic.borrow().phase;
        let delivered = Rc::new(RefCell::new(Delivery::Queued));
        view.semantic_requests.borrow_mut().push(Request {
            revision: revision.wrapping_sub(1),
            phase,
            operation: "prepared.import",
            source_operation: None,
            migration_identity: None,
            password: Some(Zeroizing::new("private transient words".into())),
            delivered: delivered.clone(),
            backup: None,
            acknowledge_without_backup: false,
        });
        drain(&mut view);
        assert!(*delivered.borrow() == Delivery::Refused);
        assert!(view.semantic_requests.borrow().is_empty());
        assert_eq!(view.prepared_setup_status(), None);
        assert!(
            view.connect_passphrase
                .lock()
                .expect("transient buffer")
                .is_empty()
        );
        assert_eq!(view.flow.screen(), Screen::Intro);
    }
}

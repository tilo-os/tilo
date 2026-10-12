//! First-use operations on the same registry and reducer as the rendered ceremony.
use super::{
    AcknowledgementKind, KEY_BACK, KEY_BACKUP_CONTINUE, KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BEGIN,
    KEY_CHOOSE_NEARBY, KEY_CHOOSE_RESTORE, KEY_CODECS_CONTINUE, KEY_CODECS_INSTALL,
    KEY_CODECS_SKIP, KEY_CONNECT_CANCEL, KEY_CREATE, KEY_EXISTING, KEY_FINISH, KEY_TOUR_BACK,
    KEY_TOUR_NEXT, KEY_TOUR_SKIP, KEY_TOUR_START, KEY_UNDERSTOOD, OnboardingView, PressedActions,
    TourPage,
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
}

impl Snapshot {
    pub(super) fn from_view(view: &OnboardingView) -> Self {
        let (phase, operations) = if view.finished {
            ("finished", vec![])
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
            }
        };
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
        }
    }

    fn value(&self) -> serde_json::Value {
        serde_json::json!({"phase": self.phase, "protection_acknowledged": self.acknowledgements.protection,
            "backup_skip_acknowledged":self.acknowledgements.skip_backup, "failed":self.failed,
            "operations":self.operations.iter().map(|(name,_)| *name).collect::<Vec<_>>()})
    }
}

pub(super) struct Provider {
    snapshot: Weak<RefCell<Snapshot>>,
    ranker: Rc<ui::search::ListIndex>,
    pressed: PressedActions,
    acknowledgements: Rc<RefCell<Vec<(AcknowledgementKind, bool)>>>,
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
        }
    }
}

fn unavailable() -> ActionError {
    ActionError::Refused(t("onboarding-semantic-unavailable"))
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
        let params = acknowledgement(operation).map_or_else(Vec::new, |_| {
            vec![ActionParam::required_owned(
                "checked",
                ParamType::Boolean,
                title.clone(),
            )]
        });
        let provider = Self {
            snapshot: self.snapshot.clone(),
            ranker: self.ranker.clone(),
            pressed: self.pressed.clone(),
            acknowledgements: self.acknowledgements.clone(),
        };
        SearchResult {
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
                provider.begin(chrome, operation, phase, args)
            })),
        }
    }

    fn begin(
        &self,
        chrome: &crate::shell::chrome_state::Chrome,
        operation: &'static str,
        phase: &str,
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
        if state.phase != phase || !state.operations.iter().any(|(name, _)| *name == operation) {
            return Err(unavailable());
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
        return Err(ActionError::Refused(t("onboarding-semantic-failed")));
    }
    if operation == KEY_CODECS_INSTALL {
        if matches!(state.codec_completion, CodecCompletion::Running) {
            return Ok(None);
        }
        if !matches!(state.codec_completion, CodecCompletion::Succeeded) {
            return Err(ActionError::Refused(t("onboarding-semantic-failed")));
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

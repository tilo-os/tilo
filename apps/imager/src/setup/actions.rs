//! What guided setup offers the palette, agents and scripts: its effects and
//! the live rows of the setup in progress.
//!
//! Setup choices describe local navigation and a reviewable plan on this
//! computer. They are effects of the imager resource, never document
//! actions, and never carry folder paths or installation authority into the
//! shared document.

use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
    ResourcePresentation,
};
use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource::{self, ActionRef, Effect};

use super::controller::SetupPage;
use super::model::{Experience, FolderMode};

/// The live row of the setup in progress, beneath the imager resource.
pub const SETUP_ROW_ID: &str = "imager.setup";

/// Guided setup's effects. Each answers with the setup's path-free status
/// ([`SetupProjection`]) as it stands afterwards.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.setup", device_only)]
pub enum SetupEffect {
    /// Read setup status.
    #[effect(returns, kind = "read")]
    ReadSetupStatus,
    /// Read this computer's Desktop readiness and current installation phase.
    #[effect(returns, kind = "read")]
    ReadDesktopStatus,
    /// Install the exact reviewed Desktop release after a fresh host probe.
    #[effect(returns)]
    InstallDesktop,
    /// Cancel the current package download; no native transaction is cancelled.
    #[effect(returns)]
    CancelDesktopDownload,
    /// Recheck Desktop availability without installing anything.
    #[effect(returns, kind = "read")]
    RecheckDesktop,
    /// Open the existing USB image writer; writing remains a separate review.
    #[effect(view, returns, kind = "read")]
    OpenUsbWizard,
    /// Choose setup experience.
    #[effect(returns, kind = "read")]
    ChooseExperience {
        #[param("Available setup experience: desktop or full_os")]
        experience: String,
    },
    /// Set common folder mode.
    #[effect(returns, kind = "read")]
    SetCommonMode {
        #[param(
            "Folder behavior applied to folders without an override: copy, connected or later"
        )]
        mode: String,
    },
    /// Set folder mode.
    #[effect(returns, kind = "read")]
    SetFolderMode {
        #[param("Identity of a selected folder")]
        folder: String,
        #[param("Folder behavior: copy, connected or later")]
        mode: String,
    },
    /// Use the common folder mode.
    #[effect(returns, kind = "read")]
    UseCommonMode {
        #[param("Identity of a selected folder")]
        folder: String,
    },
    /// Remove folder from setup.
    #[effect(returns, kind = "read")]
    RemoveFolder {
        #[param("Identity of a selected folder")]
        folder: String,
    },
    /// Add suggested folder.
    #[effect(returns, kind = "read")]
    AddSuggestedFolder {
        #[param("Identity of a suggested folder, e.g. documents")]
        suggestion: String,
    },
    /// Choose a folder.
    #[effect(returns, kind = "read")]
    ChooseFolder,
    /// Show or hide folder help.
    #[effect(returns, kind = "read")]
    ToggleHelp,
    /// Continue setup.
    #[effect(returns, kind = "read")]
    ContinueSetup,
    /// Go back in setup.
    #[effect(returns, kind = "read")]
    GoBackInSetup,
    /// Open folder imports.
    #[effect(returns, kind = "read")]
    OpenImports,
    /// Download Tilo.
    #[effect(returns)]
    StartInternalDownload,
    /// Pause download.
    #[effect(returns)]
    PauseInternalDownload,
    /// Resume download.
    #[effect(returns)]
    ResumeInternalDownload,
    /// Cancel download.
    #[effect(returns)]
    CancelInternalDownload,
    /// Discard unfinished download data.
    #[effect(returns)]
    DiscardInternalDownload,
    /// Keep download for later.
    #[effect(returns)]
    ReleaseInternalDownload,
    /// Set Tilo size.
    #[effect(returns)]
    SetInternalTargetSize {
        #[param("Tilo size in GiB")]
        gib: i64,
    },
    /// Install Tilo alongside the current system.
    ///
    /// Installs (or continues) exactly the reviewed plan with this digest.
    #[effect(returns)]
    InstallInternal {
        #[param("Digest of the reviewed installation plan")]
        reviewed: String,
    },
    /// Restart into the Tilo installer.
    #[effect(returns)]
    RestartInternal,
    /// Restart later.
    #[effect(returns)]
    RestartInternalLater,
    /// Start the Tilo installer on the next restart.
    #[effect(returns)]
    RetryInternal {
        #[param("Id of the unfinished installer operation")]
        operation_id: String,
    },
    /// Remove the Tilo installer.
    #[effect(returns)]
    RemoveInternal {
        #[param("Id of the unfinished installer operation")]
        operation_id: String,
    },
    /// Restart to firmware settings.
    #[effect(returns)]
    InternalFirmwareSettings,
    /// Turn off Fast Startup.
    #[effect(returns)]
    InternalDisableFastStartup,
    /// Check this computer again.
    #[effect(returns, kind = "read")]
    RecheckInternal,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FolderProjection {
    pub identity: String,
    pub index: usize,
    pub mode: &'static str,
    pub overridden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SuggestionProjection {
    pub identity: String,
    pub name: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SelectionProjection {
    pub experience: Option<&'static str>,
    pub common_mode: &'static str,
    pub folder_count: usize,
    pub active_folder_count: usize,
    pub folders: Vec<FolderProjection>,
    pub suggestions: Vec<SuggestionProjection>,
    pub help_open: bool,
    pub imports_open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReviewProjection {
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AvailabilityProjection {
    pub desktop: bool,
    pub full_os: bool,
    pub connected: bool,
    pub next: bool,
    pub back: bool,
    pub folder_policies: bool,
    pub folder_edits: bool,
    pub help: bool,
    pub open_imports: bool,
    pub choose_folder: bool,
}

/// Which Full Tilo installation commands may run now.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent availability flag per published action"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalAvailability {
    pub start_download: bool,
    pub pause_download: bool,
    pub resume_download: bool,
    pub cancel_download: bool,
    pub discard_download: bool,
    pub release_download: bool,
    pub set_target_size: bool,
    pub install: bool,
    pub restart: bool,
    pub restart_later: bool,
    pub retry: bool,
    pub remove: bool,
    pub firmware_settings: bool,
    pub disable_fast_startup: bool,
    pub recheck: bool,
}

/// Path-free state of the Full Tilo installation on this computer.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalProjection {
    /// `gated`, `checking`, `no-release`, `unsupported` or `ready`.
    pub readiness: &'static str,
    pub on_review: bool,
    /// Download worker phase (`none` before any download).
    pub download: &'static str,
    pub target_gib: Option<u64>,
    pub min_gib: Option<u64>,
    pub max_gib: Option<u64>,
    /// Digest `setup.internal.install` must name; changes with the plan.
    pub reviewed: Option<String>,
    pub shrink_bytes: Option<u64>,
    pub staging_bytes: Option<u64>,
    /// Stable blocker IDs from the host probe.
    pub blockers: Vec<&'static str>,
    /// `idle`, `running`, `succeeded` or `failed`.
    pub run: &'static str,
    /// Helper action of the latest run (`stage`, `arm`, `remove`).
    pub run_action: Option<&'static str>,
    pub failure: Option<String>,
    /// Unfinished staging operation the resume card offers.
    pub pending_operation: Option<String>,
    pub availability: InternalAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SetupProjection {
    pub page: &'static str,
    pub selection: SelectionProjection,
    pub review: ReviewProjection,
    pub availability: AvailabilityProjection,
    pub internal: InternalProjection,
    pub desktop: super::desktop_journey::DesktopProjection,
    pub methods: super::desktop_journey::MethodProjection,
}

impl SetupProjection {
    #[must_use]
    pub fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

#[must_use]
pub const fn page_key(page: SetupPage) -> &'static str {
    match page {
        SetupPage::Experience => "experience",
        SetupPage::Folders => "folders",
        SetupPage::Review => "review",
    }
}

#[must_use]
pub const fn experience_key(experience: Experience) -> &'static str {
    match experience {
        Experience::Desktop => "desktop",
        Experience::FullOs => "full_os",
    }
}

#[must_use]
pub const fn mode_key(mode: FolderMode) -> &'static str {
    match mode {
        FolderMode::Copy => "copy",
        FolderMode::Connected => "connected",
        FolderMode::Later => "later",
    }
}

pub fn parse_experience(value: &str) -> Result<Experience, String> {
    match value {
        "desktop" => Ok(Experience::Desktop),
        "full_os" => Ok(Experience::FullOs),
        _ => Err(format!("unknown setup experience `{value}`")),
    }
}

pub fn parse_mode(value: &str) -> Result<FolderMode, String> {
    match value {
        "copy" => Ok(FolderMode::Copy),
        "connected" => Ok(FolderMode::Connected),
        "later" => Ok(FolderMode::Later),
        _ => Err(format!("unknown folder mode `{value}`")),
    }
}

/// An effect that cannot run now: `code` names why for programs, `reason`
/// for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub id: &'static str,
    pub code: &'static str,
    pub reason: String,
}

fn block(
    blocked: &mut Vec<Blocked>,
    open: bool,
    id: &'static str,
    code: &'static str,
    reason: String,
) {
    if !open {
        blocked.push(Blocked { id, code, reason });
    }
}

/// Why the Full Tilo commands that need the review are unavailable.
fn review_gate(state: &InternalProjection) -> (&'static str, &'static str) {
    if state.on_review {
        (
            "internal_unavailable",
            "imager-setup-internal-error-command-unavailable",
        )
    } else {
        (
            "full_os_review_only",
            "imager-setup-internal-error-not-review",
        )
    }
}

/// Each Full Tilo command: its effect id, its label, whether it may run now,
/// and the code and reason (a message key) when it may not.
fn internal_commands(
    state: &InternalProjection,
) -> [(&'static str, &'static str, bool, &'static str, &'static str); 15] {
    let a = state.availability;
    let (review_code, review_reason) = review_gate(state);
    [
        (
            SetupEffect::START_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-start-download",
            a.start_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::PAUSE_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-pause-download",
            a.pause_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::RESUME_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-resume-download",
            a.resume_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::CANCEL_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-cancel-download",
            a.cancel_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::DISCARD_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-discard-download",
            a.discard_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::RELEASE_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-release-download",
            a.release_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::SET_INTERNAL_TARGET_SIZE.id,
            "imager-setup-action-internal-set-target-size",
            a.set_target_size,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::INSTALL_INTERNAL.id,
            "imager-setup-action-internal-install",
            a.install,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::RESTART_INTERNAL.id,
            "imager-setup-action-internal-restart",
            a.restart,
            "internal_not_staged",
            "imager-setup-internal-error-no-restart",
        ),
        (
            SetupEffect::RESTART_INTERNAL_LATER.id,
            "imager-setup-action-internal-restart-later",
            a.restart_later,
            "internal_not_staged",
            "imager-setup-internal-error-no-restart",
        ),
        (
            SetupEffect::RETRY_INTERNAL.id,
            "imager-setup-action-internal-retry",
            a.retry,
            "internal_no_pending",
            "imager-setup-internal-error-no-pending",
        ),
        (
            SetupEffect::REMOVE_INTERNAL.id,
            "imager-setup-action-internal-remove",
            a.remove,
            "internal_no_pending",
            "imager-setup-internal-error-no-pending",
        ),
        (
            SetupEffect::INTERNAL_FIRMWARE_SETTINGS.id,
            "imager-setup-action-internal-firmware-settings",
            a.firmware_settings,
            "remedy_unavailable",
            "imager-setup-internal-error-remedy-unavailable",
        ),
        (
            SetupEffect::INTERNAL_DISABLE_FAST_STARTUP.id,
            "imager-setup-action-internal-disable-fast-startup",
            a.disable_fast_startup,
            "remedy_unavailable",
            "imager-setup-internal-error-remedy-unavailable",
        ),
        (
            SetupEffect::RECHECK_INTERNAL.id,
            "imager-setup-action-internal-recheck",
            a.recheck,
            "internal_unavailable",
            "imager-setup-internal-error-unavailable",
        ),
    ]
}

/// The setup effects that cannot run in `state`, with why.
#[must_use]
pub fn blocked(state: &SetupProjection) -> Vec<Blocked> {
    let a = state.availability;
    let folders_closed = || "Folder choices are not open on the current setup page".to_owned();
    let mut blocked = Vec::new();
    block(
        &mut blocked,
        a.desktop || a.full_os,
        SetupEffect::CHOOSE_EXPERIENCE.id,
        "no_setup_experience",
        "No setup experience is available on this computer".into(),
    );
    block(
        &mut blocked,
        a.folder_policies,
        SetupEffect::SET_COMMON_MODE.id,
        "folder_choices_not_open",
        folders_closed(),
    );
    for id in [
        SetupEffect::SET_FOLDER_MODE.id,
        SetupEffect::USE_COMMON_MODE.id,
        SetupEffect::REMOVE_FOLDER.id,
    ] {
        block(
            &mut blocked,
            a.folder_edits,
            id,
            "folder_choices_not_open",
            folders_closed(),
        );
    }
    block(
        &mut blocked,
        a.choose_folder,
        SetupEffect::ADD_SUGGESTED_FOLDER.id,
        "folder_choices_not_open",
        folders_closed(),
    );
    block(
        &mut blocked,
        a.choose_folder,
        SetupEffect::CHOOSE_FOLDER.id,
        "folder_picker_unavailable",
        "The folder picker is not available on this setup page".into(),
    );
    block(
        &mut blocked,
        a.help,
        SetupEffect::TOGGLE_HELP.id,
        "help_not_on_page",
        "Folder help is available on the folder page".into(),
    );
    block(
        &mut blocked,
        a.next,
        SetupEffect::CONTINUE_SETUP.id,
        "setup_cannot_continue",
        "Complete the current setup choice before continuing".into(),
    );
    block(
        &mut blocked,
        a.back,
        SetupEffect::GO_BACK_IN_SETUP.id,
        "setup_at_start",
        "Setup is already on its first page".into(),
    );
    block(
        &mut blocked,
        a.open_imports,
        SetupEffect::OPEN_IMPORTS.id,
        "imports_not_on_page",
        "Folder imports are available on the experience and review pages".into(),
    );
    for (id, _, open) in super::desktop_journey::commands(&state.desktop, &state.methods) {
        block(
            &mut blocked,
            open,
            id,
            "desktop_command_unavailable",
            super::strings::t("imager-setup-desktop-command-unavailable"),
        );
    }
    for (id, _, open, code, reason) in internal_commands(&state.internal) {
        block(&mut blocked, open, id, code, super::strings::t(reason));
    }
    blocked
}

/// The setup effects as callers see them now: what is unavailable, and the
/// Full Tilo commands in the person's language.
#[must_use]
pub fn statuses(state: &SetupProjection) -> Vec<EffectStatus> {
    let effect = |id| ActionRef::<SetupEffect, ()>::new(id, &[]);
    blocked(state)
        .into_iter()
        .map(|blocked| EffectStatus::unavailable(effect(blocked.id), blocked.code, blocked.reason))
        .chain(
            internal_commands(&state.internal)
                .into_iter()
                .map(|(id, label, ..)| {
                    EffectStatus::labelled(effect(id), super::strings::t(label))
                }),
        )
        .chain(
            super::desktop_journey::commands(&state.desktop, &state.methods)
                .into_iter()
                .map(|(id, label, _)| EffectStatus::labelled(effect(id), super::strings::t(label))),
        )
        .collect()
}

fn gated(spec: ActionSpec, available: bool, code: &str, reason: &str) -> ActionSpec {
    if available {
        spec
    } else {
        spec.unavailable(code, reason)
    }
}

/// `effect` bound to the row it is offered on, as `label`.
fn bound(effect: &SetupEffect, label: impl Into<String>) -> ActionSpec {
    let mut spec = effect.bound();
    spec.label = label.into();
    spec
}

/// The effect `id` offered on a row with what the row knows already bound
/// by `build`, taking `param` from the caller.
fn partly_bound(
    id: &'static str,
    label: impl Into<String>,
    placeholder: &str,
    param: ActionParam,
    build: impl Fn(&ActionArgs) -> SetupEffect + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label,
        placeholder,
        OperationKind::Effect,
        SetupEffect::OPERATION,
        vec![param],
        move |args| {
            build(args)
                .bound()
                .build_json(&serde_json::Map::new())
                .unwrap_or_else(|_| EncodedAction {
                    type_id: SetupEffect::OPERATION.to_owned(),
                    payload: Vec::new(),
                })
        },
    );
    if let Some(declared) = SetupEffect::specs().into_iter().find(|spec| spec.id == id) {
        spec.contract = declared.contract;
    }
    spec
}

fn folder_row(folder: &FolderProjection, mode_choices: &[&'static str], edits: bool) -> LiveRow {
    let closed = "Folder choices are not open on the current setup page";
    let identity = folder.identity.clone();
    let set_mode = partly_bound(
        SetupEffect::SET_FOLDER_MODE.id,
        "Set folder mode",
        "copy",
        ActionParam::required_owned("mode", ParamType::Text, "Folder behavior")
            .with_choices(mode_choices.iter().copied()),
        move |args| SetupEffect::SetFolderMode {
            folder: identity.clone(),
            mode: args.text("mode").unwrap_or("").to_owned(),
        },
    );
    let (inherit_code, inherit_reason) = if edits {
        (
            "folder_mode_inherited",
            "This folder already follows the common choice",
        )
    } else {
        ("folder_choices_not_open", closed)
    };
    LiveRow::new(
        format!("{SETUP_ROW_ID}.folder.{}", folder.identity),
        "imager.setup.folder",
        format!("Selected folder {}", folder.index + 1),
    )
    .presentation(ResourcePresentation {
        detail: Some(format!(
            "{}, {}",
            mode_phrase(folder.mode),
            if folder.overridden {
                "chosen for this folder"
            } else {
                "the common choice"
            }
        )),
        badge: Some(format!("Folder {}", folder.index + 1)),
        preview: None,
    })
    .operation(gated(set_mode, edits, "folder_choices_not_open", closed))
    .operation(gated(
        bound(
            &SetupEffect::UseCommonMode {
                folder: folder.identity.clone(),
            },
            "Use the common folder mode",
        ),
        edits && folder.overridden,
        inherit_code,
        inherit_reason,
    ))
    .operation(gated(
        bound(
            &SetupEffect::RemoveFolder {
                folder: folder.identity.clone(),
            },
            "Remove folder from setup",
        ),
        edits,
        "folder_choices_not_open",
        closed,
    ))
}

fn suggestion_row(suggestion: &SuggestionProjection, choices_open: bool) -> LiveRow {
    let (code, reason) = if choices_open {
        (
            "suggestion_unavailable",
            suggestion
                .unavailable_reason
                .as_deref()
                .unwrap_or("This suggested folder is unavailable"),
        )
    } else {
        (
            "folder_choices_not_open",
            "Folder choices are not open on the current setup page",
        )
    };
    LiveRow::new(
        format!("{SETUP_ROW_ID}.suggestion.{}", suggestion.identity),
        "imager.setup.suggestion",
        format!("Suggested {} folder", suggestion.name),
    )
    .presentation(ResourcePresentation {
        detail: Some(
            if suggestion.available {
                "Available"
            } else {
                "Unavailable"
            }
            .into(),
        ),
        badge: Some("Suggestion".into()),
        preview: None,
    })
    .operation(gated(
        bound(
            &SetupEffect::AddSuggestedFolder {
                suggestion: suggestion.identity.clone(),
            },
            "Add suggested folder",
        ),
        choices_open && suggestion.available,
        code,
        reason,
    ))
}

/// The Full Tilo installation on this computer: one operation per visible
/// command, bound to the plan and the unfinished operation shown.
fn internal_row(state: &InternalProjection) -> LiveRow {
    let operation = state.pending_operation.clone().unwrap_or_default();
    let mut row = LiveRow::new(
        format!("{SETUP_ROW_ID}.internal"),
        "imager.setup.internal",
        super::strings::t("imager-setup-resource-internal"),
    )
    .presentation(ResourcePresentation {
        detail: Some(super::strings::t_args(
            "imager-setup-detail-internal",
            &[
                ("readiness", state.readiness.into()),
                ("download", state.download.into()),
                ("run", state.run.into()),
                (
                    "blockers",
                    if state.blockers.is_empty() {
                        "none".to_string()
                    } else {
                        state.blockers.join(", ")
                    }
                    .into(),
                ),
            ],
        )),
        badge: Some(super::strings::t("imager-setup-badge-internal")),
        preview: None,
    });
    for (id, label, open, code, reason) in internal_commands(state) {
        let label = super::strings::t(label);
        let spec = match id {
            id if id == SetupEffect::SET_INTERNAL_TARGET_SIZE.id => partly_bound(
                id,
                label,
                "64",
                ActionParam::required_owned(
                    "gib",
                    ParamType::Integer,
                    super::strings::t("imager-setup-param-internal-target-gib"),
                ),
                |args| SetupEffect::SetInternalTargetSize {
                    gib: args.integer("gib").unwrap_or(-1),
                },
            ),
            id if id == SetupEffect::INSTALL_INTERNAL.id => bound(
                &SetupEffect::InstallInternal {
                    reviewed: state.reviewed.clone().unwrap_or_default(),
                },
                label,
            ),
            id if id == SetupEffect::RETRY_INTERNAL.id => bound(
                &SetupEffect::RetryInternal {
                    operation_id: operation.clone(),
                },
                label,
            ),
            id if id == SetupEffect::REMOVE_INTERNAL.id => bound(
                &SetupEffect::RemoveInternal {
                    operation_id: operation.clone(),
                },
                label,
            ),
            id => match SetupEffect::decode(id, &serde_json::Map::new()) {
                Ok(effect) => bound(&effect, label),
                Err(_) => continue,
            },
        };
        row = row.operation(gated(spec, open, code, &super::strings::t(reason)));
    }
    row
}

/// The setup in progress as one live row beneath the imager resource: the
/// selection with its folders, the suggested folders, the review, what may
/// run now, and the Full Tilo installation.
#[must_use]
pub fn row(state: &SetupProjection) -> LiveRow {
    let mode_choices = [
        Some("copy"),
        state.availability.connected.then_some("connected"),
        Some("later"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let mut selection = LiveRow::new(
        format!("{SETUP_ROW_ID}.selection"),
        "imager.setup.selection",
        "Current setup selection",
    )
    .presentation(ResourcePresentation {
        detail: Some(format!(
            "{}, {} by default, {}{}. With help {} and imports {}.",
            experience_phrase(state.selection.experience),
            mode_phrase(state.selection.common_mode).to_lowercase(),
            folders_phrase(state.selection.folder_count),
            if state.selection.folder_count == 0 {
                String::new()
            } else {
                format!(", {} to bring over", state.selection.active_folder_count)
            },
            if state.selection.help_open {
                "open"
            } else {
                "closed"
            },
            if state.selection.imports_open {
                "open"
            } else {
                "closed"
            },
        )),
        badge: Some("Selection".into()),
        preview: None,
    });
    for folder in &state.selection.folders {
        selection = selection.row(folder_row(
            folder,
            &mode_choices,
            state.availability.folder_edits,
        ));
    }
    let mut suggestions = LiveRow::new(
        format!("{SETUP_ROW_ID}.suggestions"),
        "imager.setup.suggestions",
        "Suggested folders",
    );
    for suggestion in &state.selection.suggestions {
        suggestions = suggestions.row(suggestion_row(suggestion, state.availability.choose_folder));
    }
    let review = LiveRow::new(
        format!("{SETUP_ROW_ID}.review"),
        "imager.setup.review",
        "Setup review state",
    )
    .presentation(ResourcePresentation {
        detail: Some(format!(
            "Review ready: {}. Current page: {}.",
            yes_no(state.review.ready),
            state.page
        )),
        badge: Some("Review".into()),
        preview: None,
    });
    let availability = LiveRow::new(
        format!("{SETUP_ROW_ID}.availability"),
        "imager.setup.availability",
        "Current setup availability",
    )
    .presentation(ResourcePresentation {
        detail: Some(format!(
            "Available experiences: desktop {0}, full OS {1}. Connected: {2}. Navigation: next {3}, back {4}. Folder permissions: policies {5}, edits {6}. Help: {7}. Imports: {8}.",
            yes_no(state.availability.desktop),
            yes_no(state.availability.full_os),
            yes_no(state.availability.connected),
            yes_no(state.availability.next),
            yes_no(state.availability.back),
            yes_no(state.availability.folder_policies),
            yes_no(state.availability.folder_edits),
            yes_no(state.availability.help),
            yes_no(state.availability.open_imports),
        )),
        badge: Some("Availability".into()),
        preview: None,
    });
    LiveRow::new(SETUP_ROW_ID, "imager.setup", "Guided Tilo setup")
        .presentation(ResourcePresentation {
            detail: Some(format!(
                "{} page, {}, {}",
                match state.page {
                    "experience" => "Experience",
                    "folders" => "Folders",
                    "review" => "Review",
                    _ => "Setup",
                },
                experience_phrase(state.selection.experience),
                folders_phrase(state.selection.folder_count),
            )),
            badge: Some(
                match state.page {
                    "experience" => "Experience",
                    "folders" => "Folders",
                    "review" => "Review",
                    _ => "Setup",
                }
                .into(),
            ),
            preview: None,
        })
        .row(selection)
        .row(suggestions)
        .row(review)
        .row(availability)
        .row(internal_row(&state.internal))
        .row(super::desktop_journey::row(&state.desktop, &state.methods))
}

/// A folder mode as people read it.
fn mode_phrase(mode: &str) -> &'static str {
    match mode {
        "connected" => "Connected",
        "later" => "Decide later",
        _ => "Copy",
    }
}

/// The chosen experience as people read it.
fn experience_phrase(experience: Option<&str>) -> &'static str {
    match experience {
        Some("desktop") => "Tilo Desktop",
        Some(_) => "Full Tilo",
        None => "no experience chosen yet",
    }
}

/// "1 folder", "3 folders", "no folders".
fn folders_phrase(count: usize) -> String {
    match count {
        0 => "no folders".into(),
        1 => "1 folder".into(),
        count => format!("{count} folders"),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "Yes" } else { "No" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection(internal: InternalProjection) -> SetupProjection {
        SetupProjection {
            page: "review",
            selection: SelectionProjection {
                experience: Some("full_os"),
                common_mode: "copy",
                folder_count: 0,
                active_folder_count: 0,
                folders: Vec::new(),
                suggestions: Vec::new(),
                help_open: false,
                imports_open: false,
            },
            review: ReviewProjection { ready: true },
            availability: AvailabilityProjection {
                desktop: false,
                full_os: false,
                connected: false,
                next: false,
                back: true,
                folder_policies: false,
                folder_edits: false,
                help: false,
                open_imports: true,
                choose_folder: false,
            },
            internal,
            desktop: crate::setup::desktop_journey::DesktopProjection::default(),
            methods: crate::setup::desktop_journey::MethodProjection::default(),
        }
    }

    fn internal_actions(state: &SetupProjection) -> Vec<ActionSpec> {
        row(state)
            .into_resource()
            .children
            .into_iter()
            .find(|child| child.kind == "imager.setup.internal")
            .expect("internal row")
            .actions
    }

    fn code(action: &ActionSpec) -> Option<&str> {
        match &action.contract.availability {
            tilo_actions::Availability::Available => None,
            tilo_actions::Availability::Unavailable { code, .. } => Some(code),
        }
    }

    fn decoded(
        action: &ActionSpec,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> SetupEffect {
        let encoded = action.build_json(arguments).unwrap();
        assert_eq!(encoded.type_id, SetupEffect::OPERATION);
        SetupEffect::from_payload(&encoded.payload).unwrap()
    }

    /// Internal install row commands, availability and payload.
    #[test]
    fn internal_row_offers_gated_commands_bound_to_the_reviewed_digest() {
        // Catches an install command reachable only by clicking (spec §8).
        let actions = internal_actions(&projection(InternalProjection::default()));
        for (id, ..) in internal_commands(&InternalProjection::default()) {
            let action = actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(action.kind, OperationKind::Effect, "{id}");
            assert!(!action.contract.availability.is_available(), "{id}");
        }
        let install = actions
            .iter()
            .find(|action| action.id == "install-internal")
            .unwrap();
        assert_eq!(code(install), Some("full_os_review_only"));
        let blocked = blocked(&projection(InternalProjection::default()));
        assert!(
            blocked
                .iter()
                .any(|blocked| blocked.id == "install-internal"
                    && blocked.code == "full_os_review_only"),
            "the declared effect is gated like the row's"
        );

        // Catches availability flags wired to the wrong command.
        let actions = internal_actions(&projection(InternalProjection {
            on_review: true,
            availability: InternalAvailability {
                install: true,
                firmware_settings: true,
                ..InternalAvailability::default()
            },
            ..InternalProjection::default()
        }));
        for action in &actions {
            let expected = matches!(
                action.id.as_str(),
                "install-internal" | "internal-firmware-settings"
            );
            assert_eq!(
                action.contract.availability.is_available(),
                expected,
                "{}",
                action.id
            );
        }
        let restart = actions
            .iter()
            .find(|action| action.id == "restart-internal")
            .unwrap();
        assert_eq!(code(restart), Some("internal_not_staged"));

        // Catches an install or removal that does not name what the invoker
        // saw, which is what lets the view refuse a stale invocation.
        let state = projection(InternalProjection {
            on_review: true,
            reviewed: Some("a".repeat(64)),
            pending_operation: Some("op-7".into()),
            ..InternalProjection::default()
        });
        let actions = internal_actions(&state);
        let action = |id: &str| actions.iter().find(|action| action.id == id).unwrap();
        let none = serde_json::Map::new();
        assert_eq!(
            decoded(action("install-internal"), &none),
            SetupEffect::InstallInternal {
                reviewed: "a".repeat(64)
            }
        );
        assert_eq!(
            decoded(action("remove-internal"), &none),
            SetupEffect::RemoveInternal {
                operation_id: "op-7".into()
            }
        );
        let size = action("set-internal-target-size");
        let mut args = serde_json::Map::new();
        args.insert("gib".into(), serde_json::json!(80));
        assert_eq!(
            decoded(size, &args),
            SetupEffect::SetInternalTargetSize { gib: 80 }
        );
        args.insert("gib".into(), serde_json::json!("eighty"));
        assert!(size.build_json(&args).is_err());
    }

    #[test]
    fn folder_rows_bind_their_identity_and_take_the_mode() {
        let mut state = projection(InternalProjection::default());
        state.availability.folder_edits = true;
        state.availability.connected = false;
        state.selection.folders = vec![FolderProjection {
            identity: "f-1".into(),
            index: 0,
            mode: "copy",
            overridden: false,
        }];
        let selection = row(&state)
            .into_resource()
            .children
            .into_iter()
            .find(|child| child.kind == "imager.setup.selection")
            .unwrap();
        let folder = &selection.children[0];
        assert_eq!(folder.id.as_str(), "imager.setup.folder.f-1");
        let set_mode = folder
            .actions
            .iter()
            .find(|action| action.id == "set-folder-mode")
            .unwrap();
        let mut args = serde_json::Map::new();
        args.insert("mode".into(), serde_json::json!("later"));
        assert_eq!(
            decoded(set_mode, &args),
            SetupEffect::SetFolderMode {
                folder: "f-1".into(),
                mode: "later".into()
            }
        );
        args.insert("mode".into(), serde_json::json!("connected"));
        assert!(
            set_mode.build_json(&args).is_err(),
            "connected folders are offered only when available"
        );
        let inherit = folder
            .actions
            .iter()
            .find(|action| action.id == "use-common-mode")
            .unwrap();
        assert_eq!(code(inherit), Some("folder_mode_inherited"));
        assert!(
            folder
                .actions
                .iter()
                .all(|action| action.kind == OperationKind::Effect)
        );
    }
}

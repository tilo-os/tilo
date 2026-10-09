//! `WorkspaceSettings` — the contextual lens opened from the `TopBar`'s
//! workspace-name button, with General / People / Agents / Privacy & Security /
//! Apps & Files tabs.
//!
//! Members and Permissions are both views of [`Workspace::capabilities`]
//! — the single list `decide_capability` consults, the sandbox turns
//! into bwrap/Landlock rules, and the room replicates. A member's role
//! *is* the verb on their membership capability
//! ([`Workspace::role_of`]); nothing here keeps a parallel vocabulary
//! that could drift from what is enforced.
//!
//! Every mutation routes through [`Workspace::grant_as`] /
//! [`Workspace::revoke_as`], which refuse callers failing
//! [`Workspace::may_administer`]. Non-owners see the same information
//! with controls painted disabled, not hidden.

use crate::authority::WorkspaceAuthority as _;
use crate::i18n::{t, t_args};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::ColorCompat;
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::overlay::FocusTrap;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AvatarFace, BadgeFace, Button, ButtonFace, ChipStyle, SegmentedFace, Tone};

use collab_types::caps::{CapOverride, Effect, Expiry, Principal, Scope, Subject, Verb};
use collab_types::role::Role as CapRole;
use tilo_desktop_model::service_member::{ServiceMember, service_purpose};
use tilo_desktop_model::{UserId, Workspace, WorkspaceId};
use ui::TextRole;
use ui::text_render::TextStyle;
use ui::widget::ButtonRole;

pub mod overlay;

/// The host's application catalogue, as workspace settings name apps and
/// offer them as default openers.
pub trait AppDirectory {
    /// The name a person knows `app_id` by, when the host knows the app.
    fn app_display_name(&self, app_id: &str) -> Option<String>;
    /// Applications that can open a kind of resource, for the Apps tab.
    fn default_app_candidates(&self) -> Vec<desktop_policy::AppCandidate> {
        Vec::new()
    }
}

/// A host with no application catalogue: apps are named by their id.
pub struct NoAppDirectory;

impl AppDirectory for NoAppDirectory {
    fn app_display_name(&self, _app_id: &str) -> Option<String> {
        None
    }
}

fn app_display_name(apps: &dyn AppDirectory, app_id: &str) -> String {
    apps.app_display_name(app_id)
        .unwrap_or_else(|| app_id.to_string())
}

pub const OWNERSHIP_TRANSFER_ACTION_TYPE: &str = "tilo.workspace.ownership-transfer.v1";
pub const PERMISSION_PROFILE_ACTION_TYPE: &str = "tilo.workspace.permission-profile.v1";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PermissionProfileOperation {
    Save {
        name: String,
        description: String,
        rules: Vec<collab_types::PermissionProfileRule>,
    },
    Assign {
        profile: String,
        subject_kind: String,
        subject: String,
        expires_unix: u64,
    },
    /// Let application `app` use `action` on `target_app`'s resource
    /// `resource` here, and nothing else of `target_app`.
    GrantAppAction {
        app: String,
        target_app: String,
        resource: String,
        action: String,
        /// `session` or `always`.
        lifetime: String,
    },
    /// Take back every grant letting `app` use `action` on `target_app`'s
    /// resource `resource`.
    RevokeAppAction {
        app: String,
        target_app: String,
        resource: String,
        action: String,
    },
}

pub use tilo_desktop_model::{APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID};

/// Whether `action` is one of the workspace's governance actions, which
/// the workspace itself runs rather than an application.
#[must_use]
pub fn is_governance_action(action: &str) -> bool {
    matches!(
        action,
        tilo_desktop_model::PROFILE_SAVE_ACTION_ID
            | tilo_desktop_model::PROFILE_ASSIGN_ACTION_ID
            | APP_ACTION_GRANT_ACTION_ID
            | APP_ACTION_REVOKE_ACTION_ID
    )
}

fn encoded_profile_operation(
    operation: &PermissionProfileOperation,
) -> tilo_actions::EncodedAction {
    tilo_actions::EncodedAction::encode_operation(PERMISSION_PROFILE_ACTION_TYPE, operation)
        .unwrap_or_else(|error| tilo_actions::EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
            payload: error.to_string().into_bytes(),
        })
}

/// Searchable and agent-addressable governance surface for reusable roles.
/// Profile resources are workspace state; assigning one writes capabilities,
/// which remain the only values consulted by enforcement.
pub fn permission_profile_resource(ws: &Workspace, local_user: &UserId) -> tilo_actions::Resource {
    permission_profile_resource_with_access(ws, ws.may_administer(local_user))
}

/// Build the profile surface after an external authority boundary (the broker)
/// has established that its caller may inspect candidate governance actions.
/// Each action still carries `PermissionRequirement::Admin` and is rechecked
/// immediately before mutation.
#[expect(
    clippy::too_many_lines,
    reason = "a declarative resource description listing every governance action"
)]
pub fn permission_profile_resource_with_access(
    ws: &Workspace,
    can_administer: bool,
) -> tilo_actions::Resource {
    use tilo_actions::{
        ActionContract, ActionParam, ConfirmationPolicy, OperationKind, ParamType,
        PermissionRequirement, RiskLevel,
    };

    let mut root = tilo_actions::Resource::new(
        tilo_desktop_model::governance_resource_id(&ws.room),
        "workspace.permissions",
        t_args(
            "workspace-settings-roles-resource",
            &[("workspace", ws.name.clone().into())],
        ),
    );
    if !can_administer {
        return root;
    }
    let contract = ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        tags: vec!["workspace".into(), "permissions".into(), "roles".into()],
        ..ActionContract::default()
    };
    root.actions.push(
        tilo_actions::ActionSpec::typed_operation(
            "profile.save",
            t("workspace-settings-role-save"),
            t("workspace-settings-role-save-placeholder"),
            OperationKind::Effect,
            PERMISSION_PROFILE_ACTION_TYPE,
            vec![
                ActionParam::required_owned("name", ParamType::Text, "Unique role name"),
                ActionParam::optional_owned(
                    "description",
                    ParamType::Text,
                    "What this role is for",
                ),
                ActionParam::required_owned(
                    "rules",
                    ParamType::Array,
                    "Least-privilege rules. Scope examples: {\"App\":\"so.tilo.notes\"}, {\"Action\":{\"app\":\"so.tilo.notes\",\"action\":\"note.open\"}}, {\"FsPath\":\"/shared/design\"}, {\"FsRecord\":\"stable-record-id\"}, or \"Workspace\".",
                )
                .with_schema(serde_json::json!({
                    "minItems": 1,
                    "maxItems": 128,
                    "items": {
                        "type": "object",
                        "properties": {
                            "scope": {
                                "oneOf": [
                                    {"const": "Workspace"},
                                    {"type": "object", "required": ["App"], "properties": {"App": {"type": "string"}}, "additionalProperties": false},
                                    {"type": "object", "required": ["Action"], "properties": {"Action": {"type": "object", "required": ["app", "resource", "action"], "properties": {"app": {"type": "string"}, "resource": {"type": "string"}, "action": {"type": "string"}}, "additionalProperties": false}}, "additionalProperties": false},
                                    {"type": "object", "required": ["FsPath"], "properties": {"FsPath": {"type": "string"}}, "additionalProperties": false},
                                    {"type": "object", "required": ["FsRecord"], "properties": {"FsRecord": {"type": "string"}}, "additionalProperties": false},
                                    {"type": "object", "required": ["ResourceType"], "properties": {"ResourceType": {"type": "string"}}, "additionalProperties": false},
                                    {"type": "object", "required": ["ResourceInstance"], "properties": {"ResourceInstance": {"type": "string"}}, "additionalProperties": false},
                                    {"type": "object", "required": ["Room"], "properties": {"Room": {"type": "string"}}, "additionalProperties": false}
                                ]
                            },
                            "effect": {"enum": ["Grant", "Restrict"]},
                            "verb": {"enum": ["Read", "Comment", "Write", "Invite", "Admin"]}
                        },
                        "required": ["scope", "effect", "verb"],
                        "additionalProperties": false
                    }
                })),
            ],
            |arguments| {
                let rules = serde_json::from_value(serde_json::Value::Array(
                    arguments.array("rules").unwrap_or_default().to_vec(),
                ));
                let Ok(rules) = rules else {
                    return tilo_actions::EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"permission rules do not match a supported scope".to_vec(),
                    };
                };
                encoded_profile_operation(&PermissionProfileOperation::Save {
                    name: arguments.text("name").unwrap_or_default().to_string(),
                    description: arguments
                        .text("description")
                        .unwrap_or_default()
                        .to_string(),
                    rules,
                })
            },
        )
        .with_description(t("workspace-settings-role-save-description"))
        .with_contract(contract.clone()),
    );

    root.actions.push(
        tilo_actions::ActionSpec::typed_operation(
            "profile.assign",
            t("workspace-settings-role-assign"),
            t("workspace-settings-role-assign-placeholder"),
            OperationKind::Effect,
            PERMISSION_PROFILE_ACTION_TYPE,
            vec![
                ActionParam::required_owned(
                    "profile",
                    ParamType::Text,
                    "Exact name of an existing role, including one created by an earlier step",
                ),
                ActionParam::required_owned(
                    "subject",
                    ParamType::Text,
                    "Stable person, app, or agent identity",
                ),
                ActionParam::required_owned("subject_kind", ParamType::Text, "Identity namespace")
                    .with_choices(["person", "app", "agent"]),
                ActionParam::optional_owned(
                    "expires_unix",
                    ParamType::Integer,
                    "Unix expiry; omit for a durable assignment",
                ),
            ],
            |arguments| {
                let expires_unix = arguments
                    .integer("expires_unix")
                    .and_then(|value| u64::try_from(value).ok())
                    .unwrap_or_default();
                encoded_profile_operation(&PermissionProfileOperation::Assign {
                    profile: arguments.text("profile").unwrap_or_default().to_string(),
                    subject_kind: arguments
                        .text("subject_kind")
                        .unwrap_or_default()
                        .to_string(),
                    subject: arguments.text("subject").unwrap_or_default().to_string(),
                    expires_unix,
                })
            },
        )
        .with_description(t("workspace-settings-role-assign-description"))
        .with_contract(contract.clone()),
    );

    let app_action_params = || {
        vec![
            ActionParam::required_owned(
                "app",
                ParamType::Text,
                "Package id of the application that would use the action",
            ),
            ActionParam::required_owned(
                "target_app",
                ParamType::Text,
                "Package id of the application that publishes the action",
            ),
            ActionParam::required_owned(
                "resource",
                ParamType::Text,
                "Id of the resource that publishes the action",
            ),
            ActionParam::required_owned("action", ParamType::Text, "The action's id"),
        ]
    };
    let mut grant_params = app_action_params();
    grant_params.push(
        ActionParam::required_owned("lifetime", ParamType::Text, "How long the grant lasts")
            .with_choices(["session", "always"]),
    );
    root.actions.push(
        tilo_actions::ActionSpec::typed_operation(
            APP_ACTION_GRANT_ACTION_ID,
            t("workspace-settings-app-action-grant"),
            t("workspace-settings-app-action-placeholder"),
            OperationKind::Effect,
            PERMISSION_PROFILE_ACTION_TYPE,
            grant_params,
            |arguments| {
                encoded_profile_operation(&PermissionProfileOperation::GrantAppAction {
                    app: arguments.text("app").unwrap_or_default().to_string(),
                    target_app: arguments.text("target_app").unwrap_or_default().to_string(),
                    resource: arguments.text("resource").unwrap_or_default().to_string(),
                    action: arguments.text("action").unwrap_or_default().to_string(),
                    lifetime: arguments.text("lifetime").unwrap_or_default().to_string(),
                })
            },
        )
        .with_description(t("workspace-settings-app-action-grant-description"))
        .with_contract(contract.clone()),
    );
    root.actions.push(
        tilo_actions::ActionSpec::typed_operation(
            APP_ACTION_REVOKE_ACTION_ID,
            t("workspace-settings-app-action-revoke"),
            t("workspace-settings-app-action-placeholder"),
            OperationKind::Effect,
            PERMISSION_PROFILE_ACTION_TYPE,
            app_action_params(),
            |arguments| {
                encoded_profile_operation(&PermissionProfileOperation::RevokeAppAction {
                    app: arguments.text("app").unwrap_or_default().to_string(),
                    target_app: arguments.text("target_app").unwrap_or_default().to_string(),
                    resource: arguments.text("resource").unwrap_or_default().to_string(),
                    action: arguments.text("action").unwrap_or_default().to_string(),
                })
            },
        )
        .with_description(t("workspace-settings-app-action-revoke-description"))
        .with_contract(contract.clone()),
    );

    for profile in &ws.permission_profiles {
        let profile_name = profile.name.clone();
        let mut resource = tilo_actions::Resource::new(
            tilo_desktop_model::permission_profile_resource_id(&ws.room, &profile_name),
            "workspace.permission-profile",
            profile.name.clone(),
        );
        resource.actions.push(
            tilo_actions::ActionSpec::typed_operation(
                "profile.assign",
                t_args(
                    "workspace-settings-role-assign-named",
                    &[("role", profile.name.clone().into())],
                ),
                t("workspace-settings-role-assign-named-placeholder"),
                OperationKind::Effect,
                PERMISSION_PROFILE_ACTION_TYPE,
                vec![
                    ActionParam::required_owned(
                        "profile",
                        ParamType::Text,
                        "Exact saved role name",
                    )
                    .with_choices([profile.name.clone()]),
                    ActionParam::required_owned(
                        "subject",
                        ParamType::Text,
                        "Stable person, app, or agent identity",
                    ),
                    ActionParam::required_owned(
                        "subject_kind",
                        ParamType::Text,
                        "Identity namespace",
                    )
                    .with_choices(["person", "app", "agent"]),
                    ActionParam::optional_owned(
                        "expires_unix",
                        ParamType::Integer,
                        "Unix expiry; omit for a durable assignment",
                    ),
                ],
                move |arguments| {
                    let expires_unix = arguments
                        .integer("expires_unix")
                        .and_then(|value| u64::try_from(value).ok())
                        .unwrap_or_default();
                    encoded_profile_operation(&PermissionProfileOperation::Assign {
                        profile: arguments
                            .text("profile")
                            .unwrap_or(&profile_name)
                            .to_string(),
                        subject_kind: arguments
                            .text("subject_kind")
                            .unwrap_or_default()
                            .to_string(),
                        subject: arguments.text("subject").unwrap_or_default().to_string(),
                        expires_unix,
                    })
                },
            )
            .with_description(profile.description.clone())
            .with_contract(contract.clone()),
        );
        root.children.push(resource);
    }
    root
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum OwnershipTransferOperation {
    Propose {
        recipient: String,
        expires_unix: u64,
    },
    Accept {
        proposal_digest: String,
    },
    Cancel {
        proposal_digest: String,
    },
}

fn encoded_ownership_operation(
    operation: &OwnershipTransferOperation,
) -> tilo_actions::EncodedAction {
    tilo_actions::EncodedAction::encode_operation(OWNERSHIP_TRANSFER_ACTION_TYPE, operation)
        .unwrap_or_else(|error| tilo_actions::EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
            payload: error.to_string().into_bytes(),
        })
}

/// Publish the active workspace and its ownership ceremony as typed,
/// resource-addressed operations for search, automation, and assistive tools.
/// Generic member-role actions remain separate and never include Owner.
pub fn ownership_transfer_resource(ws: &Workspace, local_user: &UserId) -> tilo_actions::Resource {
    let mut resource = tilo_actions::Resource::new(
        format!("workspace:{}", ws.room),
        "workspace.governance",
        t_args(
            "workspace-settings-ownership-resource",
            &[("workspace", ws.name.clone().into())],
        ),
    );
    if ws.owner.as_ref() == Some(local_user) {
        resource.actions.push(ownership_proposal_action());
    }
    if let Some(pending) = &ws.pending_ownership_transfer {
        let digest = pending.digest().map_or_else(|_| String::new(), hex::encode);
        if public_key_member_id(pending.recipient) == *local_user {
            resource.actions.push(ownership_accept_action(&digest));
        }
        if ws.owner.as_ref() == Some(local_user) {
            resource.actions.push(ownership_cancel_action(&digest));
        }
    }
    resource
}

fn ownership_proposal_action() -> tilo_actions::ActionSpec {
    use tilo_actions::{
        ActionContract, ActionParam, ConfirmationPolicy, OperationKind, ParamType,
        PermissionRequirement, RiskLevel,
    };

    tilo_actions::ActionSpec::typed_operation(
        "ownership.propose",
        t("workspace-settings-ownership-propose"),
        t("workspace-settings-ownership-propose-placeholder"),
        OperationKind::Effect,
        OWNERSHIP_TRANSFER_ACTION_TYPE,
        vec![
            ActionParam::required_owned(
                "recipient",
                ParamType::Text,
                "Exact certified recipient identity",
            ),
            ActionParam::required_owned(
                "expires_unix",
                ParamType::Integer,
                "Unix expiry chosen by the current owner",
            ),
        ],
        |arguments| {
            let expires_unix = arguments
                .integer("expires_unix")
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or_default();
            encoded_ownership_operation(&OwnershipTransferOperation::Propose {
                recipient: arguments.text("recipient").unwrap_or_default().to_string(),
                expires_unix,
            })
        },
    )
    .with_description(t("workspace-settings-ownership-propose-description"))
    .with_contract(ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        tags: vec!["workspace".into(), "governance".into(), "ownership".into()],
        ..ActionContract::default()
    })
}

fn ownership_accept_action(digest: &str) -> tilo_actions::ActionSpec {
    use tilo_actions::{
        ActionContract, ConfirmationPolicy, OperationKind, PermissionRequirement, RiskLevel,
    };

    let operation = OwnershipTransferOperation::Accept {
        proposal_digest: digest.to_string(),
    };
    tilo_actions::ActionSpec::operation(
        "ownership.accept",
        t("workspace-settings-ownership-accept"),
        OperationKind::Effect,
        OWNERSHIP_TRANSFER_ACTION_TYPE,
        &operation,
    )
    .unwrap_or_else(|_| {
        tilo_actions::ActionSpec::immediate_encoded(
            "ownership.accept",
            t("workspace-settings-ownership-accept"),
            tilo_actions::ExecutionLocus::ClientLocal,
            encoded_ownership_operation(&operation),
        )
    })
    .with_description(t("workspace-settings-ownership-accept-description"))
    .with_contract(ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Phrase {
            phrase: t("workspace-settings-ownership-accept-phrase"),
        },
        tags: vec!["workspace".into(), "governance".into(), "ownership".into()],
        ..ActionContract::default()
    })
}

fn ownership_cancel_action(digest: &str) -> tilo_actions::ActionSpec {
    use tilo_actions::{
        ActionContract, ConfirmationPolicy, OperationKind, PermissionRequirement, RiskLevel,
    };

    let operation = OwnershipTransferOperation::Cancel {
        proposal_digest: digest.to_string(),
    };
    tilo_actions::ActionSpec::operation(
        "ownership.cancel",
        t("workspace-settings-ownership-cancel"),
        OperationKind::Effect,
        OWNERSHIP_TRANSFER_ACTION_TYPE,
        &operation,
    )
    .unwrap_or_else(|_| {
        tilo_actions::ActionSpec::immediate_encoded(
            "ownership.cancel",
            t("workspace-settings-ownership-cancel"),
            tilo_actions::ExecutionLocus::ClientLocal,
            encoded_ownership_operation(&operation),
        )
    })
    .with_description(t("workspace-settings-ownership-cancel-description"))
    .with_contract(ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Low,
        confirmation: ConfirmationPolicy::Confirm,
        reversible: false,
        tags: vec!["workspace".into(), "governance".into(), "ownership".into()],
        ..ActionContract::default()
    })
}

fn public_key_member_id(key: collab_types::Ed25519PublicKey) -> UserId {
    hex::encode(key.as_bytes())
}

/// The right-pane drawer [`WorkspaceSettings::draw_body`] dispatches to.
pub type PaneDrawer<'a> = dyn FnMut(&mut Scene, Affine, Rect, &mut TextRenderer, &Theme) + 'a;

/// Compose `transform` with a translation to `(dx, dy)`. `Affine`'s `Mul`
/// is pure `f64` matrix composition — it cannot overflow or panic — but
/// `arithmetic_side_effects` can't see that for a non-primitive `Mul`
/// impl, so every call site funnels through this one function instead of
/// an `#[allow]` at each of them.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn at(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// Width of the modal's left sidebar in CSS px: the docked navigation
/// sidebar width (HIG §9.6, `Metrics::sidebar_width`), unscaled like the
/// rest of this lens's geometry.
pub const SIDEBAR_WIDTH: f64 = ui::tokens::Metrics::BASE.sidebar_width as f64;

const SIDEBAR_IDENTITY_TOP: f64 = 24.0;
const NAV_ROW_HEIGHT: f64 = 40.0;
const NAV_ROW_GAP: f64 = 4.0;
/// Y origin (relative to the sidebar's top edge) of the first nav row —
/// reserves room for the identity badge + caption.
const NAV_ROWS_TOP: f64 = 120.0;

/// Tab selector for the shell-owned `WorkspaceSettings` lens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WsTab {
    /// Workspace identity, invite link, collaborators.
    #[default]
    General,
    /// Who is in the workspace, their role, and how many capabilities
    /// they hold. Rendered from [`Workspace::roster`].
    Members,
    /// Non-human collaborators admitted to the workspace, kept visibly
    /// separate from people and rendered from [`Workspace::agent_members`].
    Agents,
    /// Every capability the workspace has granted, and the affordance
    /// to grant another. Rendered from [`Workspace::capabilities`].
    Permissions,
    /// Shared default-app policy plus this person's explicit override.
    Apps,
}

impl WsTab {
    pub fn label(&self) -> String {
        t(match self {
            WsTab::General => "workspace-settings-tab-general",
            WsTab::Members => "workspace-settings-tab-people",
            WsTab::Agents => "workspace-settings-tab-agents",
            WsTab::Permissions => "workspace-settings-tab-privacy-security",
            WsTab::Apps => "workspace-settings-tab-apps-files",
        })
    }

    /// Short labels for the same destinations when the lens promotes to a
    /// narrow-screen sheet. This is responsive copy, not a second nav model.
    pub fn compact_label(&self) -> String {
        t(match self {
            WsTab::General => "workspace-settings-tab-general",
            WsTab::Members => "workspace-settings-tab-people",
            WsTab::Agents => "workspace-settings-tab-agents",
            WsTab::Permissions => "workspace-settings-tab-privacy",
            WsTab::Apps => "workspace-settings-tab-apps",
        })
    }

    /// Iteration order for the sidebar nav and tab strip. Every tab is
    /// always reachable — a host must reach Permissions before any
    /// grant exists.
    pub fn all() -> [WsTab; 5] {
        [
            WsTab::General,
            WsTab::Members,
            WsTab::Agents,
            WsTab::Permissions,
            WsTab::Apps,
        ]
    }
}

/// The `WorkspaceSettings` lens state.
#[derive(Debug)]
pub struct WorkspaceSettings {
    pub ws_id: WorkspaceId,
    pub active_tab: WsTab,
    focus_trap: FocusTrap,
}

impl WorkspaceSettings {
    pub fn new(ws_id: WorkspaceId) -> Self {
        let members = WsTab::all().into_iter().map(tab_focus_id).collect();
        Self {
            ws_id,
            active_tab: WsTab::General,
            focus_trap: FocusTrap::new(members),
        }
    }

    pub fn focus_trap(&self) -> &FocusTrap {
        &self.focus_trap
    }

    pub fn select_tab(&mut self, tab: WsTab) {
        self.active_tab = tab;
    }

    /// Synonym for [`Self::select_tab`]. Retained for callers that
    /// already use the imperative `set_*` form.
    pub fn set_tab(&mut self, tab: WsTab) {
        self.select_tab(tab);
    }

    pub fn active_tab(&self) -> WsTab {
        self.active_tab
    }

    /// Does not check `ws.id == self.ws_id`, so the caller can rename
    /// any workspace it holds a reference to.
    pub fn rename(&mut self, ws: &mut Workspace, new_name: &str) {
        ws.name = new_name.to_string();
    }

    pub fn remove_collaborator(&mut self, ws: &mut Workspace, collab_id: &str) {
        ws.collaborators.retain(|c| c.id != collab_id);
    }

    // -----------------------------------------------------------------
    // Sidebar nav layout
    // -----------------------------------------------------------------

    /// The sidebar's bounds. `Rect::ZERO` when the modal isn't wide
    /// enough to host the full sidebar.
    pub fn sidebar_rect(&self, modal_rect: Rect) -> Rect {
        if modal_rect.width() <= SIDEBAR_WIDTH {
            return Rect::ZERO;
        }
        Rect::new(
            modal_rect.x0,
            modal_rect.y0,
            modal_rect.x0 + SIDEBAR_WIDTH,
            modal_rect.y1,
        )
    }

    pub fn pane_rect(&self, modal_rect: Rect) -> Rect {
        if modal_rect.width() <= SIDEBAR_WIDTH {
            return modal_rect;
        }
        Rect::new(
            modal_rect.x0 + SIDEBAR_WIDTH,
            modal_rect.y0,
            modal_rect.x1,
            modal_rect.y1,
        )
    }

    /// Per-row hit-test rects, one per [`WsTab`] in [`WsTab::all`] order.
    pub fn nav_rows(&self, modal_rect: Rect) -> [(WsTab, Rect); 5] {
        let sidebar = self.sidebar_rect(modal_rect);
        let inset_x0 = sidebar.x0 + 16.0;
        let inset_x1 = sidebar.x1 - 16.0;
        let tabs = WsTab::all();
        std::array::from_fn(|i| {
            // `i` is a `std::array::from_fn::<_, 3>` index, always 0..=2.
            #[allow(
                clippy::cast_precision_loss,
                reason = "i is a fixed 3-element array index, always 0..=2"
            )]
            let y0 = sidebar.y0 + NAV_ROWS_TOP + i as f64 * (NAV_ROW_HEIGHT + NAV_ROW_GAP);
            let y1 = y0 + NAV_ROW_HEIGHT;
            let tab = tabs.get(i).copied().unwrap_or_default();
            (tab, Rect::new(inset_x0, y0, inset_x1, y1))
        })
    }

    /// Tab whose nav-row contains `point`, if any.
    pub fn nav_hit_test(&self, modal_rect: Rect, point: Point) -> Option<WsTab> {
        self.nav_rows(modal_rect)
            .into_iter()
            .find_map(|(tab, row)| row.contains(point).then_some(tab))
    }

    /// Paint the sidebar nav (always visible) and dispatch the right
    /// pane to the per-tab drawer. `None` paints nothing on the right.
    pub fn draw_body(
        &self,
        scene: &mut Scene,
        transform: Affine,
        modal_rect: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        pane_drawer: Option<&mut PaneDrawer<'_>>,
    ) {
        self.draw_sidebar(scene, transform, modal_rect, text_renderer, theme);

        if let Some(drawer) = pane_drawer {
            let pane = self.pane_rect(modal_rect);
            drawer(scene, transform, pane, text_renderer, theme);
        }
    }

    /// Paint the sidebar — identity card, nav rows, footer.
    #[expect(
        clippy::too_many_lines,
        reason = "a paint routine that lays its elements out top to bottom from shared running offsets"
    )]
    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        modal_rect: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let sidebar = self.sidebar_rect(modal_rect);
        if sidebar.is_zero_area() {
            return;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Sidebar surface reads distinct from the right pane — a quiet lift
        // measured against the ground (HIG §9.2/§9.3), never a ramp slot —
        // separated by a derived hairline rather than a declared border.
        scene.fill(Fill::NonZero, transform, colors.lift(0.35), None, &sidebar);
        let border_x = sidebar.x1 - 0.5;
        let border = Rect::new(border_x, sidebar.y0, border_x + 1.0, sidebar.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &border,
        );

        // Identity card, at the Control radius (16 is off the radius scale).
        let identity_size = 56.0_f64;
        let identity_rect = Rect::new(
            sidebar.x0 + 24.0,
            sidebar.y0 + SIDEBAR_IDENTITY_TOP,
            sidebar.x0 + 24.0 + identity_size,
            sidebar.y0 + SIDEBAR_IDENTITY_TOP + identity_size,
        );
        let identity_card = RoundedRect::from_rect(
            identity_rect,
            ui::chrome::ChromeRole::Control.radius(theme, identity_size),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.intent.inverted,
            None,
            &identity_card,
        );

        text_renderer.draw_style(
            scene,
            &t("workspace-settings-title"),
            colors.text_dim,
            at(transform, sidebar.x0 + 24.0, identity_rect.y1 + 12.0),
            None,
            &tokens.typography.style(TextRole::Caption),
        );

        for (tab, row) in self.nav_rows(modal_rect) {
            let active = self.active_tab == tab;
            // Selection is a lift (HIG §9.3), never an accent fill; the row
            // is a Control and takes the Control radius.
            let row_rr = RoundedRect::from_rect(
                row,
                ui::chrome::ChromeRole::Control.radius(theme, row.height()),
            );
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(active));
            if active {
                scene.fill(Fill::NonZero, transform, tier.fill, None, &row_rr);
                if let Some(border) = tier.border {
                    scene.stroke(&Stroke::new(1.0), transform, border, None, &row_rr);
                }
            }
            let label_color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            // Centred on the measured line, not at a fixed offset (HIG §7a).
            let label_style = tokens.typography.style(TextRole::Body);
            let label = tab.label();
            let (_, label_h) = text_renderer.measure_style(&label, None, &label_style);
            text_renderer.draw_style(
                scene,
                &label,
                label_color,
                at(
                    transform,
                    row.x0 + 12.0,
                    row.center().y - f64::from(label_h) / 2.0,
                ),
                None,
                &label_style,
            );
        }

        // Footer "Close Settings" row pinned to the bottom of the sidebar.
        let footer_h = f64::from(tokens.metrics.bar_height);
        let footer_y = sidebar.y1 - footer_h - 16.0;
        let footer = Rect::new(
            sidebar.x0 + 16.0,
            footer_y,
            sidebar.x1 - 16.0,
            footer_y + footer_h,
        );
        let footer_rr = RoundedRect::from_rect(
            footer,
            ui::chrome::ChromeRole::Control.radius(theme, footer_h),
        );
        // The footer row's edge is derived against the sidebar lift it sits
        // on (HIG §9.5), not a declared border over a lifted surface.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::surface::edge_on(colors.lift(0.35)),
            None,
            &footer_rr,
        );
        // The standard glyph on the row inset, centred on the row; the label
        // centred on its measured line beside it.
        let inset = f64::from(tokens.spacing.s3);
        let glyph_size = f64::from(tokens.metrics.icon_md);
        let glyph_rect = Rect::new(
            footer.x0 + inset,
            footer.center().y - glyph_size / 2.0,
            footer.x0 + inset + glyph_size,
            footer.center().y + glyph_size / 2.0,
        );
        draw_icon(
            scene,
            transform,
            glyph_rect,
            Icon::X,
            colors.text_dim,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let close_style = tokens.typography.style(TextRole::Caption);
        let close_label = t("workspace-settings-close");
        let (_, close_h) = text_renderer.measure_style(&close_label, None, &close_style);
        text_renderer.draw_style(
            scene,
            &close_label,
            colors.text_dim,
            at(
                transform,
                glyph_rect.x1 + inset,
                footer.center().y - f64::from(close_h) / 2.0,
            ),
            None,
            &close_style,
        );
    }
}

/// Stable focus id for the tab button representing `tab`, used by the
/// dialog's `FocusTrap`.
fn tab_focus_id(tab: WsTab) -> u64 {
    // Reserve a small id range so the modal's trap members can't collide
    // with shell-level focusables.
    const BASE: u64 = 0xFFFF_0000;
    match tab {
        WsTab::General => BASE,
        WsTab::Members => BASE + 1,
        WsTab::Agents => BASE + 2,
        WsTab::Permissions => BASE + 3,
        WsTab::Apps => BASE + 4,
    }
}

// Shared chrome — cards, chips, section headers

const PANE_PAD_X: f64 = 16.0;
/// The pane's lead-in under the lens body top. The body already starts `s2`
/// under the tab strip (`WorkspaceSettingsOverlay::body_rect_for`), so this
/// completes the `s4` section step (HIG §7b) from the strip to the intro card.
const PANE_PAD_TOP: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// The pane's bottom inset: the row-overflow guard and the footer copy keep
/// one `s3` clear of the lens body's bottom edge. Only the top lead-in shrank
/// to complete the section step; the bottom keeps its authored step.
const PANE_PAD_BOTTOM: f64 = ui::tokens::Spacing::BASE.s3 as f64;
/// Room for the intro's title and two lines of detail: German detail copy
/// does not fit one line at the lens's width, and cutting it mid-sentence
/// loses the explanation the intro exists for.
const PANE_INTRO_HEIGHT: f64 = 76.0;
const PANE_INTRO_DETAIL_LINES: usize = 2;
const PANE_INTRO_GAP: f64 = 12.0;
/// Two lines of text plus breathing room.
const ROW_HEIGHT: f64 = 56.0;
const ROW_GAP: f64 = 8.0;
/// A section header band inside a pane: the Bar-control step (HIG §9.6).
const SECTION_HEADER_HEIGHT: f64 = ui::tokens::Metrics::BASE.bar_control_height as f64;
/// A composer control — picker, expiry segment, submit — is never smaller
/// than the minimum pointer target (HIG §9.6).
const CONTROL_HEIGHT: f64 = ui::tokens::Metrics::BASE.min_pointer_target as f64;
/// Caption band above a control ("Until", "Who"): one `xs` line plus air.
const COMPOSER_CAPTION_BAND: f64 = 18.0;
/// The composer card: a caption band over one control row, inside `s3`
/// padding — derived from the pieces rather than restated as a total.
const COMPOSER_HEIGHT: f64 = ui::tokens::Spacing::BASE.s3 as f64
    + ui::tokens::Spacing::BASE.s6 as f64
    + CONTROL_HEIGHT
    + ui::tokens::Spacing::BASE.s3 as f64;
/// A narrow pane stacks the composer on two rows: one more control, its
/// caption band, and an `s1` seam between the rows.
const COMPOSER_COMPACT_EXTRA: f64 =
    CONTROL_HEIGHT + COMPOSER_CAPTION_BAND + ui::tokens::Spacing::BASE.s1 as f64;
const REVOKE_WIDTH: f64 = 80.0;
/// Row buttons (revoke, role, review) are full pointer targets (HIG §9.6).
const REVOKE_HEIGHT: f64 = ui::tokens::Metrics::BASE.min_pointer_target as f64;
const AGENT_REVIEW_WIDTH: f64 = 112.0;
const AGENT_ONBOARDING_HEIGHT: f64 = 34.0;
const SUBMIT_WIDTH: f64 = 92.0;
const EXPIRY_SEGMENT_WIDTH: f64 = 58.0;
/// Alpha for a control the user has no authority to use — matches the
/// permission prompt's dimmed-Revoke treatment.
const DISABLED_ALPHA: f32 = 0.40;

/// Below this the contextual lens is a narrow sheet. Controls keep their
/// meaning, but multi-column policy rows and the permission composer promote
/// onto a second line instead of escaping through the card edge.
const COMPACT_PANE_WIDTH: f64 = 460.0;

fn compact_pane(pane: Rect) -> bool {
    pane.width() < COMPACT_PANE_WIDTH
}

/// Paint a section header — a small upper-case caption plus a count.
fn draw_section_header(
    scene: &mut Scene,
    transform: Affine,
    origin: Point,
    label: &str,
    count: Option<usize>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let text = match count {
        Some(n) => format!("{label} · {n}"),
        None => label.to_string(),
    };
    text_renderer.draw_style(
        scene,
        &text,
        tokens.colors.text_dim,
        at(transform, origin.x, origin.y),
        None,
        &tokens.typography.style(TextRole::Caption),
    );
}

/// Paint a row card — the surface every subject row sits on.
fn draw_row_card(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    // Derived rest edge (HIG §9.5): measured against the card's own fill,
    // so it registers on the raised tier in every palette — a declared
    // `border_standard` is three shades from `raised` on the dark preset.
    ui::surface::card(
        scene,
        transform,
        rect,
        f64::from(theme.tokens.radii.control),
        theme.tokens.colors.surfaces.raised,
    );
}

/// A concise explanation and live status at the top of each policy pane.
/// It gives normal users orientation without hiding the full controls.
fn draw_pane_intro(
    ctx: &mut PaintCtx<'_>,
    pane: Rect,
    icon: Icon,
    title: &str,
    detail: &str,
    status: &str,
) {
    let (scene, transform, text_renderer, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let rect = Rect::new(
        pane.x0 + PANE_PAD_X,
        pane.y0 + PANE_PAD_TOP,
        pane.x1 - PANE_PAD_X,
        pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT,
    );
    ui::surface::card(
        scene,
        transform,
        rect,
        f64::from(tokens.radii.control),
        colors.surfaces.raised,
    );

    let icon_rect = Rect::new(
        rect.x0 + 14.0,
        rect.center().y - 18.0,
        rect.x0 + 50.0,
        rect.center().y + 18.0,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.text_primary.with_alpha_factor(0.16),
        None,
        &RoundedRect::from_rect(icon_rect, f64::from(tokens.radii.control)),
    );
    draw_icon(
        scene,
        transform,
        icon_rect.inset(-9.0),
        icon,
        colors.intent.inverted_hover,
        LUCIDE_DEFAULT_STROKE_PX,
    );
    let detail_style = tokens.typography.style(TextRole::Caption);
    let detail_max = (rect.width() - 62.0 - 124.0).max(80.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "settings-pane geometry is a small logical-pixel width"
    )]
    let lines = text_renderer.clamp_to_lines(
        detail,
        &detail_style,
        detail_max as f32,
        PANE_INTRO_DETAIL_LINES,
    );
    let title_h = f64::from(
        text_renderer
            .measure_style(title, None, &tokens.typography.style(TextRole::Caption))
            .1,
    );
    let line_h = f64::from(detail_style.line_height_px());
    #[allow(
        clippy::cast_precision_loss,
        reason = "at most PANE_INTRO_DETAIL_LINES lines"
    )]
    let block_h = title_h + 4.0 + line_h * lines.len() as f64;
    let top = rect.y0 + ((rect.height() - block_h) / 2.0).max(8.0);
    text_renderer.draw_style(
        scene,
        title,
        colors.text_primary,
        at(transform, rect.x0 + 62.0, top),
        None,
        &tokens.typography.style(TextRole::Caption),
    );
    let mut line_y = top + title_h + 4.0;
    for line in &lines {
        text_renderer.draw_style(
            scene,
            line,
            colors.text_dim,
            at(transform, rect.x0 + 62.0, line_y),
            None,
            &detail_style,
        );
        line_y += line_h;
    }
    draw_pill(
        scene,
        transform,
        Point::new(rect.x1 - 14.0, rect.center().y),
        status,
        ChipStyle::Neutral,
        text_renderer,
        theme,
    );
}

/// Paint a badge with `label` centred, in `style`. Returns the
/// pill's rect so callers can lay out rightwards from its left edge.
fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    anchor: Point,
    label: &str,
    style: ChipStyle,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> Rect {
    let w = BadgeFace::intrinsic_width(label, text_renderer, theme);
    let h = BadgeFace::height(theme);
    let rect = Rect::new(
        anchor.x - w,
        anchor.y - h / 2.0,
        anchor.x,
        anchor.y + h / 2.0,
    );
    BadgeFace::new(rect, label)
        .style(style)
        .paint(scene, transform, text_renderer, theme);
    rect
}

/// Paint an avatar disc carrying `label`'s first character. The tint is
/// derived from the label so the same subject keeps the same colour
/// across frames without storing one.
fn draw_avatar(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    square: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    // People use the shared avatar primitive so initials, palette choice,
    // and contrast stay identical wherever a person appears. App subjects
    // retain the square document-like treatment: it distinguishes an app
    // principal from a human identity at a glance.
    if !square {
        AvatarFace::new(label, label).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
        );
        return;
    }
    let colors = &theme.tokens.colors;
    let tint = subject_tint(label, theme);
    scene.fill(
        Fill::NonZero,
        transform,
        tint.with_alpha_factor(0.30),
        None,
        &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
    );
    let initial: String = label
        .chars()
        .next()
        .map_or_else(|| "?".to_string(), |c| c.to_uppercase().to_string());
    let style = theme.text(TextRole::Caption);
    let (tw, th) = text_renderer.measure_style(&initial, None, &style);
    text_renderer.draw_style(
        scene,
        &initial,
        colors.text_primary,
        at(
            transform,
            rect.center().x - f64::from(tw) / 2.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
        &style,
    );
}

/// Deterministic per-subject tint drawn from the theme's accent ramp, so
/// two subjects rarely share a colour and none is hard-coded.
fn subject_tint(label: &str, theme: &Theme) -> Color {
    let colors = &theme.tokens.colors;
    let bucket = label
        .bytes()
        .fold(0u32, |a, b| a.wrapping_add(u32::from(b))) as usize
        % 4;
    match bucket {
        0 => colors.intent.inverted,
        1 => colors.intent.inverted_hover,
        2 => colors.semantic.info,
        _ => colors.semantic.success,
    }
}

/// Paint a "cycler" control: a bordered field whose label advances to
/// the next value on click, with a chevron affording that it cycles.
fn draw_cycler(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    enabled: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let alpha = if enabled { 1.0 } else { DISABLED_ALPHA };
    let track = RoundedRect::from_rect(rect, f64::from(tokens.radii.control));
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.raised.with_alpha_factor(alpha),
        None,
        &track,
    );
    // The field's rest edge derives from its own fill (HIG §9.5) and
    // fades with the disabled face so rest and disabled never look alike.
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::surface::edge_on(colors.surfaces.raised).with_alpha_factor(alpha),
        None,
        &track,
    );
    let style = tokens.typography.style(TextRole::Caption);
    let chevron = 14.0_f64;
    let text_max = rect.width() - 12.0 - chevron - 10.0;
    let shown = elide(label, &style, text_max, text_renderer);
    let (_, th) = text_renderer.measure_style(&shown, None, &style);
    text_renderer.draw_style(
        scene,
        &shown,
        colors.text_primary.with_alpha_factor(alpha),
        at(
            transform,
            rect.x0 + 10.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
        &style,
    );
    let glyph = Rect::new(
        rect.x1 - 8.0 - chevron,
        rect.center().y - chevron / 2.0,
        rect.x1 - 8.0,
        rect.center().y + chevron / 2.0,
    );
    draw_icon(
        scene,
        transform,
        glyph,
        Icon::ChevronDown,
        colors.text_dim.with_alpha_factor(alpha),
        LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// Truncate `text` with an ellipsis so it fits `max_w` at `size`. Text
/// that would overflow its field is a craft defect; clipping it is not
/// a fix, so we measure and elide through the shared renderer helper.
fn elide(text: &str, style: &TextStyle, max_w: f64, text_renderer: &mut TextRenderer) -> String {
    text_renderer.elide_to_width_style(text, ui::num::f32_of_f64(max_w), style)
}

/// Paint a labelled micro-caption above a control.
fn draw_control_caption(
    scene: &mut Scene,
    transform: Affine,
    origin: Point,
    label: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    text_renderer.draw_style(
        scene,
        label,
        theme.tokens.colors.text_dim,
        at(transform, origin.x, origin.y),
        None,
        &theme.text(TextRole::Caption),
    );
}

/// Paint a destructive button (Revoke). Dimmed when `enabled` is false
/// so a viewer sees the control exists but cannot be used.
fn draw_danger_button(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    enabled: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(rect, label)
            .role(ButtonRole::Destructive)
            .disabled(!enabled),
    );
}

/// Paint the pane's one primary action.
fn draw_primary_button(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    enabled: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(rect, label)
            .role(ButtonRole::Primary)
            .disabled(!enabled),
    );
}

/// Paint a composed empty state — glyph, headline, caption.
fn draw_empty_state(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    text: (&str, &str),
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let (headline, caption) = text;
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let glyph = 28.0_f64;
    let cx = rect.center().x;
    let stack_height = glyph + 58.0;
    let top = rect.y0 + ((rect.height() - stack_height) / 2.0).clamp(12.0, 48.0);
    // A region too short for the stack (the lens at its height floor) paints
    // nothing rather than running the glyph and two lines past its bottom
    // edge into whatever sits below (HIG §7: nothing overlaps).
    if top + stack_height > rect.y1 {
        return;
    }
    draw_icon(
        scene,
        transform,
        Rect::new(cx - glyph / 2.0, top, cx + glyph / 2.0, top + glyph),
        icon,
        colors.text_dim,
        LUCIDE_DEFAULT_STROKE_PX,
    );
    let h_style = tokens.typography.style(TextRole::Body);
    let (hw, _) = text_renderer.measure_style(headline, None, &h_style);
    text_renderer.draw_style(
        scene,
        headline,
        colors.text_primary,
        at(transform, cx - f64::from(hw) / 2.0, top + glyph + 14.0),
        None,
        &h_style,
    );
    let c_style = tokens.typography.style(TextRole::Caption);
    let (cw, _) = text_renderer.measure_style(caption, None, &c_style);
    text_renderer.draw_style(
        scene,
        caption,
        colors.text_dim,
        at(transform, cx - f64::from(cw) / 2.0, top + glyph + 36.0),
        None,
        &c_style,
    );
}

// Capability vocabulary — labels

pub fn role_label(role: CapRole) -> String {
    t(match role {
        CapRole::Owner => "workspace-settings-role-owner",
        CapRole::Editor => "workspace-settings-role-editor",
        CapRole::Commenter => "workspace-settings-role-commenter",
        CapRole::Viewer => "workspace-settings-role-viewer",
        CapRole::Denied => "workspace-settings-role-blocked",
    })
}

/// Plain-language consequences shown before changing a person's workspace
/// role. Keeping this next to the role vocabulary makes the chooser describe
/// the same authority that `role_verb` ultimately writes.
pub fn role_capability_preview(role: CapRole) -> String {
    t(match role {
        CapRole::Owner => "workspace-settings-role-owner-preview",
        CapRole::Editor => "workspace-settings-role-editor-preview",
        CapRole::Commenter => "workspace-settings-role-commenter-preview",
        CapRole::Viewer => "workspace-settings-role-viewer-preview",
        CapRole::Denied => "workspace-settings-role-blocked-preview",
    })
}

/// The catalogue message for a device-scoped capability, or `None` for a
/// device the catalogue has no name for (shown by its id).
fn device_scope_key(device: &str) -> Option<&'static str> {
    match device {
        "camera" => Some("workspace-settings-scope-camera"),
        "microphone" => Some("workspace-settings-scope-microphone"),
        "screencast" => Some("workspace-settings-scope-screencast"),
        "remote-desktop" => Some("workspace-settings-scope-remote-desktop"),
        _ => None,
    }
}

/// What a capability covers, on its own (a share row, a revoke toast).
pub fn scope_label(scope: &Scope, apps: &dyn AppDirectory) -> String {
    scope_phrase(scope, false, apps)
}

/// What a capability covers, phrased as the object of the sentence saying
/// what it allows; languages that inflect the object pick their own form
/// through the `-object` messages.
fn scope_object_label(scope: &Scope, apps: &dyn AppDirectory) -> String {
    scope_phrase(scope, true, apps)
}

fn scope_phrase(scope: &Scope, object: bool, apps: &dyn AppDirectory) -> String {
    let (key, args): (&str, Vec<(&str, ui::i18n::ArgValue)>) = match scope {
        Scope::Workspace | Scope::Room(_) => ("workspace-settings-scope-workspace", Vec::new()),
        Scope::App(app) => ("workspace-settings-scope-app", vec![("app", app.into())]),
        Scope::FsPath(path) => fs_scope_phrase(path),
        Scope::FsRecord(record) => (
            "workspace-settings-scope-record",
            vec![("id", short_id(record).into())],
        ),
        Scope::Device(device) => match device_scope_key(device) {
            Some(key) => (key, Vec::new()),
            None => return device.clone(),
        },
        Scope::NetHost(host) if host == "*" => ("workspace-settings-scope-any-host", Vec::new()),
        Scope::NetHost(host) => ("workspace-settings-scope-host", vec![("host", host.into())]),
        Scope::Action {
            app,
            resource,
            action,
        } => {
            let name = app_display_name(apps, app);
            (
                "workspace-settings-scope-action",
                vec![
                    ("action", action.into()),
                    ("resource", resource.into()),
                    ("app", name.into()),
                ],
            )
        }
        Scope::ActionOnKind { app, kind, action } => {
            let name = app_display_name(apps, app);
            (
                "workspace-settings-scope-action-on-kind",
                vec![
                    ("action", action.into()),
                    ("kind", kind.into()),
                    ("app", name.into()),
                ],
            )
        }
        Scope::ResourceType(kind) => (
            "workspace-settings-scope-resource-type",
            vec![("kind", artifact_kind_selector(*kind).into())],
        ),
        Scope::ResourceInstance(id) => (
            "workspace-settings-scope-resource-instance",
            vec![("id", short_id(&id.0.to_string()).into())],
        ),
    };
    if object {
        t_args(&format!("{key}-object"), &args)
    } else {
        t_args(key, &args)
    }
}

/// A filesystem scope, phrased the way a person would say it. The
/// [`WORKSPACE_FILES_PATH`] mount point never appears in the label — it's
/// a fact about where the OS keeps the bytes, not what the user is
/// agreeing to (HIG §4).
fn fs_scope_phrase(path: &str) -> (&'static str, Vec<(&'static str, ui::i18n::ArgValue)>) {
    // Trailing slash: `/var/workspace/` is the root, not an unnamed child.
    let trimmed = path.trim_end_matches('/');
    if trimmed == WORKSPACE_FILES_PATH {
        return ("workspace-settings-scope-workspace-files", Vec::new());
    }
    match trimmed.strip_prefix(&format!("{WORKSPACE_FILES_PATH}/")) {
        Some(rest) if !rest.is_empty() => (
            "workspace-settings-scope-workspace-folder",
            vec![("folder", rest.into())],
        ),
        _ => (
            "workspace-settings-scope-path",
            vec![("path", trimmed.into())],
        ),
    }
}

/// The selector the scope messages branch on, so each language inflects
/// "every note" / "every board" for itself.
fn artifact_kind_selector(kind: collab_types::role::ArtifactKind) -> &'static str {
    use collab_types::role::ArtifactKind;
    match kind {
        ArtifactKind::Note => "note",
        ArtifactKind::File => "file",
        ArtifactKind::Cluster => "cluster",
        ArtifactKind::Kanban => "kanban",
        ArtifactKind::ChatLog => "chat-log",
    }
}

/// Shorten a long opaque identifier for display — person ids are
/// 64-char hex keys, too much to show in full on a row.
pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 14 {
        return id.to_string();
    }
    let head: String = id.chars().take(10).collect();
    format!("{head}…")
}

/// One line describing what a capability allows, built per scope kind
/// rather than concatenated — "verb · scope" would read "View · the
/// microphone" instead of "Use the microphone".
pub fn capability_allows(cap: &CapOverride, apps: &dyn AppDirectory) -> String {
    let key = match (&cap.scope, cap.verb) {
        (Scope::Device(_), Verb::Write | Verb::Admin) => "workspace-settings-allows-control",
        (Scope::Device(_), _) => "workspace-settings-allows-use",
        (Scope::NetHost(_), _) => "workspace-settings-allows-reach",
        (Scope::Action { .. } | Scope::ActionOnKind { .. }, _) => "workspace-settings-allows-run",
        (_, Verb::Admin) => "workspace-settings-allows-administer",
        (_, Verb::Write) => "workspace-settings-allows-edit",
        (_, Verb::Comment) => "workspace-settings-allows-comment",
        // Inviting names the scope on its own rather than as an object.
        (_, Verb::Invite) => {
            return t_args(
                "workspace-settings-allows-invite",
                &[("what", scope_label(&cap.scope, apps).into())],
            );
        }
        (_, Verb::Read) => "workspace-settings-allows-view",
    };
    t_args(
        key,
        &[("what", scope_object_label(&cap.scope, apps).into())],
    )
}

pub fn expiry_label(expiry: Expiry) -> String {
    t(match expiry {
        Expiry::AfterUse => "workspace-settings-expiry-once",
        Expiry::EndOfSession => "workspace-settings-expiry-session",
        Expiry::AtUnix(_) => "workspace-settings-expiry-until",
        Expiry::Never => "workspace-settings-expiry-always",
    })
}

/// Who granted a capability, as a user-facing phrase, or `None` for a
/// capability with no recorded grantor. People and agents are named as the
/// People pane names them; the viewer is "you".
pub fn granted_by_label(cap: &CapOverride, ws: &Workspace, local_user: &str) -> Option<String> {
    cap.granted_by.as_ref().map(|p| match p {
        Principal::Person(id) | Principal::Agent(id) if id == local_user => {
            t("workspace-settings-granted-by-you")
        }
        Principal::Person(id) => t_args(
            "workspace-settings-granted-by-person",
            &[("id", member_name_or_short_id(ws, id).into())],
        ),
        Principal::App(id) => t_args("workspace-settings-granted-by-app", &[("app", id.into())]),
        Principal::Agent(id) => t_args(
            "workspace-settings-granted-by-agent",
            &[("id", member_name_or_short_id(ws, id).into())],
        ),
        Principal::Token(id) => t_args(
            "workspace-settings-granted-by-token",
            &[("id", short_id(id).into())],
        ),
        Principal::Room(id) => t_args(
            "workspace-settings-granted-by-workspace",
            &[("id", short_id(id).into())],
        ),
    })
}

// Grant composer

/// A subject the grant composer can target, and the subject axis the
/// Permissions list groups by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubjectKind {
    App,
    /// An app granted another app's action: its own section, so what apps
    /// may do to each other is never lost among device and file access.
    AppAction,
    Member,
    Agent,
    Role,
    /// Bearer token or room — surfaced so no live capability is
    /// invisible in the panel, even though the composer can't mint one.
    Other,
}

impl SubjectKind {
    pub fn section_label(self) -> String {
        t(match self {
            SubjectKind::App => "workspace-settings-section-apps",
            SubjectKind::AppAction => "workspace-settings-section-app-actions",
            SubjectKind::Member => "workspace-settings-section-people",
            SubjectKind::Agent => "workspace-settings-section-agents",
            SubjectKind::Role => "workspace-settings-section-roles",
            SubjectKind::Other => "workspace-settings-section-other",
        })
    }
}

/// A resolved subject: what kind of thing it is and how it is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantSubject {
    pub kind: SubjectKind,
    pub id: String,
}

impl GrantSubject {
    pub fn to_subject(&self) -> Option<Subject> {
        match self.kind {
            SubjectKind::App | SubjectKind::AppAction => Some(Subject::app(self.id.clone())),
            SubjectKind::Member => Some(Subject::person(self.id.clone())),
            SubjectKind::Agent => Some(Subject::agent(self.id.clone())),
            // A role-wide grant is a policy decision, not a per-workspace click.
            SubjectKind::Role | SubjectKind::Other => None,
        }
    }

    pub fn display(&self, ws: &Workspace, local_user: &str, apps: &dyn AppDirectory) -> String {
        match self.kind {
            SubjectKind::Member | SubjectKind::Agent => person_name(ws, local_user, &self.id),
            SubjectKind::App | SubjectKind::AppAction => app_display_name(apps, &self.id),
            _ => self.id.clone(),
        }
    }
}

/// What the composer's scope picker maps to. Kept as a small enum
/// rather than a pre-built `Scope` so the table can stay `const`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantTarget {
    Device(&'static str),
    AnyHost,
    WorkspaceFiles,
    /// One of the workspace's own governance actions, on the resource the
    /// workspace publishes them on.
    GovernanceAction {
        action: &'static str,
    },
}

/// One entry in the composer's "what does this allow" picker. Every
/// entry produces a capability an enforcement point actually reads
/// (devices gate the portal, `NetHost` gates egress, `FsPath` gates the
/// sandbox binds and the filesystem, `Workspace` gates administration).
pub struct GrantChoice {
    /// Catalogue key of the picker's label.
    label_key: &'static str,
    target: GrantTarget,
    verb: Verb,
}

impl GrantChoice {
    pub fn label(&self) -> String {
        t(self.label_key)
    }
}

static GRANT_CHOICES: &[GrantChoice] = &[
    GrantChoice {
        label_key: "workspace-settings-grant-camera",
        target: GrantTarget::Device("camera"),
        verb: Verb::Read,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-microphone",
        target: GrantTarget::Device("microphone"),
        verb: Verb::Read,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-screencast",
        target: GrantTarget::Device("screencast"),
        verb: Verb::Read,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-remote-control",
        target: GrantTarget::Device("remote-desktop"),
        verb: Verb::Write,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-network",
        target: GrantTarget::AnyHost,
        verb: Verb::Read,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-read-files",
        target: GrantTarget::WorkspaceFiles,
        verb: Verb::Read,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-edit-files",
        target: GrantTarget::WorkspaceFiles,
        verb: Verb::Write,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-create-roles",
        target: GrantTarget::GovernanceAction {
            action: tilo_desktop_model::PROFILE_SAVE_ACTION_ID,
        },
        verb: Verb::Admin,
    },
    GrantChoice {
        label_key: "workspace-settings-grant-assign-roles",
        target: GrantTarget::GovernanceAction {
            action: tilo_desktop_model::PROFILE_ASSIGN_ACTION_ID,
        },
        verb: Verb::Admin,
    },
];

/// Fallback for [`GrantDraft::choice`] if the index and [`GRANT_CHOICES`]'s
/// length ever disagree — unreachable in practice since the index is
/// always taken modulo the live length, but a real value beats a panic.
const FALLBACK_CHOICE: GrantChoice = GrantChoice {
    label_key: "workspace-settings-grant-edit-files",
    target: GrantTarget::WorkspaceFiles,
    verb: Verb::Write,
};

/// The path workspace files are projected at. A capability scoped here
/// is enforced by the sandbox's binds and by the filesystem alike.
const WORKSPACE_FILES_PATH: &str = "/var/workspace";

pub const EXPIRY_CHOICES: [Expiry; 3] = [Expiry::Never, Expiry::EndOfSession, Expiry::AfterUse];

/// The in-flight grant the Permissions tab's composer is composing.
/// Holds indices rather than resolved values so a subject leaving the
/// workspace can't leave a dangling reference — [`grant_subjects`] is
/// re-derived every frame and the index taken modulo its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantDraft {
    pub subject_index: usize,
    pub choice_index: usize,
    pub expiry: Expiry,
}

impl Default for GrantDraft {
    fn default() -> Self {
        Self {
            subject_index: 0,
            choice_index: 0,
            expiry: Expiry::EndOfSession,
        }
    }
}

impl GrantDraft {
    /// Advances to the next subject, wrapping at `len`. `len == 0` (an
    /// empty subject list) lands back on index 0 via `checked_rem`'s
    /// `None` fallback, same as the guard this replaced.
    pub fn cycle_subject(&mut self, len: usize) {
        self.subject_index = self
            .subject_index
            .wrapping_add(1)
            .checked_rem(len)
            .unwrap_or(0);
    }

    pub fn cycle_choice(&mut self) {
        self.choice_index = self
            .choice_index
            .wrapping_add(1)
            .checked_rem(GRANT_CHOICES.len())
            .unwrap_or(0);
    }

    pub fn choice(&self) -> &'static GrantChoice {
        let idx = self
            .choice_index
            .checked_rem(GRANT_CHOICES.len())
            .unwrap_or(0);
        GRANT_CHOICES.get(idx).unwrap_or(&FALLBACK_CHOICE)
    }
}

/// The subjects the composer can target, in picker order: the
/// workspace's people first, then admitted agents, then every app running in the workspace or
/// already holding a capability — without running apps, a host could
/// only grant to an app already granted something.
pub fn grant_subjects(ws: &Workspace) -> Vec<GrantSubject> {
    let mut out: Vec<GrantSubject> = ws
        .roster()
        .into_iter()
        .map(|(user, _)| GrantSubject {
            kind: SubjectKind::Member,
            id: user,
        })
        .collect();

    out.extend(
        ws.agent_members()
            .into_iter()
            .map(|(agent, _)| GrantSubject {
                kind: SubjectKind::Agent,
                id: agent,
            }),
    );

    let mut apps: Vec<String> = Vec::new();
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                apps.extend(tilo_desktop_model::apps::canonical_app_id(&tab.tile));
            }
        }
    }
    for cap in &ws.capabilities {
        if let Subject::Principal(Principal::App(app)) = &cap.subject {
            apps.push(app.clone());
        }
    }
    apps.sort();
    apps.dedup();
    out.extend(apps.into_iter().map(|id| GrantSubject {
        kind: SubjectKind::App,
        id,
    }));
    out
}

/// Build the capability `draft` currently names against `ws`, or `None`
/// when the draft has no addressable subject (an empty workspace) or
/// names a subject this panel cannot mint for.
pub fn draft_capability(ws: &Workspace, draft: &GrantDraft) -> Option<CapOverride> {
    let subjects = grant_subjects(ws);
    if subjects.is_empty() {
        return None;
    }
    let idx = draft.subject_index.checked_rem(subjects.len()).unwrap_or(0);
    let subject_entry = subjects.get(idx)?;
    let subject = subject_entry.to_subject()?;
    let choice = draft.choice();
    let scope = match choice.target {
        GrantTarget::Device(d) => Scope::Device(d.to_string()),
        GrantTarget::AnyHost => Scope::NetHost("*".to_string()),
        GrantTarget::WorkspaceFiles => Scope::FsPath(WORKSPACE_FILES_PATH.to_string()),
        GrantTarget::GovernanceAction { action } => Scope::Action {
            app: tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID.to_string(),
            resource: tilo_desktop_model::governance_resource_id(&ws.room),
            action: action.to_string(),
        },
    };
    Some(CapOverride::grant(subject, scope, choice.verb).expiring(draft.expiry))
}

/// The subject a draft currently names, for rendering the picker.
pub fn draft_subject(ws: &Workspace, draft: &GrantDraft) -> Option<GrantSubject> {
    let subjects = grant_subjects(ws);
    if subjects.is_empty() {
        return None;
    }
    let idx = draft.subject_index.checked_rem(subjects.len()).unwrap_or(0);
    subjects.get(idx).cloned()
}

// Authority-checked mutations

/// What a panel mutation changed, so the caller can mirror it into the
/// room document and to disk. Empty means either the actor lacked
/// authority, or there was nothing to change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapEdit {
    pub revoked: Vec<String>,
    pub granted: Vec<String>,
}

impl CapEdit {
    pub fn is_empty(&self) -> bool {
        self.revoked.is_empty() && self.granted.is_empty()
    }
}

/// Grant the capability `draft` names, as `actor`. Routes through
/// [`Workspace::grant_as`], the same authority check the portal and
/// replica validator use — not a UI-side copy of it.
pub fn apply_grant_draft(ws: &mut Workspace, actor: &UserId, draft: &GrantDraft) -> CapEdit {
    let Some(cap) = draft_capability(ws, draft) else {
        return CapEdit::default();
    };
    match ws.grant_as(actor, cap) {
        Some(id) => CapEdit {
            revoked: Vec::new(),
            granted: vec![id],
        },
        None => CapEdit::default(),
    }
}

/// Revoke the capability with id `cap_id`, as `actor`. Routes through
/// [`Workspace::revoke_as`].
pub fn apply_revoke(ws: &mut Workspace, actor: &UserId, cap_id: &str) -> CapEdit {
    if ws.revoke_as(actor, cap_id) {
        CapEdit {
            revoked: vec![cap_id.to_string()],
            granted: Vec::new(),
        }
    } else {
        CapEdit::default()
    }
}

/// The roles a member's role chip cycles through. `Denied` is not in
/// the cycle — that's what Revoke expresses, not an accidental click.
const ROLE_CYCLE: [CapRole; 3] = [CapRole::Viewer, CapRole::Commenter, CapRole::Editor];

/// Inverse of the mapping [`Workspace::role_of`] reads.
fn role_verb(role: CapRole) -> Option<Verb> {
    match role {
        // Ownership is a structural governance transfer, not a role-chip
        // mutation. Until that ceremony is signed and replicated end-to-end,
        // this panel must never manufacture a second apparent owner.
        CapRole::Owner | CapRole::Denied => None,
        CapRole::Editor => Some(Verb::Write),
        CapRole::Commenter => Some(Verb::Comment),
        CapRole::Viewer => Some(Verb::Read),
    }
}

pub fn next_role(role: CapRole) -> CapRole {
    if matches!(role, CapRole::Owner | CapRole::Denied) {
        return CapRole::Viewer;
    }
    let idx = ROLE_CYCLE.iter().position(|r| *r == role).unwrap_or(0);
    let next = idx
        .wrapping_add(1)
        .checked_rem(ROLE_CYCLE.len())
        .unwrap_or(0);
    ROLE_CYCLE.get(next).copied().unwrap_or(CapRole::Viewer)
}

fn membership_ids(ws: &Workspace, member: &UserId) -> Vec<String> {
    ws.capabilities
        .iter()
        .filter(|cap| cap.is_room_membership() && subject_is_person(&cap.subject, member))
        .map(|cap| cap.id.clone())
        .collect()
}

fn subject_is_person(subject: &Subject, user: &UserId) -> bool {
    match subject {
        Subject::Principal(Principal::Person(id)) => id == user,
        Subject::User(key) => Principal::person_from_key(*key).id() == user.as_str(),
        _ => false,
    }
}

/// Move `member` to `role`, as `actor`. Lowering a role retires the
/// existing membership capabilities first — grants keep the strongest
/// of two over the same scope.
pub fn set_member_role(
    ws: &mut Workspace,
    actor: &UserId,
    member: &UserId,
    role: CapRole,
) -> CapEdit {
    let Some(verb) = role_verb(role) else {
        return CapEdit::default();
    };
    if !ws.may_administer(actor) || actor == member {
        return CapEdit::default();
    }
    // The owner's role is structural, not a capability — nothing to rewrite.
    if ws.owner.as_deref() == Some(member.as_str()) {
        return CapEdit::default();
    }
    let mut edit = CapEdit::default();
    for id in membership_ids(ws, member) {
        // Authority was proven once above. Mutate atomically from that
        // snapshot: re-checking after each removal lets a delegated admin
        // remove its own authority midway and strands a half-written edit.
        if ws.revoke_capability(&id) {
            edit.revoked.push(id);
        }
    }
    let cap = CapOverride::membership(Subject::person(member.clone()), ws.room.to_string(), verb)
        .granted_by(Principal::Person(actor.clone()));
    edit.granted.push(ws.grant_capability(cap));
    edit
}

/// Remove `member` and every capability they hold, as `actor`.
pub fn remove_member(ws: &mut Workspace, actor: &UserId, member: &UserId) -> CapEdit {
    // The owner is the workspace's root of trust; removing them would
    // leave a room no one can administer.
    if ws.owner.as_deref() == Some(member.as_str()) || actor == member || !ws.may_administer(actor)
    {
        return CapEdit::default();
    }
    let ids: Vec<String> = ws
        .capabilities
        .iter()
        .filter(|cap| subject_is_person(&cap.subject, member))
        .map(|cap| cap.id.clone())
        .collect();
    let mut edit = CapEdit::default();
    for id in ids {
        if ws.revoke_capability(&id) {
            edit.revoked.push(id);
        }
    }
    // Off the roster too, but only if a revoke actually happened.
    if !edit.revoked.is_empty() {
        ws.collaborators.retain(|c| c.id != member.as_str());
    }
    edit
}

fn subject_is_agent(subject: &Subject, agent: &str) -> bool {
    matches!(subject, Subject::Principal(Principal::Agent(id)) if id == agent)
}

/// Revoke every room and resource capability held by `agent`.
pub fn remove_agent(ws: &mut Workspace, actor: &UserId, agent: &str) -> CapEdit {
    if !ws.may_administer(actor) {
        return CapEdit::default();
    }
    let ids: Vec<String> = ws
        .capabilities
        .iter()
        .filter(|cap| subject_is_agent(&cap.subject, agent))
        .map(|cap| cap.id.clone())
        .collect();
    let mut edit = CapEdit::default();
    for id in ids {
        if ws.revoke_capability(&id) {
            edit.revoked.push(id);
        }
    }
    edit
}

// Members pane

/// One row in the Members pane, plus the geometry the renderer painted
/// so click-routing matches a point back to a member without
/// re-deriving layout.
#[derive(Clone, Debug)]
pub struct MemberRow {
    pub user: UserId,
    pub role: CapRole,
    /// The owner's role is structural, so their role chip is inert.
    pub is_workspace_owner: bool,
    pub grant_count: usize,
    pub row_rect: Rect,
    pub role_rect: Rect,
    pub remove_rect: Rect,
}

/// One explicitly non-human row in the Agents pane. Keeping this a
/// separate type makes it hard for a future renderer to accidentally
/// present agents as human roster entries.
#[derive(Clone, Debug)]
pub struct AgentRow {
    pub agent: String,
    pub role: CapRole,
    pub grant_count: usize,
    /// A human-readable summary of the *scoped* authority, not a role.
    /// Agents never inherit their admission capability as ambient access.
    pub preset: AgentAuthorityPreset,
    /// Set when the agent is a service member: a cloud or relay identity
    /// admitted to the room's encrypted channels with a stated purpose.
    pub service: Option<ServiceMember>,
    pub row_rect: Rect,
    pub role_rect: Rect,
    pub review_rect: Rect,
    pub remove_rect: Rect,
}

/// A named starting point for an agent's capability review.
///
/// These are deliberately not `CapRole`s. A person can inherit a room role;
/// an agent cannot. A preset says which explicit, resource-scoped grants to
/// review, and is useful before there are any such grants at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentAuthorityPreset {
    /// Inspect selected material only.
    Observer,
    /// Work on selected files/documents, with no system access implied.
    Contributor,
    /// Invoke selected, explicitly approved app actions.
    Operator,
    /// The current grants do not match a safe built-in bundle.
    #[default]
    Custom,
}

impl AgentAuthorityPreset {
    /// Stable id, recorded in the admission capability's id and accepted
    /// by [`Self::parse`].
    pub const fn id(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Contributor => "contributor",
            Self::Operator => "operator",
            Self::Custom => "custom",
        }
    }

    pub fn label(self) -> String {
        t(match self {
            Self::Observer => "workspace-settings-preset-observer",
            Self::Contributor => "workspace-settings-preset-contributor",
            Self::Operator => "workspace-settings-preset-operator",
            Self::Custom => "workspace-settings-preset-custom",
        })
    }

    /// Plain-language preview shown before the user grants a surface.
    pub fn review_hint(self) -> String {
        t(match self {
            Self::Observer => "workspace-settings-preset-observer-hint",
            Self::Contributor => "workspace-settings-preset-contributor-hint",
            Self::Operator => "workspace-settings-preset-operator-hint",
            Self::Custom => "workspace-settings-preset-custom-hint",
        })
    }

    /// The admission marker used for an initial, permission-free setup. It
    /// does not confer this verb on arbitrary resources: `actor_for_agent`
    /// intentionally carries no role.
    pub const fn admission_verb(self) -> Verb {
        match self {
            Self::Observer | Self::Custom => Verb::Read,
            Self::Contributor => Verb::Comment,
            Self::Operator => Verb::Write,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "observer" | "read" => Some(Self::Observer),
            "contributor" | "contribute" => Some(Self::Contributor),
            "operator" | "operate" => Some(Self::Operator),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Infer the review bundle from live *scoped* grants. Device, network,
/// workspace-wide, and administrative grants intentionally fall into Custom:
/// they need a conscious per-permission review, never a reassuring preset.
pub fn agent_authority_preset(ws: &Workspace, agent: &str) -> AgentAuthorityPreset {
    let mut has_scope = false;
    let mut all_actions = true;
    let mut all_read = true;
    for cap in &ws.capabilities {
        if cap.effect != Effect::Grant
            || !cap.is_live_at(Some(tilo_clock::unix_now()))
            || cap.is_room_membership()
            || !subject_is_agent(&cap.subject, agent)
        {
            continue;
        }
        has_scope = true;
        if matches!(
            &cap.scope,
            Scope::Workspace | Scope::Device(_) | Scope::NetHost(_)
        ) || matches!(cap.verb, Verb::Admin | Verb::Invite)
        {
            return AgentAuthorityPreset::Custom;
        }
        all_actions &= matches!(&cap.scope, Scope::Action { .. });
        all_read &= cap.verb == Verb::Read;
    }
    if !has_scope {
        // The admission record carries a stable setup intent, while the
        // absence of scopes still means no effective authority. Older rooms
        // used an unsuffixed id, so retain a conservative role-derived
        // fallback for them.
        return agent_admission_preset(ws, agent).unwrap_or_else(|| {
            match ws.role_of_agent(agent) {
                Some(CapRole::Viewer) => AgentAuthorityPreset::Observer,
                Some(CapRole::Commenter) => AgentAuthorityPreset::Contributor,
                Some(CapRole::Editor) => AgentAuthorityPreset::Operator,
                _ => AgentAuthorityPreset::Custom,
            }
        });
    }
    if all_actions {
        return AgentAuthorityPreset::Operator;
    }
    if all_read {
        AgentAuthorityPreset::Observer
    } else {
        AgentAuthorityPreset::Contributor
    }
}

fn agent_admission_preset(ws: &Workspace, agent: &str) -> Option<AgentAuthorityPreset> {
    ws.capabilities
        .iter()
        .find(|cap| cap.is_room_membership() && subject_is_agent(&cap.subject, agent))
        .and_then(|cap| cap.id.rsplit(':').next())
        .and_then(AgentAuthorityPreset::parse)
}

/// Admit a named agent without manufacturing ambient authority. The caller
/// still has to grant each file, action, device, or network surface through
/// the normal capability review.
pub fn admit_agent(
    ws: &mut Workspace,
    actor: &UserId,
    agent: impl AsRef<str>,
    preset: AgentAuthorityPreset,
) -> CapEdit {
    let agent = agent.as_ref().trim().to_ascii_lowercase();
    if agent.is_empty() || !ws.may_administer(actor) {
        return CapEdit::default();
    }
    let existing: Vec<_> = ws
        .capabilities
        .iter()
        .filter(|cap| cap.is_room_membership() && subject_is_agent(&cap.subject, &agent))
        .map(|cap| cap.id.clone())
        .collect();
    let mut edit = CapEdit::default();
    for id in existing {
        if ws.revoke_capability(&id) {
            edit.revoked.push(id);
        }
    }
    let cap = CapOverride::membership(
        Subject::agent(agent.clone()),
        ws.room.to_string(),
        preset.admission_verb(),
    )
    .with_id(format!("agent-member:{agent}:{}", preset.id()))
    .granted_by(Principal::Person(actor.clone()));
    edit.granted.push(ws.grant_capability(cap));
    edit
}

/// Geometry for the always-visible entry point into the guided setup.
pub fn agents_add_rect(pane_rect: Rect) -> Rect {
    Rect::new(
        pane_rect.x0 + PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
        pane_rect.x0 + PANE_PAD_X + 132.0,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + AGENT_ONBOARDING_HEIGHT,
    )
}

/// Stable, policy-backed rows for the Agents pane.
pub fn agents_pane_rows(ws: &Workspace, pane_rect: Rect) -> Vec<AgentRow> {
    let mut agents = ws.agent_members();
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    let mut rows = Vec::new();
    let mut cursor_y = pane_rect.y0
        + PANE_PAD_TOP
        + PANE_INTRO_HEIGHT
        + PANE_INTRO_GAP
        + AGENT_ONBOARDING_HEIGHT
        + SECTION_HEADER_HEIGHT;
    for (agent, role) in agents {
        let row_rect = Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            cursor_y,
            pane_rect.x1 - PANE_PAD_X,
            cursor_y + ROW_HEIGHT,
        );
        if row_rect.y1 > pane_rect.y1 - PANE_PAD_BOTTOM {
            break;
        }
        let remove_rect = Rect::new(
            row_rect.x1 - 12.0 - REVOKE_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            row_rect.x1 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let role_rect = Rect::new(
            remove_rect.x0 - 12.0 - AGENT_REVIEW_WIDTH - 12.0 - 96.0,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            remove_rect.x0 - 12.0 - AGENT_REVIEW_WIDTH - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let review_rect = Rect::new(
            remove_rect.x0 - 12.0 - AGENT_REVIEW_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            remove_rect.x0 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let grant_count = ws
            .capabilities
            .iter()
            .filter(|cap| {
                cap.effect == Effect::Grant
                    && !cap.is_room_membership()
                    && subject_is_agent(&cap.subject, &agent)
            })
            .count();
        let preset = agent_authority_preset(ws, &agent);
        let service = service_purpose(ws, &agent, tilo_clock::unix_now());
        rows.push(AgentRow {
            agent,
            role,
            grant_count,
            preset,
            service,
            row_rect,
            role_rect,
            review_rect,
            remove_rect,
        });
        cursor_y += ROW_HEIGHT + ROW_GAP;
    }
    rows
}

/// The roster in a stable display order: owner first, then everyone
/// else by id. [`Workspace::roster`]'s own order follows list position,
/// which moves a row when a role change rewrites its capability.
fn ordered_roster(ws: &Workspace) -> Vec<(UserId, CapRole)> {
    let mut rows = ws.roster();
    let owner = ws.owner.clone();
    rows.sort_by(|a, b| {
        let a_owner = Some(&a.0) == owner.as_ref();
        let b_owner = Some(&b.0) == owner.as_ref();
        b_owner.cmp(&a_owner).then_with(|| a.0.cmp(&b.0))
    });
    rows
}

/// Pure geometry; feeds both the renderer and the click router.
pub fn members_pane_rows(ws: &Workspace, pane_rect: Rect) -> Vec<MemberRow> {
    let mut rows = Vec::new();
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return rows;
    }
    let mut cursor_y =
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + SECTION_HEADER_HEIGHT;
    for (user, role) in ordered_roster(ws) {
        let row_rect = Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            cursor_y,
            pane_rect.x1 - PANE_PAD_X,
            cursor_y + ROW_HEIGHT,
        );
        if row_rect.y1 > pane_rect.y1 - PANE_PAD_BOTTOM {
            break;
        }
        let remove_rect = Rect::new(
            row_rect.x1 - 12.0 - REVOKE_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            row_rect.x1 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let role_w = 96.0_f64;
        let role_rect = Rect::new(
            remove_rect.x0 - 12.0 - role_w,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            remove_rect.x0 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let is_workspace_owner = ws.owner.as_deref() == Some(user.as_str());
        let grant_count = ws
            .capabilities
            .iter()
            .filter(|cap| {
                cap.effect == Effect::Grant
                    && !cap.is_room_membership()
                    && subject_is_person(&cap.subject, &user)
            })
            .count();
        rows.push(MemberRow {
            user,
            role,
            is_workspace_owner,
            grant_count,
            row_rect,
            role_rect,
            remove_rect,
        });
        cursor_y += ROW_HEIGHT + ROW_GAP;
    }
    rows
}

/// `is_owner` is the *local* user's authority — a non-owner sees every
/// row and control, disabled.
pub fn draw_members_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    ws: &Workspace,
    local_user: &UserId,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let rows = members_pane_rows(ws, pane_rect);

    let roster_count = ordered_roster(ws).len();
    let transfer_status = ownership_transfer_status(ws, local_user);
    let member_help = transfer_status.unwrap_or_else(|| t("workspace-settings-members-help"));
    let can_manage_transfer = !ownership_transfer_resource(ws, local_user)
        .actions
        .is_empty();
    draw_pane_intro(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        pane_rect,
        Icon::Home,
        &t_args(
            "workspace-settings-members-count",
            &[("count", roster_count.into())],
        ),
        &member_help,
        &t(if can_manage_transfer {
            "workspace-settings-ownership"
        } else if is_owner {
            "workspace-settings-you-manage"
        } else {
            "workspace-settings-view-only"
        }),
    );
    if can_manage_transfer {
        draw_primary_button(
            scene,
            transform,
            ownership_action_rect(pane_rect),
            &t("workspace-settings-ownership"),
            true,
            text_renderer,
            theme,
        );
    }

    if rows.is_empty() {
        let empty = Rect::new(
            pane_rect.x0,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
            pane_rect.x1,
            pane_rect.y1,
        );
        draw_empty_state(
            scene,
            transform,
            empty,
            Icon::Shield,
            (
                &t("workspace-settings-members-empty-title"),
                &t("workspace-settings-members-empty-detail"),
            ),
            text_renderer,
            theme,
        );
        return;
    }

    draw_section_header(
        scene,
        transform,
        Point::new(
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
        ),
        &t("workspace-settings-people-header"),
        Some(ordered_roster(ws).len()),
        text_renderer,
        theme,
    );

    let mut member_row_context = MemberRowDrawContext {
        scene,
        transform,
        workspace: ws,
        local_user,
        is_owner,
        text_renderer,
        theme,
    };
    for row in &rows {
        draw_member_row(&mut member_row_context, row);
    }

    if !is_owner {
        draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
    }
}

/// Pointer and accessibility target for the ownership action entry point in
/// the Members intro card.
pub fn ownership_action_rect(pane_rect: Rect) -> Rect {
    let center_y = pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT / 2.0;
    Rect::new(
        pane_rect.x1 - PANE_PAD_X - 112.0,
        center_y - 18.0,
        pane_rect.x1 - PANE_PAD_X - 8.0,
        center_y + 18.0,
    )
}

/// Plain-language ownership state shared by visual rendering, accessibility
/// snapshots, and deterministic UI tests.
pub fn ownership_transfer_status(ws: &Workspace, local_user: &UserId) -> Option<String> {
    let proposal = ws.pending_ownership_transfer.as_ref()?;
    let recipient = public_key_member_id(proposal.recipient);
    let owner = public_key_member_id(proposal.current_owner);
    if &recipient == local_user {
        Some(t_args(
            "workspace-settings-ownership-offered",
            &[("owner", member_name_or_short_id(ws, &owner).into())],
        ))
    } else if &owner == local_user {
        Some(t_args(
            "workspace-settings-ownership-waiting",
            &[("recipient", member_name_or_short_id(ws, &recipient).into())],
        ))
    } else {
        Some(t_args(
            "workspace-settings-ownership-pending",
            &[("recipient", member_name_or_short_id(ws, &recipient).into())],
        ))
    }
}

fn member_name_or_short_id(ws: &Workspace, user: &str) -> String {
    ws.collaborators
        .iter()
        .find(|person| person.id == user && !person.name.trim().is_empty())
        .map_or_else(|| short_id(user), |person| person.name.trim().to_string())
}

/// Paint one [`MemberRow`] — avatar, name, role chip, Remove button.
struct MemberRowDrawContext<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    workspace: &'a Workspace,
    local_user: &'a UserId,
    is_owner: bool,
    text_renderer: &'a mut TextRenderer,
    theme: &'a Theme,
}

fn draw_member_row(context: &mut MemberRowDrawContext<'_>, row: &MemberRow) {
    let MemberRowDrawContext {
        scene,
        transform,
        workspace,
        local_user,
        is_owner,
        text_renderer,
        theme,
    } = context;
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    draw_row_card(scene, *transform, row.row_rect, theme);

    let avatar_size = 28.0_f64;
    let avatar = Rect::new(
        row.row_rect.x0 + 12.0,
        row.row_rect.center().y - avatar_size / 2.0,
        row.row_rect.x0 + 12.0 + avatar_size,
        row.row_rect.center().y + avatar_size / 2.0,
    );
    // Membership authority is keyed by the durable id, but the row should
    // lead with the person's known display name. Falling back to a shortened
    // key keeps an unrecognised invite identifiable without presenting raw
    // cryptographic material as normal desktop copy.
    let name = ws_member_display_name(workspace, row, local_user);
    draw_avatar(
        scene,
        *transform,
        avatar,
        &name,
        false,
        text_renderer,
        theme,
    );

    // Person ids are hex keys; a name alone doesn't say which row is you.
    let mut text_limit = row.role_rect.x0 - 12.0;
    if &row.user == *local_user {
        let you = draw_pill(
            scene,
            *transform,
            Point::new(text_limit, row.row_rect.center().y),
            &t("workspace-settings-you"),
            ChipStyle::Neutral,
            text_renderer,
            theme,
        );
        text_limit = you.x0 - 8.0;
    }

    let text_x = avatar.x1 + 12.0;
    let text_max = text_limit - text_x;
    let title_style = tokens.typography.style(TextRole::Caption);
    let title_text = elide(&name, &title_style, text_max, text_renderer);
    text_renderer.draw_style(
        scene,
        &title_text,
        colors.text_primary,
        at(*transform, text_x, row.row_rect.y0 + 12.0),
        None,
        &title_style,
    );

    let displayed_role = if row.role == CapRole::Owner && !row.is_workspace_owner {
        t("workspace-settings-role-admin")
    } else {
        role_label(row.role)
    };
    let detail = t_args(
        "workspace-settings-member-detail",
        &[
            ("role", displayed_role.as_str().into()),
            ("count", row.grant_count.into()),
        ],
    );
    let detail_style = tokens.typography.style(TextRole::Caption);
    let detail_text = elide(&detail, &detail_style, text_max, text_renderer);
    text_renderer.draw_style(
        scene,
        &detail_text,
        colors.text_dim,
        at(*transform, text_x, row.row_rect.y0 + 32.0),
        None,
        &detail_style,
    );

    let can_manage_row = *is_owner && !row.is_workspace_owner && &row.user != *local_user;
    draw_member_role_chip(
        &mut PaintCtx::new(scene, *transform, text_renderer, theme),
        row.role_rect,
        row.role,
        &displayed_role,
        row.is_workspace_owner,
        can_manage_row,
        &detail_style,
    );

    draw_danger_button(
        scene,
        *transform,
        row.remove_rect,
        &t("workspace-settings-remove"),
        can_manage_row,
        text_renderer,
        theme,
    );
}

fn ws_member_display_name(ws: &Workspace, row: &MemberRow, local_user: &UserId) -> String {
    person_name(ws, local_user, &row.user)
}

/// A person or agent as every workspace-settings pane names them: "You" for
/// the viewer, their collaborator name, or a shortened id for someone the
/// workspace has no name for.
fn person_name(ws: &Workspace, local_user: &str, id: &str) -> String {
    if id == local_user {
        return t("workspace-settings-you");
    }
    member_name_or_short_id(ws, id)
}

/// The role chip half of [`draw_member_row`] — its fill, label and the
/// chevron affording that owners can open the explicit role chooser.
fn draw_member_role_chip(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    role: CapRole,
    label: &str,
    structural_owner: bool,
    interactive: bool,
    detail_style: &TextStyle,
) {
    let (scene, transform, text_renderer, theme) =
        (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let role_interactive = interactive;
    let chip_tint = if role == CapRole::Denied {
        colors.semantic.danger
    } else if structural_owner {
        colors.semantic.success
    } else {
        colors.intent.inverted
    };
    let chip_alpha = if role_interactive || structural_owner {
        1.0
    } else {
        DISABLED_ALPHA
    };
    scene.fill(
        Fill::NonZero,
        transform,
        chip_tint.with_alpha_factor(0.20 * chip_alpha),
        None,
        &RoundedRect::from_rect(rect, f64::from(tokens.radii.control)),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        chip_tint.with_alpha_factor(if role_interactive { 0.60 } else { 0.35 }),
        None,
        &RoundedRect::from_rect(rect, f64::from(tokens.radii.control)),
    );
    let role_text = label;
    let (rw, rh) = text_renderer.measure_style(role_text, None, detail_style);
    // text_primary, not the chip's own hue: same-hue label on fill is unreadable.
    text_renderer.draw_style(
        scene,
        role_text,
        colors.text_primary.with_alpha_factor(chip_alpha),
        at(
            transform,
            rect.x0 + 10.0,
            rect.center().y - f64::from(rh) / 2.0,
        ),
        None,
        detail_style,
    );
    if role_interactive {
        let glyph = 12.0_f64;
        let gx = (rect.x0 + 10.0 + f64::from(rw) + 6.0).min(rect.x1 - 6.0 - glyph);
        draw_icon(
            scene,
            transform,
            Rect::new(
                gx,
                rect.center().y - glyph / 2.0,
                gx + glyph,
                rect.center().y + glyph / 2.0,
            ),
            Icon::ChevronDown,
            chip_tint,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

/// Paint the non-human collaborator roster. The square identity mark and
/// persistent AGENT badge are intentionally redundant: shape, text and
/// placement all communicate that these rows are not people.
pub fn draw_agents_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    ws: &Workspace,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let rows = agents_pane_rows(ws, pane_rect);
    let count = ws.agent_members().len();
    draw_pane_intro(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        pane_rect,
        Icon::Sparkles,
        &t_args(
            "workspace-settings-agents-count",
            &[("count", count.into())],
        ),
        &t("workspace-settings-agents-help"),
        &t(if is_owner {
            "workspace-settings-you-manage"
        } else {
            "workspace-settings-view-only"
        }),
    );
    if rows.is_empty() {
        draw_primary_button(
            scene,
            transform,
            agents_add_rect(pane_rect),
            &t("workspace-settings-add-agent"),
            is_owner,
            text_renderer,
            theme,
        );
        let empty = Rect::new(
            pane_rect.x0,
            pane_rect.y0
                + PANE_PAD_TOP
                + PANE_INTRO_HEIGHT
                + PANE_INTRO_GAP
                + AGENT_ONBOARDING_HEIGHT,
            pane_rect.x1,
            pane_rect.y1,
        );
        draw_empty_state(
            scene,
            transform,
            empty,
            Icon::Sparkles,
            (
                &t("workspace-settings-agents-empty-title"),
                &t("workspace-settings-agents-empty-detail"),
            ),
            text_renderer,
            theme,
        );
        return;
    }
    draw_section_header(
        scene,
        transform,
        Point::new(
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y0
                + PANE_PAD_TOP
                + PANE_INTRO_HEIGHT
                + PANE_INTRO_GAP
                + AGENT_ONBOARDING_HEIGHT,
        ),
        &t("workspace-settings-agents-header"),
        Some(count),
        text_renderer,
        theme,
    );
    draw_primary_button(
        scene,
        transform,
        agents_add_rect(pane_rect),
        &t("workspace-settings-add-agent"),
        is_owner,
        text_renderer,
        theme,
    );
    for row in &rows {
        draw_agent_row(scene, transform, row, is_owner, text_renderer, theme);
    }
    if !is_owner {
        draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
    }
}

/// One line on what a service member may do in the room and for how long.
fn service_member_detail(service: &ServiceMember) -> String {
    let access = match service.verb {
        Verb::Admin => "admin",
        Verb::Write | Verb::Invite => "write",
        Verb::Comment => "comment",
        Verb::Read => "read",
    };
    let device = hex::encode(service.device.as_bytes().get(..4).unwrap_or_default());
    let until = service.expires_at.map_or_else(
        || t("workspace-settings-service-until-removed"),
        |deadline| {
            let remaining = deadline.saturating_sub(tilo_clock::unix_now());
            match remaining {
                0..60 => t("workspace-settings-service-ends-soon"),
                60..3_600 => t_args(
                    "workspace-settings-service-ends-minutes",
                    &[("minutes", (remaining / 60).into())],
                ),
                _ => t_args(
                    "workspace-settings-service-ends-hours",
                    &[("hours", (remaining / 3_600).into())],
                ),
            }
        },
    );
    t_args(
        "workspace-settings-service-member",
        &[
            ("access", access.into()),
            ("until", until.into()),
            ("device", device.into()),
        ],
    )
}

fn draw_agent_avatar(scene: &mut Scene, transform: Affine, row: &AgentRow, theme: &Theme) -> Rect {
    let avatar_size = 28.0;
    let avatar = Rect::new(
        row.row_rect.x0 + 12.0,
        row.row_rect.center().y - avatar_size / 2.0,
        row.row_rect.x0 + 12.0 + avatar_size,
        row.row_rect.center().y + avatar_size / 2.0,
    );
    ui::agent_identity::draw_agent_identity_mark(
        scene,
        transform,
        avatar,
        subject_tint(&row.agent, theme),
        theme,
        ui::agent_identity::AgentIdentityStyle::Solid,
    );
    avatar
}

fn draw_agent_row(
    scene: &mut Scene,
    transform: Affine,
    row: &AgentRow,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    draw_row_card(scene, transform, row.row_rect, theme);
    let avatar = draw_agent_avatar(scene, transform, row, theme);
    let name = match &row.service {
        Some(service) if !service.purpose.is_empty() => service.purpose.clone(),
        _ => short_id(&row.agent),
    };
    let badge = draw_pill(
        scene,
        transform,
        Point::new(row.role_rect.x0 - 12.0, row.row_rect.center().y),
        &t(if row.service.is_some() {
            "workspace-settings-badge-service"
        } else {
            "workspace-settings-badge-agent"
        }),
        ChipStyle::Neutral,
        text_renderer,
        theme,
    );
    let text_x = avatar.x1 + 12.0;
    let text_max = badge.x0 - 8.0 - text_x;
    let title_style = tokens.typography.style(TextRole::Caption);
    let title = elide(&name, &title_style, text_max, text_renderer);
    text_renderer.draw_style(
        scene,
        &title,
        colors.text_primary,
        at(transform, text_x, row.row_rect.y0 + 12.0),
        None,
        &title_style,
    );
    let admission = agent_admission_label(row.role);
    let detail = if let Some(service) = &row.service {
        service_member_detail(service)
    } else {
        t_args(
            "workspace-settings-agent-detail",
            &[
                ("admission", admission.into()),
                ("profile", row.preset.label().into()),
                ("count", row.grant_count.into()),
            ],
        )
    };
    let detail_style = tokens.typography.style(TextRole::Caption);
    let detail = elide(&detail, &detail_style, text_max, text_renderer);
    text_renderer.draw_style(
        scene,
        &detail,
        colors.text_dim,
        at(transform, text_x, row.row_rect.y0 + 32.0),
        None,
        &detail_style,
    );
    draw_pill(
        scene,
        transform,
        Point::new(row.role_rect.x1, row.role_rect.center().y),
        &t("workspace-settings-admitted"),
        ChipStyle::Tinted(Tone::Success),
        text_renderer,
        theme,
    );
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(row.review_rect, &t("workspace-settings-review-access"))
            .disabled(!is_owner),
    );
    draw_danger_button(
        scene,
        transform,
        row.remove_rect,
        &t("workspace-settings-revoke"),
        is_owner,
        text_renderer,
        theme,
    );
}

fn agent_admission_label(role: CapRole) -> String {
    t(match role {
        CapRole::Owner => "workspace-settings-admission-admin",
        CapRole::Editor => "workspace-settings-admission-write",
        CapRole::Commenter => "workspace-settings-admission-comment",
        CapRole::Viewer => "workspace-settings-admission-read",
        CapRole::Denied => "workspace-settings-role-blocked",
    })
}

#[derive(Clone, Debug)]
pub struct DefaultAppRow {
    pub resource: desktop_policy::ResourceKind,
    pub workspace_rect: Rect,
    pub user_rect: Rect,
    pub row_rect: Rect,
}

/// Top of the first default-app row: under the intro card, the section
/// header and the column-label line. The pane's empty state starts here
/// too, so it can never paint over that header block.
fn default_app_rows_top(pane: Rect) -> f64 {
    pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + 44.0
}

/// A default-app cycler keeps this far from its row card's edge.
const DEFAULT_APP_ROW_INSET: f64 = 12.0;
const DEFAULT_APP_COLUMN_GAP: f64 = 12.0;
/// What a wide row keeps for its resource label before the two cyclers
/// share the rest.
const DEFAULT_APP_LABEL_MIN: f64 = 120.0;

/// The x-extent of the workspace and personal cyclers in a row spanning
/// `x0..x1`. The column captions above the rows read the same extents, so
/// a caption always sits over its control.
fn default_app_columns(x0: f64, x1: f64, compact: bool) -> ((f64, f64), (f64, f64)) {
    let inner0 = x0 + DEFAULT_APP_ROW_INSET;
    let inner1 = x1 - DEFAULT_APP_ROW_INSET;
    if compact {
        let control_w = ((inner1 - inner0 - DEFAULT_APP_COLUMN_GAP) / 2.0).max(0.0);
        return ((inner0, inner0 + control_w), (inner1 - control_w, inner1));
    }
    let control_w = ((inner1 - inner0 - DEFAULT_APP_LABEL_MIN - 2.0 * DEFAULT_APP_COLUMN_GAP)
        / 2.0)
        .clamp(160.0, 260.0);
    let user = (inner1 - control_w, inner1);
    let workspace_x1 = user.0 - DEFAULT_APP_COLUMN_GAP;
    ((workspace_x1 - control_w, workspace_x1), user)
}

/// The row label for a resource kind in the desktop's language.
fn resource_kind_label(resource: &desktop_policy::ResourceKind) -> String {
    if let Some(ext) = resource.extension_name() {
        t_args(
            "workspace-settings-resource-extension",
            &[("ext", ext.into())],
        )
    } else if let Some(scheme) = resource.scheme_name() {
        t_args(
            "workspace-settings-resource-scheme",
            &[("scheme", scheme.into())],
        )
    } else {
        resource.display_label()
    }
}

pub fn default_app_rows(
    pane: Rect,
    candidates: &[desktop_policy::AppCandidate],
) -> Vec<DefaultAppRow> {
    let mut resources: Vec<_> = candidates
        .iter()
        .flat_map(|a| a.resources.iter().cloned())
        .collect();
    resources.sort();
    resources.dedup();
    let x0 = pane.x0 + PANE_PAD_X;
    let x1 = pane.x1 - PANE_PAD_X;
    let compact = compact_pane(pane);
    let row_h = if compact { 94.0 } else { 64.0 };
    let row_step = if compact { 104.0 } else { 72.0 };
    resources
        .into_iter()
        .enumerate()
        .filter_map(|(index, resource)| {
            // `index` is a row position in a pane-height-bounded list of
            // deduplicated resource kinds — well under 2^53.
            #[allow(
                clippy::cast_precision_loss,
                reason = "index is a small row position bounded by the resource-kind list, well under 2^53"
            )]
            let y0 = default_app_rows_top(pane) + index as f64 * row_step;
            let row_rect = Rect::new(x0, y0, x1, y0 + row_h);
            let controls_y = if compact {
                row_rect.y1 - 10.0 - CONTROL_HEIGHT
            } else {
                y0 + (row_h - CONTROL_HEIGHT) / 2.0
            };
            let ((ws_x0, ws_x1), (user_x0, user_x1)) = default_app_columns(x0, x1, compact);
            let workspace_rect = Rect::new(ws_x0, controls_y, ws_x1, controls_y + CONTROL_HEIGHT);
            let user_rect = Rect::new(user_x0, controls_y, user_x1, controls_y + CONTROL_HEIGHT);
            (row_rect.y1 <= pane.y1 - 10.0).then_some(DefaultAppRow {
                resource,
                workspace_rect,
                user_rect,
                row_rect,
            })
        })
        .collect()
}

fn app_name(id: &str, candidates: &[desktop_policy::AppCandidate]) -> String {
    candidates.iter().find(|a| a.package_id == id).map_or_else(
        || t("workspace-settings-unavailable-app"),
        |a| a.display_name.clone(),
    )
}

/// Bundles the per-workspace inputs to [`draw_default_apps_pane`] so the
/// call stays under clippy's argument-count budget, without disturbing
/// the shared `scene`/`transform`/`text_renderer`/`theme` draw parameters
/// every other pane drawer in this file takes in that same order.
pub struct DefaultAppsCtx<'a> {
    pub ws: &'a Workspace,
    pub local_user: &'a UserId,
    pub candidates: &'a [desktop_policy::AppCandidate],
}

pub fn draw_default_apps_pane(
    scene: &mut Scene,
    transform: Affine,
    pane: Rect,
    ctx: &DefaultAppsCtx<'_>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let DefaultAppsCtx {
        ws,
        local_user,
        candidates,
    } = *ctx;
    let resources = candidates
        .iter()
        .flat_map(|candidate| candidate.resources.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let custom_rules = ws.default_apps.policy.len();
    let status = if custom_rules == 0 {
        t_args(
            "workspace-settings-apps-file-types",
            &[("count", resources.into())],
        )
    } else {
        t_args(
            "workspace-settings-apps-custom-rules",
            &[("count", custom_rules.into())],
        )
    };
    draw_default_apps_header(scene, transform, pane, &status, text_renderer, theme);
    let can_admin = ws.may_administer(local_user);
    draw_default_apps_rows(scene, transform, pane, ctx, can_admin, text_renderer, theme);
    if candidates.iter().all(|a| a.resources.is_empty()) {
        // The empty state owns the row region only: centred in the whole
        // pane it painted across the intro card, the section header and the
        // column labels (HIG §7: nothing overlaps).
        let rows = Rect::new(pane.x0, default_app_rows_top(pane), pane.x1, pane.y1);
        draw_empty_state(
            scene,
            transform,
            rows,
            Icon::LayoutGrid,
            (
                &t("workspace-settings-apps-empty-title"),
                &t("workspace-settings-apps-empty-detail"),
            ),
            text_renderer,
            theme,
        );
    }
}

/// The intro card, section heading and column labels above the row list —
/// split out of [`draw_default_apps_pane`] so that function stays readable
/// as one top-to-bottom pass over the pane.
fn draw_default_apps_header(
    scene: &mut Scene,
    transform: Affine,
    pane: Rect,
    status: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    draw_pane_intro(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        pane,
        Icon::LayoutGrid,
        &t("workspace-settings-apps-intro-title"),
        &t("workspace-settings-apps-intro-detail"),
        status,
    );
    let heading_y = pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP;
    draw_section_header(
        scene,
        transform,
        Point::new(pane.x0 + PANE_PAD_X, heading_y),
        &t("workspace-settings-apps-header"),
        None,
        text_renderer,
        theme,
    );
    let ((workspace_x, _), (user_x, _)) = default_app_columns(
        pane.x0 + PANE_PAD_X,
        pane.x1 - PANE_PAD_X,
        compact_pane(pane),
    );
    for (caption, x) in [
        (t("workspace-settings-workspace-choice"), workspace_x),
        (t("workspace-settings-your-choice"), user_x),
    ] {
        text_renderer.draw_style(
            scene,
            &caption,
            theme.tokens.colors.text_dim,
            at(transform, x, heading_y + 22.0),
            None,
            &theme.text(TextRole::Caption),
        );
    }
}

/// One row per resource kind: the workspace's policy cycler and the local
/// user's own override, split out of [`draw_default_apps_pane`] to keep it
/// under `too_many_lines`.
fn draw_default_apps_rows(
    scene: &mut Scene,
    transform: Affine,
    pane: Rect,
    ctx: &DefaultAppsCtx<'_>,
    can_admin: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let DefaultAppsCtx {
        ws,
        local_user,
        candidates,
    } = *ctx;
    for row in default_app_rows(pane, candidates) {
        draw_row_card(scene, transform, row.row_rect, theme);
        let label_style = theme.text(TextRole::Caption);
        let label_x = row.row_rect.x0 + DEFAULT_APP_ROW_INSET;
        let stacked = row.workspace_rect.y0 >= row.row_rect.y0 + 20.0;
        let label_max = if stacked {
            row.row_rect.x1 - DEFAULT_APP_ROW_INSET - label_x
        } else {
            row.workspace_rect.x0 - DEFAULT_APP_COLUMN_GAP - label_x
        };
        let label = elide(
            &resource_kind_label(&row.resource),
            &label_style,
            label_max,
            text_renderer,
        );
        let label_h = f64::from(text_renderer.measure_style(&label, None, &label_style).1);
        let label_y = if stacked {
            row.row_rect.y0 + 12.0
        } else {
            row.workspace_rect.center().y - label_h / 2.0
        };
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            at(transform, label_x, label_y),
            None,
            &label_style,
        );
        let policy = ws
            .default_apps
            .policy
            .get(&row.resource)
            .unwrap_or(&desktop_policy::WorkspaceDefaultPolicy::FollowPersonal);
        let policy_label = match policy {
            desktop_policy::WorkspaceDefaultPolicy::FollowPersonal => {
                t("workspace-settings-follow-personal")
            }
            desktop_policy::WorkspaceDefaultPolicy::App(id) => app_name(id, candidates),
        };
        let choice = ws
            .default_apps
            .user_choices
            .get(local_user)
            .and_then(|m| m.get(&row.resource))
            .unwrap_or(&desktop_policy::UserWorkspaceChoice::FollowWorkspace);
        let choice_label = match choice {
            desktop_policy::UserWorkspaceChoice::FollowWorkspace => {
                t("workspace-settings-use-workspace-choice")
            }
            desktop_policy::UserWorkspaceChoice::PersonalDefault => {
                t("workspace-settings-use-personal-default")
            }
            desktop_policy::UserWorkspaceChoice::App(id) => app_name(id, candidates),
        };
        draw_cycler(
            scene,
            transform,
            row.workspace_rect,
            &policy_label,
            can_admin,
            text_renderer,
            theme,
        );
        draw_cycler(
            scene,
            transform,
            row.user_rect,
            &choice_label,
            true,
            text_renderer,
            theme,
        );
    }
}

pub fn next_workspace_default(
    current: Option<&desktop_policy::WorkspaceDefaultPolicy>,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> desktop_policy::WorkspaceDefaultPolicy {
    let apps: Vec<_> = candidates.iter().filter(|a| a.handles(resource)).collect();
    match current.unwrap_or(&desktop_policy::WorkspaceDefaultPolicy::FollowPersonal) {
        desktop_policy::WorkspaceDefaultPolicy::FollowPersonal => apps.first().map_or(
            desktop_policy::WorkspaceDefaultPolicy::FollowPersonal,
            |a| desktop_policy::WorkspaceDefaultPolicy::App(a.package_id.clone()),
        ),
        desktop_policy::WorkspaceDefaultPolicy::App(id) => apps
            .iter()
            .position(|a| &a.package_id == id)
            .and_then(|i| apps.get(i.saturating_add(1)))
            .map_or(
                desktop_policy::WorkspaceDefaultPolicy::FollowPersonal,
                |a| desktop_policy::WorkspaceDefaultPolicy::App(a.package_id.clone()),
            ),
    }
}

pub fn next_user_default(
    current: Option<&desktop_policy::UserWorkspaceChoice>,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> desktop_policy::UserWorkspaceChoice {
    let apps: Vec<_> = candidates.iter().filter(|a| a.handles(resource)).collect();
    match current.unwrap_or(&desktop_policy::UserWorkspaceChoice::FollowWorkspace) {
        desktop_policy::UserWorkspaceChoice::FollowWorkspace => {
            desktop_policy::UserWorkspaceChoice::PersonalDefault
        }
        desktop_policy::UserWorkspaceChoice::PersonalDefault => apps
            .first()
            .map_or(desktop_policy::UserWorkspaceChoice::FollowWorkspace, |a| {
                desktop_policy::UserWorkspaceChoice::App(a.package_id.clone())
            }),
        desktop_policy::UserWorkspaceChoice::App(id) => apps
            .iter()
            .position(|a| &a.package_id == id)
            .and_then(|i| apps.get(i.saturating_add(1)))
            .map_or(desktop_policy::UserWorkspaceChoice::FollowWorkspace, |a| {
                desktop_policy::UserWorkspaceChoice::App(a.package_id.clone())
            }),
    }
}

/// A disabled control with no reason reads as a broken one.
fn draw_authority_footnote(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let note = t("workspace-settings-owner-only");
    let style = tokens.typography.style(TextRole::Caption);
    text_renderer.draw_style(
        scene,
        &note,
        tokens.colors.text_dim,
        at(
            transform,
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y1 - PANE_PAD_BOTTOM - 14.0,
        ),
        None,
        &style,
    );
}

// Permissions pane

/// One capability row, keyed by the capability's stable id — the same
/// key [`Workspace::revoke_as`] takes, so a click can never revoke a
/// different capability than the one the row was painted for.
#[derive(Clone, Debug)]
pub struct PermissionRow {
    pub cap_id: String,
    pub subject: GrantSubject,
    /// Who the capability is held by, as a person reads it.
    pub title: String,
    pub allows: String,
    pub expiry: String,
    pub granted_by: Option<String>,
    pub row_rect: Rect,
    pub revoke_rect: Rect,
}

/// Pure layout — the renderer and the click router both read it, so a
/// click always lands on the rect that was painted.
#[derive(Clone, Debug, Default)]
pub struct PermissionsPaneLayout {
    /// Zero-area when the workspace has no addressable subject.
    pub subject_rect: Rect,
    pub choice_rect: Rect,
    /// In [`EXPIRY_CHOICES`] order.
    pub expiry_rects: [Rect; 3],
    pub submit_rect: Rect,
    pub rows: Vec<PermissionRow>,
    /// Capabilities that did not fit the pane.
    pub overflow: usize,
}

/// The section a capability is listed in: its subject's, except that an
/// app granted another app's action is listed under App actions.
fn classify_capability(cap: &CapOverride) -> GrantSubject {
    let mut subject = classify_subject(&cap.subject);
    if subject.kind == SubjectKind::App && matches!(cap.scope, Scope::Action { .. }) {
        subject.kind = SubjectKind::AppAction;
    }
    subject
}

fn classify_subject(subject: &Subject) -> GrantSubject {
    match subject {
        Subject::Principal(Principal::App(id)) => GrantSubject {
            kind: SubjectKind::App,
            id: id.clone(),
        },
        Subject::Principal(Principal::Person(id)) => GrantSubject {
            kind: SubjectKind::Member,
            id: id.clone(),
        },
        Subject::User(key) => GrantSubject {
            kind: SubjectKind::Member,
            id: Principal::person_from_key(*key).id().to_string(),
        },
        Subject::Role(role) => GrantSubject {
            kind: SubjectKind::Role,
            id: t_args(
                "workspace-settings-subject-role",
                &[("role", role_label(*role).into())],
            ),
        },
        Subject::Principal(Principal::Agent(id)) => GrantSubject {
            kind: SubjectKind::Agent,
            id: id.clone(),
        },
        Subject::Principal(Principal::Token(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: t_args(
                "workspace-settings-subject-token",
                &[("id", short_id(id).into())],
            ),
        },
        Subject::Principal(Principal::Room(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: t_args(
                "workspace-settings-subject-workspace",
                &[("id", short_id(id).into())],
            ),
        },
    }
}

/// The capabilities the Permissions pane lists, grouped by subject
/// kind, then subject, then capability id. A *person's* room-membership
/// capability is excluded — the Members tab renders that as a role —
/// but a room/workspace-scoped grant held by an app or agent stays
/// listed here, or it would be revocable from neither tab.
fn listed_capabilities(ws: &Workspace) -> Vec<(GrantSubject, &CapOverride)> {
    let mut out: Vec<(GrantSubject, &CapOverride)> = ws
        .capabilities
        .iter()
        .filter(|cap| {
            let membership_has_own_roster = cap.is_room_membership()
                && (cap.subject_is_person()
                    || matches!(cap.subject, Subject::Principal(Principal::Agent(_))));
            cap.effect == Effect::Grant && !membership_has_own_roster
        })
        .map(|cap| (classify_capability(cap), cap))
        .collect();
    out.sort_by(|a, b| {
        a.0.kind
            .cmp(&b.0.kind)
            .then_with(|| a.0.id.cmp(&b.0.id))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    out
}

/// Geometry does not depend on the draft, so the click router can
/// resolve a point without knowing it.
pub fn permissions_pane_layout(
    ws: &Workspace,
    pane_rect: Rect,
    apps: &dyn AppDirectory,
    local_user: &str,
) -> PermissionsPaneLayout {
    let mut layout = PermissionsPaneLayout::default();
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return layout;
    }

    // ---- Composer -------------------------------------------------
    let card = permission_composer_card(pane_rect);
    layout_composer_controls(&mut layout, ws, card, compact_pane(pane_rect));
    layout_capability_rows(&mut layout, (ws, local_user), pane_rect, card, apps);
    layout
}

/// Header-level entry point for reusable permission roles. Keeping this in
/// the pane's shared geometry module makes paint and hit testing agree.
pub fn permission_profiles_rect(pane_rect: Rect) -> Rect {
    let width = 164.0_f64.min((pane_rect.width() * 0.42).max(0.0));
    let y0 = pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP - 2.0;
    Rect::new(
        pane_rect.x1 - PANE_PAD_X - width,
        y0,
        pane_rect.x1 - PANE_PAD_X,
        y0 + 24.0,
    )
}

fn permission_composer_card(pane_rect: Rect) -> Rect {
    let composer_height = if compact_pane(pane_rect) {
        COMPOSER_HEIGHT + COMPOSER_COMPACT_EXTRA
    } else {
        COMPOSER_HEIGHT
    };
    Rect::new(
        pane_rect.x0 + PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + SECTION_HEADER_HEIGHT,
        pane_rect.x1 - PANE_PAD_X,
        pane_rect.y0
            + PANE_PAD_TOP
            + PANE_INTRO_HEIGHT
            + PANE_INTRO_GAP
            + SECTION_HEADER_HEIGHT
            + composer_height,
    )
}

/// The composer card's subject/choice/expiry/submit controls — split out of
/// [`permissions_pane_layout`] to keep it under `too_many_lines`.
fn layout_composer_controls(
    layout: &mut PermissionsPaneLayout,
    ws: &Workspace,
    card: Rect,
    compact: bool,
) {
    let inner_x0 = card.x0 + 12.0;
    let inner_x1 = card.x1 - 12.0;
    let controls_y = card.y1 - 12.0 - CONTROL_HEIGHT;
    if compact {
        let gap = 8.0_f64;
        let subject_w = (inner_x1 - inner_x0 - gap) / 2.0;
        let top_y = card.y0 + 32.0;
        layout.subject_rect = Rect::new(
            inner_x0,
            top_y,
            inner_x0 + subject_w,
            top_y + CONTROL_HEIGHT,
        );
        layout.choice_rect = Rect::new(
            layout.subject_rect.x1 + gap,
            top_y,
            inner_x1,
            top_y + CONTROL_HEIGHT,
        );
        let submit_w = 72.0;
        let expiry_gap = 6.0;
        let expiry_w = (inner_x1 - inner_x0 - submit_w - gap) / 3.0;
        layout.expiry_rects = std::array::from_fn(|i| {
            #[allow(
                clippy::cast_precision_loss,
                reason = "i is a fixed 3-element array index, always 0..=2"
            )]
            let x0 = inner_x0 + i as f64 * expiry_w;
            Rect::new(
                x0,
                controls_y,
                x0 + expiry_w - expiry_gap,
                controls_y + CONTROL_HEIGHT,
            )
        });
        layout.submit_rect = Rect::new(
            inner_x1 - submit_w,
            controls_y,
            inner_x1,
            controls_y + CONTROL_HEIGHT,
        );
        if grant_subjects(ws).is_empty() {
            layout.subject_rect = Rect::ZERO;
            layout.submit_rect = Rect::ZERO;
        }
        return;
    }
    let gap = 8.0_f64;
    let expiry_w = EXPIRY_SEGMENT_WIDTH * 3.0;
    let flexible = (inner_x1 - inner_x0) - expiry_w - SUBMIT_WIDTH - gap * 3.0;
    let subject_w = (flexible * 0.45).max(72.0);
    let choice_w = (flexible - subject_w).max(72.0);

    layout.subject_rect = Rect::new(
        inner_x0,
        controls_y,
        inner_x0 + subject_w,
        controls_y + CONTROL_HEIGHT,
    );
    layout.choice_rect = Rect::new(
        layout.subject_rect.x1 + gap,
        controls_y,
        layout.subject_rect.x1 + gap + choice_w,
        controls_y + CONTROL_HEIGHT,
    );
    let expiry_x0 = layout.choice_rect.x1 + gap;
    layout.expiry_rects = std::array::from_fn(|i| {
        // `i` is a `std::array::from_fn::<_, 3>` index, always 0..=2.
        #[allow(
            clippy::cast_precision_loss,
            reason = "i is a fixed 3-element array index, always 0..=2"
        )]
        let x0 = expiry_x0 + i as f64 * EXPIRY_SEGMENT_WIDTH;
        Rect::new(
            x0,
            controls_y,
            x0 + EXPIRY_SEGMENT_WIDTH,
            controls_y + CONTROL_HEIGHT,
        )
    });
    layout.submit_rect = Rect::new(
        inner_x1 - SUBMIT_WIDTH,
        controls_y,
        inner_x1,
        controls_y + CONTROL_HEIGHT,
    );
    if grant_subjects(ws).is_empty() {
        layout.subject_rect = Rect::ZERO;
        layout.submit_rect = Rect::ZERO;
    }
}

/// The capability list below the composer, stopping once a row would run
/// past the pane — split out of [`permissions_pane_layout`] to keep it
/// under `too_many_lines`.
fn layout_capability_rows(
    layout: &mut PermissionsPaneLayout,
    (ws, local_user): (&Workspace, &str),
    pane_rect: Rect,
    card: Rect,
    apps: &dyn AppDirectory,
) {
    let entries = listed_capabilities(ws);
    // Keep a dedicated footer band for overflow/authority copy. Rows must
    // never consume that space and then have explanatory text painted over
    // their final line.
    let footer_top = pane_rect.y1 - PANE_PAD_BOTTOM - 34.0;
    let mut cursor_y = card.y1 + 16.0;
    let mut last_kind: Option<SubjectKind> = None;
    let mut placed = 0usize;
    for (subject, cap) in &entries {
        let needs_header = last_kind != Some(subject.kind);
        let header_h = if needs_header {
            SECTION_HEADER_HEIGHT
        } else {
            0.0
        };
        let row_y0 = cursor_y + header_h;
        if row_y0 + ROW_HEIGHT > footer_top {
            break;
        }
        let row_rect = Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            row_y0,
            pane_rect.x1 - PANE_PAD_X,
            row_y0 + ROW_HEIGHT,
        );
        let revoke_rect = Rect::new(
            row_rect.x1 - 12.0 - REVOKE_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            row_rect.x1 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        layout.rows.push(PermissionRow {
            cap_id: cap.id.clone(),
            subject: subject.clone(),
            title: subject.display(ws, local_user, apps),
            allows: capability_allows(cap, apps),
            expiry: expiry_label(cap.expiry),
            granted_by: granted_by_label(cap, ws, local_user),
            row_rect,
            revoke_rect,
        });
        last_kind = Some(subject.kind);
        cursor_y = row_rect.y1 + ROW_GAP;
        placed = placed.saturating_add(1);
    }
    layout.overflow = entries.len().saturating_sub(placed);
}

/// Non-owners see the same list and controls, painted disabled, with a
/// footnote explaining why.
pub fn draw_permissions_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    (ws, apps, local_user): (&Workspace, &dyn AppDirectory, &str),
    draft: &GrantDraft,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let layout = permissions_pane_layout(ws, pane_rect, apps, local_user);
    let active_grants = listed_capabilities(ws).len();
    draw_pane_intro(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        pane_rect,
        Icon::Shield,
        &t("workspace-settings-permissions-intro-title"),
        &t("workspace-settings-permissions-intro-detail"),
        &t_args(
            "workspace-settings-active-grants",
            &[("count", active_grants.into())],
        ),
    );

    let card = draw_permission_composer(
        scene,
        transform,
        pane_rect,
        (ws, draft, apps, local_user),
        is_owner,
        &layout,
        (&mut *text_renderer, theme),
    );

    // ---- Capability rows ------------------------------------------
    if layout.rows.is_empty() {
        // Empty content belongs directly to the composer it explains. A
        // full-height body centred the message in otherwise blank space and
        // made the owner-only explanation look detached at the footer.
        let body = Rect::new(
            pane_rect.x0,
            card.y1 + 8.0,
            pane_rect.x1,
            (card.y1 + 122.0).min(pane_rect.y1 - 54.0),
        );
        draw_empty_state(
            scene,
            transform,
            body,
            Icon::Shield,
            (
                &t("workspace-settings-permissions-empty-title"),
                &t("workspace-settings-permissions-empty-detail"),
            ),
            text_renderer,
            theme,
        );
        if !is_owner {
            draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
        }
        return;
    }

    let mut last_kind: Option<SubjectKind> = None;
    let group_counts = |kind: SubjectKind| -> usize {
        layout
            .rows
            .iter()
            .filter(|r| r.subject.kind == kind)
            .count()
    };
    for row in &layout.rows {
        if last_kind != Some(row.subject.kind) {
            draw_section_header(
                scene,
                transform,
                Point::new(
                    row.row_rect.x0,
                    row.row_rect.y0 - SECTION_HEADER_HEIGHT + 6.0,
                ),
                &row.subject.kind.section_label(),
                Some(group_counts(row.subject.kind)),
                text_renderer,
                theme,
            );
            last_kind = Some(row.subject.kind);
        }
        draw_permission_row(scene, transform, row, is_owner, text_renderer, theme);
    }

    if layout.overflow > 0 {
        let more = t_args(
            "workspace-settings-more",
            &[("count", layout.overflow.into())],
        );
        text_renderer.draw_style(
            scene,
            &more,
            theme.tokens.colors.text_dim,
            at(
                transform,
                pane_rect.x0 + PANE_PAD_X,
                pane_rect.y1 - PANE_PAD_BOTTOM - 14.0,
            ),
            None,
            &theme.text(TextRole::Caption),
        );
    } else if !is_owner {
        draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
    }
}

/// The "Who" and "What it allows" cyclers inside the composer card.
fn draw_who_what_pickers(
    scene: &mut Scene,
    transform: Affine,
    card: Rect,
    subject_draft: (&Workspace, &GrantDraft, &dyn AppDirectory, &str),
    is_owner: bool,
    layout: &PermissionsPaneLayout,
    render: (&mut TextRenderer, &Theme),
) {
    let (ws, draft, apps, local_user) = subject_draft;
    let (text_renderer, theme) = render;
    let caption_y = if compact_pane(card) {
        layout.subject_rect.y0 - 18.0
    } else {
        card.y0 + 12.0
    };
    let subject = draft_subject(ws, draft);
    match &subject {
        Some(entry) => {
            let caption = t(if entry.kind == SubjectKind::Agent {
                "workspace-settings-who-agent"
            } else {
                "workspace-settings-who"
            });
            draw_control_caption(
                scene,
                transform,
                Point::new(layout.subject_rect.x0 + 2.0, caption_y),
                &caption,
                text_renderer,
                theme,
            );
            draw_cycler(
                scene,
                transform,
                layout.subject_rect,
                &entry.display(ws, local_user, apps),
                is_owner,
                text_renderer,
                theme,
            );
        }
        None => {
            draw_control_caption(
                scene,
                transform,
                Point::new(card.x0 + 12.0, caption_y),
                &t("workspace-settings-nobody-to-grant"),
                text_renderer,
                theme,
            );
        }
    }
    draw_control_caption(
        scene,
        transform,
        Point::new(layout.choice_rect.x0 + 2.0, caption_y),
        &t("workspace-settings-what-it-allows"),
        text_renderer,
        theme,
    );
    draw_cycler(
        scene,
        transform,
        layout.choice_rect,
        &draft.choice().label(),
        is_owner,
        text_renderer,
        theme,
    );
}

/// Paint the "Grant a permission" composer card — header, the who/what
/// cyclers, the expiry track, the submit button. Returns the card's rect
/// so the caller can lay out the capability rows beneath it.
fn draw_permission_composer(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    subject_draft: (&Workspace, &GrantDraft, &dyn AppDirectory, &str),
    is_owner: bool,
    layout: &PermissionsPaneLayout,
    render: (&mut TextRenderer, &Theme),
) -> Rect {
    let draft = subject_draft.1;
    let (text_renderer, theme) = render;
    let tokens = &theme.tokens;
    let colors = &tokens.colors;

    draw_section_header(
        scene,
        transform,
        Point::new(
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
        ),
        &t("workspace-settings-grant-header"),
        None,
        text_renderer,
        theme,
    );
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(
            permission_profiles_rect(pane_rect),
            &t_args(
                "workspace-settings-saved-roles",
                &[("count", subject_draft.0.permission_profiles.len().into())],
            ),
        )
        .disabled(!is_owner),
    );
    let card = permission_composer_card(pane_rect);
    ui::surface::card(
        scene,
        transform,
        card,
        f64::from(tokens.radii.control),
        colors.surfaces.raised,
    );

    draw_who_what_pickers(
        scene,
        transform,
        card,
        subject_draft,
        is_owner,
        layout,
        (&mut *text_renderer, theme),
    );

    let caption_y = if compact_pane(pane_rect) {
        layout.expiry_rects[0].y0 - COMPOSER_CAPTION_BAND
    } else {
        card.y0 + 12.0
    };
    draw_control_caption(
        scene,
        transform,
        Point::new(layout.expiry_rects[0].x0 + 2.0, caption_y),
        &t("workspace-settings-expiry-caption"),
        text_renderer,
        theme,
    );
    draw_expiry_track(
        scene,
        transform,
        layout,
        draft,
        is_owner,
        text_renderer,
        theme,
    );

    if layout.submit_rect.area() > 0.0 {
        draw_primary_button(
            scene,
            transform,
            layout.submit_rect,
            &t("workspace-settings-grant"),
            is_owner,
            text_renderer,
            theme,
        );
    }
    card
}

/// The expiry segmented control inside the composer card.
fn draw_expiry_track(
    scene: &mut Scene,
    transform: Affine,
    layout: &PermissionsPaneLayout,
    draft: &GrantDraft,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let selected = EXPIRY_CHOICES
        .iter()
        .position(|expiry| *expiry == draft.expiry)
        .unwrap_or_default();
    let owned_labels = EXPIRY_CHOICES.map(expiry_label);
    let labels = owned_labels.each_ref().map(String::as_str);
    // `SegmentedFace` owns the selected/unselected contrast, separators and
    // shared focus-compatible geometry. The surrounding disabled layer keeps
    // a viewer's immutable policy visibly unavailable without changing which
    // actions the router authorizes.
    if !is_owner {
        let track = Rect::new(
            layout.expiry_rects[0].x0,
            layout.expiry_rects[0].y0,
            layout.expiry_rects[2].x1,
            layout.expiry_rects[2].y1,
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            DISABLED_ALPHA,
            transform,
            &track,
        );
    }
    ui::widget::ToggleGroup::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &SegmentedFace::new(&layout.expiry_rects, &labels, selected),
    );
    if !is_owner {
        scene.pop_layer();
    }
}

/// Paint one [`PermissionRow`] — avatar, subject, expiry chip, Revoke button.
fn draw_permission_row(
    scene: &mut Scene,
    transform: Affine,
    row: &PermissionRow,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    draw_row_card(scene, transform, row.row_rect, theme);

    let avatar_size = 28.0_f64;
    let avatar = Rect::new(
        row.row_rect.x0 + 12.0,
        row.row_rect.center().y - avatar_size / 2.0,
        row.row_rect.x0 + 12.0 + avatar_size,
        row.row_rect.center().y + avatar_size / 2.0,
    );
    let title = &row.title;
    draw_avatar(
        scene,
        transform,
        avatar,
        title,
        matches!(row.subject.kind, SubjectKind::App | SubjectKind::Agent),
        text_renderer,
        theme,
    );

    let chip = draw_pill(
        scene,
        transform,
        Point::new(row.revoke_rect.x0 - 12.0, row.row_rect.center().y),
        &row.expiry,
        ChipStyle::Neutral,
        text_renderer,
        theme,
    );

    let text_x = avatar.x1 + 12.0;
    let text_max = chip.x0 - 12.0 - text_x;
    let title_style = tokens.typography.style(TextRole::Caption);
    let title_text = elide(title, &title_style, text_max, text_renderer);
    text_renderer.draw_style(
        scene,
        &title_text,
        colors.text_primary,
        at(transform, text_x, row.row_rect.y0 + 12.0),
        None,
        &title_style,
    );
    let detail = match &row.granted_by {
        Some(by) => t_args(
            "workspace-settings-permission-detail",
            &[
                ("allows", row.allows.as_str().into()),
                ("grantor", by.into()),
            ],
        ),
        None => row.allows.clone(),
    };
    let detail_style = tokens.typography.style(TextRole::Caption);
    let detail_text = elide(&detail, &detail_style, text_max, text_renderer);
    text_renderer.draw_style(
        scene,
        &detail_text,
        colors.text_dim,
        at(transform, text_x, row.row_rect.y0 + 32.0),
        None,
        &detail_style,
    );

    draw_danger_button(
        scene,
        transform,
        row.revoke_rect,
        &t("workspace-settings-revoke"),
        is_owner,
        text_renderer,
        theme,
    );
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact settings layout geometry is the behavior under test"
)]
mod tab_tests {
    //! The modal's tab surface: default tab, switching, sidebar
    //! geometry, and that the sidebar renders without a pane drawer.

    use super::*;
    /// Whoever is looking at the pane; never a grantor in these fixtures.
    const VIEWER: &str = "viewer-user";

    fn ws_settings_default() -> WorkspaceSettings {
        WorkspaceSettings::new(WorkspaceId::default())
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn settings_tabs_navigate_hit_test_and_draw_the_active_pane() {
        // Phase `settings_tabs_default_to_general_and_switch_in_nav_order`.
        {
            // Phase `ws_settings_default_tab_is_general`.
            {
                let ws = ws_settings_default();
                assert_eq!(ws.active_tab, WsTab::General);
            }

            // Phase `select_tab_switches_active`.
            {
                let mut ws = ws_settings_default();
                ws.select_tab(WsTab::Members);
                assert_eq!(ws.active_tab, WsTab::Members);
                ws.select_tab(WsTab::Agents);
                assert_eq!(ws.active_tab, WsTab::Agents);
                ws.select_tab(WsTab::Permissions);
                assert_eq!(ws.active_tab, WsTab::Permissions);
                ws.select_tab(WsTab::General);
                assert_eq!(ws.active_tab, WsTab::General);
            }

            // Phase `ws_tab_default_matches_general`.
            {
                assert_eq!(WsTab::default(), WsTab::General);
            }

            // Phase `ws_tab_all_iterates_in_nav_order`.
            {
                assert_eq!(
                    WsTab::all(),
                    [
                        WsTab::General,
                        WsTab::Members,
                        WsTab::Agents,
                        WsTab::Permissions,
                        WsTab::Apps
                    ]
                );
            }

            // Phase `compact_labels_keep_every_destination_distinct`.
            {
                assert_eq!(
                    WsTab::all().map(|tab| tab.compact_label()),
                    ["General", "People", "Agents", "Privacy", "Apps"]
                );
            }

            // Phase `permissions_tab_is_reachable_on_a_workspace_with_no_grants`.
            {
                // A host who wants to grant something before any grant exists
                // has to be able to reach the tab that grants it.
                let ws = Workspace::new(WorkspaceId::default(), "no-grants");
                assert!(!ws.has_app_grants());
                assert!(WsTab::all().contains(&WsTab::Permissions));
                let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
                let layout = permissions_pane_layout(&ws, pane, &NoAppDirectory, VIEWER);
                assert!(
                    layout.choice_rect.area() > 0.0,
                    "the capability picker must be laid out even with zero grants"
                );
            }
        }

        // Phase `settings_sidebar_nav_geometry_and_hit_testing`.
        {
            // Phase `sidebar_rect_is_the_token_sidebar_width`.
            {
                let ws = ws_settings_default();
                let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
                let sidebar = ws.sidebar_rect(modal);
                assert_eq!(sidebar.width(), SIDEBAR_WIDTH);
                assert_eq!(
                    SIDEBAR_WIDTH,
                    f64::from(ui::tokens::Metrics::BASE.sidebar_width),
                    "the lens sidebar is the docked-sidebar width (HIG §9.6), not a bespoke 256"
                );
                assert_eq!(sidebar.height(), modal.height());
                assert_eq!(sidebar.x0, modal.x0);
            }

            // Phase `pane_rect_starts_after_sidebar`.
            {
                let ws = ws_settings_default();
                let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
                let pane = ws.pane_rect(modal);
                assert_eq!(pane.x0, modal.x0 + SIDEBAR_WIDTH);
                assert_eq!(pane.x1, modal.x1);
                assert!((pane.width() + SIDEBAR_WIDTH - modal.width()).abs() < f64::EPSILON);
            }

            // Phase `nav_rows_yield_one_rect_per_tab_in_order`.
            {
                let ws = ws_settings_default();
                let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
                let rows = ws.nav_rows(modal);
                assert_eq!(rows.len(), 5);
                assert_eq!(rows[0].0, WsTab::General);
                assert_eq!(rows[1].0, WsTab::Members);
                assert_eq!(rows[2].0, WsTab::Agents);
                assert_eq!(rows[3].0, WsTab::Permissions);
                assert_eq!(rows[4].0, WsTab::Apps);
                for (_, r) in &rows {
                    assert!((r.height() - NAV_ROW_HEIGHT).abs() < f64::EPSILON);
                    assert!(r.x1 > r.x0);
                }
                let stride = NAV_ROW_HEIGHT + NAV_ROW_GAP;
                assert!((rows[1].1.y0 - rows[0].1.y0 - stride).abs() < f64::EPSILON);
                assert!((rows[2].1.y0 - rows[1].1.y0 - stride).abs() < f64::EPSILON);
            }

            // Phase `nav_hit_test_returns_tab_at_point`.
            {
                let ws = ws_settings_default();
                let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
                for (tab, row) in ws.nav_rows(modal) {
                    assert_eq!(ws.nav_hit_test(modal, row.center()), Some(tab));
                }
                assert_eq!(
                    ws.nav_hit_test(modal, Point::new(modal.x1 - 10.0, modal.y1 - 10.0)),
                    None
                );
            }
        }

        // Phase `draw_body_paints_the_sidebar_and_the_active_pane`.
        {
            // Phase `draw_body_paints_sidebar_when_pane_drawer_absent`.
            {
                let ws = ws_settings_default();
                let mut scene = Scene::new();
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
                ws.draw_body(&mut scene, Affine::IDENTITY, modal, &mut tr, &theme, None);
                assert!(
                    !scene.encoding().draw_tags.is_empty(),
                    "sidebar must paint at least one draw command even without a pane drawer"
                );
            }

            // Phase `draw_body_invokes_pane_drawer_for_active_tab`.
            {
                let mut ws = ws_settings_default();
                ws.select_tab(WsTab::Members);
                let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
                let expected_pane = ws.pane_rect(modal);

                let mut observed: Option<Rect> = None;
                let mut scene = Scene::new();
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let mut drawer =
                    |_s: &mut Scene, _x: Affine, pane: Rect, _t: &mut TextRenderer, _th: &Theme| {
                        observed = Some(pane);
                    };
                ws.draw_body(
                    &mut scene,
                    Affine::IDENTITY,
                    modal,
                    &mut tr,
                    &theme,
                    Some(&mut drawer),
                );
                assert_eq!(observed, Some(expected_pane));
            }
        }
    }
}

#[cfg(test)]
mod members_pane_tests {
    //! The Members pane renders [`Workspace::roster`] — the membership
    //! the capability model actually holds — and its role control
    //! rewrites the membership capability that `role_of` reads back.

    use super::*;
    use collab_types::caps::Verb;
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const PEER: &str = "peer-user";

    fn ws_with_two_members() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(7), "members-test");
        ws.set_owner(OWNER);
        let cap =
            CapOverride::membership(Subject::person(PEER), ws.id.raw().to_string(), Verb::Read);
        ws.grant_as(&OWNER.to_string(), cap)
            .expect("owner may grant membership");
        ws
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn member_roles_come_from_the_roster_and_change_only_within_owner_authority() {
        // Phase `members_rows_come_from_the_roster`.
        {
            let ws = ws_with_two_members();
            let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
            let rows = members_pane_rows(&ws, pane);
            assert_eq!(rows.len(), 2, "owner + invited peer");
            let owner_row = rows
                .iter()
                .find(|r| r.user == OWNER)
                .expect("owner is in the roster");
            assert!(owner_row.is_workspace_owner);
            assert_eq!(owner_row.role, CapRole::Owner);
            let peer_row = rows.iter().find(|r| r.user == PEER).expect("peer row");
            assert!(!peer_row.is_workspace_owner);
            assert_eq!(peer_row.role, CapRole::Viewer);
            for row in &rows {
                assert!(row.role_rect.x0 > row.row_rect.x0);
                assert!(row.remove_rect.x1 <= row.row_rect.x1);
                assert!(row.role_rect.x1 <= row.remove_rect.x0);
            }
        }

        // Phase `owner_can_promote_a_member_and_role_of_agrees`.
        {
            let mut ws = ws_with_two_members();
            assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
            let edit = set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Editor,
            );
            assert!(!edit.is_empty());
            assert_eq!(
                ws.role_of(&PEER.to_string()),
                Some(CapRole::Editor),
                "the role the panel shows must be the role the model decides with"
            );
            assert!(ws.allows(
                &ws.actor_for_person(&PEER.to_string()),
                &collab_types::caps::ResourceRef::Workspace,
                Verb::Write
            ));
        }

        // Phase `owner_can_demote_a_member`.
        {
            // `grant_capability` keeps the strongest of two grants, so the
            // old membership must be retired first or `role_of` keeps
            // answering the higher role.
            let mut ws = ws_with_two_members();
            set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Editor,
            );
            assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Editor));
            set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Viewer,
            );
            assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
            assert!(
                !ws.allows(
                    &ws.actor_for_person(&PEER.to_string()),
                    &collab_types::caps::ResourceRef::Workspace,
                    Verb::Write
                ),
                "a demoted member must stop being able to write, not merely stop reading as Editor"
            );
        }

        // Phase `non_owner_cannot_change_a_role`.
        {
            let mut ws = ws_with_two_members();
            let edit = set_member_role(
                &mut ws,
                &PEER.to_string(),
                &PEER.to_string(),
                CapRole::Owner,
            );
            assert!(
                edit.is_empty(),
                "a viewer must not be able to promote itself"
            );
            assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
        }

        // Phase `the_recorded_owner_role_is_structural`.
        {
            let mut ws = ws_with_two_members();
            let edit = set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &OWNER.to_string(),
                CapRole::Viewer,
            );
            assert!(edit.is_empty());
            assert_eq!(ws.role_of(&OWNER.to_string()), Some(CapRole::Owner));
        }

        // Phase `role_chip_cycles_through_the_positive_roles`.
        {
            assert_eq!(next_role(CapRole::Viewer), CapRole::Commenter);
            assert_eq!(next_role(CapRole::Commenter), CapRole::Editor);
            assert_eq!(next_role(CapRole::Editor), CapRole::Viewer);
            assert_eq!(next_role(CapRole::Owner), CapRole::Viewer);
            assert_eq!(next_role(CapRole::Denied), CapRole::Viewer);
        }

        // Phase `role_control_cannot_create_or_transfer_ownership`.
        {
            let mut ws = ws_with_two_members();
            let edit = set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Owner,
            );
            assert!(edit.is_empty());
            assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
            assert_eq!(ws.owner.as_deref(), Some(OWNER));
        }

        // Phase `delegated_admin_cannot_partially_demote_or_remove_itself`.
        {
            let mut ws = ws_with_two_members();
            let admin =
                CapOverride::membership(Subject::person(PEER), ws.room.to_string(), Verb::Admin);
            ws.grant_as(&OWNER.to_string(), admin)
                .expect("owner delegates administration");
            let before = ws.capabilities.clone();

            assert!(
                set_member_role(
                    &mut ws,
                    &PEER.to_string(),
                    &PEER.to_string(),
                    CapRole::Viewer,
                )
                .is_empty()
            );
            assert!(remove_member(&mut ws, &PEER.to_string(), &PEER.to_string()).is_empty());
            assert_eq!(ws.capabilities, before);
            assert!(ws.may_administer(&PEER.to_string()));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn member_removal_and_ownership_protect_the_owner() {
        // Phase `removing_a_member_takes_their_grants_but_never_removes_the_owner`.
        {
            // Phase `removing_a_member_takes_their_grants_with_them`.
            {
                let mut ws = ws_with_two_members();
                let cap = CapOverride::grant(
                    Subject::person(PEER),
                    Scope::FsPath("/var/workspace".into()),
                    Verb::Write,
                );
                ws.grant_as(&OWNER.to_string(), cap).expect("owner grants");
                assert_eq!(
                    members_pane_rows(&ws, Rect::new(0.0, 0.0, 640.0, 480.0))[1].grant_count,
                    1
                );

                let edit = remove_member(&mut ws, &OWNER.to_string(), &PEER.to_string());
                assert_eq!(edit.revoked.len(), 2, "membership + the file grant");
                assert_eq!(ws.role_of(&PEER.to_string()), None);
                assert!(
                    !ws.allows(
                        &ws.actor_for_person(&PEER.to_string()),
                        &collab_types::caps::ResourceRef::FsPath("/var/workspace/notes".into()),
                        Verb::Write
                    ),
                    "removing a member must revoke what they held, not just hide them"
                );
            }

            // Phase `the_owner_cannot_be_removed`.
            {
                let mut ws = ws_with_two_members();
                let edit = remove_member(&mut ws, &OWNER.to_string(), &OWNER.to_string());
                assert!(edit.is_empty());
                assert_eq!(ws.role_of(&OWNER.to_string()), Some(CapRole::Owner));
            }
        }

        // Phase `ownership_is_a_separate_ceremony_named_by_person`.
        {
            // Phase `ownership_ceremony_is_separate_typed_confirmed_and_accessible`.
            {
                let (owner_key, owner_public) = collab_types::generate_keypair();
                let (_, recipient_public) = collab_types::generate_keypair();
                let owner = hex::encode(owner_public.as_bytes());
                let recipient = hex::encode(recipient_public.as_bytes());
                let mut ws = Workspace::new(WorkspaceId(91), "Research");
                ws.set_owner(owner.clone());
                ws.grant_capability(CapOverride::membership(
                    Subject::person(recipient.clone()),
                    ws.room.to_string(),
                    Verb::Admin,
                ));
                ws.pending_ownership_transfer = Some(
                    collab_types::OwnershipTransferProposalV1::new(
                        ws.room.to_string(),
                        [8; 32],
                        owner_public,
                        recipient_public,
                        500,
                        &owner_key,
                    )
                    .unwrap(),
                );

                let owner_resource = ownership_transfer_resource(&ws, &owner);
                assert!(owner_resource.actions.iter().any(|action| {
                    action.id == "ownership.propose"
                        && action.contract.confirmation == tilo_actions::ConfirmationPolicy::Confirm
                }));
                assert!(
                    owner_resource
                        .actions
                        .iter()
                        .any(|action| action.id == "ownership.cancel")
                );
                let recipient_resource = ownership_transfer_resource(&ws, &recipient);
                let accept = recipient_resource
                    .actions
                    .iter()
                    .find(|action| action.id == "ownership.accept")
                    .unwrap();
                assert!(matches!(
                    accept.contract.confirmation,
                    tilo_actions::ConfirmationPolicy::Phrase { .. }
                ));
                assert!(
                    ownership_transfer_status(&ws, &recipient)
                        .unwrap()
                        .contains("offered you ownership")
                );
                assert_eq!(role_verb(CapRole::Owner), None);
            }

            // Phase `ownership_status_prefers_a_known_person_name_to_a_key`.
            {
                let mut ws = ws_with_two_members();
                let owner_key = ed25519_dalek::SigningKey::from_bytes(&[17; 32]);
                let recipient_key = ed25519_dalek::SigningKey::from_bytes(&[18; 32]);
                let owner_public = collab_types::Ed25519PublicKey::from_bytes(
                    owner_key.verifying_key().to_bytes(),
                );
                let recipient_public = collab_types::Ed25519PublicKey::from_bytes(
                    recipient_key.verifying_key().to_bytes(),
                );
                let owner = public_key_member_id(owner_public);
                let recipient = public_key_member_id(recipient_public);
                ws.collaborators.push(
                    tilo_desktop_model::Collaborator::new(recipient.clone(), "Morgan").as_invited(),
                );
                ws.pending_ownership_transfer = collab_types::OwnershipTransferProposalV1::new(
                    ws.room.to_string(),
                    [23; 32],
                    owner_public,
                    recipient_public,
                    u64::MAX,
                    &owner_key,
                )
                .ok();

                let status = ownership_transfer_status(&ws, &owner).unwrap();
                assert!(status.contains("Morgan"));
                assert!(!status.contains(&short_id(&recipient)));
            }
        }
    }

    #[test]
    fn permission_profiles_publish_typed_admin_actions() {
        let mut ws = Workspace::new(WorkspaceId(92), "Research");
        let owner = "owner".to_string();
        ws.set_owner(owner.clone());
        ws.save_permission_profile_as(
            &owner,
            collab_types::PermissionProfile {
                name: "Release reviewer".into(),
                description: "Read Builds and approve releases".into(),
                rules: vec![collab_types::PermissionProfileRule::grant(
                    Scope::Action {
                        app: "org.tilo.Builds".into(),
                        resource: "builds.release".into(),
                        action: "release.approve".into(),
                    },
                    Verb::Write,
                )],
            },
        )
        .unwrap();

        let resource = permission_profile_resource(&ws, &owner);
        let save = resource
            .actions
            .iter()
            .find(|action| action.id == "profile.save")
            .unwrap();
        assert_eq!(
            save.contract.permission,
            tilo_actions::PermissionRequirement::Admin
        );
        let rules = save
            .params()
            .iter()
            .find(|parameter| parameter.name == "rules")
            .expect("agent-visible rules parameter");
        assert_eq!(rules.ty, tilo_actions::ParamType::Array);
        assert!(
            rules.schema.is_some(),
            "rule wire shapes must be discoverable"
        );
        let arguments = serde_json::json!({
            "name": "Notes opener",
            "description": "Can open notes but cannot edit them",
            "rules": [{
                "scope": {"Action": {
                    "app": "so.tilo.notes",
                    "resource": "notes.page:1",
                    "action": "note.open"
                }},
                "effect": "Grant",
                "verb": "Read"
            }]
        })
        .as_object()
        .cloned()
        .unwrap();
        let encoded = save
            .build_json(&arguments)
            .expect("typed agent arguments bind");
        let operation: PermissionProfileOperation =
            serde_json::from_slice(&encoded.payload).expect("typed profile operation");
        assert!(matches!(
            operation,
            PermissionProfileOperation::Save { rules, .. }
                if rules == vec![collab_types::PermissionProfileRule::grant(
                    Scope::Action {
                        app: "so.tilo.notes".into(),
                        resource: "notes.page:1".into(),
                        action: "note.open".into(),
                    },
                    Verb::Read,
                )]
        ));
        let _profile = resource.children.first().expect("saved role resource");
        let assign = resource
            .actions
            .iter()
            .find(|action| action.id == "profile.assign")
            .unwrap();
        assert_eq!(assign.kind, tilo_actions::OperationKind::Effect);
        assert_eq!(
            assign.operation_type.as_deref(),
            Some(PERMISSION_PROFILE_ACTION_TYPE)
        );

        let viewer = "viewer".to_string();
        assert!(permission_profile_resource(&ws, &viewer).actions.is_empty());
    }

    #[test]
    fn draw_members_pane_paints_members_and_skips_a_zero_area_pane() {
        // Phase `draw_members_pane_paints_more_with_members_than_without`.
        {
            let theme = Theme::dark();
            let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
            let mut tr = TextRenderer::new();

            let mut empty = Scene::new();
            draw_members_pane(
                &mut empty,
                Affine::IDENTITY,
                pane,
                &Workspace::new(WorkspaceId::default(), "empty"),
                &OWNER.to_string(),
                true,
                &mut tr,
                &theme,
            );
            let mut full = Scene::new();
            draw_members_pane(
                &mut full,
                Affine::IDENTITY,
                pane,
                &ws_with_two_members(),
                &OWNER.to_string(),
                true,
                &mut tr,
                &theme,
            );
            assert!(
                full.encoding().draw_tags.len() > empty.encoding().draw_tags.len(),
                "a populated roster must paint more than the empty state"
            );
        }

        // Phase `draw_members_pane_no_op_on_zero_area_pane`.
        {
            let ws = ws_with_two_members();
            let mut scene = Scene::new();
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            draw_members_pane(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(100.0, 100.0, 100.0, 100.0),
                &ws,
                &OWNER.to_string(),
                true,
                &mut tr,
                &theme,
            );
            assert!(scene.encoding().draw_tags.is_empty());
        }
    }
}

#[cfg(test)]
mod agents_pane_tests {
    use super::*;
    /// Whoever is looking at the pane; never a grantor in these fixtures.
    const VIEWER: &str = "viewer-user";
    use collab_types::caps::{ResourceRef, Verb};
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const AGENT: &str = "scribe-agent";

    fn ws_with_agent() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(17), "agents-test");
        ws.set_owner(OWNER);
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::membership(Subject::agent(AGENT), ws.room.to_string(), Verb::Read),
        )
        .expect("owner admits agent");
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::grant(
                Subject::agent(AGENT),
                Scope::App("so.tilo.notes".into()),
                Verb::Write,
            ),
        )
        .expect("owner grants scoped access");
        ws
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn agents_are_scoped_permission_subjects_presented_honestly() {
        // Phase `agent_rows_and_profiles_present_scoped_bundles_honestly`.
        {
            // Phase `agent_rows_are_separate_and_count_only_scoped_permissions`.
            {
                let ws = ws_with_agent();
                assert_eq!(
                    members_pane_rows(&ws, Rect::new(0.0, 0.0, 640.0, 480.0)).len(),
                    1
                );
                let rows = agents_pane_rows(&ws, Rect::new(0.0, 0.0, 640.0, 480.0));
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].agent, AGENT);
                assert_eq!(rows[0].role, CapRole::Viewer);
                assert_eq!(
                    rows[0].grant_count, 1,
                    "membership is not an extra permission"
                );
                assert_eq!(rows[0].preset, AgentAuthorityPreset::Contributor);
                assert!(rows[0].review_rect.x1 < rows[0].remove_rect.x0);
            }

            // Phase `profiles_describe_scoped_bundles_not_human_roles`.
            {
                let mut ws = Workspace::new(WorkspaceId::from(18), "profile-test");
                ws.set_owner(OWNER);
                let edit = admit_agent(
                    &mut ws,
                    &OWNER.to_string(),
                    AGENT,
                    AgentAuthorityPreset::Observer,
                );
                assert_eq!(edit.granted.len(), 1);
                assert_eq!(ws.role_of_agent(AGENT), Some(CapRole::Viewer));
                assert!(
                    !ws.allows(
                        &ws.actor_for_agent(AGENT),
                        &ResourceRef::App("so.tilo.notes".into()),
                        Verb::Read,
                    ),
                    "admission must never turn the Observer label into ambient app access"
                );
                assert_eq!(
                    agent_authority_preset(&ws, AGENT),
                    AgentAuthorityPreset::Observer
                );

                ws.grant_as(
                    &OWNER.to_string(),
                    CapOverride::grant(
                        Subject::agent(AGENT),
                        Scope::FsPath("/var/workspace/brief.md".into()),
                        Verb::Read,
                    ),
                )
                .expect("owner grants one selected file");
                assert_eq!(
                    agent_authority_preset(&ws, AGENT),
                    AgentAuthorityPreset::Observer
                );
            }

            // Phase `sensitive_or_broad_agent_grants_are_never_hidden_behind_a_preset`.
            {
                let mut ws = ws_with_agent();
                ws.grant_as(
                    &OWNER.to_string(),
                    CapOverride::grant(
                        Subject::agent(AGENT),
                        Scope::NetHost("*".into()),
                        Verb::Read,
                    ),
                )
                .expect("owner grants network access for the test");
                assert_eq!(
                    agent_authority_preset(&ws, AGENT),
                    AgentAuthorityPreset::Custom
                );
            }

            // Phase `agent_onboarding_and_review_geometry_are_separate_from_revocation`.
            {
                let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
                let add = agents_add_rect(pane);
                let row = agents_pane_rows(&ws_with_agent(), pane)
                    .into_iter()
                    .next()
                    .expect("agent row");
                assert!(add.y1 <= row.row_rect.y0);
                assert!(row.review_rect.x1 < row.remove_rect.x0);
            }
        }

        // Phase `agent_admission_and_revocation_are_scoped_permission_subjects`.
        {
            // Phase `admission_does_not_become_ambient_human_role_authority`.
            {
                let ws = ws_with_agent();
                assert!(ws.allows(
                    &ws.actor_for_agent(AGENT),
                    &ResourceRef::App("so.tilo.notes".into()),
                    Verb::Write,
                ));
                assert!(
                    !ws.allows(
                        &ws.actor_for_agent(AGENT),
                        &ResourceRef::App("so.tilo.sheets".into()),
                        Verb::Write,
                    ),
                    "an Editor admission pass must not become ambient human-role authority",
                );
            }

            // Phase `revoking_an_agent_removes_membership_and_scoped_grants`.
            {
                let mut ws = ws_with_agent();
                let edit = remove_agent(&mut ws, &OWNER.to_string(), AGENT);
                assert_eq!(edit.revoked.len(), 2);
                assert!(ws.agent_members().is_empty());
                assert!(!ws.allows(
                    &ws.actor_for_agent(AGENT),
                    &ResourceRef::App("so.tilo.notes".into()),
                    Verb::Write,
                ));
            }

            // Phase `non_owner_cannot_remove_an_agent`.
            {
                let mut ws = ws_with_agent();
                let before = ws.capabilities.clone();
                assert!(remove_agent(&mut ws, &"viewer".into(), AGENT).is_empty());
                assert_eq!(ws.capabilities, before);
            }

            // Phase `admitted_agent_is_a_first_class_permission_subject`.
            {
                let ws = ws_with_agent();
                let subjects = grant_subjects(&ws);
                let index = subjects
                    .iter()
                    .position(|subject| subject.kind == SubjectKind::Agent && subject.id == AGENT)
                    .expect("agent is offered in the permission composer");
                let draft = GrantDraft {
                    subject_index: index,
                    ..GrantDraft::default()
                };
                let cap = draft_capability(&ws, &draft).expect("agent grant can be composed");
                assert!(subject_is_agent(&cap.subject, AGENT));
            }

            // Phase `agent_membership_is_not_duplicated_in_permissions`.
            {
                let ws = ws_with_agent();
                let layout = permissions_pane_layout(
                    &ws,
                    Rect::new(0.0, 0.0, 660.0, 620.0),
                    &NoAppDirectory,
                    VIEWER,
                );
                assert!(layout.rows.iter().all(|row| !row.cap_id.is_empty()));
                assert_eq!(
                    layout
                        .rows
                        .iter()
                        .filter(|row| row.subject.kind == SubjectKind::Agent)
                        .count(),
                    1,
                    "only the explicit app grant belongs in Privacy & Security",
                );
            }
        }
    }
}

#[cfg(test)]
mod permissions_pane_tests {
    //! The Permissions pane lists [`Workspace::capabilities`] and its
    //! composer writes one through [`Workspace::grant_as`].

    use super::*;
    /// Whoever is looking at the pane; never a grantor in these fixtures.
    const VIEWER: &str = "viewer-user";
    use collab_types::caps::ResourceRef;
    use tilo_desktop_model::{Capability, GRANT_ALWAYS, GRANT_SESSION, WorkspaceId};

    const OWNER: &str = "owner-user";

    fn ws_with_grants() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(3), "perm-test");
        ws.set_owner(OWNER);
        ws.grant_app(
            "firefox",
            &Capability::Camera {
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app(
            "firefox",
            &Capability::Network {
                hosts: None,
                scope: GRANT_SESSION,
            },
        );
        ws
    }

    fn pane() -> Rect {
        Rect::new(0.0, 0.0, 660.0, 480.0)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn permission_rows_list_each_grant_with_its_real_scope_and_subject() {
        // Phase `stable_file_record_scope_is_presented_without_inventing_a_path`.
        {
            let record = "b0d4e23f-c7cb-4ded-a427-b32cf9da9341";
            assert_eq!(
                scope_label(&Scope::FsRecord(record.to_string()), &NoAppDirectory),
                "one shared file (b0d4e23f-c…)"
            );
            assert_eq!(
                capability_allows(
                    &CapOverride::grant(
                        Subject::Principal(Principal::App("files".to_string())),
                        Scope::FsRecord(record.to_string()),
                        Verb::Read,
                    ),
                    &NoAppDirectory
                ),
                "View one shared file (b0d4e23f-c…)"
            );
        }

        // Phase `rows_are_one_per_granted_capability`.
        {
            let ws = ws_with_grants();
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            assert_eq!(layout.rows.len(), 2);
            for row in &layout.rows {
                assert_eq!(row.subject.kind, SubjectKind::App);
                assert_eq!(row.subject.id, "firefox");
                assert!(row.revoke_rect.x1 <= row.row_rect.x1);
                assert!(row.revoke_rect.x0 > row.row_rect.x0);
                assert!(!row.cap_id.is_empty(), "rows are keyed by capability id");
            }
            assert_ne!(layout.rows[0].cap_id, layout.rows[1].cap_id);
            assert_eq!(layout.overflow, 0);
        }

        // Phase `membership_capabilities_are_not_listed_as_permissions`.
        {
            let ws = ws_with_grants();
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            assert!(
                layout
                    .rows
                    .iter()
                    .all(|r| r.subject.kind == SubjectKind::App),
                "only non-membership grants belong in the Permissions list"
            );
        }

        // Phase `a_persons_membership_is_left_to_the_members_tab`.
        {
            let ws = ws_with_grants();
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            assert!(
                layout
                    .rows
                    .iter()
                    .all(|r| r.subject.kind == SubjectKind::App),
                "no person membership rows in the Permissions list"
            );
            assert!(
                !members_pane_rows(&ws, pane()).is_empty(),
                "but Members has one"
            );
        }

        // Phase `person_and_role_subjects_are_listed_too`.
        {
            let mut ws = ws_with_grants();
            ws.grant_as(
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::person("peer-user"),
                    Scope::FsPath("/var/workspace".into()),
                    Verb::Write,
                ),
            )
            .expect("owner grants");
            ws.grant_as(
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::Role(CapRole::Editor),
                    Scope::Device("camera".into()),
                    Verb::Read,
                ),
            )
            .expect("owner grants");
            // This assertion is about grouping every subject kind, so give
            // the viewport room for every seeded row after the explanatory
            // header and composer.
            let layout = permissions_pane_layout(
                &ws,
                Rect::new(0.0, 0.0, 660.0, 620.0),
                &NoAppDirectory,
                VIEWER,
            );
            let kinds: Vec<SubjectKind> = layout.rows.iter().map(|r| r.subject.kind).collect();
            assert!(kinds.contains(&SubjectKind::App));
            assert!(kinds.contains(&SubjectKind::Member));
            assert!(kinds.contains(&SubjectKind::Role));
            let mut sorted = kinds.clone();
            sorted.sort();
            assert_eq!(kinds, sorted, "rows must be grouped by subject kind");
        }

        // Phase `grantors_and_holders_are_named_as_the_people_pane_names_them`.
        // A 64-hex person key is what the model records; the row names the
        // collaborator, and the viewer's own grant reads "you".
        {
            let ada = "ad".repeat(32);
            let bea = "be".repeat(32);
            let mut ws = Workspace::new(WorkspaceId::from(5), "names");
            ws.set_owner(&ada);
            ws.collaborators.push(tilo_desktop_model::Collaborator::new(
                ada.clone(),
                "Ada Lovelace",
            ));
            ws.collaborators
                .push(tilo_desktop_model::Collaborator::new(bea.clone(), "Bea"));
            ws.grant_as(
                &ada,
                CapOverride::grant(
                    Subject::person(bea.clone()),
                    Scope::FsPath("/var/workspace".into()),
                    Verb::Write,
                ),
            )
            .expect("owner grants");
            let as_bea = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            let row = &as_bea.rows[0];
            assert_eq!(row.title, "Bea");
            assert_eq!(row.granted_by.as_deref(), Some("granted by Ada Lovelace"));
            let as_ada = permissions_pane_layout(&ws, pane(), &NoAppDirectory, &ada);
            assert_eq!(as_ada.rows[0].granted_by.as_deref(), Some("granted by you"));
            let as_bea = permissions_pane_layout(&ws, pane(), &NoAppDirectory, &bea);
            assert_eq!(as_bea.rows[0].title, "You");
            for row in as_bea.rows.iter().chain(&as_ada.rows) {
                let shown = format!("{} {}", row.title, row.granted_by.as_deref().unwrap_or(""));
                assert!(
                    !shown.contains("adad") && !shown.contains("bebe"),
                    "{shown}"
                );
            }
        }

        // Phase `rows_carry_the_scope_and_grantor_the_model_recorded`.
        {
            let mut ws = Workspace::new(WorkspaceId::from(4), "labels");
            ws.set_owner(OWNER);
            ws.grant_as(
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::app("terminal"),
                    Scope::FsPath("/var/workspace".into()),
                    Verb::Write,
                )
                .expiring(Expiry::EndOfSession),
            )
            .expect("owner grants");
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            let row = &layout.rows[0];
            assert_eq!(row.allows, "Edit workspace files");
            assert_eq!(row.expiry, "Session");
            assert_eq!(
                row.granted_by.as_deref(),
                Some("granted by owner-user"),
                "grant_as records the grantor; the row shows it"
            );
        }

        // Phase `no_capability_line_leaks_the_workspace_mount_point`.
        // No capability line may name the mount point workspace files are
        // projected at (HIG §4). Swept over every scope kind, not just the
        // two the panel shows today, so a new scope can't reintroduce the leak.
        {
            use collab_types::role::ArtifactKind;
            let scopes = vec![
                Scope::Workspace,
                Scope::Room("r".into()),
                Scope::App("firefox".into()),
                Scope::FsPath(WORKSPACE_FILES_PATH.into()),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/")),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design/drafts")),
                Scope::Device("camera".into()),
                Scope::NetHost("*".into()),
                Scope::NetHost("example.com".into()),
                Scope::Action {
                    app: "notes".into(),
                    resource: "notes.page:1".into(),
                    action: "export".into(),
                },
                Scope::ResourceType(ArtifactKind::Note),
            ];
            let verbs = [
                Verb::Read,
                Verb::Write,
                Verb::Admin,
                Verb::Comment,
                Verb::Invite,
            ];
            for scope in &scopes {
                for verb in verbs {
                    let cap = CapOverride::grant(Subject::app("a"), scope.clone(), verb);
                    let line = capability_allows(&cap, &NoAppDirectory);
                    assert!(
                        !line.contains(WORKSPACE_FILES_PATH),
                        "the permission line {line:?} names {WORKSPACE_FILES_PATH} \
                         — where the OS keeps workspace files is not what the user \
                         is being asked to agree to",
                    );
                    assert!(
                        !line.contains("/var/"),
                        "the permission line {line:?} exposes a system path",
                    );
                    assert!(
                        !line.is_empty(),
                        "every scope must still describe itself; {scope:?} produced \
                         an empty line",
                    );
                }
            }
        }

        // Phase `workspace_scopes_distinguish_the_root_from_a_folder_inside_it`.
        // The projection root and a path inside it must read as different
        // things, so a row never understates what is being granted.
        {
            let root = CapOverride::grant(
                Subject::app("a"),
                Scope::FsPath(WORKSPACE_FILES_PATH.into()),
                Verb::Write,
            );
            let sub = CapOverride::grant(
                Subject::app("a"),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
                Verb::Write,
            );
            assert_eq!(
                capability_allows(&root, &NoAppDirectory),
                "Edit workspace files"
            );
            assert_eq!(
                capability_allows(&sub, &NoAppDirectory),
                "Edit \u{201c}design\u{201d} in this workspace",
            );
            assert_ne!(
                capability_allows(&root, &NoAppDirectory),
                capability_allows(&sub, &NoAppDirectory),
                "granting every workspace file must not read the same as granting \
                 one folder in it",
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn granting_and_revoking_permissions_is_owner_only_and_enforced() {
        // Phase `granting_from_the_composer_is_owner_only_and_enforced`.
        {
            // Phase `the_composer_offers_the_roster_and_the_workspaces_apps`.
            {
                let ws = ws_with_grants();
                let subjects = grant_subjects(&ws);
                assert_eq!(
                    subjects[0],
                    GrantSubject {
                        kind: SubjectKind::Member,
                        id: OWNER.to_string()
                    },
                    "people come first"
                );
                assert!(subjects.contains(&GrantSubject {
                    kind: SubjectKind::App,
                    id: "firefox".to_string()
                }));
            }

            // Phase `granting_from_the_composer_writes_a_capability_that_enforces`.
            {
                let mut ws = ws_with_grants();
                let subjects = grant_subjects(&ws);
                let firefox = subjects
                    .iter()
                    .position(|s| s.id == "firefox")
                    .expect("firefox is a candidate subject");
                let mut draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: 0,
                    expiry: Expiry::Never,
                };
                while draft.choice().label() != "Microphone" {
                    draft.cycle_choice();
                }
                assert!(!ws.allows(
                    &ws.actor_for_app("firefox"),
                    &ResourceRef::Device("microphone".into()),
                    Verb::Read
                ));

                let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
                assert_eq!(edit.granted.len(), 1);
                assert!(
                    ws.allows(
                        &ws.actor_for_app("firefox"),
                        &ResourceRef::Device("microphone".into()),
                        Verb::Read
                    ),
                    "the granted capability must start working, not merely start rendering"
                );
            }

            // Phase `granting_records_the_chosen_expiry`.
            {
                let mut ws = ws_with_grants();
                let subjects = grant_subjects(&ws);
                let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
                // Re-granting an existing capability merges expiries, so pick
                // something the app doesn't already hold to observe this one.
                let mut draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: 0,
                    expiry: Expiry::AfterUse,
                };
                while draft.choice().label() != "Microphone" {
                    draft.cycle_choice();
                }
                let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
                let id = &edit.granted[0];
                let cap = ws
                    .capabilities
                    .iter()
                    .find(|c| &c.id == id)
                    .expect("stored capability");
                assert_eq!(cap.expiry, Expiry::AfterUse);
                assert!(
                    !cap.persists_across_restart(),
                    "an after-use grant must not be written down as permanent"
                );
            }

            // Phase `re_granting_never_shortens_an_existing_grant`.
            {
                // `grant_capability` keeps the longer expiry for the same
                // (subject, scope, effect, verb).
                let mut ws = ws_with_grants();
                let subjects = grant_subjects(&ws);
                let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
                let mut draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: 0,
                    expiry: Expiry::AfterUse,
                };
                while draft.choice().label() != "Camera" {
                    draft.cycle_choice();
                }
                apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
                let camera = ws
                    .capabilities
                    .iter()
                    .filter(|c| matches!(&c.scope, Scope::Device(d) if d == "camera"))
                    .collect::<Vec<_>>();
                assert_eq!(
                    camera.len(),
                    1,
                    "the re-grant merges rather than duplicating"
                );
                assert_eq!(
                    camera[0].expiry,
                    Expiry::Never,
                    "the existing permanent grant is not demoted to once-only"
                );
                let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
                let row = layout
                    .rows
                    .iter()
                    .find(|r| r.allows.contains("camera"))
                    .expect("camera row");
                assert_eq!(
                    row.expiry, "Always",
                    "the row must report the expiry that is stored, not the one that was asked for"
                );
            }

            // Phase `a_non_owner_cannot_grant`.
            {
                let mut ws = ws_with_grants();
                let subjects = grant_subjects(&ws);
                let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
                let draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: 0,
                    expiry: Expiry::Never,
                };
                let before = ws.capabilities.len();
                let edit = apply_grant_draft(&mut ws, &"someone-else".to_string(), &draft);
                assert!(edit.is_empty(), "grant_as must refuse a non-administrator");
                assert_eq!(ws.capabilities.len(), before);
            }

            // Phase `every_offered_capability_produces_a_scope_an_enforcement_point_reads`.
            {
                let ws = ws_with_grants();
                let subjects = grant_subjects(&ws);
                let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
                for (i, choice) in GRANT_CHOICES.iter().enumerate() {
                    let draft = GrantDraft {
                        subject_index: firefox,
                        choice_index: i,
                        expiry: Expiry::Never,
                    };
                    let cap = draft_capability(&ws, &draft).expect("every choice resolves");
                    assert_eq!(cap.effect, Effect::Grant);
                    let resource = match &cap.scope {
                        Scope::Device(d) => ResourceRef::Device(d.clone()),
                        Scope::NetHost(h) => ResourceRef::NetHost(h.clone()),
                        Scope::FsPath(p) => ResourceRef::FsPath(p.clone()),
                        Scope::Workspace => ResourceRef::Workspace,
                        Scope::Action {
                            app,
                            resource,
                            action,
                        } => ResourceRef::Action {
                            app: app.clone(),
                            resource: resource.clone(),
                            action: action.clone(),
                        },
                        other => panic!("unexpected scope in the grant table: {other:?}"),
                    };
                    let mut probe = ws.clone();
                    probe
                        .grant_as(&OWNER.to_string(), cap.clone())
                        .expect("owner grants");
                    assert!(
                        probe.allows(&probe.actor_for_app("firefox"), &resource, cap.verb),
                        "granting {:?} must make the decision function allow it",
                        choice.label()
                    );
                }
            }
        }

        // Phase `revoking_a_permission_row_is_owner_only_and_enforced`.
        {
            // Phase `a_workspace_scoped_app_grant_is_listed_and_revocable`.
            {
                let mut ws = ws_with_grants();
                let id = ws
                    .grant_as(
                        &OWNER.to_string(),
                        CapOverride::grant(Subject::app("firefox"), Scope::Workspace, Verb::Admin),
                    )
                    .expect("owner grants");
                let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
                let row = layout
                    .rows
                    .iter()
                    .find(|r| r.cap_id == id)
                    .expect("the workspace-scoped app grant has a row");
                assert_eq!(row.subject.kind, SubjectKind::App);
                assert_eq!(row.allows, "Administer this workspace");
                assert_eq!(
                    apply_revoke(&mut ws, &OWNER.to_string(), &row.cap_id)
                        .revoked
                        .len(),
                    1
                );
                assert!(!ws.allows(
                    &ws.actor_for_app("firefox"),
                    &ResourceRef::Workspace,
                    Verb::Admin
                ));
            }

            // Phase `revoking_a_row_stops_the_capability_working`.
            {
                let mut ws = ws_with_grants();
                let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
                let camera_row = layout
                    .rows
                    .iter()
                    .find(|r| r.allows.contains("camera"))
                    .expect("camera row");
                let edit = apply_revoke(&mut ws, &OWNER.to_string(), &camera_row.cap_id);
                assert_eq!(edit.revoked, vec![camera_row.cap_id.clone()]);
                assert!(
                    !ws.allows(
                        &ws.actor_for_app("firefox"),
                        &ResourceRef::Device("camera".into()),
                        Verb::Read
                    ),
                    "the revoked capability must stop working, not merely stop rendering"
                );
                let after = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
                assert_eq!(after.rows.len(), 1);
            }

            // Phase `a_non_owner_cannot_revoke`.
            {
                let mut ws = ws_with_grants();
                let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
                let target = layout.rows[0].cap_id.clone();
                let edit = apply_revoke(&mut ws, &"someone-else".to_string(), &target);
                assert!(edit.is_empty());
                assert_eq!(
                    permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER)
                        .rows
                        .len(),
                    2
                );
            }

            // Phase `revoking_an_unknown_id_is_a_no_op`.
            {
                let mut ws = ws_with_grants();
                let edit = apply_revoke(&mut ws, &OWNER.to_string(), "cap-does-not-exist");
                assert!(edit.is_empty());
                assert_eq!(ws.capabilities.len(), 3, "owner membership + two grants");
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_permissions_pane_draws_every_state_and_fits_its_rows() {
        // Phase `draw_permissions_pane_paints_empty_and_granted_states`.
        {
            // Phase `draw_permissions_pane_paints_an_empty_state_with_no_grants`.
            {
                let ws = Workspace::new(WorkspaceId::default(), "empty");
                let mut scene = Scene::new();
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                draw_permissions_pane(
                    &mut scene,
                    Affine::IDENTITY,
                    pane(),
                    (&ws, &NoAppDirectory, VIEWER),
                    &GrantDraft::default(),
                    true,
                    &mut tr,
                    &theme,
                );
                assert!(
                    !scene.encoding().draw_tags.is_empty(),
                    "the composer and the empty state must both paint"
                );
            }

            // Phase `draw_permissions_pane_paints_more_with_grants_than_without`.
            {
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let draft = GrantDraft::default();

                let mut empty = Scene::new();
                draw_permissions_pane(
                    &mut empty,
                    Affine::IDENTITY,
                    pane(),
                    (
                        &Workspace::new(WorkspaceId::default(), "empty"),
                        &NoAppDirectory,
                        VIEWER,
                    ),
                    &draft,
                    true,
                    &mut tr,
                    &theme,
                );
                let mut full = Scene::new();
                draw_permissions_pane(
                    &mut full,
                    Affine::IDENTITY,
                    pane(),
                    (&ws_with_grants(), &NoAppDirectory, VIEWER),
                    &draft,
                    true,
                    &mut tr,
                    &theme,
                );
                assert!(full.encoding().draw_tags.len() > empty.encoding().draw_tags.len());
            }

            // Phase `draw_permissions_pane_no_op_on_zero_area_pane`.
            {
                let ws = ws_with_grants();
                let mut scene = Scene::new();
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                draw_permissions_pane(
                    &mut scene,
                    Affine::IDENTITY,
                    Rect::new(100.0, 100.0, 100.0, 100.0),
                    (&ws, &NoAppDirectory, VIEWER),
                    &GrantDraft::default(),
                    true,
                    &mut tr,
                    &theme,
                );
                assert!(scene.encoding().draw_tags.is_empty());
            }
        }

        // Phase `permission_rows_fit_their_pane_without_clipping_or_overlap`.
        {
            // Phase `rows_that_do_not_fit_are_counted_rather_than_clipped`.
            {
                let mut ws = Workspace::new(WorkspaceId::from(9), "many");
                ws.set_owner(OWNER);
                for i in 0..12 {
                    ws.grant_as(
                        &OWNER.to_string(),
                        CapOverride::grant(
                            Subject::app(format!("app-{i}")),
                            Scope::Device("camera".into()),
                            Verb::Read,
                        ),
                    )
                    .expect("owner grants");
                }
                let short_pane = Rect::new(0.0, 0.0, 660.0, 300.0);
                let layout = permissions_pane_layout(&ws, short_pane, &NoAppDirectory, VIEWER);
                assert!(layout.rows.len() < 12);
                assert_eq!(layout.rows.len() + layout.overflow, 12);
                for row in &layout.rows {
                    assert!(
                        row.row_rect.y1 <= short_pane.y1,
                        "a laid-out row must fit inside the pane"
                    );
                }
            }

            // Phase `expiry_segments_are_laid_out_in_picker_order_without_overlap`.
            {
                let ws = ws_with_grants();
                let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
                for i in 1..3 {
                    assert!(
                        layout.expiry_rects[i].x0 >= layout.expiry_rects[i - 1].x1 - f64::EPSILON
                    );
                }
                assert!(layout.expiry_rects[2].x1 <= layout.submit_rect.x0);
                assert!(layout.subject_rect.x1 <= layout.choice_rect.x0);
                assert!(layout.choice_rect.x1 <= layout.expiry_rects[0].x0);
            }

            // Phase `narrow_panes_stack_policy_controls_inside_their_rows`.
            {
                let ws = ws_with_grants();
                let pane = Rect::new(0.0, 0.0, 326.0, 620.0);
                let layout = permissions_pane_layout(&ws, pane, &NoAppDirectory, VIEWER);
                for rect in std::iter::once(layout.subject_rect)
                    .chain(std::iter::once(layout.choice_rect))
                    .chain(layout.expiry_rects)
                    .chain(std::iter::once(layout.submit_rect))
                {
                    assert!(
                        rect.x0 >= pane.x0 && rect.x1 <= pane.x1,
                        "compact permission control must remain inside its pane: {rect:?} in {pane:?}"
                    );
                }
                assert!(
                    layout.subject_rect.y1 < layout.expiry_rects[0].y0,
                    "narrow composer promotes expiry/grant controls to a second row"
                );

                let candidates = [desktop_policy::AppCandidate {
                    package_id: "so.tilo.viewer".into(),
                    display_name: "Viewer".into(),
                    resources: [desktop_policy::ResourceKind::extension("pdf").unwrap()].into(),
                }];
                let rows = default_app_rows(pane, &candidates);
                let row = rows.first().expect("a narrow policy row fits");
                assert!(row.workspace_rect.x0 >= row.row_rect.x0 + DEFAULT_APP_ROW_INSET);
                assert!(row.user_rect.x1 <= row.row_rect.x1 - DEFAULT_APP_ROW_INSET);
                assert!(
                    row.workspace_rect.y0 > row.row_rect.y0 + 20.0,
                    "the resource label keeps its own line above both choices"
                );
                assert!(
                    row.workspace_rect.x1 <= row.user_rect.x0,
                    "the two explicit choices never overlap at phone width"
                );

                let wide = Rect::new(0.0, 0.0, 670.0, 800.0);
                let rows = default_app_rows(wide, &candidates);
                let row = rows.first().expect("a wide policy row fits");
                assert!(
                    row.workspace_rect.x0
                        >= row.row_rect.x0 + DEFAULT_APP_ROW_INSET + DEFAULT_APP_LABEL_MIN,
                    "a wide row keeps its resource label clear of the cyclers: {row:?}"
                );
                assert!(
                    row.user_rect.x1 <= row.row_rect.x1 - DEFAULT_APP_ROW_INSET,
                    "the personal cycler stays inside its row card: {row:?}"
                );
                assert!(row.workspace_rect.x1 <= row.user_rect.x0);
                assert!(
                    (row.workspace_rect.center().y - row.row_rect.center().y).abs() < 0.5,
                    "wide-row cyclers are centred on the row"
                );
            }

            // Phase `long_identifiers_are_shortened_rather_than_overrun`.
            {
                let long = "a".repeat(64);
                let shortened = short_id(&long);
                assert!(shortened.chars().count() <= 11);
                assert!(shortened.ends_with('…'));
                assert_eq!(short_id("ada"), "ada");
            }
        }
    }
}

#[cfg(test)]
mod roster_order_tests {
    //! The Members list must not reorder under the pointer.

    use super::*;
    use tilo_desktop_model::WorkspaceId;

    #[test]
    fn changing_a_role_does_not_move_the_row() {
        // A role change rewrites the capability, shifting the raw
        // roster order; the pane's order must not shift with it.
        let mut ws = Workspace::new(WorkspaceId::from(5), "order");
        ws.set_owner("host");
        let room = ws.id.raw().to_string();
        for person in ["ada", "bo", "cy"] {
            ws.grant_as(
                &"host".to_string(),
                CapOverride::membership(Subject::person(person), room.clone(), Verb::Read),
            )
            .expect("owner may grant");
        }
        let pane = Rect::new(0.0, 0.0, 640.0, 600.0);
        let before: Vec<UserId> = members_pane_rows(&ws, pane)
            .into_iter()
            .map(|r| r.user)
            .collect();
        assert_eq!(before, vec!["host", "ada", "bo", "cy"]);

        set_member_role(
            &mut ws,
            &"host".to_string(),
            &"ada".to_string(),
            CapRole::Editor,
        );
        let after: Vec<UserId> = members_pane_rows(&ws, pane)
            .into_iter()
            .map(|r| r.user)
            .collect();
        assert_eq!(after, before, "the row order must be identical");
    }
}

#[cfg(test)]
mod default_app_authority_tests {
    use super::*;

    fn candidate(id: &str) -> desktop_policy::AppCandidate {
        desktop_policy::AppCandidate {
            package_id: id.into(),
            display_name: id.into(),
            resources: [desktop_policy::ResourceKind::extension("pdf").unwrap()].into(),
        }
    }

    #[test]
    fn default_app_authority_is_separately_authorized_and_cycles_inheritance_first() {
        // Phase `admin_policy_and_personal_escape_hatch_are_separately_authorized`.
        {
            let candidates = [candidate("so.tilo.viewer"), candidate("so.tilo.other")];
            let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
            let pdf = desktop_policy::ResourceKind::extension("pdf").unwrap();
            let mut ws = Workspace::new(WorkspaceId::from(7), "Docs");
            ws.set_owner("owner");

            assert!(
                ws.set_default_app_policy_as(
                    &"member".into(),
                    pdf.clone(),
                    desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into()),
                    &resolver
                )
                .is_err()
            );
            ws.set_default_app_policy_as(
                &"owner".into(),
                pdf.clone(),
                desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into()),
                &resolver,
            )
            .unwrap();

            assert!(
                ws.set_default_app_choice_as(
                    &"owner".into(),
                    &"member".into(),
                    pdf.clone(),
                    desktop_policy::UserWorkspaceChoice::PersonalDefault,
                    &resolver
                )
                .is_err()
            );
            ws.set_default_app_choice_as(
                &"member".into(),
                &"member".into(),
                pdf.clone(),
                desktop_policy::UserWorkspaceChoice::App("so.tilo.other".into()),
                &resolver,
            )
            .unwrap();
            assert_eq!(
                ws.default_apps.user_choices["member"][&pdf],
                desktop_policy::UserWorkspaceChoice::App("so.tilo.other".into())
            );
        }

        // Phase `controls_cycle_through_inheritance_before_apps`.
        {
            let candidates = [candidate("so.tilo.viewer")];
            let pdf = desktop_policy::ResourceKind::extension("pdf").unwrap();
            assert_eq!(
                next_workspace_default(None, &pdf, &candidates),
                desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into())
            );
            assert_eq!(
                next_user_default(None, &pdf, &candidates),
                desktop_policy::UserWorkspaceChoice::PersonalDefault
            );
        }
    }
}

#[cfg(test)]
mod rest_edge_tests {
    //! HIG §9.5 rest edges: every card class this file paints derives its
    //! edge from the fill it sits on and must register on every palette.

    use super::*;

    #[test]
    fn rest_panes_keep_edges_and_the_section_step() {
        // Phase `pane_cards_and_footer_keep_an_edge_on_every_palette`.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let colors = &theme.tokens.colors;
                for (what, fill) in [
                    ("row / intro / composer card", colors.surfaces.raised),
                    ("sidebar footer row", colors.lift(0.35)),
                ] {
                    let delta = ui::surface::edge_delta(ui::surface::edge_on(fill), fill);
                    assert!(
                        delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{}: the {what} edge must separate it from its fill (Δ luminance {delta:.4})",
                        theme.name
                    );
                }
            }
        }

        // Phase `default_apps_empty_state_starts_below_the_header_block`.
        // The default-apps empty state starts where the first row would, under
        // the column-label line, instead of centring itself over the header.
        {
            let pane = Rect::new(0.0, 0.0, 508.0, 560.0);
            let heading_y = pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP;
            let label_line_bottom = heading_y + 22.0 + 14.0;
            assert!(default_app_rows_top(pane) >= label_line_bottom + 4.0);
            assert!(default_app_rows(pane, &[]).is_empty());
        }

        // Phase `pane_lead_in_completes_the_section_step`.
        // The pane lead-in and the lens body lead-in add up to one `s4` step.
        {
            let s4 = f64::from(ui::tokens::Spacing::BASE.s4);
            let s2 = f64::from(ui::tokens::Spacing::BASE.s2);
            assert_eq!(PANE_PAD_TOP + s2, s4);
        }
    }
}

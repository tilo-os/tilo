//! Workspace settings: its outcomes and drawing.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Apply the typed hit produced by [`WorkspaceSettingsOverlay`]. Policy
    /// mutations stay on their original authenticated paths here; the
    /// component owns only presentation and hit resolution.
    pub(super) fn handle_shell_ws_settings_click(&mut self, p: Point, handled: &mut bool) {
        self.workspace_settings_overlay.layout(self.overlay_area());
        let active_tab = self
            .chrome
            .as_ref()
            .map_or(WsTab::General, |chrome| chrome.borrow().ws_settings_tab);
        let outcome = self.workspace_settings_overlay.hit_test(p, active_tab);
        let is_owner = self
            .chrome
            .as_ref()
            .is_some_and(|c| is_active_workspace_owner(&c.borrow()));
        self.apply_workspace_settings_outcome(outcome, is_owner);
        *handled = true;
    }

    /// Whether the person here may take the governance action `action` in
    /// the active workspace, which this device governs: whether a sheet
    /// offering it is drawn. The action itself decides again.
    pub(super) fn may_govern_active(&self, action: &str) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let state = chrome.borrow();
        if let Some(forced) = state.test_only_owner_override {
            return forced;
        }
        let user = state.active_local_user();
        let tree = state.tree();
        tree.desktop.active_workspace().is_some_and(|ws| {
            tree.governs_room(ws.room()) && ws.may_govern_at(&user, action, tilo_clock::unix_now())
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "typed settings outcomes are intentionally applied in one exhaustive policy boundary"
    )]
    pub(super) fn apply_workspace_settings_outcome(
        &mut self,
        outcome: WorkspaceSettingsOutcome,
        is_owner: bool,
    ) {
        if let Some(hit) = workspace_settings_composer_hit(&outcome) {
            if is_owner && let Some(chrome) = self.chrome.clone() {
                apply_composer_hit(&chrome, hit);
            }
            return;
        }
        match outcome {
            WorkspaceSettingsOutcome::Close => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    state.commit_workspace_name();
                    state.overlays.ws_settings = false;
                    state.bump();
                }
            }
            WorkspaceSettingsOutcome::WorkspaceDefault(resource) => {
                self.apply_workspace_settings_default(resource, true, is_owner);
            }
            WorkspaceSettingsOutcome::UserDefault(resource) => {
                self.apply_workspace_settings_default(resource, false, is_owner);
            }
            WorkspaceSettingsOutcome::ComposerSubject
            | WorkspaceSettingsOutcome::ComposerCapability
            | WorkspaceSettingsOutcome::ComposerExpiry(_)
            | WorkspaceSettingsOutcome::ComposerSubmit => {}
            WorkspaceSettingsOutcome::ManagePermissionProfiles => {
                self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
                self.palette
                    .set_query(&crate::shell::i18n::t("palette-query-permission-roles"));
                self.last_search_query = "\0workspace-permission-roles\0".to_string();
            }
            WorkspaceSettingsOutcome::MemberRole(user) => {
                if self.may_govern_active(tilo_action_policy::roles::governance::SET_ROLE) {
                    let current = self.chrome.as_ref().and_then(|chrome| {
                        chrome
                            .borrow()
                            .tree()
                            .desktop
                            .active_workspace()
                            .and_then(|ws| ws.role_of(&user))
                    });
                    self.workspace_settings_overlay.open_member_role_sheet(
                        user,
                        current.unwrap_or(crate::shell::state::CapRole::Viewer),
                    );
                    self.bump_chrome();
                }
            }
            WorkspaceSettingsOutcome::RemoveMember(user) => {
                if self.may_govern_active(tilo_action_policy::roles::governance::REMOVE_MEMBER) {
                    self.workspace_settings_overlay
                        .open_remove_member_sheet(user);
                    self.bump_chrome();
                }
            }
            WorkspaceSettingsOutcome::ManageOwnership => {
                // The button is named by its outcome; search opens on the
                // ownership action it names, with that action's confirmation.
                let action = self.chrome.as_ref().and_then(|chrome| {
                    let state = chrome.borrow();
                    let acting_user = state.active_local_user();
                    let tree = state.tree();
                    let action = tree.desktop.active_workspace().and_then(|workspace| {
                        tilo_desktop::workspace_settings::ownership_button(workspace, &acting_user)
                            .map(|(_, action)| action)
                    });
                    drop(tree);
                    drop(state);
                    action
                });
                self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
                self.palette
                    .set_query(action.as_deref().unwrap_or("ownership"));
                self.last_search_query = "\0workspace-ownership\0".to_string();
            }
            WorkspaceSettingsOutcome::Revoke(cap_id) => {
                if self.may_govern_active(tilo_action_policy::roles::governance::REVOKE) {
                    self.workspace_settings_overlay.open_revoke_sheet(cap_id);
                    self.bump_chrome();
                }
            }
            // Authority comes from the same function the `member.role.set` /
            // `member.remove` actions call, not from `is_owner`: that flag
            // only decides whether the sheet offering the choice is drawn.
            WorkspaceSettingsOutcome::SetMemberRole(user, role) => {
                if let Some(chrome) = self.chrome.clone()
                    && let Err(error) = crate::shell::membership::set_member_role(
                        &mut chrome.borrow_mut(),
                        &user,
                        role,
                    )
                {
                    tracing::debug!(%error, "member role change refused");
                }
                self.workspace_settings_overlay.dismiss_member_sheet();
            }
            WorkspaceSettingsOutcome::ConfirmRemoveMember(user) => {
                if let Some(chrome) = self.chrome.clone()
                    && let Err(error) =
                        crate::shell::membership::remove_member(&mut chrome.borrow_mut(), &user)
                {
                    tracing::debug!(%error, "member removal refused");
                }
                self.workspace_settings_overlay.dismiss_member_sheet();
            }
            WorkspaceSettingsOutcome::AddAgent => {
                if is_owner {
                    // Naming an agent belongs in the command palette's typed
                    // parameter flow. It keeps setup usable with keyboard,
                    // pointer, accessibility tools, and automation alike.
                    self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
                    self.palette
                        .set_query(&crate::shell::i18n::t("search-action-agent-invite"));
                    self.last_search_query = "\0agent-onboarding\0".to_string();
                }
            }
            // Bots are found where apps are: the Store's bots, whose page
            // adds one to this workspace.
            WorkspaceSettingsOutcome::AddBot => {
                if let Some(chrome) = self.chrome.clone() {
                    if let Err(error) =
                        crate::shell::uri_open::open_in_workspace(STORE_BOTS_LINK, &chrome)
                    {
                        chrome.borrow_mut().push_warning_notification(
                            crate::shell::i18n::t("bots-add-failed"),
                            error.user_message(),
                        );
                    } else {
                        let mut state = chrome.borrow_mut();
                        state.overlays.ws_settings = false;
                        state.bump();
                    }
                }
            }
            WorkspaceSettingsOutcome::RemoveBot(entry) => {
                if is_owner && let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    state.ask_active_workspace_for_bot(crate::shell::network::BotAsk::Remove {
                        entry,
                    });
                    state.bump();
                }
            }
            WorkspaceSettingsOutcome::ReviewAgent(agent) => {
                // Anyone in the workspace may see what an agent may do; only
                // someone who governs it gets live controls (`owner_controls`).
                let models = agent_models(&agent_provider_config::ProviderConfig::load(), &agent);
                self.workspace_settings_overlay.open_agent_review(agent);
                self.workspace_settings_overlay.set_agent_models(models);
                self.bump_chrome();
            }
            WorkspaceSettingsOutcome::ToggleAgentModels => {
                self.workspace_settings_overlay.toggle_agent_models();
                self.bump_chrome();
            }
            WorkspaceSettingsOutcome::SetAgentModel { agent, model } => {
                // The model is the person's own Settings, and is what the
                // agent starts with when they launch it, which takes
                // administering the workspace.
                if is_owner {
                    match choose_agent_model(&agent, model) {
                        Ok(config) => self
                            .workspace_settings_overlay
                            .set_agent_models(agent_models(&config, &agent)),
                        Err(error) => tracing::warn!(%error, "agent model not saved"),
                    }
                    self.bump_chrome();
                }
            }
            WorkspaceSettingsOutcome::CloseAgentReview => {
                self.workspace_settings_overlay.close_agent_review();
                self.bump_chrome();
            }
            WorkspaceSettingsOutcome::ToggleAgentApp(scope) => {
                self.workspace_settings_overlay.toggle_agent_app(scope);
                self.bump_chrome();
            }
            WorkspaceSettingsOutcome::SetAgentAnswers { agent, answers } => {
                if self.may_govern_active(tilo_action_policy::roles::governance::GRANT)
                    && let Some(chrome) = self.chrome.clone()
                {
                    let _ = crate::shell::membership::edit_active_workspace_policy(
                        &mut chrome.borrow_mut(),
                        move |ws, actor| {
                            tilo_desktop::workspace_settings::agent_access::set_agent_answers(
                                ws, actor, &agent, &answers,
                            )
                        },
                    );
                }
            }
            WorkspaceSettingsOutcome::RemoveAgent(agent) => {
                if is_owner
                    && let Some(chrome) = self.chrome.clone()
                    && let Err(error) =
                        crate::shell::membership::remove_agent(&mut chrome.borrow_mut(), &agent)
                {
                    tracing::debug!(%error, "agent removal refused");
                }
            }
            WorkspaceSettingsOutcome::ConfirmRevoke(cap_id) => {
                if is_owner && let Some(chrome) = self.chrome.clone() {
                    let _ = crate::shell::membership::edit_active_workspace_policy(
                        &mut chrome.borrow_mut(),
                        move |ws, actor| {
                            tilo_desktop::workspace_settings::apply_revoke(ws, actor, &cap_id)
                        },
                    );
                }
                self.workspace_settings_overlay.dismiss_member_sheet();
            }
            WorkspaceSettingsOutcome::ReviewPermissionActivity => {
                self.workspace_settings_overlay.dismiss_member_sheet();
                if let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    state.open_room_activity(None);
                    state.bump();
                }
            }
            WorkspaceSettingsOutcome::DismissMemberSheet => {
                self.workspace_settings_overlay.dismiss_member_sheet();
                self.bump_chrome();
            }
            WorkspaceSettingsOutcome::SelectTab(tab) => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    st.commit_workspace_name();
                    st.ws_settings_tab = tab;
                    st.bump();
                }
            }
            // The room's host decides who may: it answered the listing only
            // to someone it lets change the room's settings, and checks the
            // change again.
            WorkspaceSettingsOutcome::TogglePublic | WorkspaceSettingsOutcome::CycleDefaultRole => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    let known = state
                        .tree()
                        .desktop
                        .active_workspace()
                        .and_then(|workspace| match state.publication(workspace.room()) {
                            Some(crate::shell::chrome_state::PublicationState::Known(known)) => {
                                Some(known.clone())
                            }
                            _ => None,
                        });
                    if let Some(known) = known {
                        let listing = |default_role| tilo_relay_protocol::directory::Listing {
                            description: known.description.clone(),
                            default_role,
                        };
                        let next = if matches!(outcome, WorkspaceSettingsOutcome::TogglePublic) {
                            (!known.public).then(|| listing(known.default_role))
                        } else {
                            Some(listing(next_door_role(known.default_role)))
                        };
                        state.publish_active_workspace(next);
                        state.bump();
                    }
                }
            }
            WorkspaceSettingsOutcome::EditDescription => {
                // The description is typed in the palette's parameter
                // field, where `workspace.publish` takes it, like every
                // other text an action takes.
                self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
                self.palette
                    .set_query(&crate::shell::i18n::t("search-action-workspace-publish"));
                self.last_search_query = "\0workspace-publish\0".to_string();
            }
            WorkspaceSettingsOutcome::ToggleDevice(index) => {
                if let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().toggle_active_workspace_device(index);
                }
            }
            WorkspaceSettingsOutcome::MoveWorkspace(step) => {
                if let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().move_active_workspace(step);
                }
            }
            WorkspaceSettingsOutcome::RestoreArchived(index) => {
                if let Some(chrome) = self.chrome.clone() {
                    let room = archived_room(&chrome.borrow(), index);
                    if let Some(room) = room {
                        chrome.borrow_mut().restore_archived_workspace(room);
                    }
                }
            }
            WorkspaceSettingsOutcome::DeleteArchived(index) => {
                if let Some(chrome) = self.chrome.clone() {
                    let room = archived_room(&chrome.borrow(), index);
                    if let Some(room) = room {
                        chrome.borrow_mut().delete_archived_workspace(room);
                    }
                }
            }
            WorkspaceSettingsOutcome::DeleteWorkspace => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    if state.delete_active_workspace() {
                        state.overlays.ws_settings = false;
                        state.bump();
                    }
                }
            }
            WorkspaceSettingsOutcome::CreateInvite => {
                if self.may_invite_here()
                    && let Some(chrome) = self.chrome.clone()
                {
                    {
                        let mut state = chrome.borrow_mut();
                        state.overlays.ws_settings = false;
                        state.bump();
                    }
                    self.open_invite_manager();
                }
            }
            WorkspaceSettingsOutcome::EditName => {
                if is_owner && let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    if st.editing_workspace_name.is_none() {
                        let new_name = st
                            .tree()
                            .desktop
                            .active_workspace()
                            .map(|w| w.name.clone())
                            .or_else(|| Some(String::new()));
                        let caret = new_name.as_ref().map_or(0, std::string::String::len);
                        st.editing_workspace_name = new_name;
                        st.editing_workspace_name_selection =
                            ui::text_selection::Selection::collapsed(caret);
                        st.bump();
                    }
                }
            }
            WorkspaceSettingsOutcome::AddEmoji => self.choose_workspace_emoji(),
            WorkspaceSettingsOutcome::RemoveEmoji(name, how) => {
                self.remove_workspace_emoji(&name, how);
            }
            WorkspaceSettingsOutcome::DismissEmojiNotice => self.dismiss_workspace_emoji_notice(),
            WorkspaceSettingsOutcome::CommitName => {
                if is_owner && let Some(chrome) = self.chrome.clone() {
                    chrome.borrow_mut().commit_workspace_name();
                }
            }
        }
    }

    pub(super) fn apply_workspace_settings_default(
        &self,
        resource: desktop_policy::ResourceKind,
        workspace_choice: bool,
        is_owner: bool,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let actor = state.local_user_id.clone();
        let candidates = crate::shell::launcher::default_app_candidates();
        let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
        let mut tree = state.tree_mut();
        if let Some(workspace) = tree.desktop.active_workspace_mut() {
            if workspace_choice && is_owner {
                let next = tilo_desktop::workspace_settings::next_workspace_default(
                    workspace.default_apps.policy.get(&resource),
                    &resource,
                    &candidates,
                );
                let _ = workspace.set_default_app_policy_as(&actor, resource, next, &resolver);
            } else if !workspace_choice {
                let next = tilo_desktop::workspace_settings::next_user_default(
                    workspace
                        .default_apps
                        .user_choices
                        .get(&actor)
                        .and_then(|choices| choices.get(&resource)),
                    &resource,
                    &candidates,
                );
                let _ =
                    workspace.set_default_app_choice_as(&actor, &actor, resource, next, &resolver);
            }
        }
        drop(tree);
        state.bump();
    }
}

impl ShellOverlayHost {
    /// The workspace-settings modal: fly-in from the anchor button, the
    /// modal-open fade, and the settings sheet itself. Second half of
    /// what was `draw_overview_and_ws_settings_overlays`.
    pub(super) fn draw_ws_settings_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        ws_settings_open: bool,
    ) {
        self.workspace_settings_overlay.layout(self.overlay_area());
        let anchor = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow().workspace_settings_anchor);
        let Some(plan) = self.workspace_settings_overlay.begin_draw(
            scene,
            transform,
            theme,
            ws_settings_open,
            anchor,
        ) else {
            return;
        };
        let emoji_grants = self.prepare_ws_emoji();
        if let Some(chrome) = self.chrome.as_ref() {
            let state = chrome.borrow();
            let tree = state.tree();
            let acting_user = state.active_local_user_in(&tree);
            let emoji = ws_emoji::pane_model(self.ws_emoji.as_ref(), emoji_grants, &acting_user);
            let governs_active_room = tree
                .desktop
                .active_workspace()
                .is_some_and(|workspace| tree.governs_room(workspace.room()));
            let owner_controls = state.test_only_owner_override.unwrap_or_else(|| {
                governs_active_room
                    && tree
                        .desktop
                        .active_workspace()
                        .is_some_and(|workspace| workspace.may_administer(&acting_user))
            });
            let devices: Vec<tilo_desktop::workspace_settings::overlay::WorkspaceDevice> = tree
                .desktop
                .active_workspace()
                .and_then(|workspace| tree.personal_device_choices.get(&workspace.room()))
                .into_iter()
                .flatten()
                .map(
                    |choice| tilo_desktop::workspace_settings::overlay::WorkspaceDevice {
                        label: choice.label.clone(),
                        shown: choice.shown,
                        here: choice.here,
                    },
                )
                .collect();
            let archived: Vec<tilo_desktop::workspace_settings::overlay::ArchivedWorkspace> = tree
                .archived_workspaces
                .iter()
                .map(
                    |workspace| tilo_desktop::workspace_settings::overlay::ArchivedWorkspace {
                        room: workspace.room(),
                        name: workspace.name.clone(),
                    },
                )
                .collect();
            let public = public_card(
                tree.desktop
                    .active_workspace()
                    .and_then(|workspace| state.publication(workspace.room())),
            );
            // Named as the roster names them, from the one source, and the
            // person here by their own name: never a key.
            let own_name = crate::shell::topbar_chrome::own_person_name(&state);
            let directory = SettingsDirectory {
                room: tree
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.room().to_string())
                    .unwrap_or_default(),
            };
            let people = tilo_desktop::collaborators::PeopleNames::of(
                &tree.desktop,
                &acting_user,
                Some(own_name.as_str()),
            );
            self.workspace_settings_overlay.draw_contents(
                scene,
                plan.transform,
                text_renderer,
                theme,
                &tilo_desktop::workspace_settings::overlay::WorkspaceSettingsModel {
                    desktop: &tree.desktop,
                    tab: state.ws_settings_tab,
                    grant_draft: state.ws_grant_draft,
                    acting_user: &acting_user,
                    people: &people,
                    editing_name: state.editing_workspace_name.as_deref(),
                    name_selection: state.editing_workspace_name_selection,
                    governs_active_room,
                    owner_controls,
                    apps: &directory,
                    devices: &devices,
                    public: &public,
                    archived: &archived,
                    emoji,
                },
            );
        }
        WorkspaceSettingsOverlay::finish_draw(scene, plan);
    }
}

/// This machine's applications, and the active room's resources by the
/// titles their apps publish, as workspace settings and the palette name
/// them.
pub(super) struct SettingsDirectory {
    pub(super) room: String,
}

impl tilo_desktop::workspace_settings::AppDirectory for SettingsDirectory {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        crate::shell::launcher::ShellApps.app_display_name(app_id)
    }

    fn default_app_candidates(&self) -> Vec<desktop_policy::AppCandidate> {
        crate::shell::launcher::ShellApps.default_app_candidates()
    }

    fn resource_label(&self, address: &str) -> Option<String> {
        crate::shell::search::app_content_index::session().title(&self.room, address)
    }

    fn resources(&self) -> Vec<(String, String)> {
        crate::shell::search::app_content_index::session().resources(&self.room)
    }
}

/// The models of `config` the person may choose for `agent`, and the one it
/// plans with; `None` while they have no switched-on model at all.
fn agent_models(
    config: &agent_provider_config::ProviderConfig,
    agent: &str,
) -> Option<tilo_desktop::workspace_settings::agent_access::AgentModels> {
    use tilo_desktop::workspace_settings::agent_access::{AgentModelOption, AgentModels};
    let option =
        |profile: &agent_provider_config::ProviderProfile,
         model: &agent_provider_config::ModelDescriptor| AgentModelOption {
            profile: profile.id.clone(),
            model: model.id.clone(),
            label: crate::shell::i18n::t_args(
                "workspace-settings-agent-model-option",
                &[
                    ("model", model.display_label().into()),
                    ("provider", profile.display_label().into()),
                ],
            ),
        };
    let offered: Vec<_> = config
        .profiles
        .iter()
        .filter(|profile| profile.enabled)
        .flat_map(|profile| {
            profile
                .models
                .iter()
                .map(move |model| option(profile, model))
        })
        .collect();
    if offered.is_empty() {
        return None;
    }
    let default = config.active_profile().and_then(|profile| {
        let model = profile.default_model.as_ref()?;
        let model = profile.models.iter().find(|m| &m.id == model)?;
        Some(option(profile, model).label)
    });
    let chosen = config.agent_models.get(agent).and_then(|chosen| {
        offered
            .iter()
            .find(|option| option.profile == chosen.profile && option.model == chosen.model)
            .cloned()
    });
    Some(AgentModels {
        default,
        chosen,
        offered,
    })
}

/// Have `agent` plan with `model`, or the default with `None`, in the
/// person's Settings, unless Settings changed meanwhile. Answers what was
/// saved.
fn choose_agent_model(
    agent: &str,
    model: Option<tilo_desktop::workspace_settings::agent_access::AgentModelOption>,
) -> Result<agent_provider_config::ProviderConfig, String> {
    let read = agent_provider_config::ProviderConfig::load();
    let mut config = read.clone();
    config.set_agent_model(
        agent,
        model.map(|model| agent_provider_config::AgentModel {
            profile: model.profile,
            model: model.model,
        }),
    )?;
    if !config
        .compare_save(&read)
        .map_err(|error| error.to_string())?
    {
        return Err("Settings changed meanwhile; choose again".into());
    }
    Ok(config)
}

/// The room of the `index`th archived workspace, in the order the settings
/// list them.
fn archived_room(
    state: &crate::shell::chrome_state::ChromeState,
    index: usize,
) -> Option<crate::shell::state::RoomId> {
    state
        .tree()
        .archived_workspaces
        .get(index)
        .map(crate::shell::state::Workspace::room)
}

/// Which grant-composer control a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ComposerHit {
    Subject,
    Capability,
    /// Index into [`EXPIRY_CHOICES`].
    Expiry(usize),
    Submit,
}

/// The Public card of workspace settings, from what the host last answered
/// about the active workspace's publication.
pub(super) fn public_card(
    state: Option<&crate::shell::chrome_state::PublicationState>,
) -> tilo_desktop::workspace_settings::overlay::PublicCard {
    use crate::shell::chrome_state::PublicationState;
    use tilo_desktop::workspace_settings::overlay::PublicCard;
    match state {
        None | Some(PublicationState::Asking) => PublicCard::Asking,
        // Without the network the host can't be asked or told, so there
        // is nothing to show about being public.
        Some(PublicationState::Unavailable(why))
            if why == super::peer_sync_dispatch::NOT_RUNNING =>
        {
            PublicCard::Offline
        }
        Some(PublicationState::Unavailable(why)) => PublicCard::Unavailable(why.clone()),
        Some(PublicationState::Known(known)) => PublicCard::Known {
            public: known.public,
            description: known.description.clone(),
            default_role: match known.default_role {
                tilo_relay_protocol::rooms::RoomRole::Viewer => {
                    crate::shell::state::CapRole::Viewer
                }
                tilo_relay_protocol::rooms::RoomRole::Commenter => {
                    crate::shell::state::CapRole::Commenter
                }
                tilo_relay_protocol::rooms::RoomRole::Editor => {
                    crate::shell::state::CapRole::Editor
                }
                tilo_relay_protocol::rooms::RoomRole::Moderator => {
                    crate::shell::state::CapRole::Moderator
                }
            },
        },
    }
}

/// The role a public workspace hands out after `role`: the card's cycle,
/// Viewer, Commenter, Editor, and round again.
pub(super) fn next_door_role(
    role: tilo_relay_protocol::rooms::RoomRole,
) -> tilo_relay_protocol::rooms::RoomRole {
    use tilo_relay_protocol::rooms::RoomRole;
    match role {
        RoomRole::Viewer => RoomRole::Commenter,
        RoomRole::Commenter => RoomRole::Editor,
        RoomRole::Editor | RoomRole::Moderator => RoomRole::Viewer,
    }
}

pub(super) fn workspace_settings_composer_hit(
    outcome: &WorkspaceSettingsOutcome,
) -> Option<ComposerHit> {
    match outcome {
        WorkspaceSettingsOutcome::ComposerSubject => Some(ComposerHit::Subject),
        WorkspaceSettingsOutcome::ComposerCapability => Some(ComposerHit::Capability),
        WorkspaceSettingsOutcome::ComposerExpiry(index) => Some(ComposerHit::Expiry(*index)),
        WorkspaceSettingsOutcome::ComposerSubmit => Some(ComposerHit::Submit),
        _ => None,
    }
}

/// Route a click on a grant-composer control. The pickers only move the
/// draft; `Submit` is the one that writes, and it writes through
/// [`tilo_desktop::workspace_settings::apply_grant_draft`] so the
/// authority check runs on the production path.
pub(super) fn apply_composer_hit(chrome: &Chrome, hit: ComposerHit) {
    // A flat match over every variant, rather than peeling `Submit` off
    // first and re-matching the rest — the latter needed an `other =>`
    // catch-all whose own exhaustive sub-match required an `unreachable!`
    // arm for `Submit`, a real panic site in a `panic = "abort"` crate
    // even though it could never fire.
    match hit {
        ComposerHit::Submit => {
            let draft = chrome.borrow().ws_grant_draft;
            let _ = crate::shell::membership::edit_active_workspace_policy(
                &mut chrome.borrow_mut(),
                move |ws, actor| {
                    tilo_desktop::workspace_settings::apply_grant_draft(ws, actor, &draft)
                },
            );
        }
        ComposerHit::Subject => {
            let mut st = chrome.borrow_mut();
            let subject_count = st.tree().desktop.active_workspace().map_or(0, |ws| {
                tilo_desktop::workspace_settings::grant_subjects(ws).len()
            });
            st.ws_grant_draft.cycle_subject(subject_count);
            st.bump();
        }
        ComposerHit::Capability => {
            let mut st = chrome.borrow_mut();
            st.ws_grant_draft.cycle_choice();
            st.bump();
        }
        ComposerHit::Expiry(i) => {
            let mut st = chrome.borrow_mut();
            if let Some(expiry) = EXPIRY_CHOICES.get(i) {
                st.ws_grant_draft.expiry = *expiry;
            }
            st.bump();
        }
    }
}

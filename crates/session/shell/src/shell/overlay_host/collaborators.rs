//! The collaborators dropdown, following and the local presence status.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(super) fn route_collaborators_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let open = self.overlay_flags().collaborators;
        if !open && !self.collaborators_overlay.wants_pointer_capture() {
            return false;
        }
        let hit = self
            .collaborators_overlay
            .handle_event(event, ctx, open, self.last_cursor);
        if let Some(hit) = hit {
            self.apply_collaborators_hit(hit, ctx.handled);
        }
        if (*ctx.handled || matches!(event, WindowEvent::Focused(false)))
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().bump();
        }
        *ctx.handled
    }

    /// Follow the painted row's person, or stop when they are the one
    /// already followed. Revalidated against the live roster first.
    pub(super) fn toggle_follow_row(&mut self, index: usize) {
        let target = self
            .collaborators_overlay
            .follow_target(index)
            .and_then(|id| {
                self.chrome.as_ref().and_then(|chrome| {
                    let state = chrome.borrow();
                    let local_user = state.active_local_user();
                    tilo_desktop::collaborators::collaborator_roster_rows(
                        &state.tree().desktop,
                        &local_user,
                    )
                    .into_iter()
                    .find(|row| row.id == id && row.followable)
                })
            });
        if let Some(target) = target {
            if self.followed_peer.as_deref() == Some(target.id.as_str()) {
                self.set_followed_peer(None);
            } else {
                self.set_followed_peer(Some(target.id));
            }
        }
    }

    /// Expand the painted row's recent summary, or collapse it when it is
    /// already the open one. Keyed by the person, not the row index.
    pub(super) fn toggle_collaborator_details(&mut self, index: usize) {
        let Some(target) = self
            .collaborators_overlay
            .details_target(index)
            .map(str::to_string)
        else {
            return;
        };
        self.expanded_collaborator =
            if self.expanded_collaborator.as_deref() == Some(target.as_str()) {
                None
            } else {
                Some(target)
            };
    }

    /// Open who is here at `id`, or close it when it is already open there.
    fn show_person(&mut self, id: String) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let again = state.overlays.collaborators
            && self.expanded_collaborator.as_deref() == Some(id.as_str());
        state.overlays.collaborators = !again;
        if !again {
            state.overlays.room_activity = false;
        }
        self.expanded_collaborator = (!again).then_some(id);
        state.bump();
    }

    pub(super) fn apply_collaborators_hit(&mut self, hit: CollaboratorsHit, handled: &mut bool) {
        *handled = true;
        match hit {
            CollaboratorsHit::ToggleDetails(index) => self.toggle_collaborator_details(index),
            CollaboratorsHit::ToggleFollow(index) => self.toggle_follow_row(index),
            CollaboratorsHit::Wave(index) => {
                if let Some(target) = self
                    .collaborators_overlay
                    .wave_target(index)
                    .map(str::to_owned)
                {
                    self.wave_at(&target);
                }
            }
            CollaboratorsHit::JoinCall(index) => {
                if let Some(call) = self.collaborators_overlay.join_target(index).cloned() {
                    self.join_voice(&call);
                }
            }
            CollaboratorsHit::OpenChanges(index) => {
                let author = self
                    .collaborators_overlay
                    .changes_author(index)
                    .map(str::to_owned);
                if let (Some(author), Some(chrome)) = (author, self.chrome.clone()) {
                    chrome.borrow_mut().open_room_activity(Some(
                        crate::shell::chrome_state::ActivityFocus {
                            author: Some(author),
                            ..Default::default()
                        },
                    ));
                }
            }
            CollaboratorsHit::OpenChange(index, change) => {
                let target = self
                    .collaborators_overlay
                    .change_target(index, change)
                    .map(|(author, entry)| (author.to_owned(), entry));
                if let (Some((author, entry)), Some(chrome)) = (target, self.chrome.clone()) {
                    chrome.borrow_mut().open_room_activity(Some(
                        crate::shell::chrome_state::ActivityFocus {
                            author: Some(author),
                            entry_id: entry,
                            ..Default::default()
                        },
                    ));
                }
            }
            CollaboratorsHit::Invite => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    state.push_peer_sync_request(PeerSyncAction::PromptInvite);
                    state.overlays.collaborators = false;
                    state.bump();
                }
            }
            CollaboratorsHit::Dismiss => {
                if let Some(chrome) = self.chrome.clone() {
                    let mut state = chrome.borrow_mut();
                    state.overlays.collaborators = false;
                    state.bump();
                }
            }
            CollaboratorsHit::Absorb => {}
        }
    }

    pub(super) fn handle_collaborators_keyboard(
        &mut self,
        logical_key: &Key,
        shift_down: bool,
        handled: &mut bool,
    ) -> bool {
        if !self.overlay_flags().collaborators {
            return false;
        }
        if let Some(hit) = self.collaborators_overlay.key(logical_key, shift_down) {
            self.apply_collaborators_hit(hit, handled);
        } else {
            self.bump_chrome();
            *handled = true;
        }
        true
    }

    pub(super) fn publish_local_status(&self, text: Option<String>) {
        let Some(bridge) = self.status_presence_bridge.as_ref() else {
            return;
        };
        let room = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            let tree = state.tree();
            let room = tree
                .desktop
                .active_workspace()
                .map(crate::shell::state::Workspace::room);
            drop(tree);
            drop(state);
            room
        });
        let Some(room) = room else { return };
        let status = text.map(|text| ui::LiveStatus {
            text,
            origin: self.local_status_origin.clone(),
            updated_at: std::time::SystemTime::now(),
        });
        bridge(room, status);
    }

    /// Say in your own words what you are doing (or clear it), to whoever
    /// your status sharing allows.
    pub(super) fn set_manual_status(&mut self, status: Option<String>) {
        self.local_status = status.filter(|text| !text.trim().is_empty());
        self.local_status_origin = ui::StatusOrigin::Manual;
        self.status_narrative_request = None;
        if self.status_sharing == ui::StatusSharing::Off {
            self.publish_local_status(None);
        } else {
            self.publish_local_status(self.local_status.clone());
        }
        self.persist_collaboration_preferences();
    }

    pub(super) fn set_status_sharing(&mut self, mode: ui::StatusSharing) {
        self.status_sharing = mode;
        if mode != ui::StatusSharing::Automatic
            && matches!(self.local_status_origin, ui::StatusOrigin::Inferred { .. })
        {
            self.local_status = None;
            self.local_status_origin = ui::StatusOrigin::Manual;
            self.status_narrative_request = None;
        }
        if mode == ui::StatusSharing::Off {
            self.publish_local_status(None);
        } else {
            self.publish_local_status(self.local_status.clone());
        }
        self.persist_collaboration_preferences();
    }

    /// Apply semantic presence requests queued by the shared action provider.
    /// This is the same local state and publisher used by pointer/keyboard UI,
    /// so agents do not get a parallel or less truthful collaboration path.
    pub(super) fn pump_presence_requests(&mut self) -> bool {
        let requests = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow_mut().take_presence_requests())
            .unwrap_or_default();
        if requests.is_empty() {
            return false;
        }
        for request in requests {
            match request {
                crate::shell::chrome_state::PresenceRequest::Follow(id) => {
                    self.set_followed_peer(Some(id));
                }
                crate::shell::chrome_state::PresenceRequest::StopFollowing => {
                    self.set_followed_peer(None);
                }
                crate::shell::chrome_state::PresenceRequest::SetManualStatus(status) => {
                    self.set_manual_status(status);
                }
                crate::shell::chrome_state::PresenceRequest::SetStatusSharing(mode) => {
                    self.set_status_sharing(mode);
                }
                crate::shell::chrome_state::PresenceRequest::SetAvailability(chosen) => {
                    self.set_chosen_availability(chosen);
                }
                crate::shell::chrome_state::PresenceRequest::SetVisibility(room, choice) => {
                    self.set_presence_visibility(room, choice);
                }
                crate::shell::chrome_state::PresenceRequest::Wave(target) => {
                    self.wave_at(&target);
                }
                crate::shell::chrome_state::PresenceRequest::Show(id) => self.show_person(id),
                crate::shell::chrome_state::PresenceRequest::JoinCall(call) => {
                    self.join_voice(&call);
                }
            }
        }
        true
    }

    pub(super) fn persist_collaboration_preferences(&self) {
        let manual_status = matches!(self.local_status_origin, ui::StatusOrigin::Manual)
            .then(|| self.local_status.clone())
            .flatten();
        let preferences = CollaborationPreferences {
            sharing: self.status_sharing,
            manual_status,
            availability: self.self_presence.chosen,
            visibility: self.self_presence.visibility.clone(),
            introduced: self.presence_notices.introduced.clone(),
        };
        if let Err(error) =
            settings_store::update_section(CollaborationPreferences::SECTION, preferences.values())
        {
            tracing::warn!(%error, "collaboration preferences save failed");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CollaborationPreferences {
    pub(super) sharing: ui::StatusSharing,
    pub(super) manual_status: Option<String>,
    /// How available the person says they are.
    pub(super) availability: ui::Availability,
    /// What the people in each workspace see, where the person chose.
    pub(super) visibility:
        std::collections::BTreeMap<crate::shell::state::RoomId, ui::PresenceVisibility>,
    /// Public workspaces whose people the person was told about.
    pub(super) introduced: std::collections::BTreeSet<crate::shell::state::RoomId>,
}

/// Human-readable projection of the compositor's screen-share authority for
/// the People popover. Unlike room presence, every clause names the exact
/// surface a participant or agent can presently see or control.
pub(super) fn collaborators_screen_exposure(
    state: &crate::shell::chrome_state::ChromeState,
) -> Option<String> {
    let tree = state.tree();
    let clauses = state
        .active_exposures()
        .iter()
        .map(|exposure| {
            let participant = tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace
                    .collaborators
                    .iter()
                    .find(|person| person.session == Some(exposure.peer))
                    .map(|person| (workspace, person))
            });
            // The people panel already marks an agent with its chip; only
            // a session nobody can name says what it is.
            let name = participant.map_or_else(
                || {
                    t_args(
                        "host-exposure-unidentified",
                        &[(
                            "handle",
                            crate::shell::state::session_handle(exposure.peer).into(),
                        )],
                    )
                },
                |(_, person)| person.name.clone(),
            );
            let target = tree
                .desktop
                .workspaces
                .iter()
                .find_map(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .flat_map(|group| &group.tabs)
                        .find(|tab| {
                            matches!(
                                &tab.tile,
                                crate::shell::state::TileContent::Foreign { surface_id, .. }
                                    if *surface_id == exposure.surface
                            )
                        })
                        .map(|tab| {
                            if workspace.id == tree.desktop.active {
                                tab.title.clone()
                            } else {
                                t_args(
                                    "host-exposure-window-in-workspace",
                                    &[
                                        ("window", tab.title.as_str().into()),
                                        ("workspace", workspace.name.as_str().into()),
                                    ],
                                )
                            }
                        })
                })
                // A window the tree does not list has no name a person knows;
                // its surface number would mean nothing to them.
                .unwrap_or_else(|| t("host-exposure-surface"));
            let args = [
                ("name", ui::i18n::ArgValue::from(name)),
                ("target", ui::i18n::ArgValue::from(target)),
            ];
            if exposure.can_control {
                t_args("host-exposure-controls", &args)
            } else {
                t_args("host-exposure-views", &args)
            }
        })
        .collect::<Vec<_>>();
    (!clauses.is_empty()).then(|| clauses.join("; "))
}

impl CollaborationPreferences {
    pub(super) fn from_values(values: &serde_json::Map<String, serde_json::Value>) -> Self {
        let sharing = match values
            .get("status_sharing")
            .and_then(serde_json::Value::as_str)
        {
            Some("off") => ui::StatusSharing::Off,
            Some("automatic") | None => ui::StatusSharing::Automatic,
            // "manual", and any value this build does not know, shares nothing
            // automatically.
            Some(_) => ui::StatusSharing::ManualOnly,
        };
        let manual_status = values
            .get("manual_status")
            .and_then(serde_json::Value::as_str)
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|text| !text.is_empty() && text.chars().count() <= ui::MAX_STATUS_CHARS);
        let availability = values
            .get("availability")
            .and_then(serde_json::Value::as_str)
            .and_then(ui::Availability::from_id)
            .filter(|chosen| *chosen != ui::Availability::Idle)
            .unwrap_or_default();
        let visibility = crate::shell::network::presence_visibility::saved_choices(values);
        let introduced = values
            .get("presence_introduced")
            .and_then(serde_json::Value::as_array)
            .map(|rooms| {
                rooms
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter_map(crate::shell::state::RoomId::parse)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            sharing,
            manual_status,
            availability,
            visibility,
            introduced,
        }
    }

    /// The settings section these preferences own as a whole.
    const SECTION: &'static str = crate::shell::network::presence_visibility::SECTION;

    pub(super) fn load() -> Self {
        Self::from_values(&settings_store::load_section(Self::SECTION))
    }

    pub(super) fn values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut values = serde_json::Map::new();
        let sharing = match self.sharing {
            ui::StatusSharing::Automatic => "automatic",
            ui::StatusSharing::ManualOnly => "manual",
            ui::StatusSharing::Off => "off",
        };
        values.insert("status_sharing".into(), sharing.into());
        values.insert(
            "manual_status".into(),
            self.manual_status
                .clone()
                .map_or(serde_json::Value::Null, serde_json::Value::String),
        );
        values.insert("availability".into(), self.availability.id().into());
        values.insert(
            "presence_introduced".into(),
            serde_json::Value::Array(
                self.introduced
                    .iter()
                    .map(|room| room.to_string().into())
                    .collect(),
            ),
        );
        values.insert(
            crate::shell::network::presence_visibility::KEY.into(),
            serde_json::Value::Object(
                self.visibility
                    .iter()
                    .map(|(room, choice)| (room.to_string(), choice.id().into()))
                    .collect(),
            ),
        );
        values
    }
}

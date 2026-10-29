//! Taking the governance changes made at this device where they are made.
//! A change to a room's members, roles, grants or settings this device
//! writes itself, signed with its own key, and publishes as it publishes
//! any room change: the room's admission decides it, here and on every
//! device that receives it.
use super::ShellNetwork;
use super::relay_rooms::HostAskError;
use crate::shell::state::{GovernanceRequest, RoomId};
use tilo_peer_state::workspace_membership::{
    AppAction, SettingsChange, WorkspaceMembershipCommand, app_action_lifetime,
};

/// Why a desktop command is not a change to the room's governance.
const NOT_GOVERNANCE: &str = "this change is not one of the room's own";

/// The command this device writes for `request`, or why it writes none.
pub(crate) fn command(request: GovernanceRequest) -> Result<WorkspaceMembershipCommand, String> {
    let settings = match request {
        GovernanceRequest::Policy(change) => {
            return Ok(WorkspaceMembershipCommand::Member { change });
        }
        GovernanceRequest::GrantSandbox(capability) => SettingsChange::GrantSandbox { capability },
        GovernanceRequest::RevokeSandbox(capability) => {
            SettingsChange::RevokeSandbox { capability }
        }
        GovernanceRequest::Command(command) => settings_change(command)?,
    };
    Ok(WorkspaceMembershipCommand::Settings(settings))
}

/// The settings change a desktop `command` makes, or why it makes none.
fn settings_change(
    command: tilo_desktop_model::commands::DesktopCommand,
) -> Result<SettingsChange, String> {
    use tilo_action_policy::{Lifetime, Subject};
    use tilo_desktop_model::commands::DesktopCommand;
    Ok(match command {
        DesktopCommand::RenameWorkspace { name } => SettingsChange::Rename { name },
        DesktopCommand::SavePreset { preset_json } => SettingsChange::SavePreset {
            preset: serde_json::from_str(&preset_json)
                .map_err(|error| format!("the grant set does not decode: {error}"))?,
        },
        DesktopCommand::AssignPreset {
            preset,
            subject_kind,
            subject,
            expires_unix,
        } => SettingsChange::AssignPreset {
            preset,
            subject: match subject_kind.as_str() {
                "person" => Subject::person(subject),
                "app" => Subject::app(subject),
                "agent" => Subject::agent(subject),
                other => {
                    return Err(format!(
                        "a grant set is assigned to a person, an app or an agent, not `{other}`"
                    ));
                }
            },
            lifetime: match expires_unix {
                0 => Lifetime::UntilRevoked,
                seconds => Lifetime::Until(seconds.saturating_mul(1000)),
            },
        },
        DesktopCommand::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            lifetime,
        } => {
            app_action_lifetime(&lifetime)?;
            SettingsChange::GrantAppAction(AppAction::new(&app, &target_app, &resource, &action)?)
        }
        DesktopCommand::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        } => {
            SettingsChange::RevokeAppAction(AppAction::new(&app, &target_app, &resource, &action)?)
        }
        _ => return Err(NOT_GOVERNANCE.into()),
    })
}

impl ShellNetwork {
    /// Take what the tree's outbox holds to ask
    /// ([`crate::shell::state::Outgoing`]) to the rooms' hosts: on the
    /// peer-sync worker, each waiting for its host's answer. An entry stays
    /// in the tree while it is asked; it leaves once its room's document
    /// carries it or its host refuses it.
    ///
    /// For the frame tick: with the outbox empty, or with nothing changed
    /// since the last look ([`Self::outbox_state`]), it is one read of the
    /// tree and a compare, with no scan and no allocation. Never waits for
    /// the tree: a tick that finds another thread writing it looks again on
    /// the tick that writer's release wakes.
    pub(super) fn send_outbox_off_loop(&self) {
        let entries: Vec<_> = {
            let Some(tree) = self.tree.try_read_or_wake(self.release_wake()) else {
                return;
            };
            let Some(state) = Self::outbox_state(&tree) else {
                return;
            };
            let Ok(mut gate) = self.outbox_waiting.lock() else {
                return;
            };
            if *gate == Some(state) {
                return;
            }
            // Nothing more to ask until a room, the outbox or a host changes.
            *gate = Some(state);
            drop(gate);
            let Ok(mut asked) = self.outbox_asked.lock() else {
                return;
            };
            let entries: Vec<_> = tree
                .outbox_to_ask()
                .filter(|entry| !asked.contains(&entry.id))
                .cloned()
                .collect();
            drop(tree);
            asked.extend(entries.iter().map(|entry| entry.id));
            drop(asked);
            entries
        };
        if entries.is_empty() {
            return;
        }
        if let Err(entries) = self.outbox_off_loop(entries) {
            self.send_outbox(&entries);
        }
    }

    /// Where the outbox stands, for the drain's gate: the room epoch and
    /// the newest entry, so a room projected or an entry made opens it.
    /// `None` when the outbox is empty.
    fn outbox_state(tree: &crate::shell::state::WorkspaceTree) -> Option<(u64, uuid::Uuid)> {
        let newest = tree.room_outbox.last()?;
        Some((tree.desktop.room_epoch, newest.id))
    }

    /// Ask each entry's host, in order, and tell the tree how each answered
    /// ([`crate::shell::state::WorkspaceTree::outbox_answered`]): a refusal
    /// is said by the chrome. An entry whose host is out of reach, or could
    /// not be reached to answer, is asked again once a room's state
    /// changed, a host's session came up ([`NetEvent::HostReady`]), another
    /// change was made, or this device's own host was installed.
    ///
    /// [`NetEvent::HostReady`]: tilo_network_actor::NetEvent::HostReady
    pub(super) fn send_outbox(&self, entries: &[crate::shell::state::Outgoing]) {
        use crate::shell::state::{HostAnswer, RoomChange};
        let mut answers = Vec::with_capacity(entries.len());
        for entry in entries {
            let answer = match &entry.change {
                RoomChange::Governance(request) => {
                    match self.write_governance_change(entry.room, request.clone()) {
                        Ok(()) => HostAnswer::Accepted,
                        Err(HostAskError::Refused(error)) => {
                            tracing::info!(room = %entry.room, %error, "a governance change was refused here");
                            HostAnswer::Refused(error)
                        }
                        Err(HostAskError::Unreachable(_)) => HostAnswer::Unreachable,
                    }
                }
                RoomChange::Receipt(receipt) => self.record_receipt(entry.room, receipt),
                RoomChange::WindowUnshare { tab, app_id } => {
                    self.stop_outlived_share(entry.room, *tab, app_id)
                }
            };
            answers.push((entry.id, answer));
        }
        let mut tree = self.tree.write();
        let unreachable = answers
            .iter()
            .any(|(_, answer)| *answer == HostAnswer::Unreachable);
        for (id, answer) in answers {
            tree.outbox_answered(id, answer);
        }
        let state = Self::outbox_state(&tree);
        drop(tree);
        if let Ok(mut asked) = self.outbox_asked.lock() {
            for entry in entries {
                asked.remove(&entry.id);
            }
        }
        // What found its host out of reach waits for a room, the outbox or
        // a host to change from where they stand now.
        if unreachable && let Ok(mut gate) = self.outbox_waiting.lock() {
            *gate = state;
        }
    }

    /// Stop sharing this device's window `tab` of `app_id` with `room`,
    /// whose window it no longer shows, written here as its sharer. A room
    /// this device no longer holds has nothing more to end: its shares were
    /// stopped as it was let go.
    fn stop_outlived_share(
        &self,
        room: RoomId,
        tab: crate::shell::state::TabId,
        app_id: &str,
    ) -> crate::shell::state::HostAnswer {
        use crate::shell::state::HostAnswer;
        if !self.tree.read().room_docs.contains_key(&room) {
            return HostAnswer::Accepted;
        }
        match self.share_window(room, tab, app_id, false, None) {
            Ok(()) => HostAnswer::Accepted,
            Err(error) => {
                tracing::info!(%room, tab, %error, "the room did not stop sharing a closed window");
                HostAnswer::Refused(error)
            }
        }
    }

    /// Write `request` into `room`'s governance as this device and wait
    /// for the room's admission: for a worker, never the frame loop.
    pub(crate) fn write_governance(
        &self,
        room: RoomId,
        request: GovernanceRequest,
    ) -> Result<(), String> {
        self.write_governance_change(room, request)
            .map_err(String::from)
    }

    /// Write `request` into `room`'s governance as this device, signed with
    /// its own key, and publish it as any room change: the room's admission
    /// decides it against this person's grants, here at once and on every
    /// device that receives it. Blocks on the owner's open: for a worker,
    /// never the frame loop. Unreachable while this device holds no replica
    /// of the room to write to yet.
    pub(crate) fn write_governance_change(
        &self,
        room: RoomId,
        request: GovernanceRequest,
    ) -> Result<(), HostAskError> {
        let command = command(request).map_err(HostAskError::Refused)?;
        let owner = self.native_owner().map_err(HostAskError::Unreachable)?;
        let caller = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .map_err(HostAskError::Unreachable)?;
        if self.own_host_governs(room, &caller) {
            // Its host may not be open yet: asked again once it is.
            let host = self.local_host().map_err(HostAskError::Unreachable)?;
            return self
                .commit_membership_at_own_host(&host, &owner, room, caller, command)
                .map_err(HostAskError::Refused);
        }
        self.commit_membership_here(&owner, room, caller, command)
            .map_err(HostAskError::Refused)
    }

    /// Whether this device's own host governs `room`, as seen by `caller`,
    /// this device: the room's governor is then this device's key, which
    /// that host signs the room's history with. A change this device makes
    /// to the room's governance is committed there, never in this device's
    /// own replica as well: two replicas writing under one key number their
    /// changes alike, and each refuses the other's history from then on.
    pub(crate) fn own_host_governs(
        &self,
        room: RoomId,
        caller: &tilo_user_identity::DeviceCert,
    ) -> bool {
        matches!(
            self.governing_host(room, caller),
            Ok(Some(host)) if self.local_host_governs(room, host)
        )
    }

    /// Commit `command`, a membership change of `caller`, this device, at
    /// this device's own `host`, which governs `room`
    /// ([`Self::own_host_governs`]), and take what the host wrote into this
    /// device's replica at once ([`Self::take_own_host_write`]). Blocks on
    /// the host: for a worker, never the frame loop.
    pub(crate) fn commit_membership_at_own_host(
        &self,
        host: &std::sync::Arc<tilo_network_actor::local_host::room_host::RoomHost>,
        owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        caller: tilo_user_identity::DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<(), String> {
        let host = std::sync::Arc::clone(host);
        let governor = caller.device_public_key;
        let (completion, added) = self
            .wait_on(async move {
                host.rooms()
                    .commit_own_membership(&room.to_string(), &caller, command)
                    .await
            })?
            .map_err(|error| error.to_string())?;
        tracing::debug!(%room, generation = completion.generation, "this device's host committed its membership change");
        self.take_own_host_write(owner, room, governor, added)
    }

    /// Take `added`, what this device's own host just wrote to `room` as its
    /// governor `governor`, into `owner`, this device's replica, as the host's
    /// publication would bring it: in process, so the change is in force
    /// here when the call that made it returns.
    pub(crate) fn take_own_host_write(
        &self,
        owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        governor: [u8; 32],
        added: Vec<Vec<u8>>,
    ) -> Result<(), String> {
        if added.is_empty() {
            return Ok(());
        }
        owner.receive(
            room,
            tilo_peer_state::TransportOrigin::RelayDelta(governor),
            neighbour_wire::NeighbourMsg::SyncResp {
                changes: added,
                room: room.to_string(),
            },
            std::time::SystemTime::now(),
        )?;
        self.schedule_native_owner_publication(room);
        Ok(())
    }

    /// Commit `command` in `room` as `caller`, this device, signed with its
    /// own key, and publish it. In a room this device's own host governs,
    /// that host commits it ([`Self::own_host_governs`]).
    pub(crate) fn commit_membership_here(
        &self,
        owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        caller: tilo_user_identity::DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<(), String> {
        if self.own_host_governs(room, &caller) {
            let host = self.local_host()?;
            return self.commit_membership_at_own_host(&host, owner, room, caller, command);
        }
        let request = tilo_owner_commit::OwnerCommitRequest {
            room: room.to_string(),
            operation: tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller,
                command,
            },
        };
        let key = self.native_signing_key_of(owner, room)?;
        owner.commit_workspace_membership_command(request, &key, std::time::SystemTime::now())?;
        self.schedule_native_owner_publication(room);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::command;
    use crate::shell::state::GovernanceRequest;
    use tilo_action_policy::{Change, Lifetime, Role, Subject};
    use tilo_desktop_model::commands::DesktopCommand;
    use tilo_peer_state::workspace_membership::{
        AppAction, SettingsChange, WorkspaceMembershipCommand,
    };

    const PERSON: &str = "ab";

    fn person() -> String {
        PERSON.repeat(32)
    }

    /// The settings change this device writes for `request`.
    fn written(request: GovernanceRequest) -> SettingsChange {
        match command(request).expect("a command to write") {
            WorkspaceMembershipCommand::Settings(change) => change,
            other => panic!("not a settings change: {other:?}"),
        }
    }

    #[test]
    fn sandbox_capabilities_and_desktop_commands_are_settings_changes() {
        let capability = crate::shell::state::CapOverride::grant(
            collab_types::caps::Subject::app("so.tilo.reader"),
            collab_types::caps::Scope::NetHost("example.org".into()),
            collab_types::caps::Access::Read,
        )
        .with_id("cap-1");
        assert_eq!(
            written(GovernanceRequest::GrantSandbox(capability.clone())),
            SettingsChange::GrantSandbox { capability }
        );
        assert_eq!(
            written(GovernanceRequest::RevokeSandbox("cap-1".into())),
            SettingsChange::RevokeSandbox {
                capability: "cap-1".into()
            }
        );
        assert_eq!(
            written(GovernanceRequest::Command(
                DesktopCommand::RenameWorkspace {
                    name: "Design".into(),
                }
            )),
            SettingsChange::Rename {
                name: "Design".into()
            }
        );
        let preset = tilo_action_policy::Preset::new("Reviewers", "Review", Vec::new());
        assert_eq!(
            written(GovernanceRequest::Command(DesktopCommand::SavePreset {
                preset_json: serde_json::to_string(&preset).unwrap(),
            })),
            SettingsChange::SavePreset { preset }
        );
        assert!(
            command(GovernanceRequest::Command(DesktopCommand::SavePreset {
                preset_json: "not json".into(),
            }))
            .is_err()
        );
        assert_eq!(
            written(GovernanceRequest::Command(DesktopCommand::AssignPreset {
                preset: "Reviewers".into(),
                subject_kind: "person".into(),
                subject: person(),
                expires_unix: 0,
            })),
            SettingsChange::AssignPreset {
                preset: "Reviewers".into(),
                subject: Subject::person(person()),
                lifetime: Lifetime::UntilRevoked,
            },
            "no expiry is none at all"
        );
        assert_eq!(
            written(GovernanceRequest::Command(DesktopCommand::AssignPreset {
                preset: "Reviewers".into(),
                subject_kind: "agent".into(),
                subject: "scribe".into(),
                expires_unix: 7,
            })),
            SettingsChange::AssignPreset {
                preset: "Reviewers".into(),
                subject: Subject::agent("scribe"),
                lifetime: Lifetime::Until(7000),
            }
        );
        let action = AppAction::new("so.tilo.notes", "so.tilo.files", "files", "read").unwrap();
        assert_eq!(
            written(GovernanceRequest::Command(
                DesktopCommand::RevokeAppAction {
                    app: "so.tilo.notes".into(),
                    target_app: "so.tilo.files".into(),
                    resource: "files".into(),
                    action: "read".into(),
                }
            )),
            SettingsChange::RevokeAppAction(action.clone())
        );
        assert_eq!(
            written(GovernanceRequest::Command(DesktopCommand::GrantAppAction {
                app: "so.tilo.notes".into(),
                target_app: "so.tilo.files".into(),
                resource: "files".into(),
                action: "read".into(),
                lifetime: "always".into(),
            })),
            SettingsChange::GrantAppAction(action)
        );
        assert!(
            command(GovernanceRequest::Command(DesktopCommand::GrantAppAction {
                app: "so.tilo.notes".into(),
                target_app: "so.tilo.files".into(),
                resource: "files".into(),
                action: "read".into(),
                lifetime: "session".into(),
            }))
            .is_err(),
            "a room has no session to end a grant with"
        );
    }

    #[test]
    fn a_command_that_is_not_the_rooms_own_is_said_so_not_written() {
        assert!(
            command(GovernanceRequest::Command(DesktopCommand::DeleteShortcut {
                id: "s".into()
            }))
            .is_err()
        );
    }

    #[test]
    fn a_membership_change_is_written_as_one() {
        let change = Change::SetRole {
            person: person(),
            role: Role::Viewer,
        };
        assert_eq!(
            command(GovernanceRequest::Policy(change.clone())),
            Ok(WorkspaceMembershipCommand::Member { change })
        );
    }

    /// Take the active workspace of `chrome`'s tree, as it stands, for what
    /// its room's admitted document says.
    fn admit_as_it_stands(chrome: &crate::shell::chrome_state::Chrome) {
        use automerge::transaction::Transactable as _;
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let workspace = tree
            .desktop
            .active_workspace()
            .expect("a workspace")
            .clone();
        let mut document = automerge::AutoCommit::new();
        document
            .put(automerge::ROOT, "room", workspace.room().to_string())
            .expect("room");
        document
            .put(automerge::ROOT, "name", workspace.name.clone())
            .expect("name");
        tilo_desktop_model::projection::project_room_policy(&mut document, &workspace.policy)
            .expect("policy");
        tilo_desktop_model::projection::project_room_sandbox(&mut document, &workspace.sandbox)
            .expect("sandbox");
        tree.room_docs.insert(workspace.room(), document);
    }

    /// Write what was queued, which this device cannot (it holds no custody
    /// of the room to sign with), and let the chrome say it.
    fn refused_here(chrome: &crate::shell::chrome_state::Chrome) {
        let handle = chrome.borrow().tree_handle();
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle, 1);
        network.send_outbox_off_loop();
        assert!(
            crate::shell::workspace_governance::report_governance_refusals(
                &mut chrome.borrow_mut()
            ),
            "the refusal is said"
        );
    }

    #[test]
    fn a_removal_not_written_is_said_and_the_member_still_shows() {
        let (chrome, _guard) =
            crate::shell::native_workspace_owner::test_chrome_governing_its_room();
        let governor = chrome.borrow().active_local_user();
        let member = "cd".repeat(32);
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let room = tree.desktop.active_workspace().expect("a workspace").room();
            tree.request_governance(
                room,
                GovernanceRequest::Policy(Change::AddMember {
                    person: member.clone(),
                    role: Role::Editor,
                }),
            );
            tree.govern_requests_here_for_test(&governor);
        }
        admit_as_it_stands(&chrome);

        crate::shell::membership::remove_member(&mut chrome.borrow_mut(), &member)
            .expect("the owner may ask to remove a member");
        // This device cannot write the removal: here it holds no custody of
        // the room to sign it with.
        let handle = chrome.borrow().tree_handle();
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle, 1);
        network.send_outbox_off_loop();
        assert!(
            crate::shell::workspace_governance::report_governance_refusals(
                &mut chrome.borrow_mut()
            ),
            "the refusal is said"
        );

        let state = chrome.borrow();
        let said = state
            .notifications()
            .iter()
            .find(|notice| {
                notice.title
                    == crate::shell::i18n::t_args(
                        "host-action-cannot-run",
                        &[(
                            "title",
                            crate::shell::i18n::t("search-action-member-remove").into(),
                        )],
                    )
            })
            .expect("the removal is said to have failed, as a failed action is");
        assert!(
            said.body.contains("custody"),
            "why it was not written: {}",
            said.body
        );
        assert_eq!(
            state
                .tree()
                .desktop
                .active_workspace()
                .expect("a workspace")
                .policy
                .members
                .get(&member),
            Some(&Role::Editor),
            "the person is still a member on screen"
        );
        assert!(
            state.tree().room_outbox.is_empty(),
            "nothing waits to be asked again"
        );
    }

    #[test]
    fn a_refused_rename_shows_the_rooms_name_again_and_a_refused_narrowing_stays() {
        let (chrome, _guard) =
            crate::shell::native_workspace_owner::test_chrome_governing_its_room();
        let governor = chrome.borrow().active_local_user();
        let capability = crate::shell::state::CapOverride::grant(
            collab_types::caps::Subject::app("so.tilo.reader"),
            collab_types::caps::Scope::NetHost("example.org".into()),
            collab_types::caps::Access::Read,
        )
        .with_id("reader-net");
        let (workspace, name) = chrome
            .borrow()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .map(|workspace| {
                workspace.sandbox.push(capability.clone());
                (workspace.id, workspace.name.clone())
            })
            .expect("a workspace");
        admit_as_it_stands(&chrome);

        {
            let mut state = chrome.borrow_mut();
            state.editing_workspace_name = Some("Elsewhere".into());
            state.commit_workspace_name();
        }
        assert!(
            chrome
                .borrow()
                .tree_mut()
                .narrow_sandbox(workspace, &governor, "reader-net"),
            "the capability is taken back here at once"
        );
        refused_here(&chrome);

        let state = chrome.borrow();
        let tree = state.tree();
        let shown = tree.desktop.active_workspace().expect("a workspace");
        assert_eq!(shown.name, name, "the name the room has shows again");
        assert!(
            !shown.sandbox.contains(&capability),
            "a capability taken back stays taken back here though the room still grants it"
        );
        assert!(
            matches!(
                tree.room_outbox.as_slice(),
                [entry] if entry.takes_back.as_ref() == Some(&capability)
                    && entry.standing == crate::shell::state::Standing::Refused
            ),
            "the refused narrowing is kept, and not asked again: {:?}",
            tree.room_outbox
        );
        drop(tree);
        let titles: Vec<_> = state
            .notifications()
            .iter()
            .map(|notice| notice.title.clone())
            .collect();
        for change in ["global-search-rename-workspace", "governance-change-access"] {
            let title = crate::shell::i18n::t_args(
                "host-action-cannot-run",
                &[("title", crate::shell::i18n::t(change).into())],
            );
            assert!(titles.contains(&title), "{title} is said, in {titles:?}");
        }
    }
}

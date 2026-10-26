//! Taking the governance changes made at this device to their rooms'
//! governors. A room's governance changes only at its governor, its host's
//! `Rooms`; this device asks and reads the answer back from the room's
//! admitted document, as every other member does.
use super::ShellNetwork;
use super::relay_rooms::HostAskError;
use crate::shell::state::{GovernanceRequest, RoomId};
use tilo_action_policy::Change;
use tilo_action_policy::roles::{governance, owner_commands as verbs};

/// Why a change has no route to its governor.
const NO_GOVERNOR_ACTION: &str = "the workspace host has no action for this change";

/// What a request asks of the room's host: one of its room governance
/// verbs (provider `tilo.workspace`), with its arguments. The host decides
/// it against the caller's own standing in the room.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GovernanceRoute {
    pub(crate) action: &'static str,
    pub(crate) arguments: serde_json::Map<String, serde_json::Value>,
}

impl GovernanceRoute {
    fn with(action: &'static str, arguments: serde_json::Value) -> Self {
        let arguments = match arguments {
            serde_json::Value::Object(arguments) => arguments,
            _ => serde_json::Map::new(),
        };
        Self { action, arguments }
    }
}

/// The route `request` takes to its room's governor, or why it has none.
pub(crate) fn route(request: GovernanceRequest, now_ms: u64) -> Result<GovernanceRoute, String> {
    use serde_json::json;
    let person = |person: &str| -> Result<(), String> {
        if person.len() != 64 || hex::decode(person).is_err() {
            return Err("a member is named by their user root key".into());
        }
        Ok(())
    };
    let member = |action: &'static str, who: String| -> Result<GovernanceRoute, String> {
        person(&who)?;
        Ok(GovernanceRoute::with(action, json!({ "person": who })))
    };
    let route = match request {
        GovernanceRequest::Policy(
            Change::AddMember { person: who, role } | Change::SetRole { person: who, role },
        ) => {
            person(&who)?;
            let role = role.name().to_ascii_lowercase();
            GovernanceRoute::with(governance::SET_ROLE, json!({ "person": who, "role": role }))
        }
        GovernanceRequest::Policy(Change::Restrict {
            person: who,
            until_ms,
        }) => {
            person(&who)?;
            let minutes = until_ms.saturating_sub(now_ms).div_ceil(60_000).max(1);
            GovernanceRoute::with(
                governance::RESTRICT,
                json!({ "person": who, "minutes": minutes }),
            )
        }
        GovernanceRequest::Policy(Change::Unrestrict { person: who }) => {
            member(governance::UNRESTRICT, who)?
        }
        GovernanceRequest::Policy(Change::Unban { person: who }) => member(governance::UNBAN, who)?,
        GovernanceRequest::Policy(Change::RemoveMember { person: who }) => {
            member(governance::REMOVE_MEMBER, who)?
        }
        GovernanceRequest::Policy(Change::Ban { person: who }) => member(governance::BAN, who)?,
        GovernanceRequest::Policy(Change::Grant(grant)) => {
            GovernanceRoute::with(governance::GRANT, json!({ "grant": grant }))
        }
        GovernanceRequest::Policy(Change::Revoke(grant)) => {
            GovernanceRoute::with(governance::REVOKE, json!({ "grant": grant }))
        }
        GovernanceRequest::GrantSandbox(capability) => {
            GovernanceRoute::with(verbs::GRANT_SANDBOX, json!({ "capability": capability }))
        }
        GovernanceRequest::RevokeSandbox(capability) => {
            GovernanceRoute::with(verbs::REVOKE_SANDBOX, json!({ "capability": capability }))
        }
        GovernanceRequest::Command(command) => command_route(command)?,
    };
    Ok(route)
}

/// The route of a desktop `command` to its room's governor, or why it has
/// none.
fn command_route(
    command: tilo_desktop_model::commands::DesktopCommand,
) -> Result<GovernanceRoute, String> {
    use serde_json::json;
    use tilo_desktop_model::commands::DesktopCommand;
    let route = match command {
        DesktopCommand::RenameWorkspace { name } => {
            GovernanceRoute::with(verbs::RENAME_WORKSPACE, json!({ "name": name }))
        }
        DesktopCommand::SavePreset { preset_json } => {
            GovernanceRoute::with(verbs::SAVE_PRESET, json!({ "preset": preset_json }))
        }
        DesktopCommand::AssignPreset {
            preset,
            subject_kind,
            subject,
            expires_unix,
        } => {
            let mut route = GovernanceRoute::with(
                verbs::ASSIGN_PRESET,
                json!({
                    "preset": preset,
                    "subject_kind": subject_kind,
                    "subject": subject,
                }),
            );
            // No expiry is written as none at all.
            if expires_unix != 0 {
                route
                    .arguments
                    .insert("expires_unix".into(), expires_unix.into());
            }
            route
        }
        DesktopCommand::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            lifetime,
        } => GovernanceRoute::with(
            verbs::GRANT_APP_ACTION,
            json!({
                "app": app,
                "target_app": target_app,
                "resource": resource,
                "action": action,
                "lifetime": lifetime,
            }),
        ),
        DesktopCommand::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        } => GovernanceRoute::with(
            verbs::REVOKE_APP_ACTION,
            json!({
                "app": app,
                "target_app": target_app,
                "resource": resource,
                "action": action,
            }),
        ),
        _ => return Err(NO_GOVERNOR_ACTION.into()),
    };
    Ok(route)
}

/// The room host this device runs, answering for the rooms placed on it
/// through its in-process caller: a request for one of those rooms is
/// asked there, with or without a running network.
pub(crate) trait LocalGovernor: Send + Sync {
    /// Whether this device's own host governs `room`.
    fn governs(&self, room: RoomId) -> bool;
    /// Ask the host for `route` in `room`, and wait for its answer.
    ///
    /// # Errors
    /// The host refused it, or could not be asked.
    fn ask(&self, room: RoomId, route: GovernanceRoute) -> Result<(), String>;
    /// Have the host record `receipt` in `room`, this device's member
    /// receipt, and wait for its answer.
    ///
    /// # Errors
    /// The host refused it, or could not be asked; a governor that holds no
    /// room's receipts says so.
    fn record_receipt(
        &self,
        room: RoomId,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> Result<(), String> {
        let _ = (room, receipt);
        Err("this governor records no receipts".into())
    }
}

impl ShellNetwork {
    /// Install the room host this device runs as the governor of the rooms
    /// placed on it.
    pub(crate) fn install_local_governor(&self, governor: std::sync::Arc<dyn LocalGovernor>) {
        let _ = self.local_governor.set(governor);
        // Changes waiting for a host out of reach may reach this one.
        if let Ok(mut waiting) = self.outbox_waiting.lock() {
            *waiting = None;
        }
    }

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
    /// [`NetEvent::HostReady`]: crate::network::actor::NetEvent::HostReady
    pub(super) fn send_outbox(&self, entries: &[crate::shell::state::Outgoing]) {
        use crate::shell::state::{HostAnswer, RoomChange};
        let mut answers = Vec::with_capacity(entries.len());
        for entry in entries {
            let answer = match &entry.change {
                RoomChange::Governance(request) => {
                    match self.ask_governor_if_reachable(entry.room, request) {
                        Some(Ok(())) => HostAnswer::Accepted,
                        Some(Err(error)) => {
                            tracing::info!(room = %entry.room, %error, "the workspace host refused a governance change");
                            HostAnswer::Refused(error)
                        }
                        None => HostAnswer::Unreachable,
                    }
                }
                RoomChange::Receipt(receipt) => self.record_receipt(entry.room, receipt),
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

    /// Ask `room`'s governor for `request` when it can be reached now:
    /// `None` when it cannot, or its host could not be reached to answer
    /// ([`HostAskError::Unreachable`]); only the governor's own refusal is
    /// `Some(Err)`.
    fn ask_governor_if_reachable(
        &self,
        room: RoomId,
        request: &GovernanceRequest,
    ) -> Option<Result<(), String>> {
        let route = match route(request.clone(), tilo_clock::unix_now_ms()) {
            Ok(route) => route,
            Err(refused) => return Some(Err(refused)),
        };
        if let Some(local) = self.local_governor.get()
            && local.governs(room)
        {
            return Some(local.ask(room, route));
        }
        if !self.governor_reachable(room) {
            return None;
        }
        match self.ask_governor_over_network(room, route) {
            Ok(()) => Some(Ok(())),
            Err(HostAskError::Refused(reason)) => Some(Err(reason)),
            Err(HostAskError::Unreachable(reason)) => {
                tracing::debug!(%room, %reason, "the workspace host is out of reach; the governance change waits");
                None
            }
        }
    }

    /// Whether `room`'s governor can be asked over the network now: the
    /// network runs and this device holds a session with the host.
    fn governor_reachable(&self, room: RoomId) -> bool {
        self.runtime.is_some() && self.administered_room(room).is_some()
    }

    /// Ask `room`'s governor for `request` and wait for its answer: for a
    /// worker, never the frame loop.
    pub(crate) fn ask_governor(
        &self,
        room: RoomId,
        request: GovernanceRequest,
    ) -> Result<(), String> {
        let route = route(request, tilo_clock::unix_now_ms())?;
        if let Some(local) = self.local_governor.get()
            && local.governs(room)
        {
            return local.ask(room, route);
        }
        self.ask_governor_over_network(room, route)
            .map_err(String::from)
    }

    /// Ask `room`'s governor over the network for `route`, answered whenever
    /// its host answers: while the room's sessions last, never on a clock.
    fn ask_governor_over_network(
        &self,
        room: RoomId,
        route: GovernanceRoute,
    ) -> Result<(), HostAskError> {
        let held = self.administered_room(room).ok_or_else(|| {
            HostAskError::Unreachable("this device holds no session with the workspace host".into())
        })?;
        self.govern_held(&held, route.action, route.arguments)
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::{GovernanceRoute, route};
    use crate::shell::state::GovernanceRequest;
    use serde_json::json;
    use tilo_action_policy::roles::{governance, owner_commands};
    use tilo_action_policy::{Change, Role};
    use tilo_desktop_model::commands::DesktopCommand;

    const PERSON: &str = "ab";

    fn person() -> String {
        PERSON.repeat(32)
    }

    /// The verb and arguments `request` asks the host for.
    fn asked(request: GovernanceRequest) -> (&'static str, serde_json::Value) {
        let route = route(request, 1_000).expect("a route to the host");
        (route.action, serde_json::Value::Object(route.arguments))
    }

    #[test]
    fn member_changes_ask_the_hosts_room_governance_by_user_root_key() {
        let who = person();
        assert_eq!(
            asked(GovernanceRequest::Policy(Change::SetRole {
                person: who.clone(),
                role: Role::Editor,
            })),
            (
                governance::SET_ROLE,
                json!({ "person": who, "role": "editor" })
            )
        );
        assert_eq!(
            asked(GovernanceRequest::Policy(Change::Restrict {
                person: who.clone(),
                until_ms: 1_000 + 90_000,
            })),
            (governance::RESTRICT, json!({ "person": who, "minutes": 2 })),
            "a restriction asks for whole minutes from now"
        );
        for (change, verb) in [
            (
                Change::Unrestrict {
                    person: who.clone(),
                },
                governance::UNRESTRICT,
            ),
            (
                Change::Unban {
                    person: who.clone(),
                },
                governance::UNBAN,
            ),
            (
                Change::RemoveMember {
                    person: who.clone(),
                },
                governance::REMOVE_MEMBER,
            ),
            (
                Change::Ban {
                    person: who.clone(),
                },
                governance::BAN,
            ),
        ] {
            assert_eq!(
                asked(GovernanceRequest::Policy(change)),
                (verb, json!({ "person": who }))
            );
        }
    }

    #[test]
    fn grants_sandbox_capabilities_and_desktop_commands_ask_the_hosts_verbs() {
        let grant = tilo_action_policy::Grant::new(
            "g-1",
            tilo_action_policy::Subject::person(person()),
            tilo_action_policy::TreePath::room("r"),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Subject::person(person()),
        );
        assert_eq!(
            asked(GovernanceRequest::Policy(Change::Grant(grant.clone()))),
            (governance::GRANT, json!({ "grant": grant }))
        );
        assert_eq!(
            asked(GovernanceRequest::Policy(Change::Revoke(
                tilo_action_policy::GrantId::new("g-1")
            ))),
            (governance::REVOKE, json!({ "grant": "g-1" }))
        );
        let capability = crate::shell::state::CapOverride::grant(
            collab_types::caps::Subject::app("so.tilo.reader"),
            collab_types::caps::Scope::NetHost("example.org".into()),
            collab_types::caps::Access::Read,
        )
        .with_id("cap-1");
        assert_eq!(
            asked(GovernanceRequest::GrantSandbox(capability.clone())),
            (
                owner_commands::GRANT_SANDBOX,
                json!({ "capability": capability })
            )
        );
        assert_eq!(
            asked(GovernanceRequest::RevokeSandbox("cap-1".into())),
            (
                owner_commands::REVOKE_SANDBOX,
                json!({ "capability": "cap-1" })
            )
        );
        assert_eq!(
            asked(GovernanceRequest::Command(
                DesktopCommand::RenameWorkspace {
                    name: "Design".into(),
                }
            )),
            (
                owner_commands::RENAME_WORKSPACE,
                json!({ "name": "Design" })
            )
        );
        assert_eq!(
            asked(GovernanceRequest::Command(DesktopCommand::SavePreset {
                preset_json: "{}".into(),
            })),
            (owner_commands::SAVE_PRESET, json!({ "preset": "{}" }))
        );
        assert_eq!(
            asked(GovernanceRequest::Command(DesktopCommand::AssignPreset {
                preset: "Reviewers".into(),
                subject_kind: "person".into(),
                subject: person(),
                expires_unix: 0,
            })),
            (
                owner_commands::ASSIGN_PRESET,
                json!({ "preset": "Reviewers", "subject_kind": "person", "subject": person() })
            ),
            "no expiry is sent as none"
        );
        let action = json!({
            "app": "so.tilo.notes",
            "target_app": "so.tilo.files",
            "resource": "files",
            "action": "read",
        });
        assert_eq!(
            asked(GovernanceRequest::Command(
                DesktopCommand::RevokeAppAction {
                    app: "so.tilo.notes".into(),
                    target_app: "so.tilo.files".into(),
                    resource: "files".into(),
                    action: "read".into(),
                }
            )),
            (owner_commands::REVOKE_APP_ACTION, action.clone())
        );
        let mut granted = action;
        granted["lifetime"] = "always".into();
        assert_eq!(
            asked(GovernanceRequest::Command(DesktopCommand::GrantAppAction {
                app: "so.tilo.notes".into(),
                target_app: "so.tilo.files".into(),
                resource: "files".into(),
                action: "read".into(),
                lifetime: "always".into(),
            })),
            (owner_commands::GRANT_APP_ACTION, granted)
        );
    }

    #[test]
    fn a_command_the_host_has_no_verb_for_is_said_so_not_applied_here() {
        assert!(
            route(
                GovernanceRequest::Command(DesktopCommand::DeleteShortcut { id: "s".into() }),
                0
            )
            .is_err()
        );
    }

    #[test]
    fn a_member_is_named_by_their_user_root_key() {
        assert!(
            route(
                GovernanceRequest::Policy(Change::SetRole {
                    person: "ada".into(),
                    role: Role::Viewer,
                }),
                0,
            )
            .is_err()
        );
    }

    /// This device's own host, applying what it is asked as the room's
    /// projection then carries it: a rename shows as the workspace's name.
    struct OwnHost {
        room: crate::shell::state::RoomId,
        tree: crate::shell::state::WorkspaceTreeHandle,
    }

    impl super::LocalGovernor for OwnHost {
        fn governs(&self, room: crate::shell::state::RoomId) -> bool {
            room == self.room
        }

        fn ask(
            &self,
            room: crate::shell::state::RoomId,
            route: GovernanceRoute,
        ) -> Result<(), String> {
            let Some(name) = route
                .arguments
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|_| route.action == owner_commands::RENAME_WORKSPACE)
            else {
                return Err("this host only renames".into());
            };
            let name = name.to_owned();
            self.tree
                .write()
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room() == room)
                .ok_or("no such room")?
                .name = name;
            Ok(())
        }
    }

    #[test]
    fn a_rename_made_with_its_host_out_of_reach_applies_after_a_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.json");
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(path.clone());
        let room = tree.desktop.active_workspace().expect("a workspace").room();
        let rename = GovernanceRequest::Command(DesktopCommand::RenameWorkspace {
            name: "Design".into(),
        });
        tree.request_governance(room, rename.clone());
        let handle = tree.shared();

        // No network and no host yet: nothing is asked, nothing is lost.
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle.clone(), 1);
        network.send_outbox_off_loop();
        assert_eq!(
            governance_in_outbox(&handle.read()),
            vec![(room, rename.clone())],
            "a change whose host is out of reach waits"
        );
        network.send_outbox_off_loop();
        assert_eq!(
            handle.read().room_outbox.len(),
            1,
            "and is not asked again on every tick"
        );
        crate::shell::persist::save_tree_to(&handle.read(), &path).expect("save");
        drop(network);
        drop(handle);

        // A restart, with this device's own host governing the room.
        let mut loaded = crate::shell::persist::load_tree_from(&path).expect("load");
        assert_eq!(
            governance_in_outbox(&loaded),
            vec![(room, rename)],
            "the waiting change survives the restart"
        );
        loaded.persist_path = Some(path);
        let handle = loaded.shared();
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle.clone(), 1);
        network.install_local_governor(std::sync::Arc::new(OwnHost {
            room,
            tree: handle.clone(),
        }));
        network.send_outbox_off_loop();
        assert!(handle.read().room_outbox.is_empty(), "the host took it");
        assert_eq!(
            handle
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.name.as_str()),
            Some("Design"),
            "and the rename applies"
        );
    }

    /// The governance changes `tree`'s outbox holds, in order.
    fn governance_in_outbox(
        tree: &crate::shell::state::WorkspaceTree,
    ) -> Vec<(crate::shell::state::RoomId, GovernanceRequest)> {
        tree.room_outbox
            .iter()
            .filter_map(|entry| match &entry.change {
                crate::shell::state::RoomChange::Governance(request) => {
                    Some((entry.room, request.clone()))
                }
                crate::shell::state::RoomChange::Receipt(_) => None,
            })
            .collect()
    }

    /// A room host that refuses everything it is asked, for `reason`.
    struct Refusing {
        room: crate::shell::state::RoomId,
        reason: &'static str,
    }

    impl super::LocalGovernor for Refusing {
        fn governs(&self, room: crate::shell::state::RoomId) -> bool {
            room == self.room
        }

        fn ask(&self, _: crate::shell::state::RoomId, _: GovernanceRoute) -> Result<(), String> {
            Err(self.reason.into())
        }
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

    /// Ask `chrome`'s room host, which refuses, for what was queued, and
    /// let the chrome say it.
    fn refused_by_the_host(chrome: &crate::shell::chrome_state::Chrome) {
        let handle = chrome.borrow().tree_handle();
        let room = handle
            .read()
            .desktop
            .active_workspace()
            .expect("a workspace")
            .room();
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle, 1);
        network.install_local_governor(std::sync::Arc::new(Refusing {
            room,
            reason: "only the room's owner may do that",
        }));
        network.send_outbox_off_loop();
        assert!(
            crate::shell::workspace_governance::report_governance_refusals(
                &mut chrome.borrow_mut()
            ),
            "the refusal is said"
        );
    }

    #[test]
    fn a_refused_removal_is_said_and_the_member_still_shows() {
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
        refused_by_the_host(&chrome);

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
        assert_eq!(said.body, "only the room's owner may do that");
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
        refused_by_the_host(&chrome);

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

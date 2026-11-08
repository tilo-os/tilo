//! A workspace's layout scope: whether its members' strips and windows are
//! shared with everyone or stay on their own devices
//! ([`tilo_desktop_model::projection::WorkspaceLayout`]).
//!
//! Its governance records the scope for every member to read
//! ([`tilo_desktop_model::projection::WORKSPACE_LAYOUT_KEY`]). The layout itself
//! is the workspace's shared desktop record.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::projection::{WORKSPACE_LAYOUT_KEY, WorkspaceLayout};
use tilo_desktop_model::shared_record;

impl PeerGovernance<'_> {
    /// The workspace's layout scope, as its governance document records it:
    /// shared unless its governor chose otherwise.
    #[must_use]
    pub fn workspace_layout(self) -> WorkspaceLayout {
        self.peer.core.workspace_doc.as_deref().map_or_else(
            WorkspaceLayout::default,
            tilo_desktop_model::projection::workspace_layout,
        )
    }

    /// The layout scope the governor chose, `None` while it chose none.
    #[must_use]
    pub fn workspace_layout_choice(self) -> Option<WorkspaceLayout> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .and_then(tilo_desktop_model::projection::workspace_layout_choice)
    }
}

impl PeerGovernanceMut<'_> {
    /// Set the workspace's layout scope to `layout`, authored by the governor
    /// `key`. Choosing what the workspace already chose changes nothing. Returns
    /// whether it changed the workspace.
    pub fn author_workspace_layout(
        self,
        layout: WorkspaceLayout,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        if self.peer.governance().workspace_layout_choice() == Some(layout) {
            return Ok(false);
        }
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(ROOT, WORKSPACE_LAYOUT_KEY, layout.id())
            .map_err(err)?;
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }

    /// Remove every strip and window someone made from the workspace's layout,
    /// with the groups left empty: in a workspace whose layout is personal, which
    /// apps a person has open and their titles stay on their devices, the
    /// governor's own included (its device takes them out itself). What the
    /// workspace itself holds (nobody made) stays. Returns whether it changed the
    /// workspace.
    pub fn drop_member_windows(self, key: &SigningKey, now: SystemTime) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        let authors = shared_record::read(&doc, None)
            .map(|desktop| desktop.authors)
            .unwrap_or_default();
        doc.set_actor(self.peer.workspace_actor_of(key));
        let dropped = shared_record::device::withdraw(&mut doc, &|id| authors.contains_key(&id))
            .map_err(err)?;
        if !dropped {
            return Ok(false);
        }
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use tilo_desktop_model::ids::compose_id;
    use tilo_desktop_model::projection::WorkspaceLayout;
    use tilo_desktop_model::shared_record::{self, device};
    use tilo_desktop_model::width::WindowWidth;
    use tilo_desktop_model::{
        GroupId, Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
        WorkspaceSlot,
    };
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const MEMBER: u16 = 42;

    fn window(salt: u16, counter: u64, title: &str) -> WindowGroup {
        let id: GroupId = compose_id(salt, counter);
        WindowGroup {
            id,
            tabs: vec![Tab {
                id,
                title: title.into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: title.into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    fn changes_since(
        doc: &automerge::AutoCommit,
        before: &[automerge::ChangeHash],
    ) -> Vec<Vec<u8>> {
        doc.clone()
            .get_changes(before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// The member's strip holding one window titled `title`, as the changes
    /// the member's device writes into the workspace's layout, built on what
    /// `peer` admitted.
    fn member_windows(
        peer: &crate::StatePeer,
        member: &tilo_user_identity::DeviceCert,
        title: &str,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
        let mut doc = peer
            .admitted_workspace()
            .ok_or("workspace missing")?
            .clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
        let workspace = WorkspaceId::parse(peer.workspace_id()).ok_or("workspace id")?;
        let mut desktop = Workspace::in_workspace(
            WorkspaceSlot::of_workspace(workspace),
            workspace,
            "Workspace",
        );
        let mut strip = Strip::new(StripId(compose_id(MEMBER, 1)), "Mine");
        strip.window_groups.push(window(MEMBER, 2, title));
        desktop.strips.push(strip);
        let person = hex::encode(member.user_public_key);
        let mine = |id: u64| tilo_desktop_model::peer_salt_of(id) == MEMBER;
        device::write(
            &mut doc,
            workspace,
            &desktop,
            &device::Writer {
                person: Some(&person),
                mine: &mine,
                retained: &|_| false,
                carried: tilo_desktop_model::shared_record::device::Writer::NOTHING_CARRIED,
                records: tilo_desktop_model::shared_record::device::Writer::NOTHING_NOTED,
            },
        )?;
        doc.commit();
        Ok(changes_since(&doc, &before))
    }

    fn windows(peer: &crate::StatePeer) -> usize {
        peer.admitted_workspace()
            .and_then(|doc| shared_record::read(doc, None))
            .map_or(0, |desktop| {
                desktop
                    .tree
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.strips)
                    .flat_map(|s| &s.window_groups)
                    .map(|g| g.tabs.len())
                    .sum()
            })
    }

    /// The governor makes a workspace personal: the windows a member's device
    /// put into the workspace document before go.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_personal_workspace_drops_its_members_windows() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert_eq!(
            peer.governance().workspace_layout(),
            WorkspaceLayout::Shared
        );
        // While the workspace shares its layout, the member's window is admitted.
        let workspace = peer.workspace_id().to_owned();
        let changes = member_windows(&peer, &caller, "~/private")?;
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp {
                changes,
                room: workspace,
            },
            now,
        )?;
        assert_eq!(windows(&peer), 1);

        assert!(peer.governance_mut().author_workspace_layout(
            WorkspaceLayout::Personal,
            &governor,
            now
        )?);
        assert!(
            !peer.governance_mut().author_workspace_layout(
                WorkspaceLayout::Personal,
                &governor,
                now
            )?,
            "choosing it again changes nothing"
        );
        assert_eq!(
            peer.governance().workspace_layout(),
            WorkspaceLayout::Personal
        );
        assert!(peer.governance_mut().drop_member_windows(&governor, now)?);
        assert_eq!(windows(&peer), 0);
        assert!(!peer.governance_mut().drop_member_windows(&governor, now)?);
        Ok(())
    }

    /// In a personal workspace a member's device may still take its own windows
    /// out of the workspace, as it does once it reads the workspace is personal.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_member_takes_its_own_windows_out_of_a_personal_workspace() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        let workspace = peer.workspace_id().to_owned();
        let changes = member_windows(&peer, &caller, "Groceries")?;
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp {
                changes,
                room: workspace.clone(),
            },
            now,
        )?;
        assert!(peer.governance_mut().author_workspace_layout(
            WorkspaceLayout::Personal,
            &governor,
            now
        )?);
        let mut doc = peer
            .admitted_workspace()
            .ok_or("workspace missing")?
            .clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
        assert!(device::withdraw(&mut doc, &|id| {
            tilo_desktop_model::ids::peer_salt_of(id) == MEMBER
        })?);
        doc.commit();
        let changes = changes_since(&doc, &before);
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp {
                changes,
                room: workspace,
            },
            now,
        )?;
        assert_eq!(windows(&peer), 0);
        Ok(())
    }

    /// Any device of the owner sets the layout scope.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn another_device_of_the_owner_changes_the_workspaces_layout_scope() -> TestResult {
        use automerge::transaction::Transactable as _;
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(peer.governance_mut().author_workspace_layout(
            WorkspaceLayout::Personal,
            &governor,
            now
        )?);
        let mut doc = peer
            .admitted_workspace()
            .ok_or("workspace missing")?
            .clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
        doc.put(
            automerge::ROOT,
            tilo_desktop_model::projection::WORKSPACE_LAYOUT_KEY,
            "shared",
        )?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let workspace = peer.workspace_id().to_owned();
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp {
                changes,
                room: workspace,
            },
            now,
        )?;
        assert_eq!(
            peer.governance().workspace_layout(),
            WorkspaceLayout::Shared
        );
        Ok(())
    }
}

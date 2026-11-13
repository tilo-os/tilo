//! A causal workspace and a host for the activity pane's own tests.
//! [`TestTree`] pairs a desktop with the workspace's layout record and the workspace's own
//! document, and writes through the same records a session does, so the pane reads real Automerge
//! history.
//! [`TestHost`] answers for a device with a fixed local user and keeps its private read state in
//! memory.
use super::{ActivityHost, ActivityTree, WorkspaceDeparture, WorkspaceSeenFrontier};
use automerge::{AutoCommit, ChangeHash};
use collab_types::caps::CapOverride;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::{
    AppKind, Collaborator, DesktopTree, Strip, StripId, Tab, TileContent, UserId, WindowGroup,
    WindowWidth, Workspace, WorkspaceEffectReceipt, WorkspaceId, WorkspaceSlot,
};

/// The local user every [`TestHost`] answers for.
pub(crate) const LOCAL_USER: &str = "local";

/// A verb named after `label` (`Rewrite opening` → `rewrite-opening` ), as a test's entry records
/// it.
pub(crate) fn verb(label: &str) -> tilo_actions::Described {
    tilo_actions::Described::new(tilo_actions::Did::verb(
        tilo_actions::ActionKey::new(
            "org.tilo.Writer",
            "document",
            tilo_actions::entry::kebab(label),
        ),
        [],
    ))
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh id for a workspace, strip, group or tab in a fixture.
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn empty_workspace(slot: WorkspaceSlot, name: &str, strips: Vec<Strip>) -> Workspace {
    let mut workspace = Workspace::in_workspace(slot, WorkspaceId::new(), name);
    workspace.strips = strips;
    workspace
}

#[derive(Clone, Default)]
pub(crate) struct TestTree {
    pub desktop: DesktopTree,
    /// The layout of the workspace in front, as its record holds it.
    pub doc: AutoCommit,
    /// Each workspace's own document: what it governs, and never a device's copy of it.
    pub governance: HashMap<WorkspaceSlot, AutoCommit>,
}

impl std::ops::Deref for TestTree {
    type Target = DesktopTree;
    fn deref(&self) -> &DesktopTree {
        &self.desktop
    }
}

impl std::ops::DerefMut for TestTree {
    fn deref_mut(&mut self) -> &mut DesktopTree {
        &mut self.desktop
    }
}

impl TestTree {
    /// One empty Home workspace.
    pub fn bootstrap_default() -> Self {
        let slot = WorkspaceSlot(next_id());
        let strip_id = StripId(next_id());
        let home = empty_workspace(slot, "Home", vec![Strip::new(strip_id, "Main")]);
        let mut tree = Self {
            desktop: DesktopTree {
                workspaces: vec![home],
                active: slot,
                active_strip: HashMap::from([(slot, strip_id)]),
                ..DesktopTree::default()
            },
            ..Self::default()
        };
        tree.seed_doc_from_cache();
        tree
    }

    /// Home with four apps open, plus empty Research and Design workspaces.
    pub fn bootstrap_demo() -> Self {
        let slot = WorkspaceSlot(next_id());
        let strip_id = StripId(next_id());
        let group_id = next_id();
        let tab = |title: &str, kind: AppKind, label: &str| Tab {
            id: next_id(),
            title: title.into(),
            tile: TileContent::AppPlaceholder {
                kind,
                label: label.into(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tabs = vec![
            tab("Notes", AppKind::Other("Notes".into()), "Notes"),
            tab("Files", AppKind::FileExplorer, "Files"),
            tab("Terminal", AppKind::Terminal, "Terminal"),
            tab("Document", AppKind::Other("Word".into()), "Document"),
        ];
        let active_tab = tabs[0].id;
        let mut strip = Strip::new(strip_id, "Main");
        strip.window_groups.push(WindowGroup {
            id: group_id,
            tabs,
            active_tab,
            width: WindowWidth::Wide,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut home = empty_workspace(slot, "Home", vec![strip]);
        home.collaborators = vec![
            Collaborator::new("alice", "Alice")
                .with_cursor_color("#ec4899")
                .with_role("editor"),
        ];
        let mut tree = Self {
            desktop: DesktopTree {
                workspaces: vec![home],
                active: slot,
                active_strip: HashMap::from([(slot, strip_id)]),
                active_group: HashMap::from([(strip_id, group_id)]),
                ..DesktopTree::default()
            },
            ..Self::default()
        };
        let research = tree.append_empty_workspace("Research");
        if let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.slot == research)
        {
            workspace.collaborators.push(
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            );
            workspace.collaborators.push(
                Collaborator::new("bob", "Bob")
                    .with_cursor_color("#f97316")
                    .with_role("viewer"),
            );
        }
        tree.append_empty_workspace("Design");
        tree.seed_doc_from_cache();
        tree
    }

    pub fn commit_workspace_change(&mut self) {
        let _ = tilo_resource::history::commit_stamped(&mut self.doc);
    }

    /// Write the desk into the workspace's layout record, as a session's pass does, and say whether
    /// it changed anything.
    pub fn seed_doc_from_cache(&mut self) -> bool {
        let layout = tilo_desktop_model::ownership::SharedDesktop {
            tree: self.desktop.clone(),
            ..Default::default()
        };
        let wrote =
            tilo_desktop_model::shared_record::write(&mut self.doc, &layout).unwrap_or(false);
        if wrote {
            self.commit_workspace_change();
        }
        wrote
    }

    /// Write what `slot` 's workspace governs into its own document, as one change: its name, its
    /// owner, and the grants it makes. Nothing that did not change is written.
    pub fn govern(&mut self, slot: WorkspaceSlot) {
        if self.write_governance(slot) {
            self.commit_governance_change(slot);
        }
    }

    /// [`Self::govern`] without the commit: what a test stamps at a time of its own.
    pub fn write_governance(&mut self, slot: WorkspaceSlot) -> bool {
        use automerge::ReadDoc as _;
        use automerge::transaction::Transactable as _;
        use tilo_desktop_model::projection::{
            WORKSPACE_ID_KEY, WORKSPACE_NAME_KEY, WORKSPACE_OWNER_KEY, project_workspace_sandbox,
            workspace_sandbox_of,
        };
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.slot == slot)
            .cloned()
        else {
            return false;
        };
        let doc = self.governance.entry(slot).or_default();
        let text = |doc: &AutoCommit, key: &str| {
            doc.get(automerge::ROOT, key)
                .ok()
                .flatten()
                .and_then(|(value, _)| value.as_str().map(str::to_owned))
        };
        let mut wrote = false;
        let room = workspace.id().to_string();
        if text(doc, WORKSPACE_ID_KEY).as_deref() != Some(room.as_str()) {
            wrote = doc.put(automerge::ROOT, WORKSPACE_ID_KEY, room).is_ok();
        }
        if text(doc, WORKSPACE_NAME_KEY).as_deref() != Some(workspace.name.as_str()) {
            wrote = doc
                .put(automerge::ROOT, WORKSPACE_NAME_KEY, workspace.name.clone())
                .is_ok();
        }
        if let Some(owner) = workspace.owner()
            && text(doc, WORKSPACE_OWNER_KEY).as_deref() != Some(owner.as_str())
        {
            wrote = doc
                .put(automerge::ROOT, WORKSPACE_OWNER_KEY, owner.clone())
                .is_ok();
        }
        if workspace_sandbox_of(doc) != workspace.sandbox {
            wrote = project_workspace_sandbox(doc, &workspace.sandbox).is_ok();
        }
        wrote
    }

    pub fn commit_governance_change(&mut self, slot: WorkspaceSlot) {
        let doc = self.governance.entry(slot).or_default();
        let _ = tilo_resource::history::commit_stamped(doc);
    }

    /// `slot` 's own document, for a test that reads or stamps it itself.
    pub fn governance_of(&mut self, slot: WorkspaceSlot) -> &mut AutoCommit {
        self.governance.entry(slot).or_default()
    }

    /// Show the desk the layout record holds, as the pane's next read of it does.
    pub fn resync_cache_from_doc(&mut self) {
        let Some(layout) = tilo_desktop_model::shared_record::read(&self.doc, None) else {
            return;
        };
        for workspace in &mut self.desktop.workspaces {
            if let Some(found) = layout
                .tree
                .workspaces
                .iter()
                .find(|found| found.id() == workspace.id())
            {
                workspace.strips.clone_from(&found.strips);
            }
        }
    }

    /// Show `id`, which the device shows alone: no record holds which workspace is in front.
    pub fn set_active(&mut self, id: WorkspaceSlot) {
        if self.desktop.workspaces.iter().any(|w| w.slot == id) {
            self.desktop.active = id;
        }
    }

    /// A workspace on the desk. Which workspaces a desk shows is the workspace list's, which no
    /// record of the layout holds.
    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceSlot {
        let slot = WorkspaceSlot(next_id());
        let strip_id = StripId(next_id());
        let workspace = empty_workspace(slot, name, vec![Strip::new(strip_id, "Main")]);
        self.desktop.active_strip.insert(slot, strip_id);
        self.desktop.workspaces.push(workspace);
        slot
    }

    /// A window opened into the first group of the workspace in front, as a launch into it is: the
    /// desk shows it and the layout record holds it.
    pub fn doc_append_tab_to_first_group_of_active_workspace(
        &mut self,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.slot == self.desktop.active)
        else {
            return false;
        };
        let Some(group) = workspace
            .strips
            .iter_mut()
            .find_map(|strip| strip.window_groups.first_mut())
        else {
            return false;
        };
        let tile = serde_json::from_slice(&tile_blob).unwrap_or(TileContent::Stub {
            kind: AppKind::Other(String::new()),
            label: title.clone(),
        });
        group.tabs.push(Tab {
            id: tab_id,
            title,
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        });
        if group.active_tab == 0 {
            group.active_tab = tab_id;
        }
        self.seed_doc_from_cache()
    }

    /// Let an app reach a sandboxed resource, as `grantor` , who must be allowed to grant in the
    /// workspace.
    pub fn grant_in_workspace(
        &mut self,
        slot: WorkspaceSlot,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        let granted = self.grant_uncommitted(slot, grantor, cap)?;
        self.govern(slot);
        Some(granted)
    }

    /// [`Self::grant_in_workspace`] written into the workspace's own document and not committed:
    /// what a test stamps at a time of its own.
    pub fn grant_uncommitted(
        &mut self,
        slot: WorkspaceSlot,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        let granted = {
            let workspace = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.slot == slot)?;
            if !workspace.may_govern_at(
                grantor,
                tilo_action_policy::roles::governance::GRANT,
                tilo_clock::unix_now(),
            ) {
                return None;
            }
            workspace.grant_sandbox(
                cap.granted_by(collab_types::caps::Principal::Person(grantor.clone())),
            )
        };
        self.write_governance(slot);
        Some(granted)
    }

    pub fn revoke_in_workspace(
        &mut self,
        slot: WorkspaceSlot,
        revoker: &UserId,
        cap_id: &str,
    ) -> bool {
        let revoked = {
            let Some(workspace) = self.desktop.workspaces.iter_mut().find(|w| w.slot == slot)
            else {
                return false;
            };
            workspace.may_govern_at(
                revoker,
                tilo_action_policy::roles::governance::REVOKE,
                tilo_clock::unix_now(),
            ) && workspace.revoke_sandbox(cap_id)
        };
        if !revoked {
            return false;
        }
        self.govern(slot);
        true
    }

    pub fn adopt_ownerless_workspaces(&mut self, user: &UserId) -> Vec<WorkspaceSlot> {
        let mut adopted = Vec::new();
        for workspace in &mut self.desktop.workspaces {
            if workspace.owner().is_none() {
                workspace.set_owner(user.clone());
                adopted.push(workspace.slot);
            }
        }
        for slot in &adopted {
            self.govern(*slot);
        }
        adopted
    }

    pub fn append_workspace_effect_receipt(
        &mut self,
        slot: WorkspaceSlot,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.slot == slot)
        else {
            return false;
        };
        if workspace
            .effect_receipts
            .iter()
            .any(|receipt| receipt.invocation == invocation)
        {
            return false;
        }
        context
            .facets
            .push(tilo_actions::ActionFacet::new("tilo.receipt", "effect"));
        context.facets.push(tilo_actions::ActionFacet::new(
            "tilo.invocation",
            invocation.0.to_string(),
        ));
        let receipt = WorkspaceEffectReceipt {
            invocation,
            actor: actor.to_string(),
            context,
            status,
            completed_at: tilo_clock::unix_now_i64(),
        };
        workspace.effect_receipts.push(receipt);
        true
    }

    pub fn export_changes_since(&mut self, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        self.doc
            .get_changes(since)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError> {
        let parsed: Result<Vec<automerge::Change>, _> = changes
            .iter()
            .map(|bytes| automerge::Change::try_from(bytes.as_slice()))
            .collect();
        self.doc.apply_incoming(parsed?)?;
        self.resync_cache_from_doc();
        Ok(())
    }
}

impl ActivityTree for TestTree {
    fn document(&self) -> &AutoCommit {
        &self.doc
    }
    fn document_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }
    fn commit_activity_change(&mut self) {
        self.commit_workspace_change();
    }
    fn governance_document(&self) -> Option<&AutoCommit> {
        self.governance.get(&self.desktop.active)
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        self.resync_cache_from_doc();
        Ok(())
    }
}

/// A device that may mutate the workspace, answers for [`LOCAL_USER`] and keeps its private read
/// state in memory.
#[derive(Default)]
pub(crate) struct TestHost {
    frontiers: RefCell<HashMap<u64, WorkspaceSeenFrontier>>,
    departures: RefCell<HashMap<u64, WorkspaceDeparture>>,
}

impl ActivityHost for TestHost {
    fn morning_sources_available(&self) -> bool {
        true
    }
    fn local_user(&self) -> String {
        LOCAL_USER.to_string()
    }
    fn local_author(&self, _authors: &mut ui::AuthorDirectory, _actor: &str) -> Option<String> {
        Some(self.local_user())
    }
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        self.frontiers.borrow().clone()
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        self.frontiers.borrow_mut().clone_from(frontiers);
    }
    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        self.departures.borrow().clone()
    }
    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        self.departures.borrow_mut().clone_from(departures);
    }
    fn mutations_available(&self) -> bool {
        true
    }
    fn all_workspaces_available(&self) -> bool {
        true
    }
}

/// The app the fixture records belong to, as a host admits them.
pub(crate) const RECORD_APP: &str = "so.tilo.tasks";

/// Every record the active workspace holds in `records` , as a host admits it to the pane: the
/// checkout its index names and the document as its app last wrote it.
pub(crate) fn detached_records(
    tree: &impl ActivityTree,
    records: &ui::RecordStore,
) -> Vec<super::DetachedRecordHistory> {
    let held = tree
        .active_workspace()
        .and_then(|workspace| tree.workspace_records_held.get(&workspace.id()))
        .cloned()
        .unwrap_or_default();
    held.into_iter()
        .filter_map(|(id, kind)| {
            let id = ui::RecordId::parse(&id)?;
            let checkout = ui::RecordCheckout::new(id.clone(), ui::RecordKind::new(kind));
            let document = records.read(&id, automerge::AutoCommit::clone);
            Some(super::DetachedRecordHistory {
                app: RECORD_APP.to_owned(),
                checkout,
                document,
            })
        })
        .collect()
}

/// The tree and the store's records, read the way a host serves them.
pub(crate) trait StoreRecords {
    fn refresh_records(&mut self, tree: &mut impl ActivityTree, records: &ui::RecordStore);
}

impl StoreRecords for super::WorkspaceActivityPane {
    fn refresh_records(&mut self, tree: &mut impl ActivityTree, records: &ui::RecordStore) {
        let detached = detached_records(tree, records);
        self.refresh_detached_records(tree, detached);
    }
}

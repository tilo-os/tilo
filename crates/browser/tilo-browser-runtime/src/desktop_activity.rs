//! Admitted history capture and a viewer-local modal over the retained desktop.
use crate::{desktop::Requirement, desktop_host::OwnedRecord};
use automerge::transaction::Transactable;
use std::{
    collections::{BTreeMap, HashMap},
    rc::Rc,
};
use taffy::{NodeId, TaffyTree};
use tilo_desktop::activity::*;
use tilo_desktop_model::WorkspaceId;
use ui::{
    portable_storage::{PortableStorage, StorageKey},
    widget::Widget,
};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

/// The admitted records' histories; the desktop's windows write them for
/// `author`.
pub(crate) fn capture_records(
    required: &[Requirement],
    owners: &BTreeMap<(String, String), OwnedRecord>,
    author: &str,
) -> Vec<DetachedRecordHistory> {
    required
        .iter()
        .filter_map(|requirement| {
            let definition = crate::desktop::definition(&requirement.app_id)?;
            let checkout = if requirement.record_id.is_empty() {
                ui::record::RecordCheckout::package_primary(definition.record_kind.clone())
            } else {
                ui::record::RecordCheckout::new(
                    ui::record::RecordId::parse(&requirement.record_id)?,
                    definition.record_kind.clone(),
                )
            };
            let document = owners
                .get(&(requirement.app_id.clone(), checkout.id.as_str().into()))
                .and_then(|owner| {
                    owner
                        .store
                        .read(&checkout.id, |doc| {
                            crate::page_authors::note_mount(Some(doc.get_actor()), author, None);
                            (doc.pending_ops() == 0).then(|| doc.clone())
                        })
                        .flatten()
                });
            Some(DetachedRecordHistory {
                app: requirement.app_id.clone(),
                checkout,
                document,
            })
        })
        .collect()
}
struct BrowserActivityHost {
    storage: PortableStorage,
    key: StorageKey,
}
impl ActivityHost for BrowserActivityHost {
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        self.storage
            .get(&self.key)
            .and_then(|saved| serde_json::from_slice(&saved.bytes).ok())
            .unwrap_or_default()
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        if let Ok(bytes) = serde_json::to_vec(frontiers) {
            let _ = self.storage.put(self.key.clone(), bytes, None);
        }
    }
    fn app_display_name(&self, app: &str) -> Option<String> {
        crate::desktop::definition(app).map(|d| d.title.into())
    }
}

/// Why a modal left presentation. External toolbar focus restoration may only
/// return to its opener after Dismissed, never after resource navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ActivityCloseReason {
    Dismissed,
    WorkspaceChanged,
    ResourceOpened,
    AccessChanged,
    Disposed,
}

pub(crate) struct BrowserActivity {
    pub pane: RoomActivityPane,
    pub snapshot: ActivitySnapshot,
    pub workspace: WorkspaceId,
    pub close_requested: bool,
    pub open_target: Option<ui::HistoryResourceTarget>,
    cursor: Option<Point>,
    dirty: bool,
    focused: bool,
    rect: Rect,
}
impl BrowserActivity {
    pub fn new(snapshot: ActivitySnapshot, view: ActivityView, storage: PortableStorage) -> Self {
        let workspace = snapshot.active;
        let key = StorageKey::View {
            workspace: format!("activity:{}", snapshot.active_workspace().unwrap().room),
            instance: 0,
        };
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(BrowserActivityHost { storage, key }));
        pane.load_frontiers();
        pane.begin_view(workspace.0);
        pane.set_view(view);
        Self {
            pane,
            snapshot,
            workspace,
            close_requested: false,
            open_target: None,
            cursor: None,
            dirty: true,
            focused: false,
            rect: Rect::ZERO,
        }
    }
    pub fn refresh(&mut self, records: Vec<DetachedRecordHistory>, excluded: &[String]) {
        let agents = self
            .snapshot
            .active_workspace()
            .into_iter()
            .flat_map(|workspace| &workspace.capabilities)
            .filter_map(|capability| match &capability.subject {
                collab_types::caps::Subject::Principal(collab_types::caps::Principal::Agent(
                    name,
                )) => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        crate::page_authors::register(self.pane.authors_mut(), &agents);
        self.pane.refresh(&mut self.snapshot);
        self.pane
            .refresh_detached_records(&mut self.snapshot, records);
        self.pane.set_excluded_apps(excluded.to_vec());
    }
    pub fn finish(&mut self) {
        self.pane.finish_view(self.workspace.0);
    }
}
impl Widget for BrowserActivity {
    fn name(&self) -> &str {
        "WorkspaceActivity"
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn modal_capture(&self) -> Option<Rect> {
        Some(self.rect)
    }
    fn traps_focus(&self) -> bool {
        true
    }
    fn wants_tab_input(&self) -> bool {
        true
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.pane.accessibility_snapshot())
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.pane.accessibility_rect_for_path(path)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let changed = self.pane.focus_accessibility_path(path);
        self.dirty |= changed;
        changed
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn needs_animation_frame(&self) -> bool {
        self.pane.replay_is_playing()
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree
            .new_leaf(taffy::Style {
                position: taffy::Position::Absolute,
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).unwrap();
        self.rect = Rect::from_origin_size(
            origin,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        self.pane.layout(self.rect);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.pane.observe_replay_presence(presence);
        self.pane.draw(scene, transform, text, theme, presence);
        self.dirty = false;
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        use winit::{
            event::{ElementState, WindowEvent},
            keyboard::{Key, NamedKey},
        };
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && event.logical_key == Key::Named(NamedKey::Escape))
        {
            self.finish();
            self.close_requested = true;
        } else {
            let outcome = self.pane.handle_event(
                event,
                ctx.scale_factor,
                ctx.modifiers,
                self.cursor,
                &mut self.snapshot,
            );
            self.open_target = outcome.open_target;
        }
        self.dirty = true;
        *ctx.handled = true;
    }
}

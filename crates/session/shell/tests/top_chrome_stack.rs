//! The stack of chrome above a window on the laptop panel (2944×1840 at
//! scale 2): the top bar, then the window's tab row, then the window. No
//! margin sits between the bar and the tab row: the bar keeps `s1` of air
//! under its controls and the tab row `TAB_PILL_INSET_Y` over its pills, so
//! a work-area margin on top of both was an empty band between them. A
//! maximized window abuts the bar and the bottom edge, also when the
//! maximize reaches a live desktop without a rebuild.

use automerge::{AutoCommit, ROOT};
use shell::compositor::backend::headless::parse_output_spec;
use shell::compositor::backend::{Output, combined_geometry, output_geometry};
use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{
    AppKind, GroupId, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree, compose_id,
};
use shell::shell::topbar_chrome::TOPBAR_HEIGHT_PX;
use ui::widget::WindowGroupBounds;
use ui::widget::tabs::{TAB_BAR_HEIGHT, TAB_PILL_INSET_Y};
use vello::kurbo::Rect;

const SALT: u16 = 1;
const STRIP: StripId = StripId(compose_id(SALT, 77));
const GROUP: GroupId = compose_id(SALT, 1001);
const ROOM: &str = "00000000-0000-4000-8000-0000000000b5";
/// The panel's physical pixels per logical one.
const SCALE: f64 = 2.0;
/// What the windows keep over the bottom edge.
const BOTTOM_MARGIN: f64 = 12.0;

fn tree() -> WorkspaceTree {
    let ws_id = WorkspaceId(7);
    let mut ws = Workspace::new(ws_id, "Desk");
    ws.set_room(RoomId::parse(ROOM).expect("room id"));
    let tab = GROUP.saturating_add(5000);
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: GROUP,
            tabs: vec![Tab {
                id: tab,
                title: "Editor".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: "Editor".into(),
                },
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(SALT);
    tree.desktop.workspaces = vec![ws];
    tree.desktop.active = ws_id;
    tree.desktop.active_strip.insert(ws_id, STRIP);
    tree.desktop.active_group.insert(STRIP, GROUP);
    tree
}

struct Panel {
    fb: FrameBuilder,
    output: Output,
}

impl Panel {
    fn new() -> Self {
        let outputs = parse_output_spec("2944x1840@2").expect("the panel spec parses");
        let mut fb = FrameBuilder::with_tree(tree(), combined_geometry(&outputs));
        fb.set_output_geometry(output_geometry(&outputs));
        let mut panel = Self {
            fb,
            output: outputs.into_iter().next().expect("one output"),
        };
        panel.settle();
        panel
    }

    /// Frames the way the compositor runs them: pump, lay out, paint.
    fn settle(&mut self) {
        for _ in 0..4 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let _ = self.fb.paint();
        }
    }

    fn top_bar(&self) -> Rect {
        let bars = self.fb.host().top_bar_rects();
        bars.iter().find(|(_, _, full)| *full).map_or_else(
            || panic!("the panel has a full top bar: {bars:?}"),
            |(_, rect, _)| *rect,
        )
    }

    /// What the tab row's maximize control does: the group flips, and the
    /// following frames write it back to the tree.
    fn toggle_maximized(&mut self) {
        self.fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .expect("the strip has a row")
            .groups
            .iter_mut()
            .find(|g| g.id == GROUP)
            .expect("the group is laid out")
            .toggle_maximized();
    }

    fn group(&mut self) -> (WindowGroupBounds, Vec<Rect>) {
        let row = self
            .fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .expect("the strip has a row");
        let group = row
            .groups
            .iter()
            .find(|g| g.id == GROUP)
            .expect("the group is laid out");
        (group.presentation_bounds(), group.tab_rects().to_vec())
    }
}

#[test]
fn the_tab_row_sits_directly_under_the_top_bar_on_the_scale_2_panel() {
    let mut panel = Panel::new();
    let screen = panel.output.logical_rect();
    assert_eq!(
        (screen.width(), screen.height()),
        (1472.0, 920.0),
        "2944×1840 at scale 2"
    );
    let bar = panel.top_bar();
    let (bounds, pills) = panel.group();
    drop(panel);
    assert!(
        (bar.y0 - screen.y0).abs() < 0.5
            && (bar.height() - f64::from(TOPBAR_HEIGHT_PX)).abs() < 0.5,
        "the bar runs along the top edge: {bar:?}"
    );
    assert!(
        (bounds.tab_bar.y0 - bar.y1).abs() < 0.5,
        "the tab row starts where the bar ends, no margin between: bar {bar:?}, tab row {:?}",
        bounds.tab_bar
    );
    let pill = pills.first().copied().expect("the tab has a pill");
    assert!(
        (pill.y0 - bar.y1 - TAB_PILL_INSET_Y).abs() < 0.5,
        "only the tab row's own inset lies between the bar and its pills: {pill:?}"
    );
    let client = bounds.client.expect("the window's surface is laid out");
    let chrome = (client.y0 - screen.y0) * SCALE;
    let expected = (f64::from(TOPBAR_HEIGHT_PX) + TAB_BAR_HEIGHT) * SCALE;
    assert!(
        (chrome - expected).abs() < 1.0,
        "{chrome} physical px of chrome above the window, the bar and the tab row are {expected}"
    );
    assert!(
        (screen.y1 - client.y1 - BOTTOM_MARGIN).abs() < 0.5,
        "the window keeps its margin over the bottom edge: {client:?}"
    );
}

#[test]
fn a_live_maximize_abuts_the_bar_and_the_bottom_edge() {
    let mut panel = Panel::new();
    let screen = panel.output.logical_rect();
    panel.toggle_maximized();
    panel.settle();
    assert!(
        panel.fb.chrome().tree().desktop.workspaces[0].strips[0].window_groups[0].maximized,
        "the maximize reached the tree"
    );
    let bar = panel.top_bar();
    let (bounds, _) = panel.group();
    assert!(
        (bounds.group.y0 - bar.y1).abs() < 0.5 && (screen.y1 - bounds.group.y1).abs() < 0.5,
        "a maximize on the live desktop keeps no margins: bar {bar:?}, window {:?}",
        bounds.group
    );
    panel.toggle_maximized();
    panel.settle();
    let (bounds, _) = panel.group();
    drop(panel);
    assert!(
        (screen.y1 - bounds.group.y1 - BOTTOM_MARGIN).abs() < 0.5,
        "restoring brings the bottom margin back: {:?}",
        bounds.group
    );
}

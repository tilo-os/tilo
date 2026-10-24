//! Nothing that comes and goes covers a window's bar.

use automerge::{AutoCommit, ROOT};
use vello::kurbo::Point;

use crate::shell::frame::FrameBuilder;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, Collaborator, DesktopTree, GroupId, Strip, StripId, Tab, TileContent,
    WindowGroup as StateGroup, WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
};

/// One window, maximized so its bar runs under the corner where the
/// transients' zone is, and Ada in the workspace to follow.
fn tree() -> WorkspaceTree {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 100;
    let tab_id = 1;
    let mut ws = Workspace::new(ws_id, "Work");
    ws.strips = vec![Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            width: StateWidth::Normal,
            maximized: true,
            tabs: vec![Tab {
                id: tab_id,
                title: "Terminal".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Terminal,
                    label: "Terminal".into(),
                },
            }],
            active_tab: tab_id,
            focus_history: Vec::new(),
        }],
    }];
    let mut ada = Collaborator::new("ada", "Ada");
    ada.active_tab = Some(tab_id);
    ws.collaborators = vec![ada];
    let mut tree = WorkspaceTree {
        desktop: DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            ..Default::default()
        },
        ..Default::default()
    };
    tree.desktop.active_strip.insert(ws_id, strip_id);
    tree.desktop.active_group.insert(strip_id, group_id);
    tree
}

fn settle(fb: &mut FrameBuilder) {
    for _ in 0..4 {
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &ROOT);
        fb.relayout();
        let _ = fb.paint();
    }
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the frame builder lives for the whole scenario, which reads top to bottom"
)]
fn no_transient_overlaps_a_window_bar() {
    let _settled = ui::motion::testing::settled();
    let mut fb = FrameBuilder::with_tree(tree(), (1600, 900));
    settle(&mut fb);
    fb.with_chrome(|chrome| {
        chrome.push_notification_full(tilo_desktop::notifications::Notification::new(
            "scribe asks first: Rename",
            "Rename files based on their contents",
        ));
    });
    fb.host_mut().set_followed_peer(Some("ada".into()));
    settle(&mut fb);

    let bars = fb.host().window_bar_rects();
    assert!(!bars.is_empty(), "the window's bar is on screen");
    let banner = fb.host().notification_banners_rect();
    let follow = fb.host().follow_chip_rect;
    assert!(!banner.is_zero_area(), "the banner is up");
    assert!(!follow.is_zero_area(), "the follow bar is up");
    let area = fb.host().overlay_area();
    for (what, transient) in [("the banner", banner), ("the follow bar", follow)] {
        for bar in &bars {
            assert!(
                bar.intersect(transient).area() <= 0.0,
                "{what} {transient:?} covers a window bar {bar:?}"
            );
        }
        assert!(
            transient.center().x > area.center().x,
            "{what} stands on the trailing side: {transient:?}"
        );
    }
    assert!(
        follow.intersect(banner).area() <= 0.0,
        "the banner stands below the follow bar"
    );

    // Hit-test the bar: everywhere on it, the window's bar has the pointer,
    // not a transient.
    for bar in &bars {
        for i in 0..=24 {
            for j in 0..=4 {
                let p = Point::new(
                    bar.x0 + bar.width() * f64::from(i) / 24.0,
                    bar.y0 + bar.height() * f64::from(j) / 4.0,
                );
                fb.host_mut().note_pointer(p);
                assert!(
                    !fb.host().notification_banner_captures_pointer(),
                    "a banner takes the pointer at {p:?} on the window bar {bar:?}"
                );
                assert!(
                    !follow.contains(p),
                    "the follow bar stands at {p:?} on the window bar {bar:?}"
                );
            }
        }
    }
}

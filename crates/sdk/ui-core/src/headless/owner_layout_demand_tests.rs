//! Actual deferred composition demand; no animation or owner wake is supplied.
use super::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Demand {
    width: Rc<Cell<f32>>,
    rect: Rc<Cell<Rect>>,
    builds: Rc<Cell<usize>>,
    refuse: Rc<Cell<bool>>,
}
struct Intrinsic(Demand);
impl Widget for Intrinsic {
    crate::testing::inert_widget_methods!(draw, name = "DeferredIntrinsic");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.0.builds.set(self.0.builds.get() + 1);
        let node = tree.leaf_node(taffy::Style {
            size: Size {
                width: Dimension::length(self.0.width.get()),
                height: Dimension::length(20.0),
            },
            ..Default::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).unwrap();
        self.0.rect.set(Rect::new(
            origin.x,
            origin.y,
            origin.x + f64::from(layout.size.width),
            origin.y + f64::from(layout.size.height),
        ));
    }
    fn get_rect(&self) -> Rect {
        self.0.rect.get()
    }
    fn needs_redraw(&self) -> bool {
        false
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.0.refuse.get()
    }
}
fn fixture() -> (FrameHarness, Demand) {
    let demand = Demand::default();
    demand.width.set(30.0);
    let frame = FrameHarness::new(
        State::new(),
        Box::new(Intrinsic(demand.clone())),
        (200, 100),
    );
    assert!(!frame.frame_schedule().queued_work);
    assert!(!frame.frame_schedule().needs_animation_frame);
    assert!(frame.frame_schedule().next_wake_deadline.is_none());
    (frame, demand)
}

#[test]
fn idle_requests_coalesce_until_one_completed_original_composition() {
    let (mut frame, demand) = fixture();
    let before = frame.relayout_frames();
    let builds = demand.builds.get();
    frame.request_layout();
    frame.request_layout();
    assert_eq!(
        frame.relayout_frames(),
        before,
        "request must not synchronously compose"
    );
    assert!(
        frame.frame_schedule().queued_work,
        "idle request must remain observable"
    );
    assert!(frame.poll_frame().unwrap());
    assert_eq!(frame.relayout_frames(), before + 1);
    assert!(
        demand.builds.get() > builds,
        "original full composition must run"
    );
    assert!(!frame.frame_schedule().queued_work);
    assert!(!frame.poll_frame().unwrap());
}

#[test]
fn same_viewport_request_rebuilds_changed_intrinsic_geometry() {
    let (mut frame, demand) = fixture();
    let before = demand.rect.get().width();
    demand.width.set(75.0);
    frame.request_layout();
    assert!(frame.poll_frame().unwrap());
    assert_eq!(before, 30.0);
    assert_eq!(demand.rect.get().width(), 75.0);
    assert!(!frame.frame_schedule().queued_work);
}

#[test]
fn failed_composition_retains_request_until_actual_success() {
    let (mut frame, demand) = fixture();
    demand.refuse.set(true);
    frame.request_layout();
    assert!(matches!(
        frame.poll_frame(),
        Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
    ));
    assert!(frame.frame_schedule().queued_work);
    demand.refuse.set(false);
    assert!(frame.poll_frame().unwrap());
    assert!(!frame.frame_schedule().queued_work);
}

#[test]
fn hidden_suspended_and_gpu_absent_host_custody_keeps_real_layout_demand_until_composition() {
    use super::owner_background_work::{Presentation, run};
    use super::owner_mount_service_custody::Lease;
    use std::cell::RefCell;
    use std::collections::HashSet;
    for presentation in [
        Presentation {
            hidden: true,
            suspended: false,
            gpu_ready: true,
        },
        Presentation {
            hidden: false,
            suspended: true,
            gpu_ready: true,
        },
        Presentation {
            hidden: false,
            suspended: false,
            gpu_ready: false,
        },
    ] {
        let (frame, demand) = fixture();
        let before = frame.relayout_frames();
        let builds = demand.builds.get();
        let mounts = RefCell::new(HashMap::from([(7, frame)]));
        let servicing = RefCell::new(HashSet::new());
        {
            let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
            let frame = lease.mount().unwrap();
            frame.request_layout();
            let outcome = run(presentation, || Ok::<usize, ()>(0)).unwrap();
            assert!(!outcome.paint_available);
            assert!(frame.frame_schedule().queued_work);
            assert_eq!(frame.relayout_frames(), before);
            assert_eq!(demand.builds.get(), builds);
        }
        let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
        let frame = lease.mount().unwrap();
        assert!(frame.frame_schedule().queued_work);
        assert!(frame.poll_frame().unwrap());
        assert_eq!(frame.relayout_frames(), before + 1);
        assert!(!frame.frame_schedule().queued_work);
        frame.close_components();
    }
}
#[test]
fn refused_host_acquisition_restores_mount_without_consuming_original_composition_demand() {
    use super::owner_mount_service_custody::Lease;
    use std::cell::RefCell;
    use std::collections::HashSet;
    let (frame, demand) = fixture();
    let before = frame.relayout_frames();
    let builds = demand.builds.get();
    let mounts = RefCell::new(HashMap::from([(7, frame)]));
    let servicing = RefCell::new(HashSet::new());
    let refused = {
        let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
        lease.mount().unwrap().request_layout();
        Err::<(), _>("host acquisition refused before composition")
    };
    assert!(refused.is_err());
    assert!(servicing.borrow().is_empty());
    let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
    let frame = lease.mount().unwrap();
    assert!(frame.frame_schedule().queued_work);
    assert_eq!(frame.relayout_frames(), before);
    assert_eq!(demand.builds.get(), builds);
    assert!(frame.poll_frame().unwrap());
    assert!(!frame.frame_schedule().queued_work);
    frame.close_components();
}

use super::{nearby_drop_candidate, spatial_departure_edge};
use crate::shell::device_fabric::DisplayEdge;

#[test]
fn mapped_pointer_return_precedes_discrete_input_and_preserves_new_motion() {
    use crate::compositor::backend::BackendEvent;
    use std::collections::VecDeque;
    use winit::event::{ElementState, MouseButton, WindowEvent};
    let cursor = |point: (f64, f64)| {
        BackendEvent::Input(WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(point.0, point.1),
        })
    };
    let click = || {
        BackendEvent::Input(WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        })
    };
    let position = |event: Option<BackendEvent>| match event {
        Some(BackendEvent::Input(WindowEvent::CursorMoved { position, .. })) => {
            (position.x, position.y)
        }
        _ => panic!("expected a cursor return before discrete input"),
    };
    for parked in [None, Some((1439.0, 450.0))] {
        let mut pending = VecDeque::from([cursor((1439.0, 450.0)), click()]);
        super::queue_spatial_return_event(&mut pending, (10.5, -20.25), parked);
        assert_eq!(position(pending.pop_front()), (10.5, -20.25));
        assert!(matches!(
            pending.pop_front(),
            Some(BackendEvent::Input(WindowEvent::MouseInput { .. }))
        ));
        assert!(pending.is_empty());
    }
    let mut pending = VecDeque::from([cursor((40.0, 80.0)), click()]);
    super::queue_spatial_return_event(&mut pending, (10.5, -20.25), Some((1439.0, 450.0)));
    assert_eq!(position(pending.pop_front()), (10.5, -20.25));
    assert_eq!(position(pending.pop_front()), (40.0, 80.0));
    assert!(matches!(
        pending.pop_front(),
        Some(BackendEvent::Input(WindowEvent::MouseInput { .. }))
    ));
}

#[test]
fn pointer_departs_through_edges_and_arms_nearby_drops() {
    // Phase `pointer_can_depart_through_all_four_edges`.
    assert_eq!(
        spatial_departure_edge(0.0, 400.0, 1_000, 800, -2.0, 0.0),
        Some(DisplayEdge::Left)
    );
    assert_eq!(
        spatial_departure_edge(999.0, 400.0, 1_000, 800, 2.0, 0.0),
        Some(DisplayEdge::Right)
    );
    assert_eq!(
        spatial_departure_edge(500.0, 0.0, 1_000, 800, 0.0, -2.0),
        Some(DisplayEdge::Top)
    );
    assert_eq!(
        spatial_departure_edge(500.0, 799.0, 1_000, 800, 0.0, 2.0),
        Some(DisplayEdge::Bottom)
    );
    // Phase `corner_departure_follows_the_dominant_motion_axis`.
    assert_eq!(
        spatial_departure_edge(999.0, 799.0, 1_000, 800, 8.0, 3.0),
        Some(DisplayEdge::Right)
    );
    assert_eq!(
        spatial_departure_edge(999.0, 799.0, 1_000, 800, 3.0, 8.0),
        Some(DisplayEdge::Bottom)
    );
    assert_eq!(
        spatial_departure_edge(500.0, 400.0, 1_000, 800, 8.0, 8.0),
        None
    );
    // Phase `one_safe_file_or_folder_can_arm_a_nearby_drop`.
    {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert_eq!(
            nearby_drop_candidate(Some(std::slice::from_ref(&path))),
            Ok(Some(path.clone()))
        );
        let folder = temp.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(
            nearby_drop_candidate(Some(std::slice::from_ref(&folder))),
            Ok(Some(folder))
        );
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(nearby_drop_candidate(Some(&[link])).is_err());
        assert!(nearby_drop_candidate(Some(&[path.clone(), path])).is_err());
        assert!(nearby_drop_candidate(Some(&[])).is_err());
    }
}

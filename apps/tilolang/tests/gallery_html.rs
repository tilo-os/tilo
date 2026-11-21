//! The gallery as a page shows it (`compose_frame`): what a page is handed after each
//! pointer move over a page of charts is what composing the whole window gives, though a frame
//! that changed only what some widgets paint composes those widgets alone, and a frame that
//! changed nothing hands nothing over.

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;

#[test]
fn a_page_is_handed_what_composing_it_all_gives() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    harness.set_scene_drawn(false);
    harness.set_coalesce_input_frames(true);
    let _ = harness.poll_frame();
    assert!(
        harness.compose_frame().is_some(),
        "the first frame is handed over"
    );
    let _ = harness.poll_frame();
    assert!(
        harness.compose_frame().is_none(),
        "a frame that changed nothing hands nothing over"
    );
    // The page of charts, picked in the sidebar.
    let (x, y) = words_at(harness.presented_composition(), "Charts and views")
        .expect("the sidebar names the page of charts");
    harness.click_at(x, y);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    // Its charts scrolled into view, as the page's own scroller moves them.
    let scroller = harness
        .presented_composition()
        .and_then(|composition| {
            let mut nodes = vec![&composition.root];
            while let Some(node) = nodes.pop() {
                if node.scroll.is_some() && node.rect[0] > 200.0 {
                    return Some(node.path.clone());
                }
                nodes.extend(&node.children);
            }
            None
        })
        .expect("the page scrolls");
    assert!(
        harness.scroll_composed(&scroller, 860.0),
        "the page takes the scroll"
    );
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    // Each picture big enough to be a chart, pointed at across its middle.
    let mut pictures = Vec::new();
    let mut nodes: Vec<&ui::composition::Composed> = harness
        .presented_composition()
        .map(|composition| vec![&composition.root])
        .unwrap_or_default();
    while let Some(node) = nodes.pop() {
        let [x, y, width, height] = node.rect;
        if matches!(node.own, Some(ui::composition::Own::Painted { .. }))
            && width > 120.0
            && height > 80.0
            && y > 56.0
            && y + height < 800.0
        {
            pictures.push((x, y + height / 2.0, width));
        }
        nodes.extend(&node.children);
    }
    assert!(!pictures.is_empty(), "the page shows charts");
    let points: Vec<(f64, f64)> = pictures
        .iter()
        .flat_map(|&(x, y, width)| (1..6).map(move |at| (x + width * f64::from(at) / 6.0, y)))
        .collect();
    let mut handed = 0_usize;
    for (x, y) in points {
        harness.hover_at(x, y);
        let _ = harness.poll_frame();
        let whole = harness.composition();
        if let Some(composed) = harness.compose_frame() {
            handed = handed.saturating_add(1);
            assert_eq!(
                *composed, whole,
                "the pointer at ({x}, {y}): what is handed over is the whole window's composition"
            );
        } else {
            assert_eq!(
                harness.presented_composition(),
                Some(&whole),
                "the pointer at ({x}, {y}) changed the composition, and nothing was handed over"
            );
        }
    }
    assert!(handed > 0, "pointing at the charts changes what they show");
}

/// The middle of the smallest part of `composition` that says `said`, in its words or its name.
fn words_at(composition: Option<&ui::composition::Composition>, said: &str) -> Option<(f64, f64)> {
    let quoted = format!("\"{said}\"");
    let mut best: Option<[f64; 4]> = None;
    let mut nodes = vec![&composition?.root];
    while let Some(node) = nodes.pop() {
        let own = ui::composition::Composed {
            children: Vec::new(),
            ..node.clone()
        };
        let [_, _, width, height] = node.rect;
        if width > 0.0
            && height > 0.0
            && serde_json::to_string(&own).is_ok_and(|json| json.contains(&quoted))
            && best.is_none_or(|[_, _, w, h]| width * height < w * h)
        {
            best = Some(node.rect);
        }
        nodes.extend(&node.children);
    }
    best.map(|[x, y, width, height]| (x + width / 2.0, y + height / 2.0))
}

/// A page picked in the sidebar shows in the frame drawn for the click's release, as a page draws
/// it before the browser paints again.
#[test]
fn a_page_picked_in_the_sidebar_shows_in_the_frame_of_the_click() {
    use ui::synthetic_input::{cursor_moved, mouse_input};
    use ui::winit::event::{ElementState, MouseButton};
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    harness.set_scene_drawn(false);
    harness.set_coalesce_input_frames(true);
    for _ in 0..8 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    for (row, shows) in [("Controls", "Boxes and switches"), ("Feedback", "Toolbar")] {
        let (x, y) = words_at(harness.presented_composition(), row).expect("the sidebar names it");
        // Each event as the page hands it over, each drawn before the browser paints again.
        for event in [
            cursor_moved(x, y),
            mouse_input(ElementState::Pressed, MouseButton::Left),
            mouse_input(ElementState::Released, MouseButton::Left),
        ] {
            harness.send_host_event(&event);
            let _ = harness.poll_frame();
            let _ = harness.compose_frame();
        }
        assert!(
            words_at(harness.presented_composition(), shows).is_some(),
            "{row}: the frame of the click shows {shows:?}"
        );
    }
}

/// A long page scrolled a step at a time: each frame moves what it shows without laying anything
/// out, and what a page is handed for it is what composing the whole window gives, with the parts
/// that only moved at their new places.
#[test]
fn a_scrolled_page_is_handed_what_composing_it_all_gives() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    harness.set_scene_drawn(false);
    harness.set_coalesce_input_frames(true);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    let (x, y) = words_at(harness.presented_composition(), "Charts and views")
        .expect("the sidebar names the page of charts");
    harness.click_at(x, y);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    let scroller = harness
        .presented_composition()
        .and_then(|composition| {
            let mut nodes = vec![&composition.root];
            while let Some(node) = nodes.pop() {
                if node.scroll.is_some() && node.rect[0] > 200.0 {
                    return Some(node.path.clone());
                }
                nodes.extend(&node.children);
            }
            None
        })
        .expect("the page scrolls");
    for step in 1..=6 {
        let offset = f64::from(step) * 40.0;
        assert!(
            harness.scroll_composed(&scroller, offset),
            "the page takes the scroll to {offset}"
        );
        let _ = harness.poll_frame();
        let whole = harness.composition();
        let composed = harness
            .compose_frame()
            .unwrap_or_else(|| panic!("scrolled to {offset}, and nothing was handed over"));
        assert_eq!(
            *composed, whole,
            "scrolled to {offset}: what is handed over is the whole window's composition"
        );
    }
}

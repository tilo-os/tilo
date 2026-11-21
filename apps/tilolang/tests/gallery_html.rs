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

/// Every page of the sidebar, picked, pointed across and scrolled: each frame a page is handed is
/// what composing the whole window gives, so no part of it stands twice or is left out.
#[test]
fn every_page_is_handed_what_composing_it_all_gives() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    harness.set_scene_drawn(false);
    harness.set_coalesce_input_frames(true);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    // Each frame at an instant of its own, a page's frame later than the last, and the whole
    // window composed at that same instant: what moves with the clock (a bar's sliding
    // segment) is where both put it.
    let started = ui::time::now();
    let mut frame = 0_u32;
    let mut checked = |harness: &mut ui::headless::FrameHarness, what: &str| {
        frame = frame.saturating_add(1);
        let at = started + std::time::Duration::from_millis(u64::from(frame) * 17);
        ui::time::testing::with_monotonic_time(at, || {
            let _ = harness.poll_frame();
            let whole = harness.composition();
            let handed = harness.compose_frame().is_some();
            let presented = harness
                .presented_composition()
                .expect("a composition was handed over");
            if let Some(differs) = first_difference(&presented.root, &whole.root) {
                panic!(
                    "{what}: what the page shows is not the whole window's composition ({}): {differs}",
                    if handed {
                        "handed over"
                    } else {
                        "nothing handed over"
                    }
                );
            }
            assert_eq!(
                *presented, whole,
                "{what}: the page shows the whole window's composition"
            );
        });
    };
    for place in [
        "Basics",
        "Controls",
        "Feedback",
        "Backlog",
        "Release board",
        "Data",
        "Media",
        "Places",
        "Shell parts",
        "Charts and views",
    ] {
        let (x, y) = words_at(harness.presented_composition(), place)
            .unwrap_or_else(|| panic!("the sidebar names {place}"));
        harness.click_at(x, y);
        for _ in 0..3 {
            checked(harness, &format!("{place}: opened"));
        }
        for step in 0..24 {
            let at = f64::from(step);
            harness.hover_at(320.0 + at * 38.0, 90.0 + at * 28.0);
            checked(harness, &format!("{place}: pointed at step {step}"));
        }
        let scroller = harness.presented_composition().and_then(|composition| {
            let mut best: Option<(String, f64)> = None;
            let mut nodes = vec![&composition.root];
            while let Some(node) = nodes.pop() {
                if let Some([_, content, viewport]) = node.scroll
                    && node.rect[0] > 200.0
                    && content > viewport
                    && best
                        .as_ref()
                        .is_none_or(|(_, room)| content - viewport > *room)
                {
                    best = Some((node.path.clone(), content - viewport));
                }
                nodes.extend(&node.children);
            }
            best
        });
        if let Some((scroller, room)) = scroller {
            for step in 1..=5 {
                let offset = (f64::from(step) * 37.5).min(room);
                harness.scroll_composed(&scroller, offset);
                checked(harness, &format!("{place}: scrolled to {offset}"));
                harness.hover_at(700.0, 300.0 + f64::from(step) * 20.0);
                checked(
                    harness,
                    &format!("{place}: pointed at after scrolling to {offset}"),
                );
            }
        }
    }
}

/// A tree's rows pointed at one after another, each frame drawn at rest: the row pointed at
/// paints again and so does the tree around it, while the other rows stand as they were. What a
/// page is handed is still what composing the whole window gives, each row with its disclosure
/// and its notes once.
#[test]
fn a_tree_pointed_across_is_handed_what_composing_it_all_gives() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    harness.set_scene_drawn(false);
    harness.set_coalesce_input_frames(true);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    let (x, y) = words_at(harness.presented_composition(), "Data").expect("the sidebar names Data");
    harness.click_at(x, y);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    let rows: Vec<(f64, f64)> = harness
        .presented_composition()
        .map(|composition| {
            let mut rows = Vec::new();
            let mut nodes = vec![&composition.root];
            while let Some(node) = nodes.pop() {
                let [x, y, width, height] = node.rect;
                if node.widget == "ListRow"
                    && node.path.contains("TreeView")
                    && width > 0.0
                    && height > 0.0
                    && y > 0.0
                    && y + height < 800.0
                {
                    rows.push((x + width / 2.0, y + height / 2.0));
                }
                nodes.extend(&node.children);
            }
            rows
        })
        .unwrap_or_default();
    assert!(rows.len() > 2, "the page shows a tree's rows");
    let started = ui::time::now();
    for (step, &(x, y)) in rows.iter().chain(rows.iter().rev()).enumerate() {
        harness.hover_at(x, y);
        for again in 0..2_u64 {
            let frame = u64::try_from(step)
                .unwrap_or(u64::MAX)
                .saturating_mul(2)
                .saturating_add(again);
            let at = started
                + std::time::Duration::from_millis(frame.saturating_add(1).saturating_mul(17));
            ui::time::testing::with_monotonic_time(at, || {
                let _ = harness.poll_frame();
                let whole = harness.composition();
                let _ = harness.compose_frame();
                let presented = harness
                    .presented_composition()
                    .expect("a composition was handed over");
                if let Some(differs) = first_difference(&presented.root, &whole.root) {
                    panic!("the pointer at the row at ({x}, {y}): {differs}");
                }
            });
        }
    }
}

/// The first part where `shown` and `whole` differ, and how.
fn first_difference(
    shown: &ui::composition::Composed,
    whole: &ui::composition::Composed,
) -> Option<String> {
    if shown.path != whole.path {
        return Some(format!("{} stands where {} does", shown.path, whole.path));
    }
    if shown.children.len() != whole.children.len() {
        return Some(format!(
            "{} holds {} parts, not {}: {:?} against {:?}",
            shown.path,
            shown.children.len(),
            whole.children.len(),
            shown.children.iter().map(|c| &c.path).collect::<Vec<_>>(),
            whole.children.iter().map(|c| &c.path).collect::<Vec<_>>()
        ));
    }
    for (a, b) in shown.children.iter().zip(&whole.children) {
        if let Some(differs) = first_difference(a, b) {
            return Some(differs);
        }
    }
    let own = |node: &ui::composition::Composed| ui::composition::Composed {
        children: Vec::new(),
        ..node.clone()
    };
    (own(shown) != own(whole)).then(|| {
        format!(
            "{} ({}): {:?} against {:?}",
            shown.path,
            shown.widget,
            own(shown),
            own(whole)
        )
    })
}

/// The Data page's conversation, opened and scrolled: each of its lines and day headings stands
/// inside the list, the first at its top, in what composing the window gives and in what the page
/// is handed, whether or not a paint placed them.
#[test]
fn a_conversation_shows_its_lines_where_the_list_stands() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    harness.set_scene_drawn(false);
    harness.set_coalesce_input_frames(true);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    let (x, y) = words_at(harness.presented_composition(), "Data").expect("the sidebar names Data");
    harness.click_at(x, y);
    for _ in 0..4 {
        let _ = harness.poll_frame();
        let _ = harness.compose_frame();
    }
    let scroller = harness
        .presented_composition()
        .and_then(|composition| {
            let mut best: Option<(String, f64)> = None;
            let mut nodes = vec![&composition.root];
            while let Some(node) = nodes.pop() {
                if let Some([_, content, viewport]) = node.scroll
                    && node.rect[0] > 200.0
                    && content > viewport
                    && best
                        .as_ref()
                        .is_none_or(|(_, room)| content - viewport > *room)
                {
                    best = Some((node.path.clone(), content - viewport));
                }
                nodes.extend(&node.children);
            }
            best
        })
        .map(|(path, _)| path)
        .expect("the Data page scrolls");
    for step in 0..=6 {
        let offset = f64::from(step) * 60.0;
        if step > 0 {
            assert!(
                harness.scroll_composed(&scroller, offset),
                "the page takes the scroll to {offset}"
            );
        }
        let _ = harness.poll_frame();
        let whole = harness.composition();
        let _ = harness.compose_frame();
        let presented = harness
            .presented_composition()
            .expect("a composition was handed over")
            .clone();
        for (what, composition) in [("composed", &whole), ("handed", &presented)] {
            let mut lists = Vec::new();
            let mut nodes = vec![&composition.root];
            while let Some(node) = nodes.pop() {
                if node.widget == "ConversationList" {
                    lists.push(node);
                }
                nodes.extend(&node.children);
            }
            assert!(!lists.is_empty(), "the Data page shows a conversation");
            for list in lists {
                let [x, y, width, height] = list.rect;
                let rows: Vec<_> = list
                    .children
                    .iter()
                    .filter(|row| row.path.contains("#line:") || row.path.contains("#day:"))
                    .collect();
                assert!(!rows.is_empty(), "{}: the list holds lines", list.path);
                let top = rows
                    .iter()
                    .map(|row| row.rect[1])
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    (top - y).abs() < 0.5,
                    "scrolled to {offset}, {what}: the first of {} stands at {top}, the list at {y}",
                    list.path
                );
                let mut parts: Vec<_> = rows.clone();
                while let Some(part) = parts.pop() {
                    let [px, py, pw, ph] = part.rect;
                    assert!(
                        px >= x - 0.5
                            && px + pw <= x + width + 0.5
                            && py >= y - 0.5
                            && py + ph <= y + height + 0.5,
                        "scrolled to {offset}, {what}: {} at {:?} stands outside its list at {:?}",
                        part.path,
                        part.rect,
                        list.rect
                    );
                    parts.extend(&part.children);
                }
            }
        }
        if let Some(differs) = first_difference(&presented.root, &whole.root) {
            panic!("scrolled to {offset}: {differs}");
        }
    }
}

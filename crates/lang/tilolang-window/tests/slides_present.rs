//! The presentation swaps its slide with the theme's cross-fade: the slide a person steps away
//! from stays laid out while it fades out, the one that comes fades in over it, and once the swap
//! has played only the new one stands.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilolang_window::{Opening, Packages, Window};

/// The app's own name, as its package declares it.
const APP: &str = "so.tilo.slides";

/// A wide window, where the stage and its rail stand side by side.
const WINDOW: (u32, u32) = (1280, 800);

/// The app compiled from `apps/tl/slides`, as `tilolang build` compiles it, and the platform's
/// packages it reads.
fn packages() -> Rc<Packages> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/slides");
    let search: Vec<PathBuf> = vec![dir.parent().expect("apps/tl").to_path_buf()];
    let (built, platform) = tilolang_tilo::catalogue::compile(
        &dir,
        &search,
        &mut tilolang_tilo::tilolang::OsEntropy::default(),
    )
    .expect("the app compiles");
    Rc::new(Packages::new(
        Rc::new(built.into_iter().map(|package| package.compiled).collect()),
        platform,
    ))
}

/// The window at `WINDOW`, its motion on: the harness opens every window with reduced motion, and
/// this test is about the fade.
fn open() -> Window {
    tilo_app_window::i18n::ensure();
    let mut window = Window::open(APP, &packages(), Opening::new(WINDOW, ui::Theme::light()))
        .expect("the window opens");
    window
        .harness
        .set_theme(ui::Theme::light().with_reduce_motion(false));
    settle(&mut window);
    window
}

/// Let the window and its instance answer each other and the layouts that follow, as a live
/// window does after an input.
fn settle(window: &mut Window) {
    window.exchange();
    window.harness.relayout();
    window.exchange();
}

/// Press the control `name` where it stands, as a person does.
fn press(window: &mut Window, name: &str) {
    let node = accessible(window)
        .into_iter()
        .find(|node| {
            node.role == ui::a11y::Role::Button
                && node.name.as_deref() == Some(name)
                && node.bounds.is_some()
        })
        .map(|node| node.bounds.expect("the control's bounds"))
        .unwrap_or_else(|| {
            let names: Vec<String> = accessible(window)
                .into_iter()
                .filter_map(|node| node.name)
                .collect();
            panic!("the window offers `{name}`: {names:?}")
        });
    window.harness.click_at(
        f64::midpoint(node.x0, node.x1),
        f64::midpoint(node.y0, node.y1),
    );
    settle(window);
}

/// Every accessible node of the window, as a screen reader reads it.
fn accessible(window: &mut Window) -> Vec<ui::a11y::AccessNode> {
    let snapshot = window.harness.inspection();
    snapshot
        .root
        .nodes()
        .into_iter()
        .filter_map(|node| node.semantics.as_ref())
        .flat_map(ui::a11y::AccessNode::nodes)
        .cloned()
        .collect()
}

/// How many pages the cross-fading stage lays out beside each other, while one is swapping:
/// `Some(2)` mid-swap, `Some(1)` for a swap that has landed but not yet been let go, `None`
/// when the stage stands on one slide.
fn pages_in_the_stage(widget: &dyn ui::widget::Widget) -> Option<usize> {
    if let Some(swapped) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<ui::widget::Swapped>())
    {
        return Some(if swapped.previous().is_some() { 2 } else { 1 });
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = pages_in_the_stage(child);
        }
    });
    found
}

/// A slide stepped to while presenting keeps the slide it left laid out under the one that comes
/// until the cross-fade has played.
#[test]
fn a_slide_change_while_presenting_cross_fades_between_the_two_pages() {
    let mut window = open();
    // Two slides, so the presentation has somewhere to step to.
    press(&mut window, "Add slide");
    press(&mut window, "Present");
    assert!(
        ui::widget::find_test_id(window.harness.root_widget(), "Presentation").is_some(),
        "the presentation's stage is shown"
    );
    assert_eq!(
        pages_in_the_stage(window.harness.root_widget()),
        None,
        "the stage shows one slide and has swapped nothing"
    );

    press(&mut window, "Next");
    assert_eq!(
        pages_in_the_stage(window.harness.root_widget()),
        Some(2),
        "the slide stepped away from fades out under the one that comes"
    );

    // Once the fade has played, the slide that came stands on its own.
    window
        .harness
        .settle_for_capture(ui::time::Duration::from_secs(2));
    settle(&mut window);
    assert_ne!(
        pages_in_the_stage(window.harness.root_widget()),
        Some(2),
        "the swap has landed"
    );
    assert!(
        ui::widget::find_test_id(window.harness.root_widget(), "Presentation").is_some(),
        "the stage stands on the slide that came"
    );
}

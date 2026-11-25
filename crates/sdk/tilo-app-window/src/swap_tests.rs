//! A stage that says its page is swapped whole (`transition: .cross_fade`): the page it showed
//! stays laid out while it fades out and the new one fades in over it, and once the swap has
//! played only the new one stands. Reduced motion lands it at once.

use super::*;
use tilo_render_protocol::patch::{Patch, local};
use ui::time::testing::with_monotonic_time as clock;
use ui::time::{Duration, Instant};
use ui::tokens::MotionSource;
use ui::widget::Swapped;

/// A page of the stage: a stack keyed by its name, holding a line of its own.
fn page(name: &str) -> UiNode {
    let mut page = UiNode::empty("stack", name);
    let mut line = UiNode::empty("ui.Text", &format!("{name}-text"));
    line.props.insert("value".to_owned(), Value::text(name));
    page.children.push(line);
    page
}

/// A window whose view is a stage (`transition` says how it swaps) showing page `shown`.
fn view(shown: &str, transition: Option<&str>) -> Vec<ToUi> {
    let mut stage = UiNode::empty("stack", "stage");
    if let Some(transition) = transition {
        stage
            .props
            .insert("transition".to_owned(), Value::text(transition));
    }
    stage.children.push(page(shown));
    let mut root = UiNode::empty("page", "page");
    root.children.push(stage);
    vec![ToUi::Open {
        level: Level::Root,
        target: ResourceRef {
            id: "doc".to_owned(),
            ty: "slides.Deck".to_owned(),
        },
        tree: root,
        front: None,
        capture_protected: false,
    }]
}

struct Shown {
    app: local::End,
    harness: ui::headless::Driver,
}

impl Shown {
    fn open(shown: &str, transition: Option<&str>, reduced: bool, now: Instant) -> Self {
        let (mut app, end) = local::pair();
        let batch = view(shown, transition);
        app.send(wire::encode_batch(&batch).expect("encodes"))
            .expect("sends");
        let window = Box::new(AppWindow::new(Box::new(end), Vec::new()));
        let harness = clock(now, || {
            let mut harness = ui::headless::Driver::new(ui::State::new(), window, (600, 800));
            harness.set_theme(ui::Theme::dark().with_reduce_motion(reduced));
            ui::widget::find_mut::<AppWindow>(harness.root_widget_mut())
                .expect("the window")
                .receive();
            harness.relayout();
            harness
        });
        Self { app, harness }
    }

    /// The runtime's change at `now`, the person's `source` having just acted.
    fn change(&mut self, now: Instant, source: MotionSource, patches: Vec<Patch>) {
        let batch = [ToUi::Patch {
            level: Level::Root,
            patches,
        }];
        clock(now, || {
            ui::motion::note_source(source);
            self.app
                .send(wire::encode_batch(&batch).expect("encodes"))
                .expect("sends");
            self.frame(now);
        });
    }

    fn frame(&mut self, now: Instant) {
        clock(now, || {
            ui::widget::find_mut::<AppWindow>(self.harness.root_widget_mut())
                .expect("the window")
                .receive();
            self.harness.relayout();
        });
    }

    fn stage(&self) -> &Swapped {
        ui::widget::find_test_id(self.harness.root_widget(), "stage")
            .and_then(|widget| widget.as_any())
            .and_then(|any| any.downcast_ref::<Swapped>())
            .expect("the stage swaps its page")
    }

    fn shows(&self, id: &str) -> bool {
        holds(self.harness.root_widget(), id)
    }
}

/// Whether `widget` or anything under it is the widget `id`.
fn holds(widget: &dyn Widget, id: &str) -> bool {
    if widget.test_id() == Some(id) {
        return true;
    }
    let mut found = false;
    widget.for_each_child(&mut |child| {
        found |= holds(child, id);
    });
    found
}

/// The pages the stage's swap lays out: both while it plays.
fn laid_out(widget: &dyn Widget) -> usize {
    let mut count = 0_usize;
    widget.for_each_layout_child(&mut |_| count = count.saturating_add(1));
    count
}

fn swap_page() -> Vec<Patch> {
    vec![
        Patch::Remove {
            parent: vec!["stage".to_owned()],
            key: "a".to_owned(),
        },
        Patch::Insert {
            parent: vec!["stage".to_owned()],
            index: 0,
            node: page("b"),
        },
    ]
}

const FRAME: Duration = Duration::from_millis(16);

#[test]
fn a_page_swapped_for_another_keeps_both_laid_out_and_fades_from_one_to_the_other() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", Some("cross_fade"), false, start);
    shown.change(start, MotionSource::Pointer, swap_page());

    // The frame the swap starts: the page it showed is still laid out, the new one over it.
    let midway = start + FRAME;
    shown.frame(midway);
    assert_eq!(laid_out(shown.stage()), 2, "both pages are in the tree");
    assert!(holds(
        shown.stage().previous().expect("the page it showed"),
        "a"
    ));
    assert!(holds(shown.stage().current(), "b"));
    let (outgoing, incoming) = shown
        .stage()
        .opacities(midway)
        .expect("the swap plays on the stage's own motion");
    assert!(
        outgoing > 0.0 && outgoing < 1.0,
        "the old page fades: {outgoing}"
    );
    assert!(
        incoming > 0.0 && incoming < 1.0,
        "the new page fades in: {incoming}"
    );

    // Once it has played, only the page that came stands.
    let later = start + Duration::from_secs(1);
    shown.frame(later);
    assert!(shown.stage().previous().is_none(), "the old page is let go");
    assert_eq!(laid_out(shown.stage()), 1);
    assert!(shown.stage().opacities(later).is_none());
    assert!(
        !shown.shows("a"),
        "the page that went is gone from the tree"
    );
    assert!(shown.shows("b"));
}

#[test]
fn a_shortcut_that_swaps_the_page_plays_the_fade_at_fast() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", Some("cross_fade"), false, start);
    shown.change(start, MotionSource::Keyboard, swap_page());
    let midway = start + FRAME;
    shown.frame(midway);
    let (outgoing, incoming) = shown
        .stage()
        .opacities(midway)
        .expect("a key plays it at fast");
    assert!(outgoing > 0.0 && outgoing < 1.0, "{outgoing}");
    assert!(incoming > 0.0 && incoming < 1.0, "{incoming}");
}

#[test]
fn reduced_motion_lands_the_swap_without_a_fade() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", Some("cross_fade"), true, start);
    shown.change(start, MotionSource::Pointer, swap_page());
    shown.frame(start + FRAME);
    assert!(
        shown.stage().previous().is_none(),
        "no fade under reduced motion"
    );
    assert!(shown.stage().opacities(start + FRAME).is_none());
    assert!(!shown.shows("a"));
    assert!(shown.shows("b"));
}

#[test]
fn a_change_of_the_same_page_is_reconciled_in_place_and_never_swaps() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", Some("cross_fade"), false, start);
    shown.change(
        start,
        MotionSource::Pointer,
        vec![Patch::SetProp {
            path: vec!["stage".to_owned(), "a".to_owned(), "a-text".to_owned()],
            name: "value".to_owned(),
            value: Some(Value::text("edited")),
        }],
    );
    let frame = start + FRAME;
    shown.frame(frame);
    assert!(
        ui::widget::find_test_id(shown.harness.root_widget(), "stage")
            .and_then(|widget| widget.as_any())
            .and_then(|any| any.downcast_ref::<Swapped>())
            .is_none(),
        "the same page changes where it stands"
    );
    assert!(shown.shows("a"));
}

/// The swap to a page `b` that holds a button the person presses.
fn swap_to_pressable() -> Vec<Patch> {
    let mut b = page("b");
    let mut go = UiNode::empty("ui.Button", "b-go");
    go.props.insert("label".to_owned(), Value::text("Go"));
    go.events.insert("press".to_owned(), wire::Event::default());
    b.children.push(go);
    vec![
        Patch::Remove {
            parent: vec!["stage".to_owned()],
            key: "a".to_owned(),
        },
        Patch::Insert {
            parent: vec!["stage".to_owned()],
            index: 0,
            node: b,
        },
    ]
}

impl Shown {
    /// Click the control `name` at `now`, and the presses the window sent the app for it.
    fn click(&mut self, now: Instant, name: &str) -> Vec<String> {
        clock(now, || {
            self.harness
                .click_named(name)
                .expect("the control is drawn");
            ui::widget::find_mut::<AppWindow>(self.harness.root_widget_mut())
                .expect("the window")
                .flush();
        });
        let mut pressed = Vec::new();
        while let Some(batch) = self.app.recv().expect("receives") {
            for message in wire::decode_batch::<ToApp>(&batch).expect("a batch") {
                if let ToApp::Fire { key, event, .. } = message {
                    pressed.push(format!("{key} {event}"));
                }
            }
        }
        pressed
    }
}

#[test]
fn the_page_a_swap_brought_takes_the_persons_press_while_it_fades_in_and_after() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", Some("cross_fade"), false, start);
    shown.change(start, MotionSource::Pointer, swap_to_pressable());

    // While the swap plays, the page coming in is the one the person acts on.
    let midway = start + FRAME;
    shown.frame(midway);
    assert!(shown.stage().is_swapping(), "the swap still plays");
    assert_eq!(
        shown.click(midway, "b-go"),
        ["b-go press"],
        "the press reached the button on the page fading in"
    );

    // Once it has played, the stage still hands the press to the page it shows.
    let later = start + Duration::from_secs(1);
    shown.frame(later);
    assert!(!shown.stage().is_swapping());
    assert_eq!(shown.click(later, "b-go"), ["b-go press"]);
}

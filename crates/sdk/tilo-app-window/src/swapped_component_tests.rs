//! `ui.Swapped`: a view that says what it shows (`shown:`) and swaps that content whole when the
//! name changes. The content it showed stays laid out and drawn under the new one while it fades
//! out, and is let go once the swap has played; a swap that interrupts one goes from what the
//! person saw, never from a ghost behind it; reduced motion lands it on the next frame.

use super::*;
use tilo_render_protocol::patch::{Patch, local};
use tilo_ui_base::tokens::MotionSource;
use ui::time::testing::with_monotonic_time as clock;
use ui::time::{Duration, Instant};
use ui::widget::Swapped;

/// A view of a `ui.Swapped` that says it shows `shown` and holds a line of `words` in it, keyed by
/// what it shows: the same nodes a view's two branches make.
fn view(shown: &str, words: &str) -> Vec<ToUi> {
    let mut line = UiNode::empty("ui.Text", "swapped-text");
    line.props.insert("value".to_owned(), Value::text(words));
    let mut body = UiNode::empty("stack", "swapped-body");
    body.children.push(line);
    let mut swapped = UiNode::empty("ui.Swapped", "swapped");
    swapped.props.insert("shown".to_owned(), Value::text(shown));
    swapped.children.push(body);
    let mut root = UiNode::empty("page", "page");
    root.children.push(swapped);
    vec![ToUi::Open {
        level: Level::Root,
        target: ResourceRef {
            id: "doc".to_owned(),
            ty: "so.tilo.gallery#Gallery".to_owned(),
        },
        tree: root,
        front: None,
        capture_protected: false,
    }]
}

/// The change that makes the view call itself `shown` and hold `words`: one tab's content for
/// another's, which is what another branch of the view patches.
fn swap(shown: &str, words: &str) -> Vec<Patch> {
    vec![
        Patch::SetProp {
            path: vec!["swapped".to_owned()],
            name: "shown".to_owned(),
            value: Some(Value::text(shown)),
        },
        Patch::SetProp {
            path: vec![
                "swapped".to_owned(),
                "swapped-body".to_owned(),
                "swapped-text".to_owned(),
            ],
            name: "value".to_owned(),
            value: Some(Value::text(words)),
        },
    ]
}

struct Shown {
    app: local::End,
    harness: ui::headless::Driver,
}

impl Shown {
    fn open(shown: &str, words: &str, reduced: bool, now: Instant) -> Self {
        Self::open_view(&view(shown, words), reduced, now)
    }

    fn open_view(batch: &[ToUi], reduced: bool, now: Instant) -> Self {
        let (mut app, end) = local::pair();
        app.send(wire::encode_batch(batch).expect("encodes"))
            .expect("sends");
        let window = Box::new(AppWindow::new(Box::new(end), Vec::new()));
        let harness = clock(now, || {
            let mut harness = ui::headless::Driver::new(ui::State::new(), window, (600, 800));
            harness.set_theme(tilo_ui_base::Theme::dark().with_reduce_motion(reduced));
            tilo_ui_base::widget::find_mut::<AppWindow>(harness.root_widget_mut())
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
            tilo_ui_base::motion::note_source(source);
            self.app
                .send(wire::encode_batch(&batch).expect("encodes"))
                .expect("sends");
            self.frame(now);
        });
    }

    fn frame(&mut self, now: Instant) {
        clock(now, || {
            tilo_ui_base::widget::find_mut::<AppWindow>(self.harness.root_widget_mut())
                .expect("the window")
                .receive();
            self.harness.relayout();
        });
    }

    fn swapped(&self) -> &Swapped {
        tilo_ui_base::widget::find_test_id(self.harness.root_widget(), "swapped")
            .and_then(|widget| widget.as_any())
            .and_then(|any| any.downcast_ref::<Swapped>())
            .expect("the view swaps its content")
    }
}

/// Everything `widget` and what it holds say to assistive technology: the lines a reader hears.
/// The content a swap keeps while it fades is drawn, so it is read here too.
fn said(widget: &dyn Widget) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(swapped) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<Swapped>())
    {
        if let Some(previous) = swapped.previous() {
            out.extend(said(previous));
        }
        out.extend(said(swapped.current()));
    } else {
        let mut held = Vec::new();
        widget.for_each_layout_child(&mut |child| held.push(said(child)));
        if held.is_empty() {
            widget.for_each_child(&mut |child| out.extend(said(child)));
        } else {
            for lines in held {
                out.extend(lines);
            }
        }
    }
    if let Some(node) = widget.accessibility() {
        for node in node.nodes() {
            if let Some(name) = &node.name {
                out.push(name.clone());
            }
            out.extend(node.description.clone());
            out.extend(node.value.clone());
        }
    }
    out
}

/// The pages the swap lays out: both while it plays.
fn laid_out(widget: &dyn Widget) -> usize {
    let mut count = 0_usize;
    widget.for_each_layout_child(&mut |_| count = count.saturating_add(1));
    count
}

/// A view of a `ui.Swapped` whose content is a window's well (`ui.WindowWell`), keyed by the
/// place it shows: a native app window's surface, which the person sees where its well stands.
fn well_view(place: &str) -> Vec<ToUi> {
    let mut well = UiNode::empty("ui.WindowWell", "well");
    well.props.insert("place".to_owned(), Value::text(place));
    let mut swapped = UiNode::empty("ui.Swapped", "swapped");
    swapped.props.insert("shown".to_owned(), Value::text(place));
    swapped.children.push(well);
    let mut root = UiNode::empty("page", "page");
    root.children.push(swapped);
    vec![ToUi::Open {
        level: Level::Root,
        target: ResourceRef {
            id: "doc".to_owned(),
            ty: "so.tilo.shell#Window".to_owned(),
        },
        tree: root,
        front: None,
        capture_protected: false,
    }]
}

/// The change from one place's well to another's.
fn well_swap(place: &str) -> Vec<Patch> {
    vec![
        Patch::SetProp {
            path: vec!["swapped".to_owned()],
            name: "shown".to_owned(),
            value: Some(Value::text(place)),
        },
        Patch::SetProp {
            path: vec!["swapped".to_owned(), "well".to_owned()],
            name: "place".to_owned(),
            value: Some(Value::text(place)),
        },
    ]
}

/// The names of `widget` and what it holds, for a failing assertion to read.
fn names(widget: &dyn Widget) -> String {
    let mut out = String::new();
    out.push_str(widget.name());
    for child in widget.children() {
        out.push_str(" > ");
        out.push_str(&names(child));
    }
    out
}

/// The place of the well `widget` holds, where it holds one.
fn well_place(widget: &dyn Widget) -> Option<String> {
    if widget.name() == "WindowWell" {
        return widget.accessibility().and_then(|node| node.item);
    }
    if let Some(swapped) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<Swapped>())
    {
        return swapped
            .previous()
            .and_then(well_place)
            .or_else(|| well_place(swapped.current()));
    }
    let mut found = None;
    widget.for_each_layout_child(&mut |child| {
        if found.is_none() {
            found = well_place(child);
        }
    });
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = well_place(child);
        }
    });
    found
}

#[test]
fn a_windows_well_is_kept_under_the_well_that_comes_and_then_let_go() {
    let start = ui::time::now();
    let mut shown = Shown::open_view(&well_view("p-1"), false, start);
    assert_eq!(
        well_place(shown.swapped().current()),
        Some("p-1".to_owned()),
        "the well stands: {}",
        names(shown.swapped().current())
    );
    shown.change(start, MotionSource::Pointer, well_swap("p-2"));
    let midway = start + FRAME;
    shown.frame(midway);
    assert_eq!(
        well_place(shown.swapped().previous().expect("the well it showed")),
        Some("p-1".to_owned()),
        "the window's surface stands where it was while the swap plays"
    );
    assert_eq!(
        well_place(shown.swapped().current()),
        Some("p-2".to_owned())
    );
    let (outgoing, _) = shown.swapped().opacities(midway).expect("the swap plays");
    assert!(outgoing > 0.0 && outgoing < 1.0, "{outgoing}");

    let later = start + Duration::from_secs(1);
    shown.frame(later);
    assert!(
        shown.swapped().previous().is_none(),
        "the old well is let go"
    );
    assert_eq!(
        well_place(shown.swapped().current()),
        Some("p-2".to_owned())
    );
}

const FRAME: Duration = Duration::from_millis(16);

#[test]
fn a_change_of_what_the_view_shows_keeps_the_one_it_showed_under_the_new_one() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", "first", false, start);
    shown.change(start, MotionSource::Pointer, swap("b", "second"));
    let midway = start + FRAME;
    shown.frame(midway);

    assert_eq!(laid_out(shown.swapped()), 2, "both contents are laid out");
    let previous = shown.swapped().previous().expect("what it showed");
    assert!(
        said(previous).iter().any(|words| words == "first"),
        "the content it showed is still drawn: {:?}",
        said(previous)
    );
    assert!(
        said(shown.swapped().current())
            .iter()
            .any(|words| words == "second"),
        "the content that came is drawn"
    );
    let (outgoing, incoming) = shown
        .swapped()
        .opacities(midway)
        .expect("the swap plays on the view's motion");
    assert!(
        outgoing > 0.0 && outgoing < 1.0,
        "the old content fades: {outgoing}"
    );
    assert!(
        incoming > 0.0 && incoming < 1.0,
        "the new content fades in: {incoming}"
    );

    // Once it has played, only the content that came stands.
    let later = start + Duration::from_secs(1);
    shown.frame(later);
    assert!(shown.swapped().previous().is_none(), "the old is let go");
    assert_eq!(laid_out(shown.swapped()), 1);
    assert!(shown.swapped().opacities(later).is_none());
    assert_eq!(
        said(shown.swapped()),
        vec!["second".to_owned()],
        "the content that went is gone from the tree"
    );
}

#[test]
fn a_second_change_mid_fade_goes_from_the_content_the_person_saw() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", "first", false, start);
    shown.change(start, MotionSource::Pointer, swap("b", "second"));
    shown.frame(start + FRAME);
    // While the first swap plays, the person chooses a third: the ghost of the first goes, and
    // what fades out is the one drawn in front.
    shown.change(start + FRAME, MotionSource::Pointer, swap("c", "third"));
    shown.frame(start + FRAME * 2);
    let previous = shown.swapped().previous().expect("what it showed");
    assert!(
        said(previous).iter().any(|words| words == "second"),
        "it fades from the content the person saw: {:?}",
        said(previous)
    );
    assert!(
        !said(previous).iter().any(|words| words == "first"),
        "no ghost of the one before it stacks behind"
    );
    assert_eq!(laid_out(shown.swapped()), 2, "one ghost at most");

    let later = start + Duration::from_secs(1);
    shown.frame(later);
    assert!(shown.swapped().previous().is_none());
    assert_eq!(
        said(shown.swapped()),
        vec!["third".to_owned()],
        "only the newest stands at the end"
    );
}

#[test]
fn reduced_motion_lands_the_swap_without_a_fade() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", "first", true, start);
    shown.change(start, MotionSource::Pointer, swap("b", "second"));
    shown.frame(start + FRAME);
    assert!(
        shown.swapped().previous().is_none(),
        "no fade under reduced motion"
    );
    assert!(shown.swapped().opacities(start + FRAME).is_none());
    assert_eq!(said(shown.swapped()), vec!["second".to_owned()]);
}

/// A view that names another shown thing while its content is the same swaps nothing, and an edit
/// inside the content in front does not fade the whole of it.
#[test]
fn naming_another_thing_with_the_same_content_swaps_nothing() {
    let start = ui::time::now();
    let mut shown = Shown::open("a", "first", false, start);
    shown.change(
        start,
        MotionSource::Pointer,
        vec![Patch::SetProp {
            path: vec!["swapped".to_owned()],
            name: "shown".to_owned(),
            value: Some(Value::text("b")),
        }],
    );
    shown.frame(start + FRAME);
    assert!(
        shown.swapped().previous().is_none(),
        "the content did not change, so nothing swaps"
    );

    // An edit inside the content in front is the content's own: it changes where it stands.
    shown.change(
        start + FRAME,
        MotionSource::Pointer,
        vec![Patch::SetProp {
            path: vec![
                "swapped".to_owned(),
                "swapped-body".to_owned(),
                "swapped-text".to_owned(),
            ],
            name: "value".to_owned(),
            value: Some(Value::text("edited")),
        }],
    );
    shown.frame(start + FRAME * 2);
    assert!(
        shown.swapped().previous().is_none(),
        "an edit inside the content in front fades nothing: {:?}",
        shown.swapped().previous().map(said)
    );
    assert_eq!(said(shown.swapped()), vec!["edited".to_owned()]);
}

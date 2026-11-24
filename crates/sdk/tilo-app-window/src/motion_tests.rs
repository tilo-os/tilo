//! How a change plays in a window: a view's tree, changed as the runtime changes it when a resource
//! does, drawn frame by frame at a pinned clock.

use super::*;
use tilo_render_protocol::patch::local;
use ui::time::testing::with_monotonic_time as clock;
use ui::time::{Duration, Instant};

/// A window showing a meter, a bar as wide as its reading, on a clock the test steps.
struct Meter {
    app: local::End,
    harness: ui::headless::FrameHarness,
    now: Instant,
}

impl Meter {
    fn open(level: f64, motion: &str, now: Instant) -> Self {
        let mut drawing = UiNode::empty("ui.Drawing", "meter");
        let props = [
            ("shapes", bar(level)),
            ("meaning", Value::Enum("progress".to_owned())),
        ];
        let props = props
            .into_iter()
            .chain([("motion", Value::Enum(motion.to_owned()))]);
        drawing
            .props
            .extend(props.map(|(name, value)| (name.to_owned(), value)));
        let mut tree = UiNode::empty("stack", "page");
        tree.children.push(drawing);
        let target = ResourceRef {
            id: "doc".to_owned(),
            ty: "notes.Note".to_owned(),
        };
        let (mut app, end) = local::pair();
        let open = ToUi::Open {
            level: Level::Root,
            target,
            tree,
            front: None,

            capture_protected: false,
        };
        app.send(wire::encode_batch(&[open]).expect("encodes"))
            .expect("sends");
        let window = Box::new(AppWindow::new(Box::new(end), Vec::new()));
        let harness = clock(now, || {
            let mut harness = ui::headless::FrameHarness::new(ui::State::new(), window, (900, 700));
            harness.set_theme(ui::Theme::dark().with_reduce_motion(false));
            harness
        });
        let mut meter = Self { app, harness, now };
        meter.frame_at(now);
        meter
    }

    /// A new reading, as the runtime sends one when the resource the view reads changes: the
    /// person's own change, another member's or an agent's alike.
    fn read(&mut self, level: f64) {
        let (path, name) = (vec!["meter".to_owned()], "shapes".to_owned());
        let patches = vec![wire::Patch::SetProp {
            path,
            name,
            value: Some(bar(level)),
        }];
        let reading = ToUi::Patch {
            level: Level::Root,
            patches,
        };
        self.app
            .send(wire::encode_batch(&[reading]).expect("encodes"))
            .expect("sends");
    }

    /// The frame at `now`, taking what the app sent at the frame's instant as a live window does.
    fn frame_at(&mut self, now: Instant) {
        self.now = now;
        clock(now, || {
            let root = self.harness.root_widget_mut();
            ui::widget::find_mut::<AppWindow>(root)
                .expect("the window")
                .receive();
            self.harness.relayout();
        });
    }

    /// Where the bar ends, as drawn.
    fn drawn(&mut self) -> f64 {
        let drawing = ui::widget::find_mut::<ui::widget::Drawing>(self.harness.root_widget_mut());
        match drawing
            .expect("the meter is drawn")
            .drawn_at(self.now)
            .first()
        {
            Some(ui::widget::Shape::Rect { w, .. }) => *w,
            other => panic!("the meter draws its bar, not {other:?}"),
        }
    }
}

fn bar(level: f64) -> Value {
    let fields = [("x", 0.0), ("y", 0.0), ("w", level), ("h", 1.0)];
    let fields = fields.map(|(name, at)| (name.to_owned(), Value::Float(at)));
    let fill = ("fill".to_owned(), Value::Enum("accent".to_owned()));
    let fields = fields.into_iter().chain([fill]).collect();
    Value::List(vec![Value::Case {
        case: "rect".to_owned(),
        fields,
    }])
}

#[test]
fn a_reading_moves_from_where_it_is_drawn_and_rests_on_the_latest() {
    let (frame, start) = (Duration::from_millis(16), ui::time::now());
    let mut meter = Meter::open(0.2, "standard", start);

    // The frame after a new reading draws the bar on its way.
    meter.read(0.8);
    meter.frame_at(start + frame);
    meter.frame_at(start + frame * 2);
    let first = meter.drawn();
    assert!(first > 0.2 && first < 0.8, "on its way: {first}");

    // Another reading mid-motion, from someone else, goes on from the drawn bar: never back to
    // where the first began, never on to 0.8 first.
    meter.read(0.5);
    meter.frame_at(start + frame * 2);
    let turned = meter.drawn();
    assert!(
        (turned - first).abs() < 1e-6,
        "on from where it is drawn: {first}, {turned}"
    );
    let mut was = first;
    for step in 3..60 {
        meter.frame_at(start + frame * step);
        was = meter.drawn();
        assert!(was > 0.2 && was < 0.8, "it turns toward the latest: {was}");
    }
    assert_eq!(
        was.to_bits(),
        0.5_f64.to_bits(),
        "it rests exactly on the latest"
    );

    // Reduced motion, and a view asking for `.still`, draw a new reading at once.
    let later = start + Duration::from_secs(4);
    let reduced = ui::Theme::dark().with_reduce_motion(true);
    clock(later, || meter.harness.set_theme(reduced));
    let mut still = Meter::open(0.2, "still", later);
    for meter in [&mut meter, &mut still] {
        meter.read(0.9);
        meter.frame_at(later + frame);
        assert_eq!(
            meter.drawn().to_bits(),
            0.9_f64.to_bits(),
            "it lands at once"
        );
    }
}

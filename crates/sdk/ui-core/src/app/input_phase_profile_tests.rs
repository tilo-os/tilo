// Included into `surface.rs`'s test module: a paced pen stroke through the
// live input path, measured per event and per phase.

thread_local! {
    static STROKE: RefCell<Option<Rc<RefCell<Vec<f64>>>>> = const { RefCell::new(None) };
}

/// The time each pointer move under the tip costs the probe, so widget
/// dispatch has a known floor the profiler must attribute to it.
const STROKE_MOVE_COST: Duration = Duration::from_millis(2);

/// A canvas stand-in: a pen drag inside it is handled, costs
/// [`STROKE_MOVE_COST`] per move, and records the pressure it saw.
struct StrokeProbe {
    rect: KurboRect,
    down: bool,
    pressures: Rc<RefCell<Vec<f64>>>,
}

impl Widget for StrokeProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(nodes, self)
    }
    crate::rect_geometry!();
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::MouseInput { state, .. } => {
                self.down = *state == ElementState::Pressed;
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { .. } if self.down => {
                std::thread::sleep(STROKE_MOVE_COST);
                self.pressures
                    .borrow_mut()
                    .push(ctx.pen().map_or(-1.0, |pen| pen.pressure));
                *ctx.handled = true;
            }
            _ => {}
        }
    }
    fn name(&self) -> &'static str {
        "Stroke"
    }
}

impl crate::widget::AppWidget for StrokeProbe {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Stroke"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("S")
    }
}

fn stroke_factory(
    mut context: crate::application::HostContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    let pressures = Rc::new(RefCell::new(Vec::new()));
    STROKE.with_borrow_mut(|slot| *slot = Some(Rc::clone(&pressures)));
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(StrokeProbe {
            rect: KurboRect::ZERO,
            down: false,
            pressures,
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}

/// Delivers `event` as the native runner does: a pen sample is kept for the
/// pointer event after it, never dispatched on its own.
fn deliver_routed(app: &mut App, surface: &dyn AppSurface, event: WindowEvent) {
    let Some(_pen) = app.input.pen.route(&event) else {
        return;
    };
    app.surface_input(surface, event, false);
}

/// A frame's banked input is many events, not one: the profiler counts them,
/// keeps the costliest, and attributes widget dispatch, hit testing,
/// resources, publication and commits separately, without double counting.
#[test]
fn paced_pen_stroke_attributes_banked_input_per_event_and_phase() {
    use crate::native_scenario::Input;

    let host = crate::application_test_support::ProbeHost::new();
    let definition = crate::application_test_support::definition(stroke_factory);
    let target = crate::application::LaunchTarget::New;
    let context = host
        .prepare_with_permissions(
            &definition,
            73,
            &target,
            crate::application_test_support::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, target, crate::application::RestoreInput::None)
        .unwrap();
    let mut app =
        App::from_component(component, crate::application_test_support::runner_inputs()).unwrap();
    let pressures = STROKE.with_borrow(|slot| Rc::clone(slot.as_ref().unwrap()));
    let region = Region {
        size: (320, 200),
        redraw: Cell::new(false),
    };
    app.surface_frame(&region, &mut Scene::new());
    app.profiler = Some(crate::profiling::FrameProfiler::for_capture());

    // Two frames of a stroke, each banking several paced pen steps, the way
    // input arrives between redraws.
    let frames: [Vec<Input>; 2] = [
        vec![
            Input::PenDown {
                x: 40.0,
                y: 40.0,
                pressure: 0.25,
            },
            Input::PenMove {
                x: 44.0,
                y: 42.0,
                pressure: 0.5,
            },
            Input::PenMove {
                x: 48.0,
                y: 44.0,
                pressure: 0.75,
            },
        ],
        vec![
            Input::PenMove {
                x: 52.0,
                y: 46.0,
                pressure: 1.0,
            },
            Input::PenUp { x: 52.0, y: 46.0 },
        ],
    ];
    let mut samples = Vec::new();
    for steps in &frames {
        let mut events = 0;
        for step in steps {
            for event in step.window_events() {
                if !matches!(event, WindowEvent::PenSample { .. }) {
                    events += 1;
                }
                deliver_routed(&mut app, &region, event);
            }
            // Paced, not a burst: real time passes between steps.
            std::thread::sleep(Duration::from_millis(1));
        }
        app.surface_frame(&region, &mut Scene::new());
        let profiler = app.profiler.as_mut().unwrap();
        profiler.commit_frame(0);
        let frame = profiler.take_capture().pop().expect("one committed frame");
        assert_eq!(
            frame.input_events, events,
            "every dispatched event, and no pen sample, is counted: {frame:?}"
        );
        samples.push(frame);
    }

    // The pen reached the widget with each step's pressure: every move while
    // the tip is down, and the lift at zero.
    assert_eq!(*pressures.borrow(), vec![0.5, 0.75, 1.0, 0.0]);

    let move_cost = u64::try_from(STROKE_MOVE_COST.as_micros()).unwrap();
    for (frame, moves) in samples.iter().zip([2_u64, 2]) {
        let phases = frame.input_dispatch_us
            + frame.input_hit_test_us
            + frame.input_resources_us
            + frame.input_publication_us
            + frame.input_commit_us;
        assert!(
            phases <= frame.handle_event_us,
            "phases are disjoint spans inside each event: {frame:?}"
        );
        assert!(
            frame.input_dispatch_us >= moves * move_cost,
            "widget dispatch carries the probe's per-move cost: {frame:?}"
        );
        assert!(
            frame.input_event_max_us >= move_cost
                && frame.input_event_max_us < frame.handle_event_us,
            "the banked sum is several events, the costliest is one: {frame:?}"
        );
        assert!(
            frame.input_to_commit_us >= frame.handle_event_us,
            "the oldest input waited at least for every event after it: {frame:?}"
        );
    }
}

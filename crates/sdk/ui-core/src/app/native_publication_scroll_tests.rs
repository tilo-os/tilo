// Connected native input/publication fixture; no GPU or physical scanout.
struct PublicationProbeTransport;
impl crate::collaboration::PeerTransport for PublicationProbeTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

struct PublicationProbeRoot {
    inner: crate::widget::Scrollable,
}
impl crate::widget::Decorator for PublicationProbeRoot {
    crate::provides!(dyn crate::resources::PublishesResources; inner = inner);
    type Inner = crate::widget::Scrollable;
    fn inner(&self) -> &Self::Inner {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.inner
    }
}
impl crate::resources::PublishesResources for PublicationProbeRoot {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![
            tilo_actions::Resource::new("probe:scroll", "probe", "Scroll fixture").with_action(
                tilo_actions::ActionSpec::operation(
                    "inspect",
                    "Inspect",
                    tilo_actions::OperationKind::View,
                    crate::a11y_tree::SEMANTIC_OPERATION_TYPE,
                    &crate::a11y_tree::SemanticOperation {
                        node: "root".into(),
                        action: "focus".into(),
                        value: None,
                    },
                )
                .unwrap(),
            ),
        ]
    }
}

#[test]
fn connected_native_long_document_wheel_publication_probe() {
    crate::i18n::I18nManager::init("en-US");
    // Keep the ordinary regression bounded while ensuring its document
    // overflows. The recorded hardware-sized benchmark explicitly selects
    // 180 paragraphs and retains its original viewport and wheel step.
    let paragraphs = std::env::var("TILO_NATIVE_SCROLL_PROBE_PARAGRAPHS")
        .map(|raw| raw.parse::<usize>().expect("integer paragraph count"))
        .unwrap_or(32);
    assert!((32..=180).contains(&paragraphs));
    let body = (0..paragraphs)
        .map(|index| {
            format!(
                "Paragraph {index}: {}\n\n",
                "Measured scrolling text with ordinary words. ".repeat(6)
            )
        })
        .collect::<String>();
    let (viewport, wheel_step) = if paragraphs == 180 {
        ((1051, 952), -1.0)
    } else {
        ((1051, 240), -0.25)
    };
    let offset = Rc::new(Cell::new((0.0, 0.0)));
    let inner = crate::widget::Scrollable::new_vertical()
        .with_persisted_offset(Rc::clone(&offset))
        .with_child(Box::new(crate::widget::BlockEditor::from_markdown(&body)));
    let root = PublicationProbeRoot { inner };
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(PublicationProbeTransport);
    let region = Region {
        size: viewport,
        redraw: Cell::new(false),
    };
    let mut scene = Scene::new();
    app.surface_frame(&region, &mut scene);
    assert!(
        app.actions
            .last_published_actions
            .as_ref()
            .is_some_and(|actions| !actions.is_empty()),
        "native publisher must run"
    );
    app.surface_input(
        &region,
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(400.0, f64::from(viewport.1) / 2.0),
        },
        false,
    );
    for sample in 0..10 {
        let before = offset.get();
        let started = crate::time::now();
        assert!(app.surface_input(
            &region,
            WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, wheel_step),
                phase: winit::event::TouchPhase::Moved
            },
            false
        ));
        let input_ns = started.elapsed().as_nanos();
        assert!(offset.get().1 > before.1, "real native wheel moves content");
        scene.reset();
        let started = crate::time::now();
        app.surface_frame(&region, &mut scene);
        eprintln!(
            "native-publication-wheel {}",
            serde_json::json!({
                "sample": sample, "input_ns": input_ns,
                "frame_ns": started.elapsed().as_nanos(),
                "paragraphs": paragraphs, "markdown_bytes": body.len(),
                "viewport": [viewport.0, viewport.1], "wheel_line_delta": wheel_step, "architecture": std::env::consts::ARCH,
                "profile": if cfg!(debug_assertions) { "debug" } else { "optimized" },
                "connected_publication": true, "notes_framework": false,
                "gpu_or_scanout": false,
            })
        );
    }
}

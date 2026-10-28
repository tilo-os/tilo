use super::*;
use crate::time::Duration;

#[test]
fn window_edge_shadow_and_focus_ring_stay_off_app_pixels() {
    // Case `a_hosted_app_cannot_square_off_the_window_or_erase_its_border`.
    // An app that fills its whole rect (as every hosted app does, since
    // it can't know it's being framed) must not square off the window's
    // rounded corners or paint over its border — both are the frame's.
    {
        use crate::offscreen::OffscreenRenderer;

        // Tile inset far enough from the surface edge that the ambient
        // shadow sprite has somewhere to land without wrapping.
        let (w, h) = (200u32, 200u32);
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let mut host = TileHost::new(TileContent::App(Box::new(FloodFill { rect })));
        host.rect = rect;

        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );

        let mut r = OffscreenRenderer::new(w, h);
        // Distinct from both the flood colour and the card so a leak from
        // any of the three is identifiable in the failure message.
        let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));

        let radius = f64::from(theme.tokens.radii.tile);
        // A point inside the tile's bounding box but outside its rounded
        // silhouette: one pixel in from the box corner, well inside the
        // corner arc's cut.
        let (cx, cy) = (
            crate::num::u32_of_f64(rect.x0) + 2,
            crate::num::u32_of_f64(rect.y0) + 2,
        );
        assert!(
            radius > 8.0,
            "test assumes a visibly rounded card; radii.tile is {radius}",
        );
        let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, w, cx, cy);
        assert!(
            cr < 128,
            "the hosted app's fill leaked outside the card's rounded \
         corner at ({cx},{cy}) — got ({cr},{cg},{cb}), which is the \
         app's flood colour. The corner belongs to the frame.",
        );

        // Dead centre is the app: the clip must not have swallowed it.
        let (mr, mg, mb, _) = OffscreenRenderer::sample(&rgba, w, 100, 100);
        assert!(
            mr > 200 && mg < 80 && mb < 80,
            "the hosted app must still paint inside the card; centre is \
         ({mr},{mg},{mb}), expected the app's flood colour",
        );

        // One pixel out from the boundary is the frame's edge: it must be
        // neither the app (flood escaped its clip) nor bare desktop (no
        // edge there at all).
        let edge_x = crate::num::u32_of_f64(rect.x0) - 1;
        let (er, eg, eb, _) = OffscreenRenderer::sample(&rgba, w, edge_x, 100);
        assert!(
            (er, eg, eb) != (0xFF, 0x00, 0x00),
            "the hosted app's fill leaked onto the window's edge at \
         ({edge_x},100) — got ({er},{eg},{eb}), the app's flood colour",
        );
        assert!(
            (er, eg, eb) != (0x00, 0x00, 0xFF),
            "the window has no edge at ({edge_x},100) — the pixel is bare \
         desktop, so nothing separates the window from it",
        );

        // ...and the first pixel *inside* the boundary is the app's, whole.
        // The frame stops at the silhouette.
        let inside_x = crate::num::u32_of_f64(rect.x0);
        let (nr, ng, nb, _) = OffscreenRenderer::sample(&rgba, w, inside_x, 100);
        assert_eq!(
            (nr, ng, nb),
            (0xFF, 0x00, 0x00),
            "the frame tinted a pixel the app owns at ({inside_x},100) — got \
         ({nr},{ng},{nb}), expected the app's flood colour",
        );
    }

    // Case `the_window_edge_never_tints_a_pixel_the_app_owns`.
    // The window's edge must never tint a pixel the hosted app owns —
    // stroking a constant over unknown app content reads right on one
    // ground and wrong on the rest of the ramp.
    {
        use crate::offscreen::OffscreenRenderer;

        // Spanning the ramp: a dark app, two mid grounds, and the light
        // chrome a word processor paints.
        let grounds: [(u8, u8, u8); 4] = [
            (0x10, 0x10, 0x12),
            (0x40, 0x40, 0x44),
            (0x80, 0x80, 0x84),
            (0xC0, 0xC0, 0xC4),
        ];
        let (w, h) = (200u32, 200u32);
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let theme = Theme::default();

        for (r8, g8, b8) in grounds {
            let color = vello::peniko::Color::from_rgb8(r8, g8, b8);
            let mut host = TileHost::new(TileContent::App(Box::new(Flood { rect, color })));
            host.rect = rect;
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            host.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            let mut renderer = OffscreenRenderer::new(w, h);
            let rgba = renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));

            // Both vertical edges at mid-height, and the horizontal edges at
            // mid-width: the first pixel inside the silhouette on every side.
            let probes = [
                ("left", crate::num::u32_of_f64(rect.x0), 100u32),
                ("right", crate::num::u32_of_f64(rect.x1) - 1, 100u32),
                ("top", 100u32, crate::num::u32_of_f64(rect.y0)),
                ("bottom", 100u32, crate::num::u32_of_f64(rect.y1) - 1),
            ];
            for (side, px, py) in probes {
                let (sr, sg, sb, _) = OffscreenRenderer::sample(&rgba, w, px, py);
                assert_eq!(
                    (sr, sg, sb),
                    (r8, g8, b8),
                    "the frame tinted the app's own pixel on the {side} edge \
                 at ({px},{py}): app painted ({r8},{g8},{b8}), the render \
                 shows ({sr},{sg},{sb}). The window's edge belongs outside \
                 the card, on ground the shell knows — over app content it \
                 is a constant on an unknown surface",
                );
            }
        }
    }

    // Case `focus_lights_the_card_s_edge_and_never_squares_off_its_corner`.
    // Focus is a change to the window's own edge, staying inside the
    // card's silhouette — not a square fence around the bounding box,
    // which would double-line the rounded corner and paint on desktop
    // diagonally outside it. Checked at the edge's flat and diagonally
    // outside the corner, across every theme preset.
    {
        use crate::offscreen::OffscreenRenderer;

        let (width, height) = (200u32, 200u32);
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        // Desktop stand-in. Deliberately a colour no palette contains, so
        // "still desktop" cannot be confused with "some theme token".
        let ground = vello::peniko::Color::from_rgb8(0x00, 0xFF, 0x00);

        for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let name = theme.name.clone();
            let focus = theme.tokens.colors.window_edge_focused();
            assert_ne!(
                focus.to_rgba8().to_u32(),
                theme.tokens.colors.intent.you.to_rgba8().to_u32(),
                "{name}: window focus is neutral, not your colour"
            );
            let (fr, fg, fb) = {
                let rgba = focus.to_rgba8();
                (rgba.r, rgba.g, rgba.b)
            };

            let shot = |focused: bool| {
                let mut host = TileHost::new(TileContent::Stub {
                    kind: AppKind::Editor,
                    label: String::new(),
                });
                host.rect = rect;
                host.set_focused(focused);
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                host.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
                let mut renderer = OffscreenRenderer::new(width, height);
                renderer.render(&scene, ground)
            };

            let lit = shot(true);
            let dark = shot(false);

            // Mid-span of the top edge: the band the stroke occupies sits
            // just outside the card boundary.
            let (sample_x, sample_y) = (100u32, crate::num::u32_of_f64(rect.y0) - 1);
            let (lr, lg, lb, _) = OffscreenRenderer::sample(&lit, width, sample_x, sample_y);
            assert_eq!(
                (lr, lg, lb),
                (fr, fg, fb),
                "{name}: the focused window's edge is not the theme's focus                  colour at ({sample_x},{sample_y}) — got ({lr},{lg},{lb}), expected                  ({fr},{fg},{fb}). Focus has to come from the theme: a baked                  constant is right on one palette and meaningless on the rest",
            );
            let (ur, ug, ub, _) = OffscreenRenderer::sample(&dark, width, sample_x, sample_y);
            assert_ne!(
                (ur, ug, ub),
                (fr, fg, fb),
                "{name}: an unfocused window paints the focus colour on its                  edge at ({sample_x},{sample_y}), so focus distinguishes nothing",
            );

            // Well outside the rounded arc, where a square fence would
            // have painted. The ambient shadow legitimately darkens this
            // pixel, so the claim is only that focus colour isn't there,
            // not that it's untouched.
            let (corner_x, corner_y) = (
                crate::num::u32_of_f64(rect.x0) - 2,
                crate::num::u32_of_f64(rect.y0) - 2,
            );
            for (label, buf) in [("focused", &lit), ("unfocused", &dark)] {
                let (sr, sg, sb, _) = OffscreenRenderer::sample(buf, width, corner_x, corner_y);
                assert_ne!(
                    (sr, sg, sb),
                    (fr, fg, fb),
                    "{name}: the {label} window painted the focus colour at ({corner_x},{corner_y}), outside its own rounded corner on bare desktop. Only a sharp-cornered rectangle drawn around the window reaches that pixel, which is the artefact this replaced",
                );
                assert!(
                    sg > sr && sg > sb,
                    "{name}: the {label} window covered the desktop at ({corner_x},{corner_y}) with ({sr},{sg},{sb}). Outside its rounded corner the desktop must still read as the desktop, shadowed at most, never stroked over",
                );
            }
        }
    }
}

/// Notes, under its `text`, the body of whatever document it is handed
/// ([`seen_by`]), and optionally declares a file it has open.
struct Scribe {
    text: String,
    opens: Option<std::path::PathBuf>,
}

thread_local! {
    /// The body each [`Scribe`] last saw, by its text; empty for a
    /// document without one.
    static SEEN: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::default();
}

/// What the [`Scribe`] named `text` last saw: `None` when it was never
/// handed a document.
fn seen_by(text: &str) -> Option<String> {
    SEEN.with_borrow(|seen| seen.get(text).cloned())
}

fn write_body(store: &RecordStore, id: &RecordId, body: &str) {
    use automerge::transaction::Transactable;
    store
        .with(id, |doc| {
            doc.put(ROOT, "body", body).unwrap();
        })
        .unwrap();
}

impl Widget for Scribe {
    crate::provides!(dyn crate::record::capability::RecordScoped);

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        use automerge::ReadDoc;
        let (state, path) = crate::doc::document_ref(state, path);
        let body = match state.get(path, "body") {
            Ok(Some((automerge::Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
            _ => None,
        };
        SEEN.with_borrow_mut(|seen| seen.insert(self.text.clone(), body.unwrap_or_default()));
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "Scribe"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document)
                .with_name("Seeded hidden document")
                .with_value(self.text.clone()),
        )
    }
}

impl crate::record::capability::RecordScoped for Scribe {
    fn record_intent(&self) -> Option<RecordCheckout> {
        self.opens.as_ref().map(|p| {
            RecordCheckout::new(
                RecordId::for_file(p),
                crate::record::RecordKind::new("file"),
            )
        })
    }
}

impl crate::widget::app_widget::AppWidget for Scribe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Scribe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

struct FloatingProbe {
    acknowledged: std::rc::Rc<std::cell::Cell<bool>>,
    update: Option<Rect>,
}

impl FloatingSurfaceProvider for FloatingProbe {
    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.update
            .take()
            .map(|rect| vec![("probe-pip".into(), rect)])
            .unwrap_or_default()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        vec!["probe-pip".into()]
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        if matches!(
            message,
            crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. }
                if id == "probe-pip"
        ) {
            self.acknowledged.set(true);
        }
    }
}

impl Widget for FloatingProbe {
    crate::provides!(dyn crate::widget::FloatingSurfaceProvider);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "FloatingProbe"
    }
}

impl crate::widget::app_widget::AppWidget for FloatingProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &'static str {
        "Floating probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("P")
    }
}

/// A tile filling its display draws its content edge to edge: no
/// rounded clip around it and no edge over it, so the content's own
/// drawing (a client's frame) is the last and unclipped thing it draws.
/// A tile beside others keeps both.
#[test]
fn a_full_bleed_tile_draws_its_content_last_and_unclipped() {
    let rect = Rect::new(0.0, 0.0, 160.0, 100.0);
    let draw = |full_bleed: bool| {
        let mut host = TileHost::new(TileContent::App(Box::new(FloodFill { rect })));
        host.rect = rect;
        host.set_full_bleed(full_bleed);
        let mut scene = Scene::new();
        host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::empty(),
        );
        scene
    };
    let framed = draw(false);
    assert!(
        framed.encoding().n_clips > 0,
        "a tile beside others clips to its rounded card"
    );
    let full = draw(true);
    let encoding = full.encoding();
    assert_eq!(
        encoding.n_clips, 0,
        "nothing clips a tile filling its display"
    );
    assert_eq!(
        encoding.draw_tags.last().map(|tag| tag.0),
        Some(vello_encoding::DrawTag::COLOR.0),
        "the content's fill is the last draw: no edge over it"
    );
    assert_eq!(
        encoding.draw_tags.len(),
        2,
        "the card's ground under the content, then the content"
    );
}

/// Reaching into the app every frame (a peer stream's late picture)
/// must not read as new content: that rebuilt the whole layout tree on
/// every frame a peer window showed.
#[test]
fn reaching_into_the_app_leaves_the_structure_alone() {
    let rect = Rect::new(0.0, 0.0, 160.0, 100.0);
    let mut host = TileHost::new(TileContent::App(Box::new(FloodFill { rect })));
    let _ = Widget::take_structure_dirty(&mut host);
    assert!(host.app_mut().is_some());
    assert!(!Widget::take_structure_dirty(&mut host));
    let _ = host.content_mut();
    assert!(
        Widget::take_structure_dirty(&mut host),
        "content handed out to replace is a new structure"
    );
}

#[test]
fn tile_host_forwards_surfaces_and_only_unhandled_events() {
    // Case `tile_host_forwards_the_complete_floating_surface_lifecycle`.
    {
        let acknowledged = std::rc::Rc::new(std::cell::Cell::new(false));
        let source = Rect::new(20.0, 30.0, 420.0, 255.0);
        let mut host = TileHost::new(TileContent::App(Box::new(FloatingProbe {
            acknowledged: acknowledged.clone(),
            update: Some(source),
        })));

        assert_eq!(
            FloatingSurfaceWidgetExt::active_floating_surface_ids(&host),
            vec!["probe-pip"]
        );
        assert_eq!(
            FloatingSurfaceWidgetExt::take_floating_surface_source_updates(&mut host),
            vec![("probe-pip".into(), source)]
        );
        assert!(
            FloatingSurfaceWidgetExt::take_floating_surface_source_updates(&mut host).is_empty()
        );

        FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut host,
            &crate::floating_surface::FloatingSurfaceHostMessage::Adopted {
                id: "probe-pip".into(),
                rect: [100.0, 80.0, 500.0, 305.0],
            },
        );
        assert!(acknowledged.get());
    }

    // Case `a_prehandled_event_never_reaches_the_hosted_app`.
    {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut host = TileHost::new(TileContent::App(Box::new(EventProbe(calls.clone()))));
        let mut taffy = TaffyTree::new();
        let mut node_map = std::collections::HashMap::new();
        let node = host.build_layout(&mut taffy, &Theme::default(), &mut node_map);
        crate::testing::Dispatch::new()
            .handled(true)
            .layout(&taffy, node)
            .send(&mut host, &WindowEvent::Focused(true));
        assert_eq!(calls.get(), 0);
    }
}

#[test]
fn an_unresolved_tile_only_refuses_events_meant_for_it() {
    use winit::event::ElementState;
    use winit::keyboard::Key;
    // A tile with no record (still restoring, or starting) refuses its
    // own input, but must not swallow the pointer over a neighbour or keys
    // for the focused window beside it.
    let mut host = TileHost::new(TileContent::Stub {
        kind: AppKind::Editor,
        label: String::new(),
    });
    host.rect = Rect::new(0.0, 0.0, 100.0, 100.0);
    // A closed document owner leaves the record unresolvable.
    host.history_lifetime.active.set(false);
    let mut taffy = TaffyTree::new();
    let mut node_map = std::collections::HashMap::new();
    let node = host.build_layout(&mut taffy, &Theme::default(), &mut node_map);
    let send = |host: &mut TileHost, event: &WindowEvent| {
        crate::testing::Dispatch::new()
            .layout(&taffy, node)
            .send(host, event)
            .handled
    };
    let key = crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
        Key::Character("a".into()),
        None,
        ElementState::Pressed,
    ));
    // Pointer outside: passes through, and so does the press after it.
    assert!(!send(
        &mut host,
        &crate::synthetic_input::cursor_moved(250.0, 50.0)
    ));
    assert!(!send(&mut host, &crate::synthetic_input::mouse_press()));
    // Pointer inside: refused.
    assert!(send(
        &mut host,
        &crate::synthetic_input::cursor_moved(50.0, 50.0)
    ));
    assert!(send(&mut host, &crate::synthetic_input::mouse_press()));
    // Keys: refused only while this tile is the focused one.
    assert!(!send(&mut host, &key));
    host.set_focused(true);
    assert!(send(&mut host, &key));
}

fn tab_checkout(id: u64) -> RecordCheckout {
    RecordCheckout::new(RecordId::for_tab(id), crate::record::RecordKind::new("tab"))
}

fn canonical_store(root: &std::path::Path) -> RecordStore {
    let mut owners = crate::record::RecordOwners::new(root.to_path_buf());
    owners.open("so.tilo.tile-host-test", "room").unwrap()
}

fn inspection_has_semantic(node: &crate::headless::InspectionNode, name: &str) -> bool {
    fn access_has(node: &crate::a11y::AccessNode, name: &str) -> bool {
        node.name.as_deref() == Some(name)
            || node.children.iter().any(|child| access_has(child, name))
    }
    node.semantics
        .as_ref()
        .is_some_and(|access| access_has(access, name))
        || node
            .children
            .iter()
            .any(|child| inspection_has_semantic(child, name))
}

#[test]
fn outgoing_records_settle_once_and_keep_failed_owners() {
    // Case `history_checked_canonical_tail_refusal_preserves_bytes_without_a_live_owner`.
    {
        use automerge::transaction::Transactable;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("records");
        let checkout = tab_checkout(3903);
        {
            let store = canonical_store(&root);
            store.create_record(&checkout).unwrap();
            store.with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "valid original history").unwrap();
            });
            store.try_flush().unwrap();
        }
        let record_dir = app_state::DocStore::room_record_dir_at(
            &root,
            "so.tilo.tile-host-test",
            "room",
            &checkout.id.storage_key(),
        )
        .unwrap();
        let path = app_state::DocStore::open_canonical_at("so.tilo.tile-host-test", &record_dir)
            .unwrap()
            .document_path()
            .to_path_buf();
        let mut doc = AutoCommit::load(&std::fs::read(&path).unwrap()).unwrap();
        let mut bytes = doc.save();
        doc.put(ROOT, "body", "later truncated history").unwrap();
        let increment = doc.save_incremental();
        assert!(increment.len() > 8);
        bytes.extend_from_slice(&increment[..increment.len() - 3]);
        std::fs::write(&path, &bytes).unwrap();
        let store = canonical_store(&root);
        let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "must never seed rejected history".into(),
            opens: None,
        })));
        host.bind_record(store.clone(), checkout.clone());
        for _ in 0..2 {
            host.sync(&AutoCommit::new(), &ROOT);
            store.tick();
            store.flush();
            assert!(
                host.history_owner().is_none(),
                "checked canonical admission must not repair and publish a partial graph"
            );
            assert!(!store.is_ready(&checkout.id));
            assert!(
                store.save_status(&checkout.id).is_err(),
                "no writer may be admitted"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        assert_eq!(
            host.accessibility().and_then(|node| node.name),
            Some(record_unavailable_title())
        );
    }

    // Case `outgoing_record_intent_keeps_failed_owner_and_unrelated_pending_record`.
    {
        use automerge::{ReadDoc, transaction::Transactable};
        for reject in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("records");
            let store = canonical_store(&root);
            let a = tab_checkout(3980);
            let sibling = tab_checkout(3981);
            let file = temp.path().join("next.txt");
            let b = RecordCheckout::new(
                RecordId::for_file(&file),
                crate::record::RecordKind::new("file"),
            );
            for checkout in [&a, &sibling] {
                store
                    .reserve_record(checkout)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
            }
            if !reject {
                store
                    .reserve_record(&b)
                    .unwrap()
                    .initialize(|_| Ok(()))
                    .unwrap();
            }
            store
                .with(&a.id, |doc| {
                    doc.put(ROOT, "body", "durable baseline").unwrap();
                })
                .unwrap();
            store.try_flush_record(&a.id).unwrap();
            store
                .with(&sibling.id, |doc| {
                    doc.put(ROOT, "pending", "sibling").unwrap();
                })
                .unwrap();
            let pending = store
                .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap();
            let path = app_state::DocStore::room_record_dir_at(
                &root,
                "so.tilo.tile-host-test",
                "room",
                &a.id.storage_key(),
            )
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: "unused".into(),
                opens: Some(file),
            })));
            host.bind_record(store.clone(), a.clone());
            let old = host.history_owner().unwrap();
            store
                .with(&a.id, |doc| {
                    doc.put(ROOT, "body", "unflushed A").unwrap();
                })
                .unwrap();
            assert_eq!(host.resolve_record().is_err(), reject);
            assert!(old.validate().is_err());
            drop(old);
            let mut reports = crate::component_lifecycle::take_component_close_reports(&mut host);
            assert_eq!(
                reports.len(),
                1,
                "intent change must settle the outgoing owner"
            );
            let mut report = reports.pop().unwrap();
            assert!(report.record_error.is_some());
            assert!(report.record_retry.as_ref().unwrap().same_owner(&store));
            assert_eq!(report.record_retry_target.as_ref(), Some(&a.id));
            let _ = host.resolve_record();
            assert!(crate::component_lifecycle::take_component_close_reports(&mut host).is_empty());
            drop(host);
            drop(store);
            std::fs::remove_dir(&path).unwrap();
            let retained = report.record_retry.as_ref().unwrap().clone();
            report.retry_record().unwrap();
            assert_eq!(
                retained
                    .read(&sibling.id, |doc| (doc.base_heads(), doc.pending_ops()))
                    .unwrap(),
                pending
            );
            drop(retained);
            let reopened = canonical_store(&root);
            reopened.try_open_record(&a).unwrap();
            assert_eq!(
                reopened
                    .read(&a.id, |doc| doc
                        .get(ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_owned())
                    .unwrap(),
                "unflushed A"
            );
        }
    }

    // Case `outgoing_settlement_is_idempotent_but_new_edits_require_another_close`.
    {
        use automerge::transaction::Transactable;
        let temp = tempfile::tempdir().unwrap();
        let store = canonical_store(temp.path());
        let checkout = tab_checkout(3982);
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
        host.bind_record(store.clone(), checkout.clone());
        store
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "first").unwrap();
            })
            .unwrap();
        host.replace_content(declared_probe(&DECLARED_HISTORY, false));
        let reports = crate::component_lifecycle::take_component_close_reports(&mut host);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].record_error.is_none());
        let settled = store
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert_eq!(settled.1, 0);
        host.bind_record(store.clone(), checkout.clone());
        assert!(crate::component_lifecycle::close_components(&mut host).is_empty());
        assert!(crate::component_lifecycle::close_components(&mut host).is_empty());
        assert_eq!(
            store
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            settled
        );
        host.replace_content(declared_probe(&DECLARED_HISTORY, false));
        store
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "later accepted edit").unwrap();
            })
            .unwrap();
        assert!(store.read(&checkout.id, |doc| doc.pending_ops()).unwrap() > 0);
        let reports = crate::component_lifecycle::close_components(&mut host);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].record_error.is_none());
        let later = store
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert_ne!(later.0, settled.0);
        assert_eq!(later.1, 0);
        assert!(crate::component_lifecycle::close_components(&mut host).is_empty());
    }
}

struct DeclaredHistoryProbe {
    definition: &'static crate::history_mode::HistoryDefinition,
    existing: bool,
    focused: bool,
}
static DECLARED_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: "so.tilo.tile-host-test",
        scopes: &[crate::history_mode::HistoryScopeDefinition {
            key: "body",
            title: "Body",
            content_schema: None,
            presentation: crate::history_mode::HistoryPresentation::Domain {
                create: |_| Err("Preview is not needed for custody admission tests".into()),
            },
        }],
    };
static FOREIGN_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: "foreign.app",
        scopes: DECLARED_HISTORY.scopes,
    };
static EXISTING_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: "so.tilo.tile-host-test",
        scopes: &[crate::history_mode::HistoryScopeDefinition {
            key: "external",
            title: "External",
            content_schema: None,
            presentation: crate::history_mode::HistoryPresentation::External {
                reason: "existing provider",
            },
        }],
    };
impl Widget for DeclaredHistoryProbe {
    crate::provides!(dyn crate::history_mode::capability::DocumentHistory);

    fn name(&self) -> &str {
        "Declared history probe"
    }
    fn test_id(&self) -> Option<&str> {
        Some("declared-probe")
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.leaf_node(Style::default());
        nodes.insert(node, self as *mut dyn Widget);
        node
    }
}

impl crate::history_mode::capability::DocumentHistory for DeclaredHistoryProbe {
    fn history_registration(
        &self,
    ) -> Result<
        Option<crate::history_mode::HistoryRegistration>,
        crate::history_mode::HistoryRegistrationError,
    > {
        if !self.existing {
            return Ok(None);
        }
        crate::history_mode::HistoryRegistration::new(
            &EXISTING_HISTORY,
            vec![
                crate::history_mode::HistorySourceRegistration::Unversioned {
                    scope: "external",
                    reason: "existing provider",
                },
            ],
        )
        .map(Some)
    }
    fn document_history_definition(
        &self,
    ) -> Option<&'static crate::history_mode::HistoryDefinition> {
        Some(self.definition)
    }
}
impl crate::widget::AppWidget for DeclaredHistoryProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Declared history probe"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("H")
    }
}
fn declared_probe(
    definition: &'static crate::history_mode::HistoryDefinition,
    existing: bool,
) -> TileContent {
    TileContent::App(Box::new(DeclaredHistoryProbe {
        definition,
        existing,
        focused: true,
    }))
}

#[test]
fn declared_history_binds_to_one_owner_for_its_lifetime() {
    // Case `declared_history_binds_once_and_replaces_its_actual_owner`.
    {
        let temp = tempfile::tempdir().unwrap();
        let first = canonical_store(&temp.path().join("first"));
        let second = canonical_store(&temp.path().join("second"));
        let checkout = tab_checkout(3910);
        for store in [&first, &second] {
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
        }
        let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
        assert!(host.history.is_none());
        host.bind_record(first.clone(), checkout.clone());
        let initial = host.history_owner().unwrap();
        assert!(host.automatic_history);
        host.bind_record(first, checkout.clone());
        assert!(initial.same_owner(&host.history_owner().unwrap()));
        host.replace_content(declared_probe(&DECLARED_HISTORY, false));
        assert!(initial.validate().is_err());
        assert!(
            host.history.is_some(),
            "replacement must consume the new root declaration"
        );
        let replaced = host.history_owner().unwrap();
        host.bind_record(second, checkout);
        assert!(replaced.validate().is_err());
        assert!(host.history.as_ref().unwrap().validate().is_ok());
        let mut count = 0;
        crate::history_mode::capability::visit_framework_history(&host, &mut |_, _| count += 1);
        assert_eq!(count, 1);
    }

    // Case `declared_history_waits_for_deferred_record_admission`.
    {
        let temp = tempfile::tempdir().unwrap();
        let store = canonical_store(&temp.path().join("records"));
        let checkout = tab_checkout(3913);
        let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
        host.bind_record(store.clone(), checkout.clone());
        assert!(host.history.is_none());
        assert!(host.history_owner().is_none());
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        host.sync(&AutoCommit::new(), &ROOT);
        assert!(host.record_error.is_none());
        assert!(host.automatic_history);
        let owner = host.history_owner().unwrap();
        host.sync(&AutoCommit::new(), &ROOT);
        assert!(owner.same_owner(&host.history_owner().unwrap()));
        let mut count = 0;
        crate::history_mode::capability::visit_framework_history(&host, &mut |_, _| count += 1);
        assert_eq!(count, 1);
    }

    // Case `declared_history_rebind_cannot_restore_focus_into_same_named_new_owner`.
    {
        use crate::widget::layout::CompositionFocus;
        let temp = tempfile::tempdir().unwrap();
        let first = canonical_store(&temp.path().join("first"));
        let second = canonical_store(&temp.path().join("second"));
        let checkout = tab_checkout(3912);
        for store in [&first, &second] {
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
        }
        let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
        host.bind_record(first, checkout.clone());
        let mut tree = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let root = host.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&host, &tree, root, &nodes);
        }
        let target = nodes
            .iter()
            .find_map(|(node, pointer)| {
                (unsafe { &**pointer }.test_id() == Some("declared-probe")).then_some(*node)
            })
            .unwrap();
        let focus = unsafe { CompositionFocus::capture(Some(target), &nodes, &bindings) }.unwrap();
        assert_eq!(
            unsafe { focus.restore(&nodes, &bindings, None) },
            Some(target)
        );
        nodes.clear();
        bindings.clear();
        host.bind_record(second, checkout);
        tree = TaffyTree::new();
        let root = host.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        unsafe {
            bindings.rebuild(&host, &tree, root, &nodes);
        }
        assert!(unsafe { focus.restore(&nodes, &bindings, None) }.is_none());
    }

    // Case `declared_history_refuses_foreign_custody_and_conflicting_root_provider`.
    {
        for (definition, existing) in [(&FOREIGN_HISTORY, false), (&DECLARED_HISTORY, true)] {
            let temp = tempfile::tempdir().unwrap();
            let store = canonical_store(&temp.path().join("records"));
            let checkout = tab_checkout(3911);
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
            let mut host = TileHost::new(declared_probe(definition, existing));
            host.bind_record(store, checkout);
            assert!(host.record_error.is_some());
            assert!(host.history_owner().is_none());
            assert!(!host.automatic_history);
            for _ in 0..2 {
                host.sync(&AutoCommit::new(), &ROOT);
                host.update(&AutoCommit::new(), &ROOT);
                assert!(
                    host.record_error.is_some(),
                    "refusal must survive ordinary frames"
                );
                assert!(host.history_owner().is_none());
            }
            if existing {
                assert!(host.record_error.as_ref().unwrap().contains("Conflicting"));
            }
        }
    }

    // Case `history_owner_lifetime_expires_on_replacement_close_and_store_rebind`.
    {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = canonical_store(&first_dir.path().join("records"));
        let second = canonical_store(&second_dir.path().join("records"));
        let checkout = tab_checkout(3902);
        for store in [&first, &second] {
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
        }
        let content = || {
            TileContent::App(Box::new(Scribe {
                text: "live".into(),
                opens: None,
            }))
        };
        let mut host = TileHost::new(content());
        host.bind_record(first.clone(), checkout.clone());
        let original = host.history_owner().unwrap();
        host.bind_record(first, checkout.clone());
        assert!(original.same_owner(&host.history_owner().unwrap()));
        assert!(original.validate().is_ok());
        host.bind_record(second.clone(), checkout.clone());
        assert!(original.validate().is_err());
        let rebound = host.history_owner().unwrap();
        host.replace_content(content());
        assert!(rebound.validate().is_err());
        let replacement = host.history_owner().unwrap();
        let _ = crate::component_lifecycle::close_components(&mut host);
        assert!(replacement.validate().is_err());
        assert!(host.history_owner().is_none());
        host.replace_content(content());
        let before_missing = host.history_owner().unwrap();
        host.bind_record(second, tab_checkout(999999));
        assert!(before_missing.validate().is_err());
        assert!(host.history_owner().is_none());
        let mut dropped = TileHost::new(content());
        let store = canonical_store(&first_dir.path().join("third"));
        dropped.bind_or_create_record(store, checkout);
        let witness = dropped.history_owner().unwrap();
        drop(dropped);
        assert!(witness.validate().is_err());
    }
}

#[test]
fn record_rebinding_revalidates_store_and_kind() {
    // Case `same_record_id_rebinding_revalidates_store_and_checkout`.
    {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = canonical_store(&first_dir.path().join("records"));
        let second = canonical_store(&second_dir.path().join("records"));
        let checkout = tab_checkout(3901);
        for store in [&first, &second] {
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|_| Ok(()))
                .unwrap();
        }
        let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "first owner".into(),
            opens: None,
        })));
        host.bind_record(first.clone(), checkout.clone());
        host.sync(&AutoCommit::new(), &ROOT);
        host.bind_record(second.clone(), checkout.clone());
        assert!(
            host.binding.as_ref().unwrap().store.same_owner(&second),
            "same spelling in a new store must replace actual custody"
        );
        let wrong =
            RecordCheckout::new(checkout.id.clone(), crate::record::RecordKind::new("wrong"));
        host.bind_record(second, wrong);
        assert!(
            host.binding.is_none(),
            "same ID must not bypass kind validation"
        );
        assert!(host.record_error.is_some());
    }

    // Case `incompatible_canonical_kind_keeps_the_scratch_binding_and_surfaces_error`.
    {
        let directory = tempfile::tempdir().unwrap();
        let store = canonical_store(&directory.path().join("records"));
        let file = std::path::PathBuf::from("/work/report.txt");
        let file_id = RecordId::for_file(&file);
        let wrong = RecordCheckout::new(
            file_id.clone(),
            crate::record::RecordKind::new("different-kind"),
        );
        store
            .reserve_record(&wrong)
            .unwrap()
            .initialize(|doc| {
                use automerge::transaction::Transactable;
                doc.put(ROOT, "body", "the other kind's body")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let scratch = tab_checkout(91);
        let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "kept off another kind".into(),
            opens: Some(file),
        })));
        host.bind_or_create_record(store.clone(), scratch.clone());
        host.sync(&AutoCommit::new(), &ROOT);

        assert_eq!(host.binding.as_ref().unwrap().live, scratch.id);
        assert!(
            host.record_error
                .as_deref()
                .is_some_and(|error| error.contains("identity does not match"))
        );
        assert_eq!(
            host.accessibility().and_then(|node| node.name),
            Some(record_unavailable_title())
        );
        assert_ne!(
            seen_by("kept off another kind"),
            Some(String::from("the other kind's body")),
            "the app is never handed a record of another kind"
        );
        let driver =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(host), (400, 240));
        let inspection = driver.inspection();
        assert!(inspection_has_semantic(
            &inspection.root,
            &record_unavailable_title()
        ));
        assert!(
            !inspection_has_semantic(&inspection.root, "Seeded hidden document"),
            "the complete projected tree must omit the app hidden by the error card"
        );
    }

    // Case `unavailable_canonical_store_keeps_pending_binding_and_recovers_on_retry`.
    {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("records");
        let store = canonical_store(&root);
        std::fs::write(&root, "temporarily not a directory").unwrap();
        let scratch = tab_checkout(92);
        let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "recovered".into(),
            opens: None,
        })));
        host.bind_or_create_record(store.clone(), scratch.clone());
        assert!(host.binding.is_none());
        assert!(host.pending_binding.is_some());
        assert!(host.record_error.is_some());
        assert_eq!(
            host.accessibility().and_then(|node| node.name),
            Some(record_unavailable_title())
        );

        std::fs::remove_file(&root).unwrap();
        host.sync(&AutoCommit::new(), &ROOT);
        assert!(host.pending_binding.is_none());
        assert!(host.record_error.is_none());
        assert_eq!(host.binding.as_ref().unwrap().live, scratch.id);
        assert_eq!(
            seen_by("recovered"),
            Some(String::new()),
            "the app is handed the recovered scratch record"
        );
    }

    // Case `existing_follow_validates_kind_even_when_record_id_does_not_change`.
    {
        let store = RecordStore::in_memory();
        let file = std::path::PathBuf::from("/work/kind-conflict");
        let checkout = RecordCheckout::new(
            RecordId::for_file(&file),
            crate::record::RecordKind::new("note"),
        );
        let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "kept off a conflicting kind".into(),
            opens: Some(file),
        })));
        host.bind_or_create_record(store.clone(), checkout.clone());
        host.sync(&AutoCommit::new(), &ROOT);
        assert!(host.record_error.is_some());
        assert_eq!(host.binding.as_ref().unwrap().live, checkout.id);
        assert_eq!(
            seen_by("kept off a conflicting kind"),
            None,
            "an app whose record conflicts is shown nothing"
        );
    }
}

struct EventProbe(std::rc::Rc<std::cell::Cell<usize>>);

impl Widget for EventProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        self.0.set(self.0.get().checked_add(1).unwrap());
    }
    fn name(&self) -> &'static str {
        "EventProbe"
    }
}

impl crate::widget::app_widget::AppWidget for EventProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Event probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("E")
    }
}

/// An app with a History of its own (as an embedded App is): records
/// where the window told it its controls are.
struct OwnHistoryProbe(std::rc::Rc<std::cell::Cell<Option<bool>>>);

impl Widget for OwnHistoryProbe {
    crate::provides!(dyn crate::widget::WindowHistory);
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "OwnHistoryProbe"
    }
}

impl crate::widget::WindowHistory for OwnHistoryProbe {
    fn set_window_presents_history(&mut self, tab_presents: bool) {
        self.0.set(Some(tab_presents));
    }
    fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        Some(crate::widget::HistoryControl::Closed)
    }
    fn invoke_window_history_control(
        &mut self,
        _control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        Ok(())
    }
}

impl crate::widget::app_widget::AppWidget for OwnHistoryProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Own history probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("H")
    }
}

// An app that replaces a tile's content (its placeholder, an earlier
// app) hears where the window presents its History and search, so it
// draws neither beside the bar's.
#[test]
fn an_app_that_replaces_a_tiles_content_hears_the_window_presents_its_controls() {
    let first = std::rc::Rc::new(std::cell::Cell::new(None));
    let mut host = TileHost::new(TileContent::App(Box::new(OwnHistoryProbe(
        std::rc::Rc::clone(&first),
    ))));
    host.set_window_presents_history(true);
    assert_eq!(first.get(), Some(true));
    let second = std::rc::Rc::new(std::cell::Cell::new(None));
    host.replace_content(TileContent::App(Box::new(OwnHistoryProbe(
        std::rc::Rc::clone(&second),
    ))));
    assert_eq!(
        second.get(),
        Some(true),
        "the new app is told what the old one was"
    );
}

/// Records the window a panic hook would name while it lays out.
struct ScopeProbe(std::rc::Rc<std::cell::RefCell<Vec<Option<u64>>>>);

impl Widget for ScopeProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.0
            .borrow_mut()
            .push(super::super::panic_scope::current());
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "ScopeProbe"
    }
}

impl crate::widget::app_widget::AppWidget for ScopeProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Scope probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

#[test]
fn a_windows_application_runs_under_its_tab_for_a_panic_hook() {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let host = TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
    let mut group = crate::widget::WindowGroup::from_hosts(
        3,
        vec![(crate::widget::TabDescriptor::new(41, "Probe"), host)],
        41,
        crate::widget::WindowWidth::Fraction(500),
    );
    // A host swapped in after the group was built is named too.
    *group.host_for_tab_mut(41).unwrap() =
        TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
    let mut taffy = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    for child in group.children_mut() {
        child.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    }
    assert_eq!(*seen.borrow(), [Some(41)]);
    assert_eq!(super::super::panic_scope::current(), None);
}

#[test]
fn tiles_follow_the_record_their_app_names() {
    // Case `a_tile_follows_the_record_its_app_names`.
    // A tab with no file is its own record; a tab whose app has a file
    // open is that file's.
    {
        let store = RecordStore::in_memory();
        let parent = AutoCommit::new();

        let mut scratch = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "unsaved".into(),
            opens: None,
        })));
        scratch.bind_or_create_record(store.clone(), tab_checkout(1));
        write_body(&store, &RecordId::for_tab(1), "the first tab");
        scratch.sync(&parent, &ROOT);
        assert_eq!(
            seen_by("unsaved"),
            Some(String::from("the first tab")),
            "a tab with no file shows the record the tab is",
        );

        let file = std::path::PathBuf::from("/work/report.odt");
        store.open_record(&RecordCheckout::new(
            RecordId::for_file(&file),
            crate::record::RecordKind::new("file"),
        ));
        let mut opened = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "the report".into(),
            opens: Some(file.clone()),
        })));
        write_body(&store, &RecordId::for_file(&file), "the report's body");
        opened.bind_or_create_record(store.clone(), tab_checkout(2));
        write_body(&store, &RecordId::for_tab(2), "the second tab");
        opened.sync(&parent, &ROOT);
        assert_eq!(
            seen_by("the report"),
            Some(String::from("the report's body")),
            "an app that has a file open shows that file's record, \
             not the scratch record its tab started on",
        );
        assert_eq!(
            seen_by("unsaved"),
            Some(String::from("the first tab")),
            "the other tab is untouched by any of it",
        );
    }

    // Case `explicit_scratch_creation_does_not_authorize_missing_file_follow`.
    {
        let store = RecordStore::in_memory();
        let file = std::path::PathBuf::from("/work/missing.txt");
        let file_id = RecordId::for_file(&file);
        let scratch = tab_checkout(94);
        let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
            text: "never creates the file's record".into(),
            opens: Some(file),
        })));
        host.bind_or_create_record(store.clone(), scratch.clone());
        assert!(store.open_existing_record_checked(&scratch).is_ok());
        host.sync(&AutoCommit::new(), &ROOT);
        assert_eq!(host.binding.as_ref().unwrap().live, scratch.id);
        assert!(host.record_error.is_some());
        assert!(
            !store
                .open_records()
                .iter()
                .any(|checkout| checkout.id == file_id)
        );
    }

    // Case `two_tabs_on_one_file_are_one_document`.
    // Two tabs opened on one file are one document — a copy would
    // diverge, and no amount of merging makes two copies one file again.
    {
        let store = RecordStore::in_memory();
        let parent = AutoCommit::new();
        let file = std::path::PathBuf::from("/work/shared.odt");
        store.open_record(&RecordCheckout::new(
            RecordId::for_file(&file),
            crate::record::RecordKind::new("file"),
        ));
        write_body(&store, &RecordId::for_file(&file), "one shared body");
        for (tab, text) in [(3u64, "first"), (4u64, "second")] {
            let mut host = TileHost::new(TileContent::App(Box::new(Scribe {
                text: text.into(),
                opens: Some(file.clone()),
            })));
            host.bind_or_create_record(store.clone(), tab_checkout(tab));
            host.sync(&parent, &ROOT);
        }
        assert_eq!(
            store.open_records().len(),
            3,
            "two scratch records and the one file both tabs opened",
        );
        for text in ["first", "second"] {
            assert_eq!(
                seen_by(text),
                Some(String::from("one shared body")),
                "both tabs show the one document",
            );
        }
    }
}

/// Backdate the spawn so its fade window has already elapsed.
fn expire_spawn_animation(h: &mut TileHost) {
    h.spawned_at = crate::time::now().checked_sub(spawn_time() + Duration::from_millis(20));
}

fn stub_host() -> TileHost {
    TileHost::new(TileContent::Stub {
        kind: AppKind::Terminal,
        label: "t".into(),
    })
}

#[test]
fn spawn_animation_is_in_flight_only_for_its_duration() {
    // Fresh curves are read in the instant they start: a descheduled
    // runner cannot age them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        {
            let mut h = stub_host();
            assert!(h.spawned_at.is_none(), "fresh host has no spawn timestamp");
            h.start_spawn_animation();
            assert!(
                h.spawned_at.is_some(),
                "start_spawn_animation must record an Instant",
            );
        }

        {
            let mut h = stub_host();
            h.start_spawn_animation();
            assert!(
                h.spawn_in_flight(),
                "spawn_in_flight must be true within SPAWN_FADE_MS",
            );
            let alpha = h.spawn_alpha().expect("alpha in flight");
            assert!(
                (0.0..1.0).contains(&alpha),
                "eased alpha mid-flight must be in [0, 1); got {alpha}",
            );
        }

        {
            let mut h = stub_host();
            h.start_spawn_animation();
            expire_spawn_animation(&mut h);
            assert!(
                !h.spawn_in_flight(),
                "spawn_in_flight must be false after SPAWN_FADE_MS elapses",
            );
            assert!(
                h.spawn_alpha().is_none(),
                "expired spawn must report no alpha",
            );
        }

        {
            // Indirect: no direct hook to check the draw path skips the
            // wrapper layer, so this pins spawn_in_flight/spawn_alpha instead
            // — guards against the wrapper becoming always-on and costing an
            // extra layer + affine compose every frame.
            let h = stub_host();
            assert!(!h.spawn_in_flight(), "fresh host must not be in flight");
            assert!(
                h.spawn_alpha().is_none(),
                "fresh host must report no spawn alpha",
            );
        }

        {
            let mut h = stub_host();
            assert!(
                !h.needs_animation_frame(),
                "static host must not demand animation frames",
            );
            h.start_spawn_animation();
            assert!(
                h.needs_animation_frame(),
                "in-flight spawn must pump the App run loop",
            );
            expire_spawn_animation(&mut h);
            assert!(
                !h.needs_animation_frame(),
                "after the spawn window elapses, no more frames are needed",
            );
        }
    });
}

/// Fills its entire rect with one flat opaque colour, exactly as
/// `FileExplorerWidget::paint_background` and every other hosted
/// app's background pass does. Module-scoped (rather than nested in
/// its one test) purely to keep that test's line count sane.
struct FloodFill {
    rect: Rect,
}
const FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);
impl Widget for FloodFill {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            FLOOD,
            None,
            &self.rect,
        );
    }
    fn name(&self) -> &'static str {
        "FloodFill"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
impl crate::widget::app_widget::AppWidget for FloodFill {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "FloodFill"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("F")
    }
}

/// Module-scoped alongside [`FloodFill`] purely to keep its one test's
/// line count sane.
struct Flood {
    rect: Rect,
    color: vello::peniko::Color,
}
impl Widget for Flood {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.color,
            None,
            &self.rect,
        );
    }
    fn name(&self) -> &'static str {
        "Flood"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
impl crate::widget::app_widget::AppWidget for Flood {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Flood"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("F")
    }
}

/// An app that has been asked for and has not drawn: the two things
/// its widget is allowed to say about itself.
struct Starting {
    rect: Rect,
    drawn: std::rc::Rc<std::cell::Cell<bool>>,
    presented: bool,
}

impl Widget for Starting {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &Theme,
        _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
        self.drawn.set(true);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            STARTING_FLOOD,
            None,
            &self.rect,
        );
    }
    fn name(&self) -> &'static str {
        "Starting"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn presented(&self) -> bool {
        self.presented
    }
}

impl crate::widget::app_widget::AppWidget for Starting {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Starting"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

/// A presented app that remembers the transform it was drawn with.
struct TransformProbe {
    rect: Rect,
    drawn_with: std::rc::Rc<std::cell::Cell<Option<Affine>>>,
}

impl Widget for TransformProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &Theme,
        _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
        self.drawn_with.set(Some(transform));
    }
    fn name(&self) -> &'static str {
        "TransformProbe"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl crate::widget::app_widget::AppWidget for TransformProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Transform probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("T")
    }
}

#[test]
fn a_moved_tile_draws_its_app_where_the_tile_went() {
    let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
    let drawn_with = std::rc::Rc::new(std::cell::Cell::new(None));
    let mut host = TileHost::new(TileContent::App(Box::new(TransformProbe {
        rect,
        drawn_with: std::rc::Rc::clone(&drawn_with),
    })));
    host.rect = rect;
    let theme = Theme::default();
    let _ = paint(&mut host, &theme);
    assert_eq!(drawn_with.get(), Some(Affine::IDENTITY));

    host.offset_layout(40.0);
    assert_eq!(host.rect, rect + vello::kurbo::Vec2::new(40.0, 0.0));
    let _ = paint(&mut host, &theme);
    assert_eq!(
        drawn_with.get(),
        Some(Affine::translate((40.0, 0.0))),
        "the app keeps its layout rect and is drawn translated with the tile"
    );

    host.offset_layout(-40.0);
    assert_eq!(host.rect, rect);
    let _ = paint(&mut host, &theme);
    assert_eq!(drawn_with.get(), Some(Affine::IDENTITY));
}

/// Loud and unmistakable, so a single sampled pixel says whether the
/// app painted.
const STARTING_FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);

pub(super) fn starting_host(presented: bool) -> (TileHost, std::rc::Rc<std::cell::Cell<bool>>) {
    let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
    let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut host = TileHost::new(TileContent::App(Box::new(Starting {
        rect,
        drawn: std::rc::Rc::clone(&drawn),
        presented,
    })));
    host.rect = rect;
    (host, drawn)
}

pub(super) fn paint(host: &mut TileHost, theme: &Theme) -> Scene {
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    host.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        &PresenceCtx::empty(),
    );
    scene
}

#[test]
fn symbolic_notices_do_not_acquire_scratch_record_custody() {
    let store = RecordStore::in_memory();
    let checkout = tab_checkout(7120);
    let mut host = TileHost::new(TileContent::Notice {
        title: "Browser".into(),
        detail: "Opening here…".into(),
    });
    host.rect = Rect::new(20.0, 20.0, 480.0, 300.0);
    for create in [false, true, false] {
        if create {
            host.bind_or_create_record(store.clone(), checkout.clone());
        } else {
            host.bind_record(store.clone(), checkout.clone());
        }
        host.sync(&AutoCommit::new(), &ROOT);
        assert!(host.binding.is_none());
        assert!(host.pending_binding.is_none());
        assert!(
            host.record_error.is_none(),
            "notice must retain its launch status"
        );
        assert!(host.history_owner().is_none());
        assert!(
            store.open_records().is_empty(),
            "a notice owns no scratch document"
        );
        assert!(Widget::presented(&host));
        let _ = paint(&mut host, &Theme::default());
    }

    // A genuine failed document creation remains refused on group rebind.
    host.refuse_record_binding("creation refused".into());
    host.bind_record(store.clone(), checkout.clone());
    assert!(host.record_creation_refused());
    assert_eq!(host.record_error.as_deref(), Some("creation refused"));

    // Explicit replacement with an external notice clears that old refusal.
    host.replace_content(TileContent::Notice {
        title: "Browser".into(),
        detail: "Could not open it here".into(),
    });
    host.bind_record(store, checkout);
    assert!(!host.record_creation_refused());
    assert!(host.record_error.is_none());
    assert!(host.binding.is_none());
}

#[test]
fn replacing_a_document_with_a_notice_detaches_the_record() {
    let store = RecordStore::in_memory();
    let checkout = tab_checkout(7121);
    store.create_record(&checkout).unwrap();
    let mut host = TileHost::new(declared_probe(&DECLARED_HISTORY, false));
    host.bind_record(store.clone(), checkout.clone());
    let old_owner = host.history_owner().expect("document history");
    host.replace_content(TileContent::Notice {
        title: "Browser".into(),
        detail: "Restoring this window…".into(),
    });
    assert!(
        old_owner.validate().is_err(),
        "outgoing document owner is retired"
    );
    assert!(host.binding.is_none());
    assert!(host.history_owner().is_none());
    assert!(host.record_error.is_none());
    assert!(
        store.open_records().contains(&checkout),
        "saved document survives"
    );
}

#[test]
fn an_arrived_app_after_a_visible_placeholder_still_asks_for_a_frame() {
    // Case `retiring_the_placeholder_keeps_one_paint_so_the_app_replaces_it`.
    let t0 = crate::time::now();
    let (mut host, _) = crate::time::testing::with_monotonic_time(t0, || starting_host(false));
    let theme = Theme::default();
    let later = t0 + crate::time::Duration::from_secs(1);
    crate::time::testing::with_monotonic_time(later, || {
        let _ = paint(&mut host, &theme);
        host.mark_drawn();
    });
    let TileContent::App(app) = &mut host.content else {
        panic!("the starting app");
    };
    app.as_any_mut()
        .and_then(|app| app.downcast_mut::<Starting>())
        .expect("controlled app")
        .presented = true;
    crate::time::testing::with_monotonic_time(later, || {
        assert!(
            Widget::needs_redraw(&host),
            "the person already saw the placeholder; without one more frame \
             the compositor sleeps on it (A05 Store)"
        );
    });
}

#[test]
fn a_tile_holds_its_place_opaquely_until_the_app_can_be_shown() {
    // Case `visible_notice_replacement_tracks_new_content_readiness_without_hiding_the_tile`.
    // On one instant: the replacement answers inside its frame budget, so
    // it shows at once, with no placeholder to outlast.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let theme = Theme::default();
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let mut host = TileHost::new(TileContent::Notice {
            title: "Calculator".into(),
            detail: "Restoring this window…".into(),
        });
        host.rect = rect;
        assert!(Widget::presented(&host));
        let _ = paint(&mut host, &theme);
        let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
        host.replace_content(TileContent::App(Box::new(Starting {
            rect,
            drawn: std::rc::Rc::clone(&drawn),
            presented: false,
        })));

        assert!(
            Widget::presented(&host),
            "an already visible tab must not disappear"
        );
        assert!(
            host.is_arriving(),
            "pending replacement content stands as an arrival at once"
        );
        let _ = paint(&mut host, &theme);
        assert!(
            !drawn.get(),
            "unready replacement must not draw behind its placeholder"
        );
        let TileContent::App(app) = &mut host.content else {
            panic!("replacement app");
        };
        app.as_any_mut()
            .and_then(|app| app.downcast_mut::<Starting>())
            .expect("controlled replacement")
            .presented = true;
        assert!(
            !host.is_arriving(),
            "the arrival observes new content readiness without a parent presentation poll"
        );
        assert!(Widget::presented(&host));
        let _ = paint(&mut host, &theme);
        assert!(drawn.get(), "ready replacement must become visible");

        let TileContent::App(app) = &mut host.content else {
            panic!("replacement app");
        };
        let app = app
            .as_any_mut()
            .and_then(|app| app.downcast_mut::<Starting>())
            .expect("controlled replacement");
        app.presented = false;
        assert!(Widget::presented(&host));
        assert!(
            !host.is_arriving(),
            "the same content resizing must not stand for an arrival again"
        );
    });

    // Case `a_tile_holding_a_place_does_not_paint_the_app_behind_it`.
    // An unpresented app has only its own early buffer size, so painting
    // it is the window appearing magnified then snapping down. The frame
    // holds the place instead, since it's the one thing that knows the
    // tab's size.
    {
        let theme = Theme::default();
        let (mut host, drawn) = starting_host(false);
        let _ = paint(&mut host, &theme);
        assert!(
            !drawn.get(),
            "the app painted into a tile that is still holding its place; \
         its buffer is not the size of the tab yet, and putting it on \
         screen is the resize-after-open this gate exists to prevent",
        );
    }

    // Case `a_tile_paints_the_app_as_soon_as_it_has_something_to_show`.
    // And the moment it has something to show, it is what is shown.
    {
        let theme = Theme::default();
        let (mut host, drawn) = starting_host(true);
        let _ = paint(&mut host, &theme);
        assert!(
            drawn.get(),
            "an app with a frame that fits its tab must be what the tile \
         paints; anything else is a placeholder standing in front of a \
         window that is already there",
        );
    }

    // Case `a_static_tile_holding_a_place_does_not_pump_frames_forever`.
    // Holding a place is not itself an animation: the placeholder's steps
    // are deadlines, so a slow app's first frame never keeps the desktop
    // awake.
    {
        let (mut host, _) = starting_host(false);
        let _ = paint(&mut host, &Theme::default());
        assert!(
            !host.needs_animation_frame(),
            "arriving content alone must not turn a slow app into a full-rate compositor loop",
        );
        assert!(
            host.next_wake_deadline().is_some(),
            "its next step is a deadline"
        );
    }

    // Case `a_tile_that_has_shown_a_window_never_holds_a_place_again`.
    // A tile that has ever had a window in it never goes back to holding
    // a place for one — without the latch, a drag on a group edge would
    // flash a placeholder over a window the person is using.
    {
        let (mut host, _) = starting_host(true);
        assert!(Widget::presented(&host), "precondition: it presented");
        let TileContent::App(app) = &mut host.content else {
            panic!("the app");
        };
        // The app now says it has not drawn, as a client whose buffer
        // stopped matching its tab mid-resize would.
        app.as_any_mut()
            .and_then(|app| app.downcast_mut::<Starting>())
            .expect("controlled app")
            .presented = false;
        assert!(Widget::presented(&host));
        assert!(
            !host.is_arriving(),
            "a tile that has presented must never stand for an arrival again — \
         a window the person is looking at would be replaced by a placeholder",
        );
    }

    // Case `the_place_held_is_opaque`.
    // What is in the tile while the place is held is the frame's own
    // ground, not the desktop showing through.
    {
        use crate::offscreen::OffscreenRenderer;
        let theme = Theme::default();
        let (mut host, _) = starting_host(false);
        let scene = paint(&mut host, &theme);
        let mut r = OffscreenRenderer::new(200, 200);
        // A ground colour that appears nowhere else, so a pixel of it in
        // the middle of the tile means the ground did not cover it.
        let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));
        let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, 200, 100, 100);
        assert!(
            !(cr < 40 && cg < 40 && cb > 200),
            "the desktop showed through the middle of a window's own place \
         at (100,100) — got ({cr},{cg},{cb}), the backdrop colour. The \
         ground has to hold the space, or the window is a hole until it \
         arrives",
        );
        assert!(
            !(cr > 200 && cg < 80 && cb < 80),
            "the app painted after all; got ({cr},{cg},{cb})",
        );
    }
}

/// Native Shell dispatches directly into TileHost: unlike App and the
/// headless `FrameHarness`, it does not pre-observe embedded History pointers.
#[test]
fn live_body_pointer_edges_follow_motion_out_of_history_toolbar() {
    use taffy::prelude::TaffyMaxContent;
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};

    for scale in [1.0, 2.0] {
        let directory = tempfile::tempdir().unwrap();
        let store = canonical_store(&directory.path().join("records"));
        let checkout = tab_checkout(3914);
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut host = TileHost::new(TileContent::App(Box::new(EventProbe(calls.clone()))));
        host.bind_record(store, checkout);
        host.register_document_history(&DECLARED_HISTORY).unwrap();
        let mut tree = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let node = host.build_layout(&mut tree, &Theme::default(), &mut nodes);
        let mut style = tree.style(node).unwrap().clone();
        style.size = Size {
            width: Dimension::length(800.0),
            height: Dimension::length(480.0),
        };
        tree.set_style(node, style).unwrap();
        tree.compute_layout(node, Size::MAX_CONTENT).unwrap();
        host.apply_layout(&tree, node, Point::ORIGIN);
        let history = host.history.as_ref().unwrap();
        assert!(history.live_interactive());
        let toolbar = history.children().first().unwrap().get_rect();
        assert!(toolbar.width() > 0.0 && toolbar.height() > 0.0);
        let body = Point::new(toolbar.center().x, toolbar.y1 + 40.0);
        assert!(host.get_rect().contains(body) && !toolbar.contains(body));
        let device = winit::event::DeviceId::dummy();
        let motion = |point: Point| WindowEvent::CursorMoved {
            device_id: device,
            position: (point.x * scale, point.y * scale).into(),
        };
        let mut sender = crate::testing::Dispatch::new()
            .scale_factor(scale)
            .layout(&tree, node);
        let mut send = |event: &WindowEvent| {
            calls.set(0);
            let handled = sender.send(&mut host, event).handled;
            (handled, calls.get())
        };
        let wheel = WindowEvent::MouseWheel {
            device_id: device,
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        };
        assert_eq!(
            send(&motion(toolbar.center())),
            (true, 0),
            "toolbar owns its motion"
        );
        assert_eq!(
            send(&wheel),
            (true, 0),
            "toolbar input must not reach the app"
        );
        assert_eq!(
            send(&motion(body)),
            (false, 1),
            "live body receives pointer motion"
        );
        let button = |state| WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Left,
        };
        for (edge, event) in [
            ("press", button(ElementState::Pressed)),
            ("release", button(ElementState::Released)),
            ("wheel", wheel),
        ] {
            assert_eq!(
                send(&event),
                (false, 1),
                "live body must receive {edge} after leaving toolbar at scale {scale}"
            );
        }
        assert_eq!(
            send(&motion(toolbar.center())),
            (true, 0),
            "returning to toolbar restores its input ownership"
        );
    }
}

/// Every pixel an edge paints, focused or not, flush or not, at 100 %,
/// 150 % and 200 %, lies in the damage bands the edge part reports: a
/// focus change repaints them and nothing the edge drew outside them.
#[test]
fn edge_bands_hold_every_pixel_the_edge_paints() {
    use crate::offscreen::OffscreenRenderer;
    let theme = Theme::default();
    let ground = vello::peniko::Color::from_rgb8(0, 255, 0);
    let size = 320_u32;
    let mut renderer = OffscreenRenderer::new(size, size);
    let radius = f64::from(theme.tokens.radii.tile);
    for scale in [1.0, 1.5, 2.0] {
        for rect in [
            Rect::new(20.5, 18.0, 190.0, 200.25),
            Rect::new(30.0, 30.0, 30.0 + radius, 120.0),
        ] {
            for (focused, flush) in [(false, false), (true, false), (true, true)] {
                let mut host = TileHost::new(TileContent::Stub {
                    kind: AppKind::Editor,
                    label: String::new(),
                });
                host.rect = rect;
                host.set_flush_edges(flush);
                host.set_focused(focused);
                let transform = Affine::scale(scale);
                let mut scene = Scene::new();
                host.draw_edge(&mut scene, transform, &theme, radius);
                let bands: Vec<Rect> = edge_bands(rect, radius)
                    .into_iter()
                    .map(|band| transform.transform_rect_bbox(band))
                    .collect();
                let rgba = renderer.render(&scene, ground);
                let mut painted = 0;
                for y in 0..size {
                    for x in 0..size {
                        if OffscreenRenderer::sample(&rgba, size, x, y) == (0, 255, 0, 255) {
                            continue;
                        }
                        painted += 1;
                        let pixel = Rect::new(
                            f64::from(x),
                            f64::from(y),
                            f64::from(x) + 1.0,
                            f64::from(y) + 1.0,
                        );
                        assert!(
                            bands.iter().any(|band| band.overlaps(pixel)),
                            "edge pixel ({x},{y}) outside its bands: scale={scale}, \
                             rect={rect:?}, focused={focused}, flush={flush}, bands={bands:?}"
                        );
                    }
                }
                assert!(painted > 0, "the edge paints: {rect:?} at {scale}");
            }
        }
    }
}

#[test]
fn flush_focus_edge_leaves_rounded_corner_cutouts_clear() {
    use crate::offscreen::OffscreenRenderer;
    let theme = Theme::default();
    let ground = vello::peniko::Color::from_rgb8(0, 255, 0);
    let mut renderer = OffscreenRenderer::new(256, 256);
    for right in [62.0, 62.5, 100.0, 120.0] {
        for top in [0.0, 0.5, 16.0] {
            let rect = Rect::new(-400.0, top, right, 400.0);
            let mut host = TileHost::new(TileContent::Stub {
                kind: AppKind::Editor,
                label: String::new(),
            });
            host.rect = rect;
            host.set_flush_edges(true);
            host.set_focused(true);
            let radius = f64::from(theme.tokens.radii.tile);
            let mut scene = Scene::new();
            host.draw_edge(&mut scene, Affine::scale(2.0), &theme, radius);
            let rgba = renderer.render(&scene, ground);
            for y in 0..crate::num::u32_of_f64((top + radius) * 2.0) {
                for x in crate::num::u32_of_f64((right - radius) * 2.0)
                    ..crate::num::u32_of_f64(right * 2.0)
                {
                    let dx = (f64::from(x) + 0.5) / 2.0 - (right - radius);
                    let dy = (f64::from(y) + 0.5) / 2.0 - (top + radius);
                    if dx * dx + dy * dy > (radius + 1.0).powi(2) {
                        let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 256, x, y);
                        assert_eq!(
                            (r, g, b),
                            (0, 255, 0),
                            "focus paint outside corner: right={right}, top={top}, pixel=({x},{y})"
                        );
                    }
                }
            }
        }
    }
}

/// Focus recolours a window's edge and never moves its outline: the
/// focused window's top edge stands where its unfocused neighbour's
/// does, at 1x and 2x, so the window the pointer is on never looks
/// lifted above the others.
#[test]
fn focus_leaves_the_windows_outline_where_it_was() {
    use crate::offscreen::OffscreenRenderer;
    let theme = Theme::default();
    let ground = vello::peniko::Color::from_rgb8(0, 255, 0);
    let rect = Rect::new(20.0, 20.0, 100.0, 100.0);
    for scale in [1.0, 2.0] {
        let size = crate::num::u32_of_f64(120.0 * scale);
        let mut renderer = OffscreenRenderer::new(size, size);
        let mut shot = |focused: bool| {
            let mut host = TileHost::new(TileContent::Stub {
                kind: AppKind::Editor,
                label: String::new(),
            });
            host.rect = rect;
            host.set_focused(focused);
            let mut scene = Scene::new();
            host.draw(
                &mut scene,
                Affine::scale(scale),
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            renderer.render(&scene, ground)
        };
        let (lit, plain) = (shot(true), shot(false));
        // The topmost painted row above the card's middle, and the
        // leftmost painted column beside it: the outline's reach.
        let middle = crate::num::u32_of_f64(60.0 * scale);
        let reach = |rgba: &[u8]| {
            let painted = |x: u32, y: u32| {
                let (r, g, b, _) = OffscreenRenderer::sample(rgba, size, x, y);
                (r, g, b) != (0, 255, 0)
            };
            let top = (0..size).find(|&y| painted(middle, y));
            let left = (0..size).find(|&x| painted(x, middle));
            (top, left)
        };
        assert_eq!(
            reach(&lit),
            reach(&plain),
            "at {scale}x the focused window's outline (top row, left column) \
             differs from the unfocused one's: focus moved the window's edge"
        );
    }
}

/// A tile with History, alone and inside a desktop window.
fn history_tile(record: u64, directory: &std::path::Path) -> TileHost {
    let store = canonical_store(&directory.join("records"));
    let checkout = tab_checkout(record);
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut host = TileHost::new(TileContent::App(Box::new(EventProbe(calls))));
    host.bind_record(store, checkout);
    host.register_document_history(&DECLARED_HISTORY).unwrap();
    host
}

fn harness(root: Box<dyn Widget>) -> crate::headless::FrameHarness {
    crate::headless::FrameHarness::new_with_controlled_inputs(
        crate::State::new(),
        root,
        (900, 600),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::UserId("reader".into()),
    )
}

fn window_history_control(driver: &crate::headless::FrameHarness) -> Option<bool> {
    fn find(node: &crate::a11y::AccessNode, target: &str) -> Option<bool> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node.state.checked == Some(true));
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    fn walk(node: &crate::headless::InspectionNode, target: &str) -> Option<bool> {
        node.semantics
            .as_ref()
            .and_then(|access| find(access, target))
            .or_else(|| node.children.iter().find_map(|child| walk(child, target)))
    }
    // The window's History control acts on its active tab.
    walk(
        &driver.inspection().root,
        crate::widget::tabs::WINDOW_HISTORY_TARGET,
    )
}

#[test]
fn a_desktop_window_presents_history_on_its_tab_not_in_a_band() {
    use crate::widget::{TabDescriptor, WindowGroup, WindowWidth};
    let directory = tempfile::tempdir().unwrap();

    // Alone, the tile has nowhere else to put History: its own bar.
    let alone = harness(Box::new(history_tile(3920, &directory.path().join("a"))));
    assert!(alone.targets().contains_key("history-open"));

    // In a desktop window, the window's tab carries it, and the tile
    // draws no band (nor a header control) of its own.
    let tile = history_tile(3921, &directory.path().join("b"));
    let group = WindowGroup::from_hosts(
        1,
        vec![(TabDescriptor::new(7, "Chat"), tile)],
        7,
        WindowWidth::Normal,
    );
    let mut driver = harness(Box::new(group));
    let targets = driver.targets();
    assert!(
        !targets.contains_key("history-open")
            && !targets.contains_key(crate::widget::HISTORY_CONTROL_ID),
        "a hosted tile draws no History band: {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    assert_eq!(window_history_control(&driver), Some(false));

    // The tab's control opens History through the tile's History door,
    // and reads as on while it shows; pressed again, it closes.
    driver
        .perform_semantic_named(
            "tabs",
            Some(crate::widget::tabs::WINDOW_HISTORY_TARGET),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    driver.relayout();
    assert!(driver.targets().contains_key("activity_timeline"));
    assert_eq!(window_history_control(&driver), Some(true));
    assert!(
        !driver.targets().contains_key("history-open")
            && !driver.targets().contains_key("history-back-live"),
        "open, History draws no toggle or close of its own: the tab's closes it"
    );
    let press_tab = |driver: &mut crate::headless::FrameHarness| {
        driver
            .perform_semantic_named(
                "tabs",
                Some(crate::widget::tabs::WINDOW_HISTORY_TARGET),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        driver.relayout();
    };
    press_tab(&mut driver);
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert_eq!(window_history_control(&driver), Some(false));

    // Escape leaves History as well.
    press_tab(&mut driver);
    assert!(driver.targets().contains_key("activity_timeline"));
    driver.click_named("activity_timeline").unwrap();
    driver.send(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::Escape,
    ));
    driver.relayout();
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert_eq!(window_history_control(&driver), Some(false));
}

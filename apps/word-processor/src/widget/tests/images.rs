use super::*;

#[test]
fn inserted_images_lay_out_draw_and_honour_their_size() {
    {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "caption above");
        wp.model_mut()
            .insert_image_bytes(&mut edits(&mut state.doc), TEST_PNG);
        wp.sync(&state.doc, &state.root);
        let mut framed = mount(state, wp, 1000, 700);
        let (wp, _) = split(&mut framed);
        assert!(
            wp.laid.iter().any(|l| l.kind == "image"),
            "an image block was laid out"
        );
    }

    {
        let mut state = crate::initialized_state();
        with_doc(&mut state, |dm| {
            let hash = tilo_content::hash_bytes(TEST_PNG);
            dm.reference_content(&hash, &tilo_content::sniff(TEST_PNG));
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "caption");
            let img = dm.insert_block(Some(&p), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 120_i64);
        });
        let wp = Editor::detached();
        wp.model().content_store().put(TEST_PNG).unwrap();
        let mut framed = mount(state, wp, 1100, 900);
        let (wp, _) = split(&mut framed);

        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        assert!(
            (img.width - 200.0).abs() < 0.5,
            "image uses its explicit width, got {}",
            img.width
        );
        assert!(
            (img.height - 120.0).abs() < 0.5,
            "image uses its explicit height, got {}",
            img.height
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn floated_images_narrow_and_offset_the_following_paragraph() {
    {
        let mut state = crate::initialized_state();
        with_doc(&mut state, |dm| {
            let r = dm.insert_block(None, "paragraph");
            dm.splice_text(&r, 0, 0, "reference paragraph at full content width");
            let hash = tilo_content::hash_bytes(TEST_PNG);
            dm.reference_content(&hash, &tilo_content::sniff(TEST_PNG));
            let img = dm.insert_block(Some(&r), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "right");
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 160_i64);
            let beside = dm.insert_block(Some(&img), "paragraph");
            dm.splice_text(
                &beside,
                0,
                0,
                "this paragraph should flow beside the floated image to its left",
            );
        });
        let wp = Editor::detached();
        wp.model().content_store().put(TEST_PNG).unwrap();
        let mut framed = mount(state, wp, 1100, 900);
        let (wp, _) = split(&mut framed);

        let reference = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("reference"))
            .expect("ref laid");
        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        let beside = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("this paragraph"))
            .expect("wrapped para laid");

        let content_left = reference.text_x;
        let content_right = reference.text_x + reference.width;

        let img_right = img.text_x + img.width;
        assert!(
            (img_right - content_right).abs() < 1.0,
            "right-floated image hugs the content right edge: img_right={img_right} content_right={content_right}"
        );
        assert!(
            img.text_x > content_left + 1.0,
            "the image is offset toward the right, not at the left edge"
        );

        assert!(
            (beside.text_x - content_left).abs() < 0.5,
            "right-float text keeps its left x-origin: {} vs {}",
            beside.text_x,
            content_left
        );
        assert!(
            beside.width < reference.width - 100.0,
            "wrapped paragraph is narrowed beside the image: beside={} reference={}",
            beside.width,
            reference.width
        );
        assert!(
            beside.width <= content_right - content_left - 200.0 + 0.5,
            "narrowed by at least the image width"
        );
    }

    {
        let mut state = crate::initialized_state();
        with_doc(&mut state, |dm| {
            let r = dm.insert_block(None, "paragraph");
            dm.splice_text(&r, 0, 0, "reference paragraph at full content width");
            let hash = tilo_content::hash_bytes(TEST_PNG);
            dm.reference_content(&hash, &tilo_content::sniff(TEST_PNG));
            let img = dm.insert_block(Some(&r), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "left");
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 160_i64);
            let beside = dm.insert_block(Some(&img), "paragraph");
            dm.splice_text(
                &beside,
                0,
                0,
                "this paragraph should flow to the right of the floated image",
            );
        });
        let wp = Editor::detached();
        wp.model().content_store().put(TEST_PNG).unwrap();
        let mut framed = mount(state, wp, 1100, 900);
        let (wp, _) = split(&mut framed);

        let reference = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("reference"))
            .expect("ref laid");
        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        let beside = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("this paragraph"))
            .expect("wrapped para laid");

        let content_left = reference.text_x;
        assert!(
            (img.text_x - content_left).abs() < 1.0,
            "left-floated image hugs the content left edge"
        );
        assert!(
            beside.text_x > content_left + 199.0,
            "left-float text is offset right past the image: {} vs {}",
            beside.text_x,
            content_left
        );
        assert!(
            beside.width < reference.width - 100.0,
            "wrapped paragraph is narrowed beside the image"
        );
        assert!(
            beside.text_x + beside.width <= content_left + reference.width + 0.5,
            "narrowed text fits within the content column"
        );
    }
}

fn temp_png(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wp-img-{tag}-{}.png", std::process::id()));
    std::fs::write(&p, TEST_PNG).expect("write temp png");
    p
}

fn host_answering(outcome: portal::PortalOutcome) -> (ui::host::HostScope, ui::host::HostRecorder) {
    let (services, recorder) = ui::host::HostServices::recording();
    let scope = ui::host::enter(&services);
    recorder.respond_to_pick(outcome);
    (scope, recorder)
}

fn pick_titles(recorder: &ui::host::HostRecorder) -> Vec<String> {
    recorder
        .pick_requests()
        .iter()
        .map(|r| r.title().to_string())
        .collect()
}

fn image_block_count(wp: &Editor, state: &ui::State) -> usize {
    let c = wp.model().containers.clone().unwrap();
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .iter()
        .filter(|b| b.kind == "image")
        .count()
}

#[test]
fn the_image_portal_inserts_only_resolved_picks() {
    {
        let (mut state, mut wp) = bound();
        assert!(
            wp.portal_rx.is_none(),
            "no portal request before the button is pressed"
        );
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertImage);
        assert!(
            wp.portal_rx.is_some(),
            "the Image button opens the host portal chooser"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            0,
            "no image inserted until the portal returns a file"
        );
    }

    {
        let (mut state, mut wp) = bound();
        wp.model_mut()
            .insert_str(&mut edits(&mut state.doc), "before");
        let path = temp_png("pick");
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        let (_host, recorder) = host_answering(portal::PortalOutcome::Opened(path.clone()));
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertImage);
        work.run_pending();
        assert_eq!(pick_titles(&recorder), ["Insert image"]);
        let before = (state.doc.base_heads(), state.doc.pending_ops());
        wp.sync(&state.doc, &state.root);
        assert_eq!(
            (state.doc.base_heads(), state.doc.pending_ops()),
            before,
            "sync cannot apply the chooser's document action"
        );
        assert_eq!(image_block_count(&wp, &state), 0);
        wp.settle(&mut edits(&mut state.doc), &state.root);
        let _ = std::fs::remove_file(&path);
        assert!(
            wp.portal_rx.is_none(),
            "a resolved portal request clears the channel"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            1,
            "the picked file inserts one image block"
        );
    }

    {
        let (mut state, mut wp) = bound();
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        let (_host, _recorder) = host_answering(portal::PortalOutcome::Cancelled);
        wp.apply_tool(&mut edits(&mut state.doc), ToolAction::InsertImage);
        work.run_pending();
        let before = (state.doc.base_heads(), state.doc.pending_ops());
        wp.sync(&state.doc, &state.root);
        assert_eq!(
            (state.doc.base_heads(), state.doc.pending_ops()),
            before,
            "sync cannot apply the chooser's document action"
        );
        assert_eq!(image_block_count(&wp, &state), 0);
        wp.settle(&mut edits(&mut state.doc), &state.root);
        assert!(
            wp.portal_rx.is_none(),
            "a cancel clears the in-flight channel"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            0,
            "a cancel inserts nothing"
        );
    }
}

#[test]
fn dropped_and_selected_images_expose_their_controls() {
    {
        let mut framed = doc_with_headings(1000, 700);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        let target = wp
            .laid
            .iter()
            .find(|l| l.kind == "paragraph" && l.y_top > wp.rect.y0 + 200.0)
            .expect("a body paragraph to drop onto")
            .clone();
        let drop_pt = Point::new(target.text_x + 20.0, target.y_top + target.height / 2.0);

        let hovered = Dispatch::new().doc(&mut state.doc).path(&state.root).send(
            &mut Lent(wp),
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (drop_pt.x, drop_pt.y).into(),
            },
        );
        let path = temp_png("drop");
        Dispatch::new()
            .doc(&mut state.doc)
            .path(&state.root)
            .handled(hovered.handled)
            .focus_target(hovered.focus_target)
            .send(&mut Lent(wp), &WindowEvent::DroppedFile(path.clone()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            image_block_count(wp, state),
            1,
            "the dropped file inserts one image"
        );

        let c = wp.model().containers.clone().unwrap();
        let order: Vec<String> = tilo_doc::model::blocks_in_order(&state.doc, &c);
        let img_idx = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .position(|b| b.kind == "image")
            .and_then(|i| order.get(i).cloned());
        let target_idx = order
            .iter()
            .position(|id| id == &target.id)
            .expect("drop target in order");
        let img_pos = order
            .iter()
            .position(|id| Some(id) == img_idx.as_ref())
            .expect("image in order");
        assert!(
            img_pos == target_idx + 1 || img_pos == target_idx,
            "image inserted at the drop point (target idx {target_idx}, image idx {img_pos})"
        );
    }

    {
        let (mut wp, _id) = doc_with_selected_image();
        let wp = editor(&mut wp);
        assert!(wp.selected_image_id().is_some(), "the image is selected");
        let has = |want: &PanelHit| wp.panel_hits.iter().any(|(_, h)| h == want);
        assert!(
            wp.panel_hits
                .iter()
                .any(|(_, h)| matches!(h, PanelHit::ImageScale(_))),
            "resize steppers present"
        );
        assert!(
            has(&PanelHit::ImageFlipH),
            "flip-horizontal control present"
        );
        assert!(has(&PanelHit::ImageFlipV), "flip-vertical control present");
    }

    {
        let (mut framed, _id) = doc_with_selected_image();
        let (wp, _) = pipeline_draw(&mut framed, 390, 844);

        let rect_for = |want: PanelHit| {
            wp.panel_hits
                .iter()
                .find(|(_, hit)| *hit == want)
                .map(|(rect, _)| *rect)
                .expect("position control is painted")
        };
        let free = rect_for(PanelHit::ImageFreePosition);
        let left = rect_for(PanelHit::ImageWrapLeft);
        let right = rect_for(PanelHit::ImageWrapRight);
        for rect in [free, left, right] {
            assert!(
                rect.x0 >= wp.panel_rect.x0 && rect.x1 <= wp.panel_rect.x1,
                "position control stays inside the narrow inspector: {rect:?}",
            );
        }
        assert!(
            free.x1 <= left.x0 && left.x1 <= right.x0,
            "controls do not overlap"
        );
        assert!(
            (free.y0 - left.y0).abs() < f64::EPSILON && (left.y0 - right.y0).abs() < f64::EPSILON,
            "placement choices share their own row below Position",
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn image_controls_are_named_and_resize_and_flip() {
    {
        use ui::a11y::Role;

        crate::i18n::set_locale("en-US");
        let (mut framed, _id) = doc_with_selected_image();
        let (wp, state) = split(&mut framed);
        let tree = wp.accessibility_tree();
        let group = tree
            .children
            .iter()
            .find(|child| child.node.name.as_deref() == Some("Image position"))
            .expect("selected image placement group is exported");
        let expected = [
            "Image in line with text",
            "Free image position",
            "Image left with text wrap",
            "Image right with text wrap",
        ];
        assert_eq!(
            group
                .children
                .iter()
                .filter_map(|child| child.node.name.as_deref())
                .collect::<Vec<_>>(),
            expected,
        );
        assert!(group.children.iter().all(|child| {
            child.node.role == Role::Button
                && child.node.description.is_some()
                && child.node.bounds.is_some()
                && child.node.action_target.is_some()
                && child.node.state.focusable
        }));

        let target = "word-processor.image-placement:left".to_string();
        assert!(wp.perform_access_action(
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&target)],
            &mut edits(&mut state.doc),
            &state.root,
        ));
        let containers = wp.model().containers.clone().expect("open document");
        let image = tilo_doc::model::materialize(&state.doc, &containers)
            .blocks
            .into_iter()
            .find(|block| block.kind == "image")
            .expect("selected image remains materialized");
        assert!(matches!(
            image.attrs.get("anchor"),
            Some(tilo_doc::AttrVal::Str(anchor)) if anchor == "paragraph"
        ));
        assert!(matches!(
            image.attrs.get("wrap"),
            Some(tilo_doc::AttrVal::Str(wrap)) if wrap == "left"
        ));
    }

    {
        let (mut framed, id) = doc_with_selected_image();
        let (wp, state) = split(&mut framed);
        let (larger_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| matches!(h, PanelHit::ImageScale(f) if *f > 1.0))
            .expect("a 'Larger' resize control")
            .clone();
        let pt = Point::new(
            f64::midpoint(larger_rect.x0, larger_rect.x1),
            f64::midpoint(larger_rect.y0, larger_rect.y1),
        );
        left_click(wp, state, pt);
        let (w, _h) = wp
            .model()
            .image_size(&state.doc, &id)
            .expect("image now has an explicit size");
        assert!(
            w > 2.0,
            "the image grew beyond its 2px intrinsic width (w={w})"
        );
    }

    {
        let (mut framed, id) = doc_with_selected_image();
        let (wp, state) = split(&mut framed);
        let (flip_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| *h == PanelHit::ImageFlipH)
            .expect("a flip-horizontal control")
            .clone();
        let pt = Point::new(
            f64::midpoint(flip_rect.x0, flip_rect.x1),
            f64::midpoint(flip_rect.y0, flip_rect.y1),
        );
        left_click(wp, state, pt);
        let c = wp.model().containers.clone().unwrap();
        let flipped = tilo_doc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .find(|b| b.id == id)
            .is_some_and(|b| b.flag("flip_h"));
        assert!(
            flipped,
            "the image's flip_h attr is set after clicking Flip Horizontal"
        );
    }
}

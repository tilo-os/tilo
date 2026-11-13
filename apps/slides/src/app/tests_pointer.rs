//! Working on the deck with the pointer and the inspector: inserting,
//! selecting, moving and resizing elements, restyling them, reordering and
//! deleting slides by dragging thumbnails, context menus and pictures. Each
//! gesture lands in the deck as the resource's action when it ends.
#![allow(
    clippy::float_cmp,
    reason = "exact geometry and values the editor computes, not measurements"
)]

use super::bench::{Bench, cursor_ev, left_press_ev};
use super::*;
use ui::portal::PortalOutcome;

fn selected_text() -> (Bench, Id) {
    let mut s = Slide::new(Layout::TitleContent);
    let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 100.0, 40.0));
    let mut bench = Bench::one_slide(s);
    let selected = id.clone();
    bench.with(|app| app.sel = Some(selected));
    (bench, id)
}

fn sel_center(app: &SlidesApp) -> Point {
    let chrome = app.compute_chrome();
    let el = app.selected_element().unwrap().clone();
    crate::render::element_dest(chrome.slide_dest, chrome.scale, &el).center()
}

fn element_center(app: &SlidesApp, id: &str) -> Point {
    let chrome = app.compute_chrome();
    let el = app.slide().unwrap().element(id).unwrap().clone();
    crate::render::element_dest(chrome.slide_dest, chrome.scale, &el).center()
}

fn hover(app: &mut SlidesApp, p: Point) -> bool {
    let mut handled = false;
    app.dispatch_unmodal_event(&cursor_ev(p), 1.0, Modifiers::default(), &mut handled);
    handled
}

fn press(app: &mut SlidesApp, p: Point) -> bool {
    let mut handled = false;
    app.last_mouse = p;
    app.on_press(p, &mut handled);
    handled
}

fn release(app: &mut SlidesApp) -> bool {
    let mut handled = false;
    app.on_release(&mut handled);
    handled
}

fn element(bench: &Bench, id: &str) -> Element {
    bench.deck().slides[0].element(id).unwrap().clone()
}

fn order(bench: &Bench) -> Vec<Id> {
    bench.deck().slides.iter().map(|s| s.id.clone()).collect()
}

fn reorder_bench() -> Bench {
    Bench::open(&crate::model::seed_deck(), (1280, 900))
}

fn write_png(directory: &std::path::Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
    use image::{Rgb, RgbImage};
    let mut img = RgbImage::new(w, h);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, 90, 140]);
    }
    let path = directory.join(name);
    img.save(&path).expect("write test png");
    path
}

#[test]
fn a_tool_key_inserts_a_shape_into_the_deck() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    let before = bench.deck().slides[0].elements.len();
    let handled = bench.with(|app| {
        let mut handled = false;
        app.on_key(
            &Key::Character("r".into()),
            Some("r"),
            Modifiers::default(),
            &mut handled,
        );
        handled
    });
    assert!(handled, "'r' is handled as insert-rectangle");
    let deck = bench.deck();
    assert_eq!(
        deck.slides[0].elements.len(),
        before + 1,
        "the inserted shape is in the document"
    );
    let inserted = deck.slides[0].elements.last().unwrap();
    assert_eq!(inserted.kind, ElementKind::Rect);
    assert_eq!(
        bench.read(|app| app.sel.clone()).as_deref(),
        Some(inserted.id.as_str()),
        "the freshly inserted element is selected"
    );
    assert!(bench.undo(), "an insert is one undo step");
    assert_eq!(bench.deck().slides[0].elements.len(), before);
}

#[test]
fn the_font_size_stepper_writes_the_size() {
    let mut s = Slide::new(Layout::Blank);
    let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 140.0, 40.0));
    let mut bench = Bench::one_slide(s);
    let selected = id.clone();
    let plus = bench.with(|app| {
        app.sel = Some(selected);
        let chrome = app.compute_chrome();
        let fr = chrome
            .font_input_rect
            .expect("a text selection shows the font-size field");
        Point::new(fr.x1 - fr.height() / 2.0, fr.center().y)
    });
    bench.move_to(plus);
    bench.send(&left_press_ev());
    bench.release();
    assert_eq!(
        element(&bench, &id).font_size,
        42.0,
        "the + stepper raised the font size by the step (2)"
    );
}

#[test]
fn the_fill_colour_picker_writes_a_hex_fill() {
    let mut s = Slide::new(Layout::Blank);
    let id = s.add(Element::shape(
        ElementKind::Rect,
        100.0,
        100.0,
        300.0,
        200.0,
    ));
    let mut bench = Bench::one_slide(s);
    let selected = id.clone();
    bench.with(|app| {
        app.sel = Some(selected);
        app.apply_hit(Hit::OpenFillPicker);
        assert!(
            app.fill_picker_open,
            "the Custom colour button opens the picker"
        );
    });
    let centre = bench.with(|app| {
        let chrome = app.compute_chrome();
        app.position_inspector_widgets(&chrome);
        app.fill_picker.set_hex_text_for_test("#FF8800");
        app.fill_picker.get_rect().center()
    });
    bench.move_to(centre);
    assert_eq!(
        element(&bench, &id).fill,
        "#FF8800",
        "the picker wrote a concrete hex fill"
    );
}

#[test]
fn every_inspector_property_edit_lands_in_the_deck() {
    let (mut bench, id) = selected_text();
    let el0 = element(&bench, &id);

    bench.with(|app| app.apply_hit(Hit::SetFill("accent".into())));
    assert_eq!(element(&bench, &id).fill, "accent");

    bench.with(|app| app.apply_hit(Hit::SetStroke("text".into())));
    let stroked = element(&bench, &id);
    assert_eq!(stroked.stroke, "text");
    assert!(
        stroked.stroke_width >= 1.0,
        "a freshly stroked element gets a default width"
    );

    bench.with(|app| app.set_sel_f64("font_size", el0.font_size + 6.0));
    assert!((element(&bench, &id).font_size - (el0.font_size + 6.0)).abs() < 0.001);

    bench.with(|app| app.apply_hit(Hit::ToggleBold));
    assert!(element(&bench, &id).bold);
    bench.with(|app| app.apply_hit(Hit::ToggleItalic));
    assert!(element(&bench, &id).italic);

    bench.with(|app| app.apply_hit(Hit::SetAlign(TextAlign::Center)));
    assert_eq!(element(&bench, &id).align, TextAlign::Center);
    bench.with(|app| app.apply_hit(Hit::SetAlign(TextAlign::Justify)));
    assert_eq!(element(&bench, &id).align, TextAlign::Justify);

    let slide = bench.deck().slides[0].id.clone();
    let other = Element::shape(ElementKind::Rect, 700.0, 400.0, 100.0, 100.0);
    bench
        .app
        .invoke(
            ui::tilo_resource::Invoke::apply(&crate::actions::add_element_action(&slide, &other))
                .unwrap(),
        )
        .unwrap();
    bench.with(|app| app.apply_hit(Hit::BringFront));
    let zf = element(&bench, &id).z;
    bench.with(|app| app.apply_hit(Hit::SendBack));
    let zb = element(&bench, &id).z;
    assert!(zb < zf, "SendBack lowers z below BringFront ({zf} → {zb})");

    bench.with(|app| app.apply_hit(Hit::SetLayout(Layout::Blank)));
    assert_eq!(bench.deck().slides[0].layout, Layout::Blank);
    bench.with(|app| app.apply_hit(Hit::SetBackground(Background::Accent)));
    assert_eq!(bench.deck().slides[0].background, Background::Accent);
    bench.with(|app| app.apply_hit(Hit::SetPalette(PaletteKind::Amber)));
    assert_eq!(bench.deck().palette, PaletteKind::Amber);
    assert_eq!(
        bench.read(|app| app.deck.palette),
        PaletteKind::Amber,
        "the editor shows the deck's new theme"
    );

    let n = bench.deck().slides[0].elements.len();
    bench.with(|app| app.apply_hit(Hit::DuplicateElement));
    assert_eq!(
        bench.deck().slides[0].elements.len(),
        n + 1,
        "duplicate adds an element"
    );
    // Deleting changes only the deck: it runs at once and Undo takes it
    // back.
    bench.with(|app| app.apply_hit(Hit::DeleteElement));
    assert_eq!(bench.deck().slides[0].elements.len(), n, "deleted at once");
    assert!(bench.undo(), "a delete is an undo step");
    assert_eq!(bench.deck().slides[0].elements.len(), n + 1);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn element_pointer_gestures_select_move_resize_and_edit() {
    // Case `selection_handles_never_cover_the_element_content`.
    // Every resize handle's visible pip sits outside a left-anchored text
    // box, so it can't cover the first glyph.
    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text(
            "Goals for this phase",
            100.0,
            100.0,
            600.0,
            90.0,
            40.0,
        ));
        Bench::one_slide(s).with(|app| {
            app.sel = Some(id);
            let chrome = app.compute_chrome();
            let el = app.selected_element().expect("a selected element").clone();
            let er = render::element_dest(chrome.slide_dest, chrome.scale, &el);
            for (h, hr) in app.handles_of(chrome.slide_dest, chrome.scale, &el) {
                let vis = hr.inflate(-4.0, -4.0);
                let overlaps = vis.x0 < er.x1 && vis.x1 > er.x0 && vis.y0 < er.y1 && vis.y1 > er.y0;
                assert!(
                    !overlaps,
                    "handle {h:?} pip {vis:?} must not overlap the content rect {er:?}"
                );
            }
        });
    }

    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 160.0, 40.0));
        let mut bench = Bench::one_slide(s);
        let selected = id.clone();
        bench.with(|app| {
            app.sel = Some(selected);
            let p = sel_center(app);
            assert!(
                press(app, p),
                "the press on the selected element is handled"
            );
            assert!(
                app.drag.active(),
                "a single grab of a selected element arms a move"
            );
            assert!(!app.editing_text, "a single grab must NOT enter text edit");
            app.drag_move(Point::new(p.x + 60.0, p.y));
            release(app);
        });
        let x = element(&bench, &id).x;
        assert!(x > 100.5, "the element moved right on first grab (x = {x})");
    }

    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 160.0, 40.0));
        Bench::one_slide(s).with(|app| {
            app.sel = Some(id);
            let p = sel_center(app);
            press(app, p);
            release(app);
            assert!(!app.editing_text, "one click selects, does not edit");
            // A second click at the same spot right after → double-click → edit.
            press(app, p);
            assert!(
                app.editing_text,
                "double-click opens the text box for editing"
            );
            assert!(!app.drag.active(), "entering edit does not arm a drag");
        });
    }

    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 120.0, 40.0));
        let mut bench = Bench::one_slide(s);
        let p = bench.read(|app| element_center(app, &id));
        bench.with(|app| {
            press(app, p);
            assert_eq!(app.sel.as_deref(), Some(id.as_str()));
            assert!(!app.editing_text);
            release(app);
        });
        bench.with(|app| {
            press(app, p);
            assert!(
                app.editing_text,
                "a second click on a selected text box edits it"
            );
        });
    }

    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 300.0, 300.0, 160.0, 90.0));
        let mut bench = Bench::one_slide(s);
        let start = bench.read(|app| element_center(app, &id));
        let shown = bench.with(|app| {
            press(app, start);
            assert!(
                matches!(app.drag, Drag::Move { .. }),
                "pressing an element starts a move drag"
            );
            app.drag_move(Point::new(start.x - 130.0, start.y + 70.0));
            release(app);
            assert!(!app.drag.active(), "release ends the drag");
            app.slide().unwrap().element(&id).unwrap().clone()
        });
        let moved = element(&bench, &id);
        assert!(
            (moved.x - shown.x).abs() < 0.001 && (moved.y - shown.y).abs() < 0.001,
            "the deck holds where the drag left the element"
        );
        assert!((moved.x - 300.0).abs() > 5.0, "x changed (x={})", moved.x);
        assert!((moved.y - 300.0).abs() > 5.0, "y changed (y={})", moved.y);
        assert!(bench.undo(), "the whole drag is one undo step");
        let back = element(&bench, &id);
        assert_eq!((back.x, back.y), (300.0, 300.0));
    }

    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(
            ElementKind::Rect,
            300.0,
            300.0,
            200.0,
            120.0,
        ));
        let mut bench = Bench::one_slide(s);
        let selected = id.clone();
        let (el, shown) = bench.with(|app| {
            app.sel = Some(selected);
            let chrome = app.compute_chrome();
            let el = app.slide().unwrap().element(&id).unwrap().clone();
            let (_, hr) = app
                .handles_of(chrome.slide_dest, chrome.scale, &el)
                .into_iter()
                .find(|(h, _)| matches!(h, Handle::Br))
                .expect("a bottom-right resize handle");
            let start = hr.center();
            press(app, start);
            assert!(
                matches!(app.drag, Drag::Resize { .. }),
                "pressing a handle starts a resize"
            );
            app.drag_move(Point::new(start.x + 120.0, start.y + 90.0));
            release(app);
            (el, app.slide().unwrap().element(&id).unwrap().clone())
        });
        let resized = element(&bench, &id);
        assert!(
            (resized.w - shown.w).abs() < 0.001 && (resized.h - shown.h).abs() < 0.001,
            "the deck holds the size the drag showed"
        );
        assert!(
            resized.w > el.w + 1.0,
            "width grew ({} → {})",
            el.w,
            resized.w
        );
        assert!(
            resized.h > el.h + 1.0,
            "height grew ({} → {})",
            el.h,
            resized.h
        );
    }

    {
        let (mut bench, _id) = selected_text();
        bench.with(|app| app.begin_text_edit(false));
        let centre = bench.read(|app| {
            let chrome = app.compute_chrome();
            render::element_dest(
                chrome.slide_dest,
                chrome.scale,
                app.selected_element().unwrap(),
            )
            .center()
        });
        bench.move_to(centre);
        bench.press();
        bench.read(|app| {
            assert!(
                app.text_drag,
                "a press inside the editing box starts a selection drag"
            );
            assert!(!app.drag.active(), "the box is not being moved");
        });
        bench.release();
        bench.read(|app| {
            assert!(!app.text_drag);
            assert!(app.editing_text, "the box is still being edited");
        });
    }
}

#[test]
fn an_edit_arriving_mid_drag_shows_after_the_drop() {
    let mut s = Slide::new(Layout::Blank);
    let id = s.add(Element::shape(ElementKind::Rect, 300.0, 300.0, 160.0, 90.0));
    let mut bench = Bench::one_slide(s);
    let start = bench.read(|app| element_center(app, &id));
    bench.with(|app| {
        press(app, start);
        app.drag_move(Point::new(start.x + 200.0, start.y));
    });
    bench.app.merge_peer_edit(|doc| {
        tilo_actions::Action::apply(&actions::set_deck_title_action("Renamed elsewhere"), doc)
            .unwrap();
    });
    bench.frame();
    bench.read(|app| {
        assert!(app.drag.active(), "the drag goes on");
        assert_ne!(app.deck.title, "Renamed elsewhere", "shown after the drop");
    });
    bench.with(|app| {
        release(app);
    });
    bench.frame();
    bench.read(|app| assert_eq!(app.deck.title, "Renamed elsewhere"));
    assert!(
        element(&bench, &id).x > 300.0,
        "the drop still moved the element"
    );
}

/// Thumbnail drags reorder with a ghost/gap, plain clicks select, and the delete zone removes only while dragging.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn thumbnail_drag_reorders_selects_and_deletes() {
    {
        let mut bench = reorder_bench();
        let first_id = bench.deck().slides[0].id.clone();
        bench.with(|app| {
            let thumbs = app.rail_thumb_rects();
            let start = thumbs[0].1.center();
            let target = thumbs[3].1.center();
            press(app, start);
            assert!(
                app.page_sidebar.is_pending_drag(),
                "pressing a thumbnail arms a reorder drag"
            );
            hover(app, target);
            assert!(
                app.page_sidebar.is_dragging(),
                "moving past the threshold promotes to a live reorder"
            );
            assert_eq!(
                app.page_sidebar.drag_insertion(),
                Some(3),
                "the gap tracks to the drop row"
            );
            assert!(release(app), "release commits the drag");
            assert!(!app.page_sidebar.is_pending_drag(), "and clears it");
        });
        let new_pos = order(&bench)
            .iter()
            .position(|s| *s == first_id)
            .expect("slide survived");
        assert_eq!(new_pos, 3, "the dragged slide landed where the gap showed");
        assert_eq!(
            bench.read(|app| app.cur_slide),
            3,
            "the dragged slide stays current at its new index"
        );
    }

    {
        let mut bench = reorder_bench();
        let order0 = order(&bench);
        let idx = bench.with(|app| {
            let (idx, rect) = app.rail_thumb_rects()[2];
            press(app, rect.center());
            release(app);
            idx
        });
        assert_eq!(
            bench.read(|app| app.cur_slide),
            idx,
            "a plain click selects"
        );
        assert_eq!(
            order0,
            order(&bench),
            "a plain click leaves the order alone"
        );
    }

    {
        let mut bench = reorder_bench();
        let order0 = order(&bench);
        bench.with(|app| {
            let thumbs = app.rail_thumb_rects();
            press(app, thumbs[0].1.center());
            hover(app, thumbs[2].1.center());
            assert!(app.page_sidebar.is_dragging(), "the drag is live");
            assert_eq!(
                app.page_sidebar.drag_from(),
                Some(0),
                "slide 0 is lifted out"
            );
            assert_eq!(
                app.page_sidebar.drag_insertion(),
                Some(2),
                "the reflow gap opens at the drop row"
            );
            let mut handled = false;
            app.on_key(
                &Key::Named(NamedKey::Escape),
                None,
                Modifiers::default(),
                &mut handled,
            );
            assert!(handled);
            assert!(
                !app.page_sidebar.is_pending_drag(),
                "Escape cancels the drag"
            );
        });
        assert_eq!(
            order0,
            order(&bench),
            "a cancelled drag never touched the order"
        );
    }

    {
        let mut bench = reorder_bench();
        let before = bench.deck().slides.len();
        let dropped = bench.deck().slides[1].id.clone();
        bench.with(|app| {
            let start = app.rail_thumb_rects()[1].1.center();
            press(app, start);
            let drop = app.slide_delete_zone(app.rail_rect()).center();
            hover(app, drop);
            assert!(app.page_sidebar.is_dragging(), "the drag is live");
            assert!(
                app.over_delete_zone(),
                "the pointer is over the delete zone"
            );
            assert!(release(app), "the drop is handled");
            assert!(!app.page_sidebar.is_pending_drag(), "the drag is cleared");
        });
        // Deleting changes only the deck: the drop deletes the slide at
        // once and Undo takes it back.
        assert_eq!(bench.deck().slides.len(), before - 1, "deleted at once");
        assert!(bench.deck().slides.iter().all(|slide| slide.id != dropped));
        assert!(bench.undo(), "a delete is an undo step");
        assert_eq!(bench.deck().slides.len(), before);
    }

    reorder_bench().with(|app| {
        let zone = app.slide_delete_zone(app.rail_rect());
        app.last_mouse = zone.center();
        assert!(
            !app.over_delete_zone(),
            "no drag in progress → the zone is inert"
        );
    });
}

#[test]
fn context_menus_act_on_elements_and_slides() {
    {
        let mut slide = Slide::new(Layout::Blank);
        let a = slide.add(Element::shape(
            ElementKind::Rect,
            100.0,
            100.0,
            300.0,
            200.0,
        ));
        let b = slide.add(Element::shape(
            ElementKind::Ellipse,
            500.0,
            300.0,
            200.0,
            200.0,
        ));
        let mut bench = Bench::one_slide(slide);
        let pt = bench.read(|app| element_center(app, &a));
        bench.with(|app| {
            let mut h = false;
            app.last_mouse = pt;
            app.on_right_press(pt, &mut h);
            assert!(app.ctx_menu.is_open(), "right-click opens the element menu");
            assert_eq!(
                app.sel.as_deref(),
                Some(a.as_str()),
                "the element is selected"
            );
            app.ctx_menu.click_item(0);
            if let Some(idx) = app.ctx_menu.take_selection()
                && let Some(Some(hit)) = app.ctx_hits.get(idx).cloned()
            {
                app.apply_hit(hit);
            }
            assert!(!app.ctx_menu.is_open(), "choosing a row closes the menu");
        });
        let za = element(&bench, &a).z;
        let zb = element(&bench, &b).z;
        assert!(za > zb, "Bring to front raised the element ({za} vs {zb})");
    }

    {
        let mut bench = Bench::sample();
        let idx = bench.with(|app| {
            let (idx, rect) = app.rail_thumb_rects()[2];
            let p = rect.center();
            let mut h = false;
            app.last_mouse = p;
            app.on_right_press(p, &mut h);
            assert!(
                app.page_sidebar.menu_open(),
                "right-click on a thumbnail opens the rail menu"
            );
            assert_eq!(
                app.cur_slide, idx,
                "the right-clicked slide becomes current"
            );
            idx
        });
        let before = bench.deck().slides.len();
        bench.with(|app| app.apply_rail_event(PageSidebarEvent::Duplicate(idx)));
        assert_eq!(
            bench.deck().slides.len(),
            before + 1,
            "Duplicate slide added one"
        );
    }
}

#[test]
fn image_insert_uses_the_host_portal_and_decodes_real_pngs() {
    let directory = tempfile::tempdir().unwrap();
    {
        use crate::assets::{ImageState, ImageStore};
        let path = write_png(directory.path(), "decode.png", 8, 6);
        let content = ContentStore::memory();
        let key =
            crate::assets::import_asset_file(&content, &path).expect("fixture imports as an asset");
        let store = ImageStore::new(content);
        store.load_blocking(&key);
        match store.get(&key) {
            ImageState::Ready(img) => {
                assert_eq!((img.width, img.height), (8, 6), "decoded dimensions match");
            }
            _ => panic!("a real PNG must decode to Ready"),
        }
        store.load_blocking(crate::sample::MISSING_SRC);
        assert!(
            matches!(store.get(crate::sample::MISSING_SRC), ImageState::Failed),
            "a missing asset resolves to Failed, not a panic"
        );
        assert!(
            matches!(store.get(&path.to_string_lossy()), ImageState::Failed),
            "a peer-supplied local path is never opened"
        );
    }

    {
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck().slides[0].elements.len();
        let path = write_png(directory.path(), "insert.png", 12, 8);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
        ui::host::scoped(&services, || {
            bench.app.harness().with_widget_mut::<SlidesApp, _>(|app| {
                app.apply_hit(Hit::Insert(ElementKind::Image));
            })
        });
        assert_eq!(
            recorder.pick_requests().len(),
            1,
            "exactly one host chooser request"
        );
        tilo_test_env::wait_until("the chosen picture to land on the slide", || {
            bench.frame();
            bench.deck().slides[0].elements.len() > before
        });
        let deck = bench.deck();
        let el = deck.slides[0]
            .elements
            .last()
            .expect("an element was inserted");
        assert_eq!(deck.slides[0].elements.len(), before + 1);
        assert_eq!(
            el.kind,
            ElementKind::Image,
            "the inserted element is an image"
        );
        assert!(
            crate::assets::is_asset_id(&el.src),
            "only a portable asset id enters the deck"
        );
        let stored =
            bench.read(|app| crate::assets::read_asset(&app.store.borrow().content, &el.src));
        assert_eq!(stored.as_deref(), Some(&std::fs::read(&path).unwrap()[..]));
        assert_eq!(
            bench.read(|app| app.sel.clone()).as_deref(),
            Some(el.id.as_str()),
            "the new image is selected"
        );
    }

    {
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck().slides[0].elements.len();
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Cancelled);
        ui::host::scoped(&services, || {
            bench.app.harness().with_widget_mut::<SlidesApp, _>(|app| {
                app.apply_hit(Hit::Insert(ElementKind::Image));
            })
        });
        bench.frame();
        bench.frame();
        assert_eq!(
            recorder.pick_requests().len(),
            1,
            "the Image tool asked the host"
        );
        assert_eq!(
            bench.deck().slides[0].elements.len(),
            before,
            "a cancelled pick leaves the slide untouched"
        );
    }
}

#[test]
fn a_finger_held_on_a_slide_moves_it_in_the_list() {
    use winit::event::TouchPhase;
    {
        let mut bench = reorder_bench();
        bench.frame();
        let first_id = bench.deck().slides[0].id.clone();
        let thumbs = bench.with(SlidesApp::rail_thumb_rects);
        let (start, target) = (thumbs[0].1.center(), thumbs[3].1.center());
        bench.app.harness().touch_down_and_hold_at(start.x, start.y);
        assert!(
            bench.read(|app| app.page_sidebar.is_pending_drag()),
            "the hold picks the slide up"
        );
        let steps = 8_u32;
        for step in 1..=steps {
            let t = f64::from(step) / f64::from(steps);
            let y = start.y + (target.y - start.y) * t;
            bench.app.harness().touch(0, TouchPhase::Moved, start.x, y);
        }
        assert!(
            bench.read(|app| app.page_sidebar.is_dragging()),
            "the drag moves it live"
        );
        bench
            .app
            .harness()
            .touch(0, TouchPhase::Ended, target.x, target.y);
        let at = order(&bench).iter().position(|s| *s == first_id);
        assert_eq!(at, Some(3), "the slide lands where the finger lifted");
        assert_eq!(bench.read(|app| app.cur_slide), 3, "and stays current");
        assert!(!bench.read(|app| app.page_sidebar.menu_open()), "no menu");
    }
    {
        let mut bench = Bench::open(&crate::model::seed_deck(), (1280, 420));
        bench.frame();
        let order0 = order(&bench);
        let thumbs = bench.with(SlidesApp::rail_thumb_rects);
        let start = thumbs[1].1.center();
        let harness = bench.app.harness();
        harness.touch(0, TouchPhase::Started, start.x, start.y);
        for step in 1..=6_u32 {
            let y = start.y - 20.0 * f64::from(step);
            harness.touch(0, TouchPhase::Moved, start.x, y);
        }
        harness.touch(0, TouchPhase::Ended, start.x, start.y - 120.0);
        bench.frame();
        assert_eq!(order(&bench), order0, "a swipe reorders nothing");
        assert!(
            bench.read(|app| app.page_sidebar.scroll()) > 0.0,
            "a swipe scrolls the list"
        );
    }
    {
        let mut bench = reorder_bench();
        bench.frame();
        let order0 = order(&bench);
        let thumbs = bench.with(SlidesApp::rail_thumb_rects);
        let (index, rect) = thumbs[2];
        let centre = rect.center();
        bench.app.harness().touch_hold_at(centre.x, centre.y);
        bench.read(|app| {
            assert!(app.page_sidebar.menu_open(), "the slide's menu opens");
            assert!(!app.page_sidebar.is_pending_drag(), "nothing is held");
            assert_eq!(app.cur_slide, index, "the slide is selected");
        });
        assert_eq!(order(&bench), order0, "nothing moved");
    }
}

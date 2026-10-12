//! Working on the deck with the pointer and the inspector: inserting,
//! selecting, moving and resizing elements, restyling them, reordering and
//! deleting slides by dragging thumbnails, context menus and pictures. Each
//! gesture lands in the deck as the resource's action when it ends.
#![allow(
    clippy::float_cmp,
    reason = "exact geometry and values the editor computes, not measurements"
)]

use super::bench::{Bench, cursor_ev, deck_of, left_press_ev};
use super::*;
use ui::portal::PortalOutcome;

/// The editor over one slide whose single text box is selected, and the
/// box's id.
fn selected_text() -> (Bench, Id) {
    let mut s = Slide::new(Layout::TitleContent);
    let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 100.0, 40.0));
    let mut bench = Bench::one_slide(s);
    let selected = id.clone();
    bench.with(|app| app.sel = Some(selected));
    (bench, id)
}

/// Where the selected element is drawn.
fn sel_center(app: &SlidesApp) -> Point {
    let chrome = app.compute_chrome();
    let el = app.selected_element().unwrap().clone();
    crate::render::element_dest(chrome.slide_dest, chrome.scale, &el).center()
}

/// Where element `id` of the current slide is drawn.
fn element_center(app: &SlidesApp, id: &str) -> Point {
    let chrome = app.compute_chrome();
    let el = app.slide().unwrap().element(id).unwrap().clone();
    crate::render::element_dest(chrome.slide_dest, chrome.scale, &el).center()
}

/// Move the pointer to `p` through the editor's own event path.
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

/// The sample deck at a window tall enough that the whole rail fits.
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

/// A key typed on the insert tools inserts that shape, selected.
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

/// Clicking a row of the toolbar's overflow menu inserts that element.
#[test]
fn clicking_an_overflow_menu_row_inserts_that_element() {
    let mut s = Slide::new(Layout::TitleContent);
    s.add(Element::text("hello", 96.0, 90.0, 1088.0, 100.0, 54.0));
    let mut bench = Bench::open(&deck_of(vec![s]), (390, 800));
    let before = bench.deck().slides[0].elements.len();
    let row = bench.with(|app| {
        app.open_tools_menu();
        let index = app
            .ctx_hits
            .iter()
            .position(|hit| matches!(hit, Some(Hit::Insert(ElementKind::Rect))))
            .expect("the Rectangle tool is in the overflow menu at 390px");
        app.ctx_menu
            .item_rect(index)
            .expect("the real menu row is laid out")
            .center()
    });
    bench.app.harness().dispatch_to_root(&cursor_ev(row));
    let handled = bench.app.harness().dispatch_to_root(&left_press_ev());
    bench.release();
    assert!(handled, "clicking a menu row is handled");
    assert!(
        !bench.read(|app| app.ctx_menu.is_open()),
        "picking a tool closes the menu"
    );
    assert_eq!(
        bench.deck().slides[0].elements.len(),
        before + 1,
        "the chosen shape was inserted"
    );
}

/// The font-size field's + stepper raises the selected text's size.
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

/// The fill colour picker writes a concrete hex fill.
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
        // A hex typed into the picker; the next event over it flushes it.
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

/// Every inspector property edit lands in the deck.
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

    // The font-size field commits through `set_sel_f64`.
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

    // Stacking is relative: another element shares the slide.
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
    // Deleting is destructive: held for the person's confirmation.
    bench.with(|app| app.apply_hit(Hit::DeleteElement));
    assert_eq!(bench.deck().slides[0].elements.len(), n + 1);
}

/// Selection handles, first-grab drags, edit-entry clicks, moves, resizes and caret placement.
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
                // The painted pip is the grab rect inset by 4 (see `draw_editor`).
                let vis = hr.inflate(-4.0, -4.0);
                let overlaps = vis.x0 < er.x1 && vis.x1 > er.x0 && vis.y0 < er.y1 && vis.y1 > er.y0;
                assert!(
                    !overlaps,
                    "handle {h:?} pip {vis:?} must not overlap the content rect {er:?}"
                );
            }
        });
    }

    // Case `selected_element_drags_on_first_grab`.
    // An already-selected element arms a move on the very first press (not
    // text edit), and dragging moves it.
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

    // Case `double_click_text_box_enters_edit`.
    {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 160.0, 40.0));
        Bench::one_slide(s).with(|app| {
            app.sel = Some(id);
            let p = sel_center(app);
            // First click + release: selects, arms then releases a zero-length move.
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

    // Case `second_click_on_a_selected_text_box_enters_edit_mode`.
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

    // Case `dragging_an_element_moves_it_in_the_deck`.
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
            // Far enough that no guide snaps it back to the origin.
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

    // Case `resizing_via_a_selection_handle_resizes_it_in_the_deck`.
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

    // Case `clicking_inside_the_editing_text_arms_caret_placement_not_a_box_move`.
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

/// A collaborator's edit that arrives during a drag leaves the drag alone
/// and shows once it is dropped.
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
    // Case `dragging_a_thumbnail_reorders_the_deck`.
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

    // Case `a_plain_thumbnail_click_selects_without_reordering`.
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

    // Case `a_live_thumbnail_drag_lifts_a_ghost_and_opens_a_reflow_gap`.
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
            // Escape aborts without touching the deck.
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

    // Case `dropping_a_slide_on_the_delete_zone_asks_to_delete_it`.
    {
        let mut bench = reorder_bench();
        let before = bench.deck().slides.len();
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
        // Deleting is destructive: the drop is held for confirmation.
        assert_eq!(bench.deck().slides.len(), before, "held for confirmation");
    }

    // Case `delete_zone_is_inert_when_not_dragging`.
    reorder_bench().with(|app| {
        let zone = app.slide_delete_zone(app.rail_rect());
        app.last_mouse = zone.center();
        assert!(
            !app.over_delete_zone(),
            "no drag in progress → the zone is inert"
        );
    });
}

/// Right-click menus for canvas elements and slide thumbnails.
#[test]
fn context_menus_act_on_elements_and_slides() {
    // Case `right_click_element_opens_menu_and_actions_apply`.
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
            // Row 0 = Bring to front.
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

    // Case `right_click_thumbnail_opens_slide_menu`.
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
        // The menu's Duplicate adds a copy after it.
        let before = bench.deck().slides.len();
        bench.with(|app| app.apply_rail_event(PageSidebarEvent::Duplicate(idx)));
        assert_eq!(
            bench.deck().slides.len(),
            before + 1,
            "Duplicate slide added one"
        );
    }
}

/// The image store decodes PNGs; the image tool uses the host portal and a cancel inserts nothing.
#[test]
fn image_insert_uses_the_host_portal_and_decodes_real_pngs() {
    let directory = tempfile::tempdir().unwrap();
    // Case `image_store_decodes_a_real_png_and_fails_gracefully`.
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

    // Case `image_tool_asks_the_host_chooser_then_a_pick_inserts_a_real_image`.
    {
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck().slides[0].elements.len();
        let path = write_png(directory.path(), "insert.png", 12, 8);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
        // The Image tool asks the host's chooser for a file, without
        // fabricating an element and without drawing an in-app picker.
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
        // A frame takes up the chosen file and lands a real image in the deck.
        bench.frame();
        bench.frame();
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
        let stored = bench.read(|app| crate::assets::read_asset(&app.content, &el.src));
        assert_eq!(stored.as_deref(), Some(&std::fs::read(&path).unwrap()[..]));
        assert_eq!(
            bench.read(|app| app.sel.clone()).as_deref(),
            Some(el.id.as_str()),
            "the new image is selected"
        );
    }

    // Case `a_cancelled_pick_inserts_nothing`.
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

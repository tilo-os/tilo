use super::*;
use crate::composition::Picture;
use tilo_ui_base::{a11y::AccessNode, a11y::AccessState};

use super::testing::*;

/// A window whose content flows is laid out by the browser from its containers' styles: where
/// the frame put each widget is nowhere in it.
#[test]
fn a_flowing_window_carries_no_frame_geometry() {
    let html = Page::new().render(&window("Groceries"));
    for geometry in ["left:", "top:", "13px", "27px", "position:absolute"] {
        assert!(!html.contains(geometry), "{geometry} in {html}");
    }
    assert!(
        html.contains("display:flex;flex-direction:column;gap:8px 8px;"),
        "{html}"
    );
}

/// Each node is the element its role names, with its words and its action.
#[test]
fn nodes_render_as_the_elements_their_roles_name() {
    let html = Page::new().render(&window("Fish & <chips>"));
    assert!(html.contains(">Fish &amp; &lt;chips&gt;</span>"), "{html}");
    let button = html
        .split('<')
        .find(|tag| tag.starts_with("button"))
        .unwrap_or_else(|| panic!("a button is a <button>: {html}"));
    assert!(button.contains("data-a=\"activate\""), "{button}");
    assert!(button.ends_with(">New note"), "it says its name: {button}");
}

#[test]
fn a_change_patches_only_what_changed_and_keeps_every_number() {
    let mut page = Page::new();
    let first = page.render(&window("Groceries"));
    assert_eq!(first, page.render(&window("Groceries")));
    let patches = page.patch(&window("Groceries list"));
    assert_eq!(patches.len(), 1, "{patches:?}");
    let (id, html) = replaced(&patches[0]);
    assert_eq!(page.path(id), Some("Root[0]/Label[0]"));
    assert!(html.contains("Groceries list"));
    assert!(page.patch(&window("Groceries list")).is_empty());
}

/// A menu opening over the window is put in alone, and taken out alone when it closes: every
/// element of the window stays the one the browser holds, so the list scrolled half way down
/// stays scrolled and the button that opened the menu keeps the keyboard's focus. The menu grows
/// from that button.
#[test]
fn a_menu_opening_and_closing_leaves_the_window_s_elements_in_place() {
    let mut page = Page::new();
    let closed = window("Notes");
    page.render(&closed);
    let mut open = closed.clone();
    let mut menu = Composed::new("Menu[0]".to_owned(), "Popover", [40.0, 92.0, 160.0, 64.0]);
    menu.semantics = Some(AccessNode::new(Role::Menu).with_name("Note".to_owned()));
    menu.appears_from = Some([85.0, 74.0]);
    let mut item = Composed::new(
        "Menu[0]/Item[0]".to_owned(),
        "MenuItem",
        [44.0, 96.0, 152.0, 28.0],
    );
    item.semantics = Some(AccessNode::new(Role::MenuItem).with_name("Pin".to_owned()));
    menu.children = vec![item];
    open.layers.push(menu);

    let opened = page.patch(&open);
    let [Patch::Insert { parent, at, html }] = opened.as_slice() else {
        panic!("the menu alone is put in: {opened:?}");
    };
    assert_eq!(page.path(*parent), Some(""), "it goes in over the window");
    assert_eq!(*at, 1, "after the window itself");
    assert!(
        html.contains("data-layer") && html.contains(">Pin"),
        "{html}"
    );
    assert!(
        html.contains("transform-origin:85.0px 92.0px;"),
        "it grows from the edge nearest the button that opened it: {html}"
    );
    assert!(
        html.contains("--tilo-appear-from:translate(-5.9px,-5.4px) scale(0.920);"),
        "it starts smaller and a little toward the button: {html}"
    );

    let shut = page.patch(&closed);
    let [Patch::Remove { id }] = shut.as_slice() else {
        panic!("the menu alone is taken out: {shut:?}");
    };
    assert!(
        page.render(&open).contains(&format!("data-p=\"{id}\"")),
        "it takes out the menu it put in"
    );
}

/// A widget that publishes its rows (a list drawing them itself) shows each as an element of
/// its own, numbered under the widget, a press on it the row's action.
#[test]
fn a_widgets_own_nodes_are_elements_a_press_acts_on() {
    let mut composition = window("Notes");
    let mut list = Composed::new(
        "Root[0]/List[0]".to_owned(),
        "List",
        [0.0, 100.0, 300.0, 400.0],
    );
    list.semantics = Some(
        AccessNode::new(Role::List)
            .with_name("Notes".to_owned())
            .with_children(
                ["Milk", "Eggs"]
                    .into_iter()
                    .map(|name| {
                        AccessNode::new(Role::ListItem)
                            .with_name(name.to_owned())
                            .with_description("Yesterday".to_owned())
                            .with_action_target(format!("note-{name}"))
                    })
                    .collect(),
            ),
    );
    composition.root.children.push(list);
    let mut page = Page::new();
    let html = page.render(&composition);
    for name in ["Milk", "Eggs"] {
        let target = format!("data-t=\"note-{name}\"");
        let row = html
            .split('<')
            .find(|tag| tag.contains(&target))
            .unwrap_or_else(|| panic!("{name} is a row: {html}"));
        assert!(row.contains("data-a=\"activate\""), "{row}");
        assert!(
            html.contains(&format!(
                "<span class=\"tilo-title\">{name}</span><span class=\"tilo-detail\">Yesterday</span>"
            )),
            "{html}"
        );
    }
    // A row is numbered by what it acts on, so its number holds while rows come and go.
    let id = numbers(&html)
        .into_iter()
        .find(|&id| {
            page.path(id)
                .is_some_and(|path| path.ends_with("#note-Eggs"))
        })
        .expect("the second row is numbered");
    assert_eq!(page.path(id), Some("Root[0]/List[0]#note-Eggs"));
}

/// What a widget only paints is its picture, and what acts in it stands over the picture where
/// it is drawn.
#[test]
fn a_painted_widget_is_its_picture_with_what_acts_over_it() {
    let mut composition = window("Chart");
    let mut chart = Composed::new(
        "Root[0]/Chart[0]".to_owned(),
        "Chart",
        [10.0, 100.0, 400.0, 200.0],
    );
    chart.semantics = Some(
        AccessNode::new(Role::Image)
            .with_name("Sales".to_owned())
            .with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name("March".to_owned())
                    .with_action_target("bar:3")
                    .with_bounds(vello::kurbo::Rect::new(50.0, 120.0, 70.0, 280.0))
                    .with_state(AccessState::new().focusable(true)),
            ]),
    );
    chart.picture = Some(Picture {
        version: 1,
        data: Some("data:image/png;base64,AAAA".into()),
    });
    composition.root.children.push(chart);
    let html = Page::new().render(&composition);
    assert!(
        html.contains("<img class=\"tilo-paint\" src=\"data:image/png;base64,AAAA\" alt=\"\">"),
        "{html}"
    );
    assert!(html.contains("aria-label=\"Sales\""), "{html}");
    let over = html
        .split('<')
        .find(|tag| tag.contains("data-t=\"bar:3\""))
        .unwrap_or_else(|| panic!("March stands over the picture: {html}"));
    assert!(
        over.contains("class=\"tilo-over\"")
            && over.contains("left:40px;top:20px;width:20px;height:160px;"),
        "{over}"
    );
}

/// An editable text node is the browser's own field, holding its value.
#[test]
fn a_text_field_is_an_input() {
    let mut composition = window("Title");
    let mut field = Composed::new(
        "Root[0]/TextInput[0]".to_owned(),
        "TextInput",
        [0.0, 100.0, 300.0, 30.0],
    );
    field.semantics = Some(
        AccessNode::new(Role::TextInput)
            .with_name("Title".to_owned())
            .with_value("Fish & chips".to_owned()),
    );
    composition.root.children.push(field);
    let html = Page::new().render(&composition);
    let input = html
        .split('<')
        .find(|tag| tag.starts_with("input"))
        .unwrap_or_else(|| panic!("the field is an <input>: {html}"));
    assert!(input.contains("value=\"Fish &amp; chips\""), "{input}");
    assert!(input.contains("aria-label=\"Title\""), "{input}");
}

#[test]
fn a_canvas_places_what_it_holds_and_a_host_fills_its_wells() {
    let mut page = Page::new();
    page.wells.insert(
        "tab-7".to_owned(),
        "<a href=\"/w/ws/notes\">Notes</a>".to_owned(),
    );
    let html = page.render(&canvas([500.0, 350.0]));
    assert!(
        html.contains("position:absolute;left:40px;top:40px;width:300px;height:200px;"),
        "a place stands at its point from the canvas's corner: {html}"
    );
    assert!(
        html.contains("position:absolute;left:400px;top:300px;width:400px;height:300px;"),
        "{html}"
    );
    assert!(
        html.contains("data-well=\"tab-7\"><a href=\"/w/ws/notes\">Notes</a>"),
        "the host's window stands in its well: {html}"
    );
    // Another member moved the second place: only its element changes.
    let patches = page.patch(&canvas([520.0, 330.0]));
    assert_eq!(patches.len(), 1, "{patches:?}");
    let (id, html) = replaced(&patches[0]);
    assert!(
        html.contains("position:absolute;left:420px;top:280px;"),
        "{patches:?}"
    );
    assert_eq!(page.path(id), Some("Root[0]/MapPin[1]"));
}

#[test]
fn an_iso_date_time_or_span_is_a_time() {
    for time in [
        "2026-11-23",
        "2026-11-23T09:30",
        "2026-11-23T09:30:15",
        "2026-11-23T09:30:15.250Z",
        "2026-11-23T09:30+01:00",
        "2026-11-23T09:00/2026-11-23T09:30",
        "2026-11-23/2026-11-25",
    ] {
        assert!(is_iso_time(time), "{time} is a time");
    }
    for not in [
        "",
        "Monday",
        "23 November",
        "2026-11",
        "2026-11-23T9:30",
        "2026-11-23T09:30/",
        "2026-11-23/2026-11-24/2026-11-25",
        "12:30",
        "2026-11-23T09:30:15.",
    ] {
        assert!(!is_iso_time(not), "{not:?} is no time");
    }
}

/// A dark theme is a dark page: the browser draws its own parts dark, and the words and grounds
/// take the theme's colours and its type's spacing.
#[test]
fn a_dark_theme_is_a_dark_page() {
    let light = theme_css(&tilo_ui_base::Theme::light());
    let dark = theme_css(&tilo_ui_base::Theme::dark());
    assert!(light.contains("color-scheme:light"), "{light}");
    assert!(dark.contains("color-scheme:dark"), "{dark}");
    let text = |css: &str| {
        css.split("--tilo-text:")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .expect("the words' colour")
            .to_owned()
    };
    assert_ne!(
        text(&light),
        text(&dark),
        "the words are in each theme's ink"
    );
    for role in ["title", "heading", "body", "label", "caption", "code"] {
        assert!(
            dark.contains(&format!("--tilo-{role}-tracking:")),
            "the {role} type's spacing: {dark}"
        );
    }
    let html = document("Notes", &tilo_ui_base::Theme::dark(), "", "");
    assert!(html.contains(&dark), "the page carries its theme");
}

/// The page moves as the theme does. A control's tint fades and a switch's knob travels over the
/// theme's `fast`, easing out the way the native controls settle. Reduced motion keeps the fade and
/// lands the travel, and a theme that holds still lands both.
#[test]
fn the_page_moves_by_the_themes_motion() {
    let prop = |css: &str, name: &str| {
        css.split(&format!("--tilo-{name}:"))
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .unwrap_or_else(|| panic!("--tilo-{name} in {css}"))
            .to_owned()
    };
    let theme = tilo_ui_base::Theme::light();
    let fast = format!("{}ms", theme.tokens.motion.durations.fast_ms);
    let css = theme_css(&theme);
    assert_eq!(prop(&css, "fade"), fast);
    assert_eq!(prop(&css, "move"), fast);
    assert_eq!(
        prop(&css, "appear-time"),
        format!("{}ms", theme.tokens.motion.durations.base_ms),
        "a menu arrives over `base`"
    );
    assert_eq!(prop(&css, "appear-props"), "opacity,transform");
    // One curve per property: its surface is opaque and at its size a third of the way in, and
    // from then on only its rows move, on their own curve.
    let curves: Vec<Vec<f64>> = prop(&css, "appear-curve")
        .split("linear(")
        .skip(1)
        .map(|curve| {
            curve
                .trim_end_matches([',', ')'])
                .split(',')
                .map(|point| point.parse().expect("a number"))
                .collect()
        })
        .collect();
    let [opacity, transform] = curves.as_slice() else {
        panic!("an opacity and a transform curve: {curves:?}");
    };
    let third = opacity.len() / 3 + 1;
    assert_eq!(opacity[third], 1.0, "opaque by a third: {opacity:?}");
    assert_eq!(
        transform[third], 1.0,
        "at its size by a third: {transform:?}"
    );
    assert!(
        transform[1] < opacity[1],
        "it grows behind its fade: {transform:?}"
    );
    let held: Vec<f64> = prop(&css, "held-curve")
        .strip_prefix("linear(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("a linear() curve for the rows"))
        .split(',')
        .map(|point| point.parse().expect("a number"))
        .collect();
    let opaque = opacity
        .iter()
        .position(|&o| o >= 1.0)
        .unwrap_or(opacity.len());
    assert!(
        held.iter().take(opaque).all(|&h| h == 0.0),
        "its rows wait for an opaque surface: {held:?} against {opacity:?}"
    );
    assert!(
        held[third] > 0.0 && held[third] < 1.0 && held[held.len() * 2 / 3] == 1.0,
        "its rows come in after a third and are in place by two thirds: {held:?}"
    );
    assert_eq!(prop(&css, "held-rise"), "4px");
    let settle = prop(&css, "settle");
    let points: Vec<f64> = settle
        .strip_prefix("linear(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("a linear() curve: {settle}"))
        .split(',')
        .map(|point| point.parse().expect("a number"))
        .collect();
    assert_eq!(points.first(), Some(&0.0), "{settle}");
    assert_eq!(points.last(), Some(&1.0), "{settle}");
    assert!(
        points.windows(2).all(|pair| pair[0] <= pair[1]),
        "it never turns back: {settle}"
    );
    assert!(
        points[points.len() / 2] > 0.9,
        "it eases out, most of the way there by half its time: {settle}"
    );

    let mut reduced = tilo_ui_base::Theme::light();
    reduced.reduce_motion = true;
    let css = theme_css(&reduced);
    assert_eq!(prop(&css, "fade"), fast, "reduced motion still fades");
    assert_eq!(prop(&css, "move"), "0ms", "reduced motion lands the travel");
    assert_eq!(
        (prop(&css, "appear-time"), prop(&css, "appear-props")),
        (fast.clone(), "opacity".to_owned()),
        "under reduced motion a menu only fades in, over `fast`"
    );

    let mut still = tilo_ui_base::Theme::light();
    still.tokens.motion.still = true;
    let css = theme_css(&still);
    assert_eq!(prop(&css, "fade"), "0ms");
    assert_eq!(prop(&css, "move"), "0ms");
    assert_eq!(prop(&css, "appear-time"), "0ms");

    for name in [
        "fade",
        "move",
        "settle",
        "appear-time",
        "appear-curve",
        "appear-props",
        "appear-from",
    ] {
        assert!(
            PAGE_CSS.contains(&format!("var(--tilo-{name})")),
            "the stylesheet moves by --tilo-{name}"
        );
    }
}

/// A glyph is a vector: the page strokes it from the icon set's own sources, in the ink around
/// it, where the host drew no picture of the widget.
#[test]
fn a_glyph_is_drawn_from_the_icon_set_where_no_host_drew_it() {
    let mut composition = window("Icon");
    let mut icon = Composed::new(
        "Root[0]/IconWidget[0]".to_owned(),
        "IconWidget",
        [40.0, 100.0, 20.0, 20.0],
    );
    icon.glyph = Some("clock".to_owned());
    icon.picture = Some(Picture::default());
    composition.root.children.push(icon);
    let html = Page::new().render(&composition);
    assert!(
        html.contains("<svg class=\"tilo-glyph\" viewBox=\"0 0 24 24\""),
        "the glyph is drawn: {html}"
    );
    assert!(html.contains("<path d=\"M"), "its source path: {html}");
    assert!(!html.contains("tilo-paint"), "no empty picture: {html}");
}

/// A widget shown as a picture of its paint that no host drew says its words on the page
/// instead of standing there an empty box.
#[test]
fn a_widget_with_no_picture_shows_the_words_it_says() {
    let mut composition = window("Empty");
    let mut empty = Composed::new(
        "Root[0]/EmptyState[0]".to_owned(),
        "EmptyState",
        [40.0, 100.0, 400.0, 200.0],
    );
    empty.picture = Some(Picture::default());
    empty.semantics = Some(
        AccessNode::new(Role::Group)
            .with_name("No artwork yet".to_owned())
            .with_description("New artwork starts one".to_owned()),
    );
    composition.root.children.push(empty);
    let html = Page::new().render(&composition);
    assert!(
        html.contains("<span class=\"tilo-title\">No artwork yet</span>")
            && html.contains("<span class=\"tilo-detail\">New artwork starts one</span>"),
        "{html}"
    );
}

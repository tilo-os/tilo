use super::*;
use crate::a11y::{AccessNode, AccessState};
use crate::composition::Picture;

fn text(path: &str, words: &str) -> Composed {
    let mut node = Composed::new(path.to_owned(), "Label", [13.0, 27.0, 200.0, 20.0]);
    node.semantics = Some(AccessNode::new(Role::StaticText).with_value(words.to_owned()));
    node.layout = Some("display:flex;width:120px;".to_owned());
    node
}

fn window(title: &str) -> Composition {
    let mut root = Composed::new("Root[0]".to_owned(), "Column", [0.0, 0.0, 1280.0, 720.0]);
    root.layout = Some("display:flex;flex-direction:column;gap:8px 8px;".to_owned());
    root.flows = true;
    let mut button = Composed::new(
        "Root[0]/Button[0]".to_owned(),
        "Button",
        [40.0, 60.0, 90.0, 28.0],
    );
    button.semantics = Some(AccessNode::new(Role::Button).with_name("New note".to_owned()));
    root.children = vec![text("Root[0]/Label[0]", title), button];
    Composition {
        viewport: [1280, 720],
        focus: None,
        root,
        layers: Vec::new(),
    }
}

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
    assert_eq!(page.path(patches[0].id), Some("Root[0]/Label[0]"));
    assert!(patches[0].html.contains("Groceries list"));
    assert!(page.patch(&window("Groceries list")).is_empty());
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

/// Every `data-p` in `html`, as the number it names.
fn numbers(html: &str) -> Vec<u32> {
    html.match_indices("data-p=\"")
        .filter_map(|(at, _)| {
            let rest = &html[at + "data-p=\"".len()..];
            let end = rest.find('"')?;
            rest.get(..end)?.parse().ok()
        })
        .collect()
}

/// A canvas at (100, 50) holding two places, the second a window's well.
fn canvas(second_at: [f64; 2]) -> Composition {
    let mut map = Composed::new("Root[0]".to_owned(), "Map", [100.0, 50.0, 800.0, 600.0]);
    let mut first = text("Root[0]/MapPin[0]", "Groceries");
    first.rect = [140.0, 90.0, 300.0, 200.0];
    first.layout = None;
    let mut second = Composed::new(
        "Root[0]/MapPin[1]".to_owned(),
        "MapPin",
        [second_at[0], second_at[1], 400.0, 300.0],
    );
    let mut well = Composed::new(
        "Root[0]/MapPin[1]/WindowWell[0]".to_owned(),
        "WindowWell",
        [second_at[0], second_at[1], 400.0, 300.0],
    );
    well.semantics = Some(AccessNode::new(Role::Window).with_item("tab-7".to_owned()));
    // A window's frame lays its well out itself, as a column.
    second.flows = true;
    second.children = vec![well];
    map.children = vec![first, second];
    Composition {
        viewport: [1024, 720],
        focus: None,
        root: map,
        layers: Vec::new(),
    }
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
    assert!(
        patches[0]
            .html
            .contains("position:absolute;left:420px;top:280px;"),
        "{patches:?}"
    );
    assert_eq!(page.path(patches[0].id), Some("Root[0]/MapPin[1]"));
}

/// A real widget's frame, laid out at `size`, as a page reads it: its composition with the
/// pictures of what only paints.
fn framed(root: Box<dyn crate::widget::Widget>, size: (u32, u32)) -> crate::headless::FrameHarness {
    let mut frame = crate::headless::FrameHarness::new(crate::State::new(), root, size);
    frame.relayout();
    frame
}

fn shown(frame: &mut crate::headless::FrameHarness) -> Composition {
    let mut composition = frame.composition();
    frame.paint_pictures(&mut composition);
    composition
}

/// The opening tag of the first element of `html` whose tag holds `mark`, and what follows it.
fn element_with<'a>(html: &'a str, mark: &str) -> &'a str {
    let at = html
        .find(mark)
        .unwrap_or_else(|| panic!("no element with {mark}: {html}"));
    &html[html[..at].rfind('<').expect("a tag")..]
}

/// The number the page gave the element whose tag holds `mark`.
fn number_of(html: &str, mark: &str) -> u32 {
    numbers(element_with(html, mark))
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the element with {mark} is numbered: {html}"))
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

/// A picked day says on the page the date it stands for, which a browser and a reader read.
#[test]
fn a_picked_date_is_a_time_on_the_page() {
    let day = jiff::civil::date(2026, 11, 23);
    let mut frame = framed(Box::new(crate::widget::DatePicker::new(day)), (360, 420));
    let html = Page::new().render(&shown(&mut frame));
    assert!(
        html.contains("<time datetime=\"2026-11-23\">"),
        "the picked day is a time: {html}"
    );
}

/// A link is the browser's own link to its address, saying its words.
#[test]
fn a_link_is_a_link_to_its_address() {
    let mut frame = framed(
        Box::new(crate::widget::Link::new("https://tilo.so/docs").with_label("Read the docs")),
        (320, 60),
    );
    let html = Page::new().render(&shown(&mut frame));
    let link = element_with(&html, "href=");
    let open = &link[..link.find('>').expect("a tag")];
    assert!(
        open.starts_with("<a ") && open.contains("href=\"https://tilo.so/docs\""),
        "{open}"
    );
    assert!(
        link.contains(">Read the docs</a>"),
        "its words: {link:.300}"
    );
}

/// What a person does in a field on the page is the field's own: the keyboard moving onto it,
/// the words typed into it and the caret moved in them come back as its actions, and the page
/// follows each.
#[test]
fn a_fields_focus_words_and_caret_come_from_the_page() {
    let mut frame = framed(Box::new(crate::widget::TextInput::local()), (400, 80));
    let mut page = Page::new();
    let html = page.render(&shown(&mut frame));
    assert!(
        page.patch(&shown(&mut frame)).is_empty(),
        "an unchanged frame sends nothing"
    );
    let id = number_of(&html, "data-a=\"value\"");
    let path = page.path(id).expect("the field's widget").to_owned();

    frame
        .perform_semantic_named(&path, None, &crate::a11y::Action::Focus)
        .expect("the field takes the keyboard");
    let html = page.render(&shown(&mut frame));
    let body = &html[..html.find('>').expect("the body's tag")];
    assert!(
        body.contains(&format!("data-focus=\"{id}\"")),
        "the page puts its caret where the window's is: {body}"
    );

    frame
        .perform_semantic_named(
            &path,
            None,
            &crate::a11y::Action::SetValue("Buy oat milk".to_owned()),
        )
        .expect("the words are set");
    let patches = page.patch(&shown(&mut frame));
    assert!(
        patches
            .iter()
            .any(|patch| patch.html.contains("value=\"Buy oat milk\"")),
        "the field's new words come back: {patches:?}"
    );

    frame
        .perform_semantic_named(
            &path,
            None,
            &crate::a11y::Action::SetSelection {
                anchor: 4,
                focus: 7,
            },
        )
        .expect("the caret is moved");
    assert_eq!(
        frame
            .widget_mut::<crate::widget::TextInput>()
            .expect("the field")
            .selection_anchor_focus_public(),
        Some((4, 7)),
        "\"oat\" chosen on the page is chosen in the field"
    );
}

/// A dark theme is a dark page: the browser draws its own parts dark, and the words and grounds
/// take the theme's colours and its type's spacing.
#[test]
fn a_dark_theme_is_a_dark_page() {
    let light = theme_css(&crate::Theme::light());
    let dark = theme_css(&crate::Theme::dark());
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
    let html = document("Notes", &crate::Theme::dark(), "", "");
    assert!(html.contains(&dark), "the page carries its theme");
}

/// A window well in a real frame is where the page's host shows that place's window.
#[test]
fn a_window_well_holds_what_its_host_shows_for_the_window() {
    let mut frame = framed(
        Box::new(crate::widget::WindowWell::new("tab-7")),
        (640, 400),
    );
    let mut page = Page::new();
    page.wells.insert(
        "tab-7".to_owned(),
        "<a href=\"/w/ws/notes\">Notes</a>".to_owned(),
    );
    let html = page.render(&shown(&mut frame));
    assert!(
        html.contains("data-well=\"tab-7\"><a href=\"/w/ws/notes\">Notes</a>"),
        "the host's window stands in its well: {html}"
    );
}

/// The screenshot being marked up is its picture on the page, and each of the editor's tools
/// stands over it as a control a press acts on.
#[test]
fn a_screenshot_being_marked_up_is_its_picture_with_its_tools_over_it() {
    let rgba: std::sync::Arc<[u8]> = std::sync::Arc::from(vec![20_u8; 64 * 64 * 4]);
    let mut frame = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        crate::widget::ScreenshotMarkup::new(rgba, 64, 64),
        (900, 700),
        crate::Theme::dark().with_reduce_motion(true),
    );
    frame.relayout();
    let html = Page::new().render(&shown(&mut frame));
    assert!(
        html.contains("<img class=\"tilo-paint\" src=\"data:image/png;base64,"),
        "the screenshot is a picture: {html:.2000}"
    );
    let tools = html.matches("class=\"tilo-over\"").count();
    assert!(
        tools >= 3,
        "the tools stand over it, {tools} do: {html:.2000}"
    );
    assert!(
        element_with(&html, "class=\"tilo-over\"").contains("data-a="),
        "a tool acts on a press: {html:.2000}"
    );
}

//! A composition rendered as a page, driven through a real widget frame: the elements its
//! roles name, a call's tile as a canvas, a picked date and a link as what they are, a field's
//! words and caret, a screenshot's tools over it, and a menu as its own layer.
use tilo_ui_app::composition::page::Page;
use tilo_ui_app::composition::page::testing::{element_with, number_of, numbers};
use tilo_ui_app::composition::page::{PageAction, Patch};
use tilo_ui_app::composition::{Composed, Composition};

/// A real widget's frame, laid out at `size`, as a page reads it: its composition with the
/// pictures of what only paints.
fn framed(
    root: Box<dyn tilo_ui_base::widget::Widget>,
    size: (u32, u32),
) -> tilo_ui_app::headless::Driver {
    let mut frame = tilo_ui_app::headless::Driver::new(tilo_ui_app::State::new(), root, size);
    frame.relayout();
    frame
}

fn shown(frame: &mut tilo_ui_app::headless::Driver) -> Composition {
    let mut composition = frame.composition();
    frame.paint_pictures(&mut composition);
    composition
}

/// A call's tile is the browser's picture: the page marks where it stands and carries a canvas
/// the page decodes the call's stream into, never a still of the tile's paint.
#[test]
fn a_call_tile_is_a_canvas_the_page_decodes_and_never_a_picture() {
    use tilo_ui_app::widget::{CallSource, CallTile};
    let person = "ab".repeat(32);
    let tile = CallTile::of(
        tilo_ui_base::component::Person::new(tilo_actions::MemberId::person(&person), "Ada"),
        CallSource::Camera,
    );
    let mut frame = framed(Box::new(tile), (640, 360));
    let composition = shown(&mut frame);
    // No still of the tile is drawn: the page carries the stream.
    fn painted(node: &Composed) -> bool {
        node.picture.is_some() || node.children.iter().any(painted)
    }
    assert!(
        !painted(&composition.root),
        "a live picture is not painted: {composition:?}"
    );
    let html = Page::new().render(&composition);
    assert!(
        html.contains(&format!("data-live=\"tilo-live://call/{person}/camera\"")),
        "the page says which stream the tile shows: {html}"
    );
    assert!(
        html.contains("<canvas class=\"tilo-call-picture\""),
        "the tile holds the canvas the page draws: {html}"
    );
    assert!(
        html.contains("tilo-call-name") && html.contains("Ada"),
        "the tile says whose it is: {html}"
    );
    assert!(
        !html.contains("data:image/png"),
        "no picture of the tile is sent: {html}"
    );
}

/// A picked day says on the page the date it stands for, which a browser and a reader read.
#[test]
fn a_picked_date_is_a_time_on_the_page() {
    let day = tilo_ui_app::jiff::civil::date(2026, 11, 23);
    let mut frame = framed(
        Box::new(tilo_ui_app::widget::DatePicker::new(day)),
        (360, 420),
    );
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
        Box::new(
            tilo_ui_app::widget::Link::new("https://tilo.so/docs").with_label("Read the docs"),
        ),
        (320, 60),
    );
    let html = Page::new().render(&shown(&mut frame));
    let link = element_with(&html, "href=");
    let open = link.split('>').next().expect("a tag");
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
    let mut frame = framed(Box::new(tilo_ui_app::widget::TextInput::local()), (400, 80));
    let mut page = Page::new();
    let html = page.render(&shown(&mut frame));
    assert!(
        page.patch(&shown(&mut frame)).is_empty(),
        "an unchanged frame sends nothing"
    );
    let id = number_of(&html, "data-a=\"value\"");
    let path = page.path(id).expect("the field's widget").to_owned();

    frame
        .perform_semantic_named(&path, None, &tilo_ui_base::a11y::Action::Focus)
        .expect("the field takes the keyboard");
    let html = page.render(&shown(&mut frame));
    let body = html.split('>').next().expect("the body's tag");
    assert!(
        body.contains(&format!("data-focus=\"{id}\"")),
        "the page puts its caret where the window's is: {body}"
    );

    frame
        .perform_semantic_named(
            &path,
            None,
            &tilo_ui_base::a11y::Action::SetValue("Buy oat milk".to_owned()),
        )
        .expect("the words are set");
    let patches = page.patch(&shown(&mut frame));
    assert!(
        patches
            .iter()
            .any(|patch| matches!(patch, Patch::Replace { html, .. } if html.contains("value=\"Buy oat milk\""))),
        "the field's new words come back: {patches:?}"
    );

    frame
        .perform_semantic_named(
            &path,
            None,
            &tilo_ui_base::a11y::Action::SetSelection {
                anchor: 4,
                focus: 7,
            },
        )
        .expect("the caret is moved");
    assert_eq!(
        frame
            .widget_mut::<tilo_ui_app::widget::TextInput>()
            .expect("the field")
            .selection_anchor_focus_public(),
        Some((4, 7)),
        "\"oat\" chosen on the page is chosen in the field"
    );
}

/// A window well in a real frame is where the page's host shows that place's window.
#[test]
fn a_window_well_holds_what_its_host_shows_for_the_window() {
    let mut frame = framed(
        Box::new(tilo_ui_app::widget::WindowWell::new("tab-7")),
        (640, 400),
    );
    let mut page = Page::new();
    page.wells.insert(
        "tab-7".to_owned(),
        "<a href=\"/w/ws/notes\">Notes</a>".to_owned(),
    );
    let html = page.render(&shown(&mut frame));
    // The well is the frame's root here, so its element also carries the root's mark: what it
    // holds starts where its opening tag ends.
    let well = html
        .find("data-well=\"tab-7\"")
        .map(|at| &html[at..])
        .unwrap_or_else(|| panic!("the page has the window's well: {html}"));
    let held = well.find('>').map_or("", |end| &well[end + 1..]);
    assert!(
        held.starts_with("<a href=\"/w/ws/notes\">Notes</a>"),
        "the host's window stands in its well: {html}"
    );
}

/// The screenshot being marked up is its picture on the page, and each of the editor's tools
/// stands over it as a control a press acts on.
#[test]
fn a_screenshot_being_marked_up_is_its_picture_with_its_tools_over_it() {
    let rgba: std::sync::Arc<[u8]> = std::sync::Arc::from(vec![20_u8; 64 * 64 * 4]);
    let mut frame = tilo_ui_app::headless::Driver::for_root(
        tilo_ui_app::State::new(),
        tilo_ui_app::widget::ScreenshotMarkup::new(rgba, 64, 64),
        (900, 700),
        tilo_ui_base::Theme::dark().with_reduce_motion(true),
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

/// A menu button's menu, drawn over the window from the button's own draw, stands on the page as
/// a layer of its own: opening it puts the menu in and changes the button alone, so the window
/// keeps every element it had (its scroll, the button's focus), and the menu grows from the
/// button. A press on one of its rows, on the page, runs the row's action.
#[test]
fn a_menu_buttons_menu_is_a_layer_of_its_own_that_grows_from_the_button() {
    use tilo_ui_app::widget::{Button, Column, MenuItem, Text};
    use tilo_ui_base::icons::Icon;
    let pinned = std::rc::Rc::new(std::cell::Cell::new(false));
    let pin = std::rc::Rc::clone(&pinned);
    let tree = Column::new()
        .push(Box::new(Text::new("Sunday lunch")))
        .push(Box::new(Button::new("more", "More").with_menu(vec![
            MenuItem::icon_action(Icon::Copy, "Copy link", || {}),
            MenuItem::icon_action(Icon::Pin, "Pin to the top", move || pin.set(true)),
        ])))
        .push(Box::new(Text::new("The boiler man comes at ten")));
    let mut driver = tilo_ui_app::app::driver::Driver::new(
        tilo_ui_app::State::new(),
        Box::new(tree),
        (480, 320),
    );
    driver.relayout();
    let mut page = Page::new();
    let closed = driver.composition();
    let html = page.render(&closed);
    let button = numbers(&html)
        .into_iter()
        .find(|&id| {
            page.path(id)
                .is_some_and(|path| path.ends_with("MenuButton[0]"))
        })
        .expect("the page numbers the button");
    let path = page.path(button).expect("its path").to_owned();
    page.semantic_target(button, PageAction::Activate)
        .and_then(|(widget, action)| driver.perform_semantic_named(widget, None, &action))
        .expect("a press on More is taken");
    driver.relayout();
    driver.settle();

    let open = driver.composition();
    let [menu] = open.layers.as_slice() else {
        panic!("the open menu is a layer of its own: {:?}", open.layers);
    };
    assert_eq!(menu.path, format!("{path}#ContextMenu"));
    let [x, y, width, height] = closed
        .root
        .children
        .iter()
        .find(|node| node.path == path)
        .expect("the button")
        .rect;
    assert_eq!(
        menu.appears_from,
        Some([x + width / 2.0, y + height / 2.0]),
        "it grows from the button"
    );
    let patches = page.patch(&open);
    let inserted: Vec<&str> = patches
        .iter()
        .filter_map(|patch| match patch {
            Patch::Insert { html, .. } => Some(html.as_str()),
            _ => None,
        })
        .collect();
    let [layer] = inserted.as_slice() else {
        panic!("the menu alone is put in: {patches:?}");
    };
    assert!(
        layer.contains("data-layer")
            && layer.contains("--tilo-appear-from:")
            && layer.contains("Pin to the top"),
        "{layer}"
    );
    // The page sets the menu's rows in its own face, so it takes its width from them, never less
    // than the app laid it out, and its height from its rows: a width measured by another renderer
    // cut a label short.
    let surface = layer
        .split("data-w=\"ContextMenu\"")
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .unwrap_or_else(|| panic!("the menu stands in its layer: {layer}"));
    assert!(
        surface.contains("width:max-content;")
            && surface.contains("min-width:")
            && !surface
                .split(";width:")
                .skip(1)
                .any(|value| value.starts_with(|c: char| c.is_ascii_digit()))
            && !surface.contains("height:"),
        "the menu is as wide and as tall as its rows: {surface}"
    );
    for patch in &patches {
        if let Patch::Replace { id, html } = patch {
            assert_eq!(
                page.path(*id),
                Some(path.as_str()),
                "only the button changes, never the window: {html}"
            );
            assert!(
                !html.contains("Pin to the top"),
                "the menu is not in the button: {html}"
            );
        }
    }

    let shown = page.render(&open);
    let row = numbers(&shown).into_iter().find(|&id| {
        page.path(id)
            .is_some_and(|row| row.starts_with(&format!("{path}#ContextMenu#")))
            && element_start(&shown, id).is_some_and(|rest| {
                rest.split("data-p=\"")
                    .nth(1)
                    .unwrap_or(rest)
                    .contains("Pin to the top")
            })
    });
    let row = row.unwrap_or_else(|| panic!("the page numbers Pin to the top: {shown}"));
    // The row is the row the app paints: its pin glyph, then its words.
    let start = element_start(&shown, row).unwrap_or_default();
    let words = start.find(">Pin to the top<").unwrap_or(usize::MAX);
    let pin = tilo_ui_base::icons::glyph_svg(Icon::Pin);
    assert!(
        start.find(&pin).is_some_and(|glyph| glyph < words),
        "the row shows its glyph before its words: {start}"
    );
    let target = opening_tag(&shown, row)
        .and_then(|tag| tag.split("data-t=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_owned);
    page.semantic_target(row, PageAction::Activate)
        .and_then(|(widget, action)| {
            driver.perform_semantic_named(widget, target.as_deref(), &action)
        })
        .expect("a press on the row is taken");
    driver.settle();
    assert!(pinned.get(), "the row's action ran");
}

/// `html` from the element numbered `id` on.
fn element_start(html: &str, id: u32) -> Option<&str> {
    let at = html.find(&format!("data-p=\"{id}\""))?;
    html.get(at..)
}

/// The opening tag of the element numbered `id` in `html`.
fn opening_tag(html: &str, id: u32) -> Option<&str> {
    let marker = format!("data-p=\"{id}\"");
    let at = html.find(&marker)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    html.get(start..=end)
}

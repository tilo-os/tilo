//! The shell's windows as a person sees them, staged from its own scenarios through the UI
//! host's path: windows standing with their apps' content, one bar per window, filling the
//! strips the desk gives them, and the trailing parts of the top bar at its edge.

use std::path::Path;
use std::rc::Rc;

use super::stage;
use ui::kurbo::Rect;
use ui::widget::Widget;

fn packages() -> (String, Rc<stage::Packages>) {
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    let search = vec![shell.parent().expect("apps/tl").to_path_buf()];
    let (built, platform) = super::build(&shell, &search).expect("the shell compiles");
    let app = built.last().expect("the shell").compiled.meta.app.clone();
    let packages = Rc::new(stage::Packages(
        Rc::new(built.into_iter().map(|b| b.compiled).collect()),
        platform,
    ));
    (app, packages)
}

/// The app `name` and every package it uses, staged from `apps/tl/<name>`.
fn app_packages(name: &str) -> Rc<stage::Packages> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/tl")
        .join(name);
    let search = vec![dir.parent().expect("apps/tl").to_path_buf()];
    let (built, platform) = super::build(&dir, &search).expect("the app compiles");
    Rc::new(stage::Packages(
        Rc::new(built.into_iter().map(|b| b.compiled).collect()),
        platform,
    ))
}

/// The tasks app and every package it uses, staged from `apps/tl/tasks`.
fn tasks_packages() -> Rc<stage::Packages> {
    app_packages("tasks")
}

/// Every widget under `root` named `name`, with its rect, in drawing order.
fn all<'a>(root: &'a dyn Widget, name: &str, out: &mut Vec<&'a dyn Widget>) {
    if root.name() == name {
        out.push(root);
    }
    for child in root.children() {
        all(child, name, out);
    }
}

fn named<'a>(root: &'a dyn Widget, name: &str) -> Vec<&'a dyn Widget> {
    let mut out = Vec::new();
    all(root, name, &mut out);
    out
}

/// Every node of `kind` under `node`, in drawing order.
fn nodes<'a>(node: &'a tilo_ui_host::UiNode, kind: &str, out: &mut Vec<&'a tilo_ui_host::UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        nodes(child, kind, out);
    }
}

/// Every node of `kind` under `node`.
fn nodes_of<'a>(node: &'a tilo_ui_host::UiNode, kind: &str) -> Vec<&'a tilo_ui_host::UiNode> {
    let mut out = Vec::new();
    nodes(node, kind, &mut out);
    out
}

/// The catch-up a window's mark opens: each item it changed, newest first, with who and
/// when, and the one move that marks them all seen.
fn the_catch_up_names_each_item_it_changed(window: &tilo_ui_host::UiNode) {
    let content = window
        .children
        .iter()
        .find(|child| {
            child.kind == "slot"
                && child.props.get("name")
                    == Some(&tilo_render_protocol::patch::Value::text("content"))
        })
        .and_then(|slot| slot.children.first())
        .unwrap_or_else(|| panic!("the window's mark opens a catch-up: {window:?}"));
    let labels: Vec<String> = nodes_of(content, "ui.Button")
        .into_iter()
        .filter_map(|button| match button.props.get("label") {
            Some(tilo_render_protocol::patch::Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            "Buy oat milk and eggs",
            "Read the survey responses",
            "Sort the paper mail",
            "Mark all seen",
        ],
        "the catch-up names each item it changed, newest first, then the one move"
    );
    assert!(
        !nodes_of(content, "ui.Avatar").is_empty() && !nodes_of(content, "ui.Time").is_empty(),
        "each step says who made it and when: {content:?}"
    );
    // A line reads as history writes a sentence: the name first, then what they did
    // ("Scout marked it done"). A face alone does not say who.
    let mut rows = Vec::new();
    nodes(content, "row", &mut rows);
    let steps: Vec<&tilo_ui_host::UiNode> = rows
        .into_iter()
        .filter(|row| row.key.contains("~step-"))
        .collect();
    assert!(
        !steps.is_empty(),
        "the catch-up holds a line per step: {content:?}"
    );
    let mut lines = Vec::new();
    for step in &steps {
        let texts: Vec<(&str, String)> = nodes_of(step, "ui.Text")
            .into_iter()
            .map(|text| {
                let role = match text.props.get("role") {
                    Some(tilo_render_protocol::patch::Value::Text(role)) => role.as_str(),
                    _ => "",
                };
                match text.props.get("value") {
                    Some(tilo_render_protocol::patch::Value::Text(value)) => (role, value.clone()),
                    _ => panic!("a step's text says something: {text:?}"),
                }
            })
            .collect();
        assert!(
            texts.len() >= 2 && texts[0].0 == "label" && !texts[0].1.is_empty(),
            "a step leads with who made it, by name: {texts:?}"
        );
        assert!(
            !texts[1].1.is_empty() && !texts[1].1.contains("this"),
            "and goes on with what they did: {texts:?}"
        );
        lines.push(format!("{} {}", texts[0].1, texts[1].1));
    }
    assert!(
        lines.iter().all(|line| ["Bo", "Scout", "You"]
            .iter()
            .any(|name| line.starts_with(name))),
        "every step names its person before its verb: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("Bo "))
            && lines.iter().any(|line| line.starts_with("Scout ")),
        "the steps name Bo and her agent: {lines:?}"
    );
}

/// What the audit finds wrong with `staged`'s window as it stands at `viewport`.
fn wrong_in(staged: &stage::Staged, viewport: (u32, u32)) -> Vec<String> {
    let inspection = staged.harness().inspection();
    super::audit::inspect(&inspection.root, viewport, None, 1.0)
        .into_iter()
        .map(|problem| {
            format!(
                "{}: {} in {}",
                problem.kind.name(),
                problem.detail,
                problem.widget
            )
        })
        .collect()
}

/// Every place the widget `name` (`ui.ChangeMark`, `Popover`) is laid out, in tree order.
fn rects(staged: &stage::Staged, name: &str) -> Vec<Rect> {
    fn all(node: &ui::headless::InspectionNode, name: &str, out: &mut Vec<Rect>) {
        if node.widget == name {
            let [x, y, width, height] = node.rect;
            out.push(Rect::new(x, y, x + width, y + height));
        }
        for child in &node.children {
            all(child, name, out);
        }
    }
    let mut out = Vec::new();
    all(&staged.harness().inspection().root, name, &mut out);
    out
}

/// Where the floating surface the widget `name` draws, a popover or a sheet, stands.
fn floating_rect(staged: &stage::Staged, name: &str) -> Option<Rect> {
    rects(staged, name).into_iter().next()
}

/// Where the floating surface the widget `name` draws stands once it is shown: a page's fields
/// carry popovers of their own, laid out only while open, so the first one is not the surface a
/// press opened.
fn shown_floating_rect(staged: &stage::Staged, name: &str) -> Option<Rect> {
    rects(staged, name)
        .into_iter()
        .find(|rect| rect.width() > 0.0 && rect.height() > 0.0)
}

/// The change marks the rows of the window's lists draw, by the row they stand in.
fn row_marks(
    rows: &std::collections::HashMap<String, tilo_ui_host::UiNode>,
) -> Vec<tilo_ui_host::UiNode> {
    let mut out = Vec::new();
    for row in rows.values() {
        let mut marks = Vec::new();
        nodes(row, "ui.ChangeMark", &mut marks);
        out.extend(marks.into_iter().cloned());
    }
    out
}

/// The changes the rows of `staged`'s window show, as who made it, what they are called and
/// what they did: a list row draws its mark in the row instead of holding a node of it, so
/// what the person reads is read off the rows themselves.
fn row_changes(staged: &stage::Staged) -> Vec<(String, String, String)> {
    fn collect<'a>(root: &'a dyn ui::widget::Widget, out: &mut Vec<&'a ui::widget::ListRow>) {
        if let Some(row) = root
            .as_any()
            .and_then(|widget| widget.downcast_ref::<ui::widget::ListRow>())
        {
            out.push(row);
        }
        for child in root.children() {
            collect(child, out);
        }
        // A row that swipes holds its row inside itself, not among its children.
        if let Some(widget) = root.as_any() {
            if let Some(swipe) = widget.downcast_ref::<ui::widget::SwipeRow>() {
                collect(swipe.content(), out);
            }
            if let Some(toolbar) = widget.downcast_ref::<ui::widget::HoverToolbar>()
                && let Some(row) = toolbar.row()
            {
                collect(row, out);
            }
        }
    }
    let mut rows = Vec::new();
    collect(staged.harness().root_widget(), &mut rows);

    // One change reads once however many rows show it: a task open where it stands shows it
    // on its row and again over the step that made it, and both are the same change.
    let mut changes = Vec::new();
    for change in rows
        .into_iter()
        .filter_map(ui::widget::ListRow::shown_change)
    {
        let change = (
            change.identity.to_owned(),
            change.name.to_owned(),
            change.said.to_owned(),
        );
        if !changes.contains(&change) {
            changes.push(change);
        }
    }
    changes
}

/// The mark a window's header carries: what changed in what it shows, and by whom.
fn window_mark(tree: &tilo_ui_host::UiNode) -> Option<tilo_ui_host::UiNode> {
    let mut marks = Vec::new();
    nodes(tree, "ui.ChangeMark", &mut marks);
    marks
        .into_iter()
        .find(|mark| {
            mark.props.get("window") == Some(&tilo_render_protocol::patch::Value::Bool(true))
        })
        .cloned()
}

/// The window's own tabs under `root`: the strip a `TabStrip` draws (it draws as the
/// toolkit's bar), never a tab bar an app puts in its own content.
fn tab_strips(root: &dyn Widget) -> Vec<&dyn Widget> {
    named(root, "TabBar")
        .into_iter()
        .filter(|bar| is_tab_strip(*bar))
        .collect()
}

/// Whether `bar` is a window's own tabs rather than a tab bar an app put in its content.
fn is_tab_strip(bar: &dyn Widget) -> bool {
    match bar.as_any() {
        Some(any) => any.is::<ui::widget::TabStrip>(),
        None => false,
    }
}

/// How many places the tab strip `bar` shows.
fn tabs_of(bar: &dyn Widget) -> usize {
    bar.as_any()
        .and_then(|any| any.downcast_ref::<ui::widget::TabStrip>())
        .map_or(0, |strip| strip.bar().tabs().len())
}

#[test]
fn its_windows_show_their_apps_content_under_one_bar_each() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    let staged = stage::Staged::over(
        &app,
        &packages,
        "the traditional desktop",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let root = staged.harness().root_widget();
    let frames = named(root, "WindowFrame");
    assert_eq!(frames.len(), 3, "two notes and a channel stand on the desk");
    for frame in frames {
        let window = frame.get_rect();
        // What the window holds is cut at its edge, so a list scrolled past its top is
        // never read over the window behind it.
        assert_eq!(
            frame.child_clip_rect(),
            Some(window),
            "the window clips what it holds"
        );
        // The window shows its resource through its type's own view, filling it.
        let shells = named(frame, "NavShell");
        let [shell] = shells[..] else {
            panic!("one app window in each frame, not {}", shells.len());
        };
        let content = shell.get_rect();
        assert!(
            content.height() > window.height() * 0.8,
            "the app's window fills the frame: {content:?} in {window:?}"
        );
        // The frame's controls stand in the app's own header row, at its trailing end,
        // so the window has one bar: no bar of the frame's above the app's.
        assert!(
            (content.y0 - window.y0).abs() < 1.0,
            "no bar of the frame's stands over the app's header: {content:?} in {window:?}"
        );
        let buttons = named(frame, "Button");
        let close = buttons
            .iter()
            .map(|b| b.get_rect())
            .max_by(|a, b| a.x1.total_cmp(&b.x1))
            .expect("the frame's close");
        assert!(
            close.y1 < window.y0 + 64.0 && window.x1 - close.x1 < 24.0,
            "the close stands at the end of the window's bar: {close:?} in {window:?}"
        );
        let overlapping: Vec<Rect> = buttons
            .iter()
            .map(|b| b.get_rect())
            .filter(|b| *b != close && b.intersect(close).area() > 0.0)
            .collect();
        assert!(
            overlapping.is_empty(),
            "the app's header keeps room for the frame's controls: {overlapping:?}"
        );
    }
}

#[test]
fn the_strips_windows_fill_their_strip_and_hold_their_apps_under_one_bar() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    let staged = stage::Staged::over(
        &app,
        &packages,
        "a desk of places",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let frames = named(staged.harness().root_widget(), "WindowFrame");
    assert!(
        frames.len() >= 6,
        "the desk's strips stand as windows, not cards: {} frames",
        frames.len()
    );
    windows_fill_their_strips(&frames);

    // A strip of several places is one window whose bar is its tabs, the place in front
    // filling it under them; a strip of one has no tab bar above its app.
    let grouped = stage::Staged::over(
        &app,
        &packages,
        "a window of several places",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let group = named(grouped.harness().root_widget(), "WindowFrame");
    assert_eq!(group.len(), 2, "a window of places beside a window of one");
    the_places_of_a_strip_are_its_tabs(&group);

    // Two windows the desk holds stand side by side, each the share of the desk its strip's
    // width names, not the floor two of them would need: a row spreads across the desk while
    // its windows fit, and pans to them whole once they do not.
    let from = group
        .iter()
        .map(|frame| frame.get_rect().x0)
        .fold(f64::MAX, f64::min);
    let to = group
        .iter()
        .map(|frame| frame.get_rect().x1)
        .fold(f64::MIN, f64::max);
    assert!(
        to - from >= 1270.0,
        "the desk's row spreads across it at 1280: {} to {to}",
        from.round()
    );
}

#[test]
fn a_window_alone_on_the_desk_takes_the_desk() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    let staged = stage::Staged::over(
        &app,
        &packages,
        "a window alone on the desk",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let frames = named(staged.harness().root_widget(), "WindowFrame");
    assert_eq!(
        frames.len(),
        1,
        "one window on the desk, not {}",
        frames.len()
    );
    let window = frames[0].get_rect();
    assert!(
        window.width() > 1200.0,
        "a lone window takes the desk's width at 1280: {window:?}"
    );
    bar_and_content_in(frames[0], window);
}

/// A window is a window: it fills its strip's whole room, and every window of the desk
/// shares that height, as the chrome's groups do. A card is a few hundred pixels tall with
/// a sliver of its app under it.
fn windows_fill_their_strips(frames: &[&dyn Widget]) {
    // The desk's own windows: the overlays the shell stages beside them are frames too.
    let frames: Vec<&dyn Widget> = frames
        .iter()
        .copied()
        .filter(|frame| frame.get_rect().height() > 100.0)
        .collect();
    let height = frames
        .iter()
        .map(|f| f.get_rect().height())
        .fold(0.0, f64::max);
    assert!(
        height > 500.0,
        "a window fills the desk's height: {height} at 1280x800"
    );
    for frame in &frames {
        let window = frame.get_rect();
        assert!(
            (window.height() - height).abs() < 1.0,
            "every window of the desk is as tall as the desk: {window:?} beside {height}"
        );
        // Its share of the row, never narrower than the chrome's own floor: a row of more
        // windows than the desk holds pans to them whole.
        assert!(
            window.width() >= 160.0,
            "a window keeps its share of the row: {window:?}"
        );
        bar_and_content_in(*frame, window);
    }
}

/// One bar per window, whichever state it is in — the tabs of a window of several places,
/// or the app's own header row — and its app's content under that, filling the rest.
fn bar_and_content_in(frame: &dyn Widget, window: Rect) {
    let strips = tab_strips(frame);
    let shells = named(frame, "NavShell");
    if let [tabs] = strips[..] {
        let bar = tabs.get_rect();
        assert!(
            bar.height() > 40.0 && bar.height() < 56.0,
            "the tabs are one window bar tall: {bar:?}"
        );
        assert!(
            bar.y0 - window.y0 < 1.0,
            "the tabs are the window's own bar, at its top edge: {bar:?} in {window:?}"
        );
        if let [shell] = shells[..] {
            assert!(
                shell.get_rect().y0 >= bar.y1 - 1.0,
                "the app stands under the tabs, not over them: {:?} under {bar:?}",
                shell.get_rect()
            );
        }
    }
    for shell in shells {
        let content = shell.get_rect();
        assert!(
            content.y0 - window.y0 < 56.0,
            "one bar stands over the app, never two: {content:?} in {window:?}"
        );
        assert!(
            window.y1 - content.y1 < 1.0,
            "the app fills the window to its foot: {content:?} in {window:?}"
        );
    }
}

/// A strip of several places shows them as one window's tabs, the place in front under
/// them; the strip of one shows its app from the window's own edge.
fn the_places_of_a_strip_are_its_tabs(group: &[&dyn Widget]) {
    for frame in group {
        let window = frame.get_rect();
        let strips = tab_strips(*frame);
        match &strips[..] {
            [] => {}
            [bar] => {
                let tabs = bar.get_rect();
                assert!(
                    (tabs.y0 - window.y0).abs() < 1.0 && (tabs.x0 - window.x0).abs() < 1.0,
                    "the tabs are the window's own bar, at its top leading corner: {tabs:?} in {window:?}"
                );
                assert!(
                    tabs.height() > 40.0 && tabs.height() < 56.0,
                    "the tabs are one window bar tall: {tabs:?}"
                );
                assert!(
                    tabs.width() > window.width() / 2.0,
                    "the tabs are the window's bar, not a chip in it: {tabs:?} in {window:?}"
                );
                let items = tabs_of(*bar);
                assert!(
                    items >= 3,
                    "every place of the strip is a tab: {items} tabs"
                );
                let shells = named(*frame, "NavShell");
                let [shell] = shells[..] else {
                    panic!("one app in front of the tabs, not {}", shells.len());
                };
                let content = shell.get_rect();
                assert!(
                    content.y0 >= tabs.y1 - 1.0 && window.y1 - content.y1 < 1.0,
                    "the place in front fills the window under its bar: {content:?} in {window:?}"
                );
            }
            _ => panic!("one tab bar to a window, not {}", strips.len()),
        }
    }
    assert_eq!(
        group
            .iter()
            .filter(|frame| !tab_strips(**frame).is_empty())
            .count(),
        1,
        "the strip of several places shows its tabs"
    );
}

/// The controls under `root` outside any window, with their names: the shell's own.
fn shell_controls(root: &dyn Widget, out: &mut Vec<(String, Rect)>) {
    if root.name() == "WindowFrame" {
        return;
    }
    if matches!(root.name(), "Button" | "MenuButton" | "Badge" | "Avatar") {
        out.push((root.name().to_owned(), root.get_rect()));
    }
    // A count a control carries on its icon, and that icon.
    for (part, rect) in root.test_rects() {
        if part == "count" || part == "icon" {
            out.push((format!("{} {part}", root.name()), rect));
        }
    }
    for child in root.children() {
        shell_controls(child, out);
    }
}

#[test]
fn its_top_bar_is_one_row_whose_parts_never_press_into_each_other() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    for (width, height, text) in [(1280, 800, 1.0), (768, 1024, 1.0), (320, 568, 1.5)] {
        let staged = stage::Staged::over(
            &app,
            &packages,
            "the traditional desktop",
            (width, height),
            ui::Theme::light().with_text_scale(text),
            None,
            &[],
        )
        .expect("the scenario stages");
        let mut controls = Vec::new();
        shell_controls(staged.harness().root_widget(), &mut controls);
        let at = format!("{width}×{height} text {text}");
        // The bar is the band the system menu stands in; the dock's are lower down.
        let menu = controls
            .iter()
            .filter(|(n, _)| matches!(n.as_str(), "Button" | "MenuButton" | "Avatar"))
            .map(|(_, r)| *r)
            .min_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)))
            .expect("the system menu");
        let bar: Vec<&(String, Rect)> = controls
            .iter()
            .filter(|(_, r)| r.y0 < menu.y1 && r.y1 > menu.y0)
            .collect();
        let pressed: Vec<Rect> = bar
            .iter()
            .filter(|(n, _)| matches!(n.as_str(), "Button" | "MenuButton" | "Avatar"))
            .map(|(_, r)| *r)
            .collect();
        for r in &pressed {
            assert!(
                r.x0 >= 0.0 && r.x1 <= f64::from(width),
                "{r:?} stays on the output at {at}"
            );
            // One row: every part shares the system menu's band.
            assert!(
                (r.center().y - menu.center().y).abs() < 2.0,
                "{r:?} stands in the one row at {at}: {bar:?}"
            );
            for other in &pressed {
                assert!(
                    r == other || r.intersect(*other).area() <= 0.0,
                    "{r:?} presses into {other:?} at {at}"
                );
            }
        }
        assert_eq!(
            bar.iter().filter(|(n, _)| n == "Button count").count(),
            1,
            "the bell's count, the bar's one, at {at}: {bar:?}"
        );
    }
}

#[test]
fn a_count_stands_on_its_icons_corner_and_over_no_other_control() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    for (width, height, text) in [(1280, 800, 1.0), (768, 1024, 1.0), (320, 568, 1.5)] {
        let staged = stage::Staged::over(
            &app,
            &packages,
            "the traditional desktop",
            (width, height),
            ui::Theme::light().with_text_scale(text),
            None,
            &[],
        )
        .expect("the scenario stages");
        let mut controls = Vec::new();
        shell_controls(staged.harness().root_widget(), &mut controls);
        let at = format!("{width}×{height} text {text}");
        let pressed: Vec<Rect> = controls
            .iter()
            .filter(|(n, _)| matches!(n.as_str(), "Button" | "MenuButton" | "Avatar"))
            .map(|(_, r)| *r)
            .collect();
        let counts: Vec<(Rect, Rect)> = controls
            .windows(2)
            .filter_map(|pair| match pair {
                [(count, c), (icon, i)] if count.ends_with(" count") && icon.ends_with(" icon") => {
                    Some((*c, *i))
                }
                _ => None,
            })
            .collect();
        // The bell's and the mail's in the dock.
        assert_eq!(
            counts.len(),
            2,
            "the bell's count and the dock's at {at}: {controls:?}"
        );
        for (count, icon) in counts {
            // Over its own control alone: never the one beside it.
            let own = pressed
                .iter()
                .find(|r| r.contains(icon.center()))
                .expect("the control the icon is on");
            // Centred on the icon's top trailing corner, as an app icon's count stands, and
            // held inside its own face where that corner would take it past the face's edge.
            let corner = ui::kurbo::Point::new(icon.x1, icon.y0);
            let wanted = Rect::from_center_size(corner, count.size());
            let left = wanted
                .x0
                .clamp(own.x0, (own.x1 - wanted.width()).max(own.x0));
            let top = wanted
                .y0
                .clamp(own.y0, (own.y1 - wanted.height()).max(own.y0));
            let expected = Rect::new(left, top, left + wanted.width(), top + wanted.height());
            assert!(
                (count.center() - expected.center()).hypot() < 1.0,
                "the count {count:?} stands on the corner of its icon {icon:?} inside its face {own:?} at {at}"
            );
            assert!(
                count.x0 >= 0.0 && count.x1 <= f64::from(width) && count.y0 >= 0.0,
                "the count {count:?} stays on the output at {at}"
            );
            for other in pressed.iter().filter(|r| *r != own) {
                assert!(
                    other.intersect(count).area() <= 0.0,
                    "the count {count:?} stands over {other:?} at {at}"
                );
            }
        }
    }
}

#[test]
fn on_a_phone_the_window_in_front_fills_the_room_between_the_bar_and_the_dock() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    let staged = stage::Staged::over(
        &app,
        &packages,
        "the traditional desktop",
        (320, 568),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let root = staged.harness().root_widget();
    let frames = named(root, "WindowFrame");
    let [frame] = frames[..] else {
        panic!("the window in front alone, not {} windows", frames.len());
    };
    let said = {
        let mut said = Vec::new();
        read_out(&staged.harness().inspection().root, false, &mut said);
        said
    };
    assert!(
        said.iter()
            .any(|(in_window, name)| *in_window && name == "Q3 plan"),
        "the window is the one in front: {said:?}"
    );
    let chrome = named(root, "Chrome");
    let bar = chrome
        .iter()
        .map(|c| c.get_rect())
        .min_by(|a, b| a.y0.total_cmp(&b.y0))
        .expect("the top bar");
    let dock = chrome
        .iter()
        .map(|c| c.get_rect())
        .max_by(|a, b| a.y1.total_cmp(&b.y1))
        .expect("the dock");
    let window = frame.get_rect();
    assert!(
        window.y0 >= bar.y1 && window.y1 <= dock.y0,
        "the window {window:?} stands between the bar {bar:?} and the dock {dock:?}"
    );
    // It fills that room: no more than the gaps between them is left over.
    let gap = 16.0;
    assert!(
        window.y0 - bar.y1 <= gap && dock.y0 - window.y1 <= gap,
        "the window {window:?} fills the room between {bar:?} and {dock:?}"
    );
    assert!(
        window.x0 >= 0.0 && window.x1 <= 320.0 && window.width() >= 320.0 - 2.0 * gap,
        "the window {window:?} spans the output"
    );
}

/// What assistive technology reads under `node` (names and values), each with whether it
/// stands in a window.
fn read_out(node: &ui::headless::InspectionNode, in_window: bool, out: &mut Vec<(bool, String)>) {
    let in_window = in_window || node.widget == "WindowFrame";
    if let Some(access) = &node.semantics {
        for said in [&access.name, &access.value].into_iter().flatten() {
            out.push((in_window, said.clone()));
        }
    }
    for child in &node.children {
        read_out(child, in_window, out);
    }
}

/// What the traditional desktop reads out at `size`, laid out for a finger on a phone's
/// width (its controls a finger's target), as the layout check lays a phone out.
fn desktop_read_out(size: (u32, u32)) -> Vec<(bool, String)> {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    let mut staged = stage::Staged::over(
        &app,
        &packages,
        "the traditional desktop",
        size,
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let input = if size.0 < 600 {
        ui::interaction::InteractionEnvironment::TOUCH
    } else {
        ui::interaction::InteractionEnvironment::POINTER
    };
    staged
        .lay_out(size, ui::Theme::light(), input, 1.0)
        .expect("the desktop lays out");
    let mut said = Vec::new();
    read_out(&staged.harness().inspection().root, false, &mut said);
    said
}

/// Whether `node` is a shell header's overflow door, whatever language the window it
/// frames reads in: the door is known by its id, not by the words on it.
fn is_shell_door(node: &ui::headless::InspectionNode) -> bool {
    let [_, _, w, h] = node.rect;
    w > 0.0
        && h > 0.0
        && node
            .test_id
            .as_deref()
            .is_some_and(|id| id.starts_with("nav-shell-more-"))
}

/// Whether `node` is the "…" beside a page's title: the menu the page keeps its own
/// actions behind when it is a window of one thing rather than a bar over columns.
fn is_title_door(node: &ui::headless::InspectionNode) -> bool {
    let [_, _, w, h] = node.rect;
    w > 0.0
        && h > 0.0
        && node.widget.as_str() == "MenuButton"
        && node
            .test_id
            .as_deref()
            .is_some_and(|id| id.ends_with("~more"))
}

/// The window's menu on the page: the bar's door or the "…" beside a title, whichever
/// the window draws. Its items are the host's, read where the view is built; here it must
/// stand where the person reaches it.
fn shown_menu(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if is_shell_door(node) || is_title_door(node) {
        return Some(node);
    }
    node.children.iter().find_map(shown_menu)
}

#[test]
fn a_windows_header_holds_the_rest_of_its_actions_in_one_menu() {
    // A window draws its main actions as buttons and keeps the rest behind one menu: never
    // every action as a button of its own, and one door to what does not fit in one bar. The
    // desktop's bar over the window is where they stand, at a desk's width and in a hand's.
    for size in [(1280, 800), (390, 844)] {
        tilo_ui_host::i18n::ensure();
        let packages = tasks_packages();
        let mut staged = stage::Staged::over(
            "so.tilo.lang.tasks",
            &packages,
            "a busy week",
            size,
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        let input = if size.0 < 600 {
            ui::interaction::InteractionEnvironment::TOUCH
        } else {
            ui::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, ui::Theme::light(), input, 1.0)
            .expect("the app lays out");
        let bar = staged.harness_mut().window_bar_layout(f64::from(size.0));
        let buttons = bar
            .controls
            .iter()
            .filter(|control| control.slot == "leading" || control.slot == "action")
            .count();
        assert!(
            buttons <= ui::window_toolbar::MAX_LEADING + ui::window_toolbar::MAX_ACTIONS,
            "a window draws no more actions as buttons than its bar holds at {size:?}: {buttons}"
        );
        let menus: Vec<&ui::window_toolbar::BarControl> =
            bar.controls.iter().filter(|control| control.menu).collect();
        assert!(
            !menus.is_empty(),
            "a window keeps the rest of its actions in a menu at {size:?}: {bar:?}"
        );
        assert!(
            menus
                .iter()
                .all(|menu| menu.rect[2] > 0.0 && menu.rect[3] > 0.0),
            "the window's menu stands where the person reaches it at {size:?}: {menus:?}"
        );
        assert!(
            menus.iter().all(|menu| !menu.id.is_empty()),
            "the window's menu is named at {size:?}: {menus:?}"
        );
    }
}

/// A control named `name` that stands on the window (a rect with room), by its test id.
fn shown_named(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
    let [_, _, w, h] = node.rect;
    if w > 0.0
        && h > 0.0
        && node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
        && let Some(id) = &node.test_id
    {
        return Some(id.clone());
    }
    node.children
        .iter()
        .find_map(|child| shown_named(child, name))
}

#[test]
fn an_action_without_an_icon_is_reachable_from_its_window_where_nothing_lists_it() {
    // In the strips preset a window is the app's own: nothing lists its actions, so its
    // bar holds the notes app's icon-less Untag as a labelled button, or its menu does
    // when the row has no room for it.
    tilo_ui_host::i18n::ensure();
    let (_, packages) = packages();
    for size in [(1280, 800), (390, 844)] {
        let mut staged = stage::Staged::over(
            "so.tilo.lang.notes",
            &packages,
            "a week of notes",
            size,
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the notes stage");
        let input = if size.0 < 600 {
            ui::interaction::InteractionEnvironment::TOUCH
        } else {
            ui::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, ui::Theme::light(), input, 1.0)
            .expect("the notes lay out");
        let root = staged.harness().inspection().root;
        if shown_named(&root, "Untag…").is_some() {
            continue;
        }
        let door = shown_menu(&root)
            .unwrap_or_else(|| panic!("Untag… is neither shown nor behind a menu at {size:?}"));
        let [_, _, w, h] = door.rect;
        assert!(
            w > 0.0 && h > 0.0,
            "the window's menu stands where the person reaches it at {size:?}"
        );
        assert!(
            door.semantics
                .as_ref()
                .and_then(|said| said.name.as_deref())
                .is_some_and(|name| !name.is_empty()),
            "the window's menu is named at {size:?}"
        );
    }
}

#[test]
fn its_people_are_called_by_their_names_never_their_ids() {
    let said = desktop_read_out((1280, 800));
    let ids: Vec<&String> = said
        .iter()
        .map(|(_, name)| name)
        .filter(|name| name.contains("scenario:"))
        .collect();
    assert!(ids.is_empty(), "people read as ids: {ids:?}");
    assert!(
        said.iter().any(|(_, name)| name.contains("Leo")),
        "the channel names who spoke: {said:?}"
    );
}

#[test]
fn a_scenario_on_two_layers_is_named_apart() {
    // The shots of the window's own layer and of the shell's settings layer must not land
    // in one file, or the desk's shot is the settings page.
    assert_eq!(
        super::on_layer("the traditional desktop", None),
        "the traditional desktop"
    );
    assert_ne!(
        super::on_layer("the traditional desktop", Some("Shell")),
        super::on_layer("the traditional desktop", None)
    );
}

/// Lay `body` out on a thread with a window's stack: a window's tree is as deep as the
/// documents it shows, and a rich-text editor inside a row standing open needs more room than
/// a test thread's two megabytes. A window in the app lays out on the thread running the app,
/// which is the main thread's; a test gets the room to match.
fn on_a_windows_stack(body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(super::WORKER_STACK)
        .spawn(body)
        .expect("a thread starts")
        .join()
        .expect("the window lays out");
}

/// The tasks app staged as the "while you were away" scenario has it, laid out at `size`.
fn away_at(packages: &Rc<stage::Packages>, size: (u32, u32)) -> stage::Staged {
    let mut staged = stage::Staged::over(
        "so.tilo.lang.tasks",
        packages,
        "while you were away",
        size,
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    staged
        .lay_out(size, ui::Theme::light(), input_at(size), 1.0)
        .expect("the app lays out");
    staged
}

/// What a person holds a window with at `size`: a pointer at a desk's width, a finger on
/// one.
fn input_at(size: (u32, u32)) -> ui::interaction::InteractionEnvironment {
    if size.0 < 600 {
        ui::interaction::InteractionEnvironment::TOUCH
    } else {
        ui::interaction::InteractionEnvironment::POINTER
    }
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "frame and mark sizes are small counts, far inside usize and f64"
)]
fn a_capture_writes_the_scenarios_steps_down_so_what_changed_shows() {
    on_a_windows_stack(|| {
        // The marks a row and a window carry are read from the record's steps, which are the
        // store's versions and the person's seen marks: a scenario staged for a capture is
        // written down as a session writes it, or the capture shows nothing of what it staged.
        tilo_ui_host::i18n::ensure();
        ui::clock::freeze_for_capture();
        let packages = tasks_packages();
        let mut away = away_at(&packages, (1280, 800));
        let rows = row_changes(&away);
        // The two tasks of today's list that someone else changed while she was away: Bo took
        // the mail and her agent moved the survey to today, each row saying so in its words.
        assert_eq!(
            rows.len(),
            2,
            "the two rows of the tasks Bo and her agent changed are marked: {rows:?}"
        );
        for (identity, name, said) in &rows {
            assert!(
                !identity.is_empty() && identity != "ada",
                "a marked row names who changed it, never the person's own step: {identity}"
            );
            assert!(!name.is_empty(), "a marked row names the changer: {name}");
            assert!(
                !said.is_empty() && !said.contains("this") && !said.contains("done to"),
                "a marked row says what they did, in words, of \"it\": {said}"
            );
        }
        assert!(
            rows.iter()
                .any(|(_, _, said)| said.contains("assigned") && said.contains("you")),
            "the mail Bo gave her says it was given to her: {rows:?}"
        );
        let composed = away.composed().expect("the window composed its view");
        let window = window_mark(&composed)
            .unwrap_or_else(|| panic!("the window's header says what changed in it: {composed:?}"));
        // Three: Bo's assign, and the agent's two, one of them the task that is in Done now.
        assert!(
            matches!(
                window.props.get("count"),
                Some(tilo_render_protocol::patch::Value::Int(3))
            ),
            "the window counts the three changes she has not seen: {window:?}"
        );
        the_catch_up_names_each_item_it_changed(&window);
        // The list is the window's content, so its header is the window's: the mark stands among
        // the header actions, first, in the row that carries the title it counts.
        let mut slots = Vec::new();
        nodes(&composed, "slot", &mut slots);
        let header = slots
            .iter()
            .find(|slot| {
                slot.props.get("name") == Some(&tilo_render_protocol::patch::Value::text("actions"))
                    && slot
                        .children
                        .iter()
                        .any(|child| child.kind == "ui.ChangeMark")
            })
            .unwrap_or_else(|| panic!("the window's header holds the window's mark: {composed:?}"));
        assert_eq!(
            header.children[0].kind, "ui.ChangeMark",
            "the window's mark stands first among the header's actions: {header:?}"
        );
        // A node in the tree is not a pixel: the mark stands in the window's own header row,
        // over the rows it counts, and the frame has its ink there.
        let mark = floating_rect(&away, "ChangeMark").expect("the window's mark is laid out");
        assert!(
            mark.width() > 40.0 && mark.height() > 20.0,
            "the mark has room to stand in the header: {mark:?}"
        );
        let listed = rects(&away, "ListRow");
        assert!(!listed.is_empty(), "the window lists the tasks it marks");
        assert!(
            listed.iter().all(|row| mark.y1 <= row.y0),
            "the mark stands over the rows it counts, not among them: mark {mark:?}, rows {listed:?}"
        );
        assert!(
            mark.x0 >= 0.0 && mark.x1 <= 1280.0,
            "the mark stands inside the window it counts: {mark:?}"
        );
        let rgba = away.harness().render_rgba();
        let (wide, tall) = (1280usize, 800usize);
        assert!(!rgba.is_empty(), "the frame renders");
        let device = rgba.len() / 4;
        let scale = ((device / (wide * tall)) as f64).sqrt().round().max(1.0) as usize;
        let ground = ui::Theme::light().tokens.colors.surfaces.content.to_rgba8();
        let ground = [ground.r, ground.g, ground.b];
        let pixel = |x: usize, y: usize| {
            let at = (y * wide * scale + x) * 4;
            [rgba[at], rgba[at + 1], rgba[at + 2]]
        };
        let mut inked = 0;
        for y in (mark.y0 as usize * scale)..(mark.y1 as usize * scale) {
            for x in (mark.x0 as usize * scale)..(mark.x1 as usize * scale) {
                if !pixel(x, y)
                    .iter()
                    .zip(ground)
                    .all(|(painted, beneath)| painted.abs_diff(beneath) <= 12)
                {
                    inked += 1;
                }
            }
        }
        assert!(
            inked > 500,
            "the mark is painted in the header, not only built: {inked} px in {mark:?}"
        );
    });
}

#[test]
fn a_catch_up_hangs_inside_the_window_it_belongs_to() {
    on_a_windows_stack(|| {
        // A mark's catch-up is a panel under the mark where the window is wide, a sheet at its
        // foot where it is narrow, and neither reaches past the window it belongs to.
        tilo_ui_host::i18n::ensure();
        ui::clock::freeze_for_capture();
        let packages = tasks_packages();
        let mut away = away_at(&packages, (1280, 800));
        let window = window_mark(&away.composed().expect("the window composed its view"))
            .expect("the window's header says what changed in it");
        away.press(&window.key, false)
            .expect("the window's mark opens its catch-up");
        away.lay_out((1280, 800), ui::Theme::light(), input_at((1280, 800)), 1.0)
            .expect("the app lays out with its catch-up open");
        let wide = floating_rect(&away, "Popover").unwrap_or_else(|| {
            fn all(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
                out.push(format!("{} {:?}", node.widget, node.test_id));
                for child in &node.children {
                    all(child, out);
                }
            }
            let mut names = Vec::new();
            all(&away.harness().inspection().root, &mut names);
            panic!("the catch-up hangs over the window: {}", names.join(" "));
        });
        assert!(
            wide.x0 >= 0.0 && wide.x1 <= 1280.0 && wide.width() <= 400.0,
            "the catch-up stays inside a window 1280 wide, and no wider than a list panel: {wide:?}"
        );
        let wrong = wrong_in(&away, (1280, 800));
        assert!(
            wrong.is_empty(),
            "nothing reaches past the window: {wrong:?}"
        );

        // A window held in the hand is staged at its own size, as a capture stages it. It stands on
        // the place the task it answers with belongs to, over the places behind it; the mark in its
        // header is the window's own either way.
        let mut narrow = away_at(&packages, (390, 844));
        let mark = shown_floating_rect(&narrow, "ChangeMark")
            .expect("the narrow window's header shows its mark");
        narrow
            .press_at(mark.center().x, mark.center().y)
            .expect("the narrow window's mark opens its catch-up");
        narrow
            .freshen()
            .expect("the app lays out narrow with its catch-up open");
        let sheet =
            shown_floating_rect(&narrow, "Popover").expect("the catch-up hangs over the window");
        let (_, frame) = narrow.harness().frame_extent((390, 844));
        assert!(
            sheet.x0 >= 0.0
                && sheet.x1 <= 390.0
                && sheet.y1 <= frame
                && sheet.y1 >= frame - 24.0
                && sheet.width() >= 300.0,
            "a narrow window holds the catch-up as a sheet across its foot: {sheet:?} of {frame}"
        );
        let wrong = wrong_in(&narrow, (390, 844));
        assert!(
            wrong.is_empty(),
            "the sheet reaches nothing past the window: {wrong:?}"
        );
    });
}

#[test]
fn a_task_that_changed_since_you_looked_says_so_on_its_page() {
    tilo_ui_host::i18n::ensure();
    ui::clock::freeze_for_capture();
    let packages = tasks_packages();
    let mut changed = stage::Staged::over(
        "so.tilo.lang.tasks",
        &packages,
        "a task that changed since you looked",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    changed
        .lay_out((1280, 800), ui::Theme::light(), input_at((1280, 800)), 1.0)
        .expect("the app lays out");
    let composed = changed.composed().expect("the window composed its view");
    let window = window_mark(&composed).unwrap_or_else(|| {
        panic!("the page of the task that changed says the change is unseen: {composed:?}")
    });
    assert!(
        matches!(
            window.props.get("count"),
            Some(tilo_render_protocol::patch::Value::Int(1))
        ),
        "the page counts its one change: {window:?}"
    );
    assert!(
        matches!(
            window.props.get("by"),
            Some(tilo_render_protocol::patch::Value::Principal(_))
        ),
        "the page names who changed it: {window:?}"
    );
    // Its catch-up hangs inside its window too, and nothing else about the window is wrong.
    changed
        .press(&window.key, false)
        .expect("the page's mark opens its catch-up");
    changed
        .lay_out((1280, 800), ui::Theme::light(), input_at((1280, 800)), 1.0)
        .expect("the page lays out with its catch-up open");
    let wide = floating_rect(&changed, "Popover").expect("the catch-up hangs over the window");
    assert!(
        wide.x0 >= 0.0 && wide.x1 <= 1280.0,
        "the page's catch-up stays inside its window: {wide:?}"
    );
    let wrong = wrong_in(&changed, (1280, 800));
    assert!(
        wrong.is_empty(),
        "nothing reaches past the window: {wrong:?}"
    );
}

#[test]
fn a_channels_messages_the_person_has_not_seen_take_no_mark_of_their_own() {
    // Marks are every resource's, for every type: a channel declares nothing about them. What
    // the person has not seen of a conversation stands under the divider where its unseen part
    // starts, and the rows there say nothing further: a face on each of Mia's and Leo's lines
    // would read as who read them, and the divider already says they are new.
    tilo_ui_host::i18n::ensure();
    ui::clock::freeze_for_capture();
    let packages = app_packages("chat");
    let mut channels = stage::Staged::over(
        "so.tilo.lang.chat",
        &packages,
        "a team's channels",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    channels
        .lay_out(
            (1280, 800),
            ui::Theme::light(),
            ui::interaction::InteractionEnvironment::POINTER,
            1.0,
        )
        .expect("the app lays out");
    let tree = channels.composed().expect("the window composes");
    // The transcript is told where the part the person has not read starts.
    let mut lists = Vec::new();
    nodes(&tree, "ui.VirtualList", &mut lists);
    let unseen = lists
        .iter()
        .find(|list| list.props.contains_key("unseen_from"));
    assert!(
        unseen.is_some(),
        "the conversation says where its unseen part starts: {lists:?}"
    );
    // The toolkit draws one divider there, over the row where that part begins.
    let dividers = rects(&channels, "UnreadDivider");
    assert_eq!(
        dividers.len(),
        1,
        "one divider, over the row where the unseen part starts: {dividers:?}"
    );
    assert!(
        dividers[0].x0 > 300.0 && dividers[0].y1 > 100.0,
        "the divider stands in the conversation's column, under its header: {:?}",
        dividers[0]
    );
    // It says what stands under it, in the toolkit's words for every conversation.
    let mut words = Vec::new();
    {
        fn labels(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
            if node.widget == "UnreadDivider"
                && let Some(name) = node.semantics.as_ref().and_then(|node| node.name.clone())
            {
                out.push(name);
            }
            for child in &node.children {
                labels(child, out);
            }
        }
        labels(&channels.harness().inspection().root, &mut words);
    }
    assert_eq!(
        words,
        vec!["New messages".to_owned()],
        "the divider says what the rows under it are"
    );
    // Nothing marks a message of its own: a face over each line under the divider would read as
    // who read it, and the divider already says those rows are new. The one mark left counts
    // what is new in the window's header.
    let marks = rects(&channels, "ChangeMark");
    assert_eq!(
        marks.len(),
        1,
        "the window's header counts what is new, and no message says it again: {marks:?}"
    );
    assert!(
        marks[0].y1 < 100.0,
        "the one mark stands in the window's header: {:?}",
        marks[0]
    );
    let said = row_marks(&channels.rows());
    assert!(
        said.is_empty(),
        "the rows carry no mark of their own: {said:?}"
    );
}

#[test]
fn a_channels_header_says_what_the_person_has_not_seen_of_it_by_whom() {
    // A catch-up says what happened while the person was away, in the terms of what the window
    // shows: a channel of items Mia, Leo and Ada put in says how many are new, never that three
    // people changed one message.
    tilo_ui_host::i18n::ensure();
    ui::clock::freeze_for_capture();
    let packages = app_packages("chat");
    let mut channels = stage::Staged::over(
        "so.tilo.lang.chat",
        &packages,
        "a team's channels",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    channels
        .lay_out(
            (1280, 800),
            ui::Theme::light(),
            ui::interaction::InteractionEnvironment::POINTER,
            1.0,
        )
        .expect("the app lays out");
    let tree = channels.composed().expect("the window composes");
    let mark = window_mark(&tree).expect("the channel's header says what is new");
    assert_eq!(
        mark.props.get("count"),
        Some(&tilo_render_protocol::patch::Value::Int(7)),
        "seven items wait, not one change: {mark:?}"
    );
    assert_eq!(
        mark.props.get("added"),
        Some(&tilo_render_protocol::patch::Value::Bool(true)),
        "every one of them is new: {mark:?}"
    );
}

#[test]
fn a_scenario_that_fails_is_said_rather_than_shown_empty() {
    tilo_ui_host::i18n::ensure();
    let (app, packages) = packages();
    let failed = stage::Staged::over(
        &app,
        &packages,
        "no such scenario",
        (1280, 800),
        ui::Theme::light(),
        None,
        &[],
    );
    let Err(why) = failed else {
        panic!("a scenario that does not run is an error");
    };
    assert!(why.contains("no such scenario"), "{why}");
}

/// The one main action an app declares for its window stands in the window's bar as the
/// primary button, at a desk's width and in a hand's: a person sees at once what the window is
/// for, and the action narrows to its icon before it ever leaves the row.
#[test]
fn the_windows_main_action_stands_in_the_bar_as_the_primary_button() {
    tilo_ui_host::i18n::ensure();
    for (app, scenario, action) in [
        ("tasks", "a busy week", "New task"),
        ("notes", "a first note", "New note"),
    ] {
        let packages = app_packages(app);
        the_main_action_of(app, &packages, scenario, action);
    }
}

fn the_main_action_of(app: &str, packages: &Rc<stage::Packages>, scenario: &str, action: &str) {
    for (size, dark) in [
        ((1280, 800), false),
        ((1280, 800), true),
        ((390, 844), false),
        ((390, 844), true),
        ((320, 568), false),
        ((320, 568), true),
    ] {
        let theme = if dark {
            ui::Theme::dark()
        } else {
            ui::Theme::light()
        };
        let mut staged = stage::Staged::over(
            &format!("so.tilo.lang.{app}"),
            packages,
            scenario,
            size,
            theme.clone(),
            None,
            &[],
        )
        .expect("the scenario stages");
        let input = if size.0 < 600 {
            ui::interaction::InteractionEnvironment::TOUCH
        } else {
            ui::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, theme, input, 1.0)
            .expect("the app lays out");
        let bar = staged.harness_mut().window_bar_layout(f64::from(size.0));
        let primary: Vec<&ui::window_toolbar::BarControl> = bar
            .controls
            .iter()
            .filter(|control| control.primary)
            .collect();
        assert_eq!(
            primary.len(),
            1,
            "one bar draws one main action at {size:?}: {bar:?}"
        );
        let control = primary[0];
        assert_eq!(
            control.name, action,
            "the app's own main action: {control:?}"
        );
        assert_eq!(
            control.slot, "action",
            "it stands among the app's actions: {control:?}"
        );
        assert!(
            !control.menu,
            "what the window is for never hides behind the bar's door: {control:?}"
        );
        assert!(
            control.rect[2] > 0.0 && control.rect[3] > 0.0,
            "it stands where the person reaches it at {size:?}: {control:?}"
        );
        if size.0 >= 1280 {
            assert!(
                control.labelled,
                "a desk's bar says the main action's words at {size:?}: {control:?}"
            );
        }
        if size.0 <= 320 {
            // The narrowest bar says the words where it has room for them and stands as its
            // icon alone where it has not: either way it is the primary button still.
            assert!(
                control.labelled || control.rect[2] <= 28.5,
                "the narrowest bar keeps the main action to its icon alone, still on the \
                 accent at {size:?}: {control:?}"
            );
        }
    }
}

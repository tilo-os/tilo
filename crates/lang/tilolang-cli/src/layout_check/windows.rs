//! The shell's windows as a person sees them, staged from its own scenarios through the UI
//! host's path: windows with their apps' content, one bar per window, filling the
//! strips the shell gives them, and the trailing parts of the top bar at its edge.

use std::path::Path;
use std::rc::Rc;

use super::stage;
use tilo_ui_base::widget::Widget;
use ui::kurbo::Rect;

fn packages() -> (String, Rc<stage::Packages>) {
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    let search = vec![shell.parent().expect("apps/tl").to_path_buf()];
    let (built, platform) = super::build(&shell, &search).expect("the shell compiles");
    let app = built.last().expect("the shell").compiled.meta.app.clone();
    let packages = Rc::new(stage::Packages::new(
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
    Rc::new(stage::Packages::new(
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

/// What the menus under `node` say, in drawing order.
fn menu_labels(node: &tilo_app_window::UiNode) -> Vec<String> {
    node.of_kind("ui.MenuItem")
        .into_iter()
        .filter_map(|item| match item.props.get("label") {
            Some(tilo_render_protocol::patch::Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect()
}

/// The catch-up a window's mark opens: a row per record someone changed, newest first, with who
/// changed it and when, and the one move that marks them all seen.
fn the_catch_up_names_each_item_it_changed(window: &tilo_app_window::UiNode) {
    let content = window
        .children
        .iter()
        .find(|child| {
            child.kind == "part"
                && child.props.get("name")
                    == Some(&tilo_render_protocol::patch::Value::text("content"))
        })
        .and_then(|part| part.children.first())
        .unwrap_or_else(|| panic!("the window's mark opens a catch-up: {window:?}"));
    let labels: Vec<String> = content
        .of_kind("ui.Button")
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
        !content.of_kind("ui.Avatar").is_empty() && !content.of_kind("ui.Time").is_empty(),
        "each step says who made it and when: {content:?}"
    );
    // A row's line reads as history writes a sentence: the name first, then what they did
    // ("Scout marked it done"). A face alone does not say who.
    let rows = content.of_kind("row");
    let steps: Vec<&tilo_app_window::UiNode> = rows
        .into_iter()
        .filter(|row| row.key.ends_with("~step"))
        .collect();
    assert!(
        !steps.is_empty(),
        "the catch-up holds a line per record it changed: {content:?}"
    );
    let mut lines = Vec::new();
    for step in &steps {
        let texts: Vec<(&str, String)> = step
            .of_kind("ui.Text")
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
    let mut wrong: Vec<String> = super::audit::inspect(&inspection.root, viewport, None, 1.0)
        .into_iter()
        .map(|problem| {
            format!(
                "{}: {} in {}",
                problem.kind.name(),
                problem.detail,
                problem.widget
            )
        })
        .collect();
    // And what only the frame says: a divider a person reads as a band sits in pixels, whatever the
    // widgets that built it say they drew.
    wrong.extend(divider_rules(
        staged,
        (viewport.0 as usize, viewport.1 as usize),
    ));
    wrong
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

/// Each divider of a conversation the inspection sees, in
/// reading order: the words it says and where it stands.
fn dividers_of(staged: &stage::Staged) -> Vec<(String, Rect)> {
    fn all(node: &ui::headless::InspectionNode, out: &mut Vec<(String, Rect)>) {
        if node.widget == "SectionDivider" {
            let [x, y, width, height] = node.rect;
            let words = node
                .semantics
                .as_ref()
                .and_then(|semantics| semantics.name.clone())
                .unwrap_or_default();
            out.push((words, Rect::new(x, y, x + width, y + height)));
        }
        for child in &node.children {
            all(child, out);
        }
    }
    let mut out = Vec::new();
    all(&staged.harness().inspection().root, &mut out);
    out
}

/// The day bands of the conversation in `staged` that its list shows: those inside the box of the
/// list that holds them (and any clip around it), not the rows it keeps mounted above or below
/// its view.
fn shown_dividers(staged: &stage::Staged) -> Vec<(String, Rect)> {
    fn all(node: &ui::headless::InspectionNode, clip: Option<Rect>, out: &mut Vec<(String, Rect)>) {
        let [x, y, width, height] = node.rect;
        let rect = Rect::new(x, y, x + width, y + height);
        if node.widget == "SectionDivider"
            && clip.is_none_or(|clip| clip.y0 < rect.y1 && rect.y0 < clip.y1)
        {
            let words = node
                .semantics
                .as_ref()
                .and_then(|semantics| semantics.name.clone())
                .unwrap_or_default();
            out.push((words, rect));
        }
        let own = node
            .child_clip
            .map(|[x, y, width, height]| Rect::new(x, y, x + width, y + height))
            .or_else(|| (node.widget == "DescribedList").then_some(rect));
        let clip = own.map_or(clip, |own| {
            Some(clip.map_or(own, |outer| outer.intersect(own)))
        });
        for child in &node.children {
            all(child, clip, out);
        }
    }
    let mut out = Vec::new();
    all(&staged.harness().inspection().root, None, &mut out);
    out
}

/// The shell layout of the window `staged` shows, as the shell says it held its header rows.
fn shell_at(staged: &stage::Staged) -> Option<tilo_ui_base::widget::ShellLayout> {
    fn find(node: &ui::headless::InspectionNode) -> Option<tilo_ui_base::widget::ShellLayout> {
        if let Some(tilo_ui_base::widget::LayoutInfo::Shell(shell)) = &node.layout {
            return Some(*shell);
        }
        node.children.iter().find_map(find)
    }
    find(&staged.harness().inspection().root)
}

/// What the shown floating surface the widget `name` draws is called, as a reader hears it.
fn shown_floating_name(staged: &stage::Staged, name: &str) -> Option<String> {
    fn find(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
        if node.widget == name && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
            return node
                .semantics
                .as_ref()
                .and_then(|access| access.name.clone());
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    find(&staged.harness().inspection().root, name)
}

/// Where the floating surface the widget `name` draws, a popover or a sheet, stands.
fn floating_rect(staged: &stage::Staged, name: &str) -> Option<Rect> {
    rects(staged, name).into_iter().next()
}

/// Where the floating surface the widget `name` draws stands once it
/// is shown: a page's fields carry popovers of their own, laid out
/// only while open, so the first one is not the surface a press
/// opened.
fn shown_floating_rect(staged: &stage::Staged, name: &str) -> Option<Rect> {
    rects(staged, name)
        .into_iter()
        .find(|rect| rect.width() > 0.0 && rect.height() > 0.0)
}

/// The change marks the rows of the window's lists draw, by the row they stand in.
fn row_marks(rows: &[(String, tilo_app_window::UiNode)]) -> Vec<tilo_app_window::UiNode> {
    let mut out = Vec::new();
    for (_, row) in rows {
        let marks = row.of_kind("ui.ChangeMark");
        out.extend(marks.into_iter().cloned());
    }
    out
}

/// The changes the rows of `staged`'s window set down, as the row they stand in, who made it and
/// what they did: a row the app draws at the item's page holds the mark as a node of its own, where
/// a list row draws it in the row instead.
fn staged_changes(staged: &mut stage::Staged) -> Vec<(String, tilo_actions::Actor, String)> {
    let rows = staged.rows();
    let mut changes = Vec::new();
    for (key, row) in &rows {
        for mark in row.of_kind("ui.ChangeMark") {
            let identity = match mark.props.get("by") {
                Some(tilo_render_protocol::patch::Value::Actor(who)) => who.id.clone(),
                other => panic!("a marked row names who changed it: {other:?}"),
            };
            let said = match mark.props.get("said") {
                Some(tilo_render_protocol::patch::Value::Text(said)) => said.clone(),
                other => panic!("a marked row says what they did: {other:?}"),
            };
            changes.push((key.clone(), identity, said));
        }
    }
    changes
}

/// How many pixels of `rect` the frame paints in something other than the content's ground: a mark
/// a person reads is painted where it stands, not only built.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "frame and mark sizes are small counts, far inside usize and f64"
)]
fn painted(staged: &stage::Staged, rect: Rect, viewport: (usize, usize)) -> usize {
    let rgba = staged.harness().render_rgba();
    let (wide, tall) = viewport;
    assert!(!rgba.is_empty(), "the frame renders");
    let device = rgba.len() / 4;
    let scale = ((device / (wide * tall)) as f64).sqrt().round().max(1.0) as usize;
    let ground = tilo_ui_base::Theme::light()
        .tokens
        .colors
        .surfaces
        .content
        .to_rgba8();
    let ground = [ground.r, ground.g, ground.b];
    let mut inked = 0;
    for y in (rect.y0 as usize * scale)..(rect.y1 as usize * scale) {
        for x in (rect.x0 as usize * scale)..(rect.x1 as usize * scale) {
            let at = (y * wide * scale + x) * 4;
            let painted = [rgba[at], rgba[at + 1], rgba[at + 2]];
            if !painted
                .iter()
                .zip(ground)
                .all(|(painted, beneath)| painted.abs_diff(beneath) <= 12)
            {
                inked += 1;
            }
        }
    }
    inked
}

/// The frame `staged` painted at `viewport`, and how many device pixels one point covers.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "frame and band sizes are small counts, far inside usize and f64"
)]
fn frame(staged: &stage::Staged, viewport: (usize, usize)) -> (Vec<u8>, usize) {
    let rgba = staged.harness().render_rgba();
    let (wide, tall) = viewport;
    assert!(!rgba.is_empty(), "the frame renders");
    let device = rgba.len() / 4;
    let scale = ((device / (wide * tall)) as f64).sqrt().round().max(1.0) as usize;
    (rgba, scale)
}

/// Where a divider's rule falls short: for every divider `staged`'s window shows, the rule that
/// runs from its words out to each of the column's edges, as the frame paints it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "frame and band sizes are small counts, far inside usize and f64"
)]
fn divider_rules(staged: &stage::Staged, viewport: (usize, usize)) -> Vec<String> {
    let (wide, _) = viewport;
    let (rgba, scale) = frame(staged, viewport);
    let pixel = |x: usize, y: usize| {
        let at = (y * wide * scale + x) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2]]
    };
    let luminance = |painted: [u8; 3]| {
        tilo_ui_base::a11y::relative_luminance(ui::peniko::Color::from_rgb8(
            painted[0], painted[1], painted[2],
        ))
    };
    let moved = |painted: [u8; 3], beneath: [u8; 3]| {
        painted
            .iter()
            .zip(beneath)
            .any(|(painted, beneath)| painted.abs_diff(beneath) > 12)
    };
    let mut wrong = Vec::new();
    // How far the strongest rule of a day moves its ground, against which the band where reading
    // starts again is held; and that band's own rule, when the frame shows one.
    let mut quiet_rule = 0.0f64;
    let mut accent_rule: Option<(String, f64)> = None;
    for (words, divider) in shown_dividers(staged) {
        // A band too narrow to rule moves no column a person reads, and its label may take the
        // whole of it: only a band with room on either side of its words is held to its rules.
        if divider.width() < 200.0 {
            continue;
        }
        let (row, scale) = (
            (divider.center().y.round() as usize).saturating_mul(scale),
            scale,
        );
        let (left, right) = (
            divider.x0.round() as usize * scale,
            divider.x1.round() as usize * scale,
        );
        // The ground the band is painted on: the column beside it, outside the band itself.
        let ground = pixel((divider.x0.round() as usize).saturating_sub(2) * scale, row);
        let beneath = luminance(ground);
        // The rule runs from the column's edge in to its words: the first 16 points on either side
        // stand clear of any label a band this wide centres.
        let span = 16 * scale;
        let mut strength = 0.0f64;
        for (side, from, to) in [
            ("leading", left, left + span),
            ("trailing", right - span, right),
        ] {
            let mut ruled = false;
            for x in from..to.min(wide * scale) {
                let painted = pixel(x, row);
                if moved(painted, ground) {
                    ruled = true;
                    strength = strength.max((luminance(painted) - beneath).abs());
                }
            }
            if !ruled {
                wrong.push(format!(
                    "the divider {words:?} at {divider:?} has no {side} rule: bare text, not a \
                     band ruled out to the column's edges"
                ));
            }
        }
        // The band saying where reading starts again is the one in the accent, and what tells the
        // two kinds apart is the ink: its rule is read at a glance.
        if ui::widget::conversation::band::says_where_reading_starts(&words) {
            accent_rule = Some((
                words,
                accent_rule.map_or(strength, |(_, s)| s.max(strength)),
            ));
        } else {
            quiet_rule = quiet_rule.max(strength);
        }
    }
    if let Some((words, strength)) = accent_rule {
        if strength < tilo_ui_base::surface::COLOURED_RULE_MIN_DELTA {
            wrong.push(format!(
                "the divider {words:?} rules at {strength:.3}, under the \
                 {:.3} a rule that says where reading starts again moves its ground: a day's \
                 rule at another weight, not a mark",
                tilo_ui_base::surface::COLOURED_RULE_MIN_DELTA
            ));
        }
        if strength < quiet_rule * 2.0 {
            wrong.push(format!(
                "the divider {words:?} rules at {strength:.3}, no more than a day's \
                 {quiet_rule:.3}: one weight for a division and for where reading starts again"
            ));
        }
    }
    wrong
}

/// The rows the window of `staged` laid out, as the person has them: a list keeps the row of an
/// item under the swipe the item has, not among its children.
fn list_rows(staged: &stage::Staged) -> Vec<&ui::widget::ListRow> {
    fn collect<'a>(
        root: &'a dyn tilo_ui_base::widget::Widget,
        out: &mut Vec<&'a ui::widget::ListRow>,
    ) {
        if let Some(row) = root
            .as_any()
            .and_then(|widget| widget.downcast_ref::<ui::widget::ListRow>())
        {
            out.push(row);
        }
        for child in root.children() {
            collect(child, out);
        }
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
    rows
}

/// The changes the rows of `staged`'s window show, as who made it, what they are called and what
/// they did: a list row draws its mark in the row instead of holding a node of it, so what the
/// person reads is read off the rows themselves.
fn row_changes(staged: &stage::Staged) -> Vec<(tilo_actions::Actor, String, String)> {
    // One change reads once however many rows show it: a task open where it stands shows it on its
    // row and again over the step that made it, and both are the same change.
    let mut changes = Vec::new();
    for change in list_rows(staged)
        .into_iter()
        .filter_map(ui::widget::ListRow::shown_change)
    {
        let change = (
            change.by.identity.clone(),
            change.by.name.clone(),
            change.said.to_owned(),
        );
        if !changes.contains(&change) {
            changes.push(change);
        }
    }
    changes
}

/// What a mark of a change the person has not seen says of it: who changed it, never the person's
/// own step, and what they did, in words of "it".
fn a_change_names_its_maker(identity: &tilo_actions::Actor, said: &str) {
    assert!(
        *identity != tilo_actions::Actor::Member(tilo_actions::MemberId::person(stage::ME)),
        "a marked row names who changed it, never the person's own step: {identity}"
    );
    it_says_what_they_did(said);
}

/// What a mark says a change did: in words, of "it".
fn it_says_what_they_did(said: &str) {
    assert!(
        !said.is_empty() && !said.contains("this") && !said.contains("done to"),
        "a marked row says what they did, in words, of \"it\": {said}"
    );
}

/// The mark the header of `staged`'s window carries for the `count` changes the person has not
/// seen: first among the header's actions, opening the catch-up that names each of them.
fn the_windows_header_mark(staged: &mut stage::Staged, count: i64) -> tilo_app_window::UiNode {
    let composed = staged.composed().expect("the window composed its view");
    let window = window_mark(&composed)
        .unwrap_or_else(|| panic!("the window's header says what changed in it: {composed:?}"));
    assert!(
        matches!(
            window.props.get("count"),
            Some(tilo_render_protocol::patch::Value::Int(counted)) if *counted == count
        ),
        "the window counts the {count} changes she has not seen: {window:?}"
    );
    the_catch_up_names_each_item_it_changed(&window);
    // The mark stands among the header's actions, first, in the row that carries the title it
    // counts: the overview's header where the window lists its items beside the one it opens,
    // else the content's.
    let parts = composed.of_kind("part");
    let lists_beside = parts.iter().any(|part| {
        part.props.get("name") == Some(&tilo_render_protocol::patch::Value::text("overview"))
    });
    let actions = if lists_beside {
        "overview_actions"
    } else {
        "actions"
    };
    let header = parts
        .iter()
        .find(|part| {
            part.props.get("name") == Some(&tilo_render_protocol::patch::Value::text(actions))
                && part
                    .children
                    .iter()
                    .any(|child| child.kind == "ui.ChangeMark")
        })
        .unwrap_or_else(|| panic!("the window's header holds the window's mark: {composed:?}"));
    assert_eq!(
        header.children[0].kind, "ui.ChangeMark",
        "the window's mark stands first among the header's actions: {header:?}"
    );
    window
}

/// The mark a window's header carries: what changed in what it shows, and by whom.
fn window_mark(tree: &tilo_app_window::UiNode) -> Option<tilo_app_window::UiNode> {
    let marks = tree.of_kind("ui.ChangeMark");
    marks
        .into_iter()
        .find(|mark| {
            mark.props.get("window") == Some(&tilo_render_protocol::patch::Value::Bool(true))
        })
        .cloned()
}

/// What a window's mark opens, one line of text per catch-up row: each row's item, its sentence,
/// its posts counted in that sentence where the app's words say how many, and the count of changes
/// it folds beside it where they do not.
fn catch_up_rows(tree: &tilo_app_window::UiNode) -> Vec<String> {
    let window = window_mark(tree).expect("the window says what changed since the person looked");
    let content = window
        .children
        .iter()
        .find(|child| {
            child.kind == "part"
                && child.props.get("name")
                    == Some(&tilo_render_protocol::patch::Value::text("content"))
        })
        .and_then(|part| part.children.first())
        .unwrap_or_else(|| panic!("the mark opens its catch-up: {window:?}"));
    let said = |node: &tilo_app_window::UiNode| match node.props.get("value") {
        Some(tilo_render_protocol::patch::Value::Text(value)) => Some(value.clone()),
        _ => None,
    };
    let found = content.of_kind("stack");
    let mut rows: Vec<&tilo_app_window::UiNode> = found
        .into_iter()
        .filter(|stack| {
            stack
                .key
                .rsplit_once("~since-")
                .is_some_and(|(_, row)| row.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    rows.sort_by_key(|stack| stack.key.clone());
    rows.into_iter()
        .map(|stack| {
            let mut words = Vec::new();
            let inside = stack.of_kind("ui.Text");
            for node in inside {
                if node.kind == "ui.Text"
                    && (node.key.contains("~step") || node.key.ends_with("~changes"))
                {
                    words.extend(said(node));
                }
            }
            words.join(" ")
        })
        .collect()
}

/// The window's own tabs under `root`: the strip a `TabStrip` draws (it draws as the toolkit's
/// bar), never a tab bar an app puts in its own content.
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

/// The shell's traditional desktop staged at `size` under `theme`, as each part below reads it.
fn the_traditional_desktop(
    app: &str,
    packages: &Rc<stage::Packages>,
    size: (u32, u32),
    theme: tilo_ui_base::Theme,
) -> stage::Staged {
    stage::Staged::over(
        app,
        packages,
        "the traditional desktop",
        size,
        theme,
        None,
        &[],
    )
    .expect("the scenario stages")
}

/// The traditional desktop as a person meets it, from one build of the shell: its windows at a
/// screen's size and what they read out, its top bar and its counts at three sizes, and the window
/// in front on a phone. Every part reads a window staged at its own size and theme; the one
/// window two parts share is only read by the first, and laid out again by the second, last.
#[test]
fn the_traditional_desktop_stands_as_a_person_reads_it() {
    tilo_app_window::i18n::ensure();
    let (app, packages) = packages();
    {
        // Gone before the next window is staged: one window stands at a time, as when each part
        // was a test of its own.
        let mut shell =
            the_traditional_desktop(&app, &packages, (1280, 800), tilo_ui_base::Theme::light());
        its_windows_show_their_apps_content_under_one_bar_each(&shell);
        its_people_are_called_by_their_names_never_their_ids(&mut shell);
    }
    for (width, height, text) in [(1280, 800, 1.0), (768, 1024, 1.0), (320, 568, 1.5)] {
        let staged = the_traditional_desktop(
            &app,
            &packages,
            (width, height),
            tilo_ui_base::Theme::light().with_text_scale(text),
        );
        let mut controls = Vec::new();
        shell_controls(staged.harness().root_widget(), &mut controls);
        let at = format!("{width}×{height} text {text}");
        its_top_bar_is_one_row_whose_parts_never_press_into_each_other(&controls, width, &at);
        a_count_stands_on_its_icons_corner_and_over_no_other_control(&controls, width, &at);
    }
    let phone = the_traditional_desktop(&app, &packages, (320, 568), tilo_ui_base::Theme::light());
    let said = {
        let mut said = Vec::new();
        read_out(&phone.harness().inspection().root, false, &mut said);
        said
    };
    assert!(
        said.iter()
            .any(|(in_window, name)| *in_window && name == "Q3 plan"),
        "the window is the one in front: {said:?}"
    );
    the_window_fills_the_room_between_the_bar_and_the_dock(&phone, 320.0);
}

fn its_windows_show_their_apps_content_under_one_bar_each(staged: &stage::Staged) {
    let root = staged.harness().root_widget();
    let frames = named(root, "WindowFrame");
    assert_eq!(
        frames.len(),
        3,
        "two notes and a channel stand in the shell"
    );
    for frame in frames {
        let window = frame.get_rect();
        // What the window holds is cut at its edge, so a list
        // scrolled past its top is never read over the window
        // behind it.
        assert_eq!(
            frame.child_clip_rect(),
            Some(window),
            "the window clips what it holds"
        );
        let shells = named(frame, "NavShell");
        let [shell] = shells[..] else {
            panic!("one app window in each frame, not {}", shells.len());
        };
        let content = shell.get_rect();
        assert!(
            content.height() > window.height() * 0.8,
            "the app's window fills the frame: {content:?} in {window:?}"
        );
        // The frame's controls stand in the app's own header row, at its trailing end, so the
        // window has one bar: no bar of the frame's above the app's.
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

/// A layout of places as a person meets it, from one build of the shell and one window of the
/// shell at 1280×800: each window's bar keeps one button, each place shows its app, and its strips
/// stand as windows. No part changes the window: each reads its widgets, and the second the
/// places the layout's own record holds.
#[test]
fn a_layout_of_places_stands_as_windows_that_show_their_apps() {
    tilo_app_window::i18n::ensure();
    let (app, packages) = packages();
    let shell = stage::Staged::over(
        &app,
        &packages,
        "a layout of places",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    // The two parts that read the shell alone come first. The last stages a second window from
    // the same build, as it did in a test of its own, and nothing reads the shell after that.
    a_window_bar_keeps_one_overflow_menu(&shell);
    a_place_the_shell_has_shows_its_app(&shell, &packages);
    the_strips_windows_fill_their_strip_and_hold_their_apps_under_one_bar(&shell, &app, &packages);
}

/// `staged` is the layout of places; the window of several places beside it is staged here.
fn the_strips_windows_fill_their_strip_and_hold_their_apps_under_one_bar(
    staged: &stage::Staged,
    app: &str,
    packages: &Rc<stage::Packages>,
) {
    let frames = named(staged.harness().root_widget(), "WindowFrame");
    assert!(
        frames.len() >= 6,
        "the layout's strips stand as windows, not cards: {} frames",
        frames.len()
    );
    windows_fill_their_strips(&frames);

    // A strip of several places is one window whose bar is its tabs, the place in front filling it
    // under them; a strip of one has no tab bar above its app.
    let grouped = stage::Staged::over(
        app,
        packages,
        "a window of several places",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let group = named(grouped.harness().root_widget(), "WindowFrame");
    assert_eq!(group.len(), 2, "a window of places beside a window of one");
    the_places_of_a_strip_are_its_tabs(&group);

    // Two windows the shell holds stand side by side, each the share of the shell its strip's width
    // names, not the floor two of them would need: a row spreads across the shell while its windows
    // fit, and pans to them whole once they do not.
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
        "the layout's row spreads across it at 1280: {} to {to}",
        from.round()
    );
}

#[test]
fn a_window_alone_in_the_shell_takes_the_shell() {
    tilo_app_window::i18n::ensure();
    let (app, packages) = packages();
    let staged = stage::Staged::over(
        &app,
        &packages,
        "a window alone in the shell",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    let frames = named(staged.harness().root_widget(), "WindowFrame");
    assert_eq!(
        frames.len(),
        1,
        "one window in the shell, not {}",
        frames.len()
    );
    let window = frames[0].get_rect();
    assert!(
        window.width() > 1200.0,
        "a lone window takes the shell's width at 1280: {window:?}"
    );
    bar_and_content_in(frames[0], window);
    the_window_fills_the_room_between_the_bar_and_the_dock(&staged, 1280.0);
}

/// A place is made from the resource it shows (`session.Layout`'s `record`), so a place
/// whose app the shell has draws that app's views, that app's header row and, with nothing in it
/// yet, that app's empty state. `staged` is the layout of places.
fn a_place_the_shell_has_shows_its_app(staged: &stage::Staged, packages: &Rc<stage::Packages>) {
    let places = staged.places();
    assert!(!places.is_empty(), "the staged layout holds its places");
    // The apps the shell's own build has: a place that shows one of them is drawn by it.
    let known: Vec<&str> = packages
        .compiled
        .iter()
        .map(|held| held.meta.app.as_str())
        .collect();
    let wells: Vec<&str> = named(staged.harness().root_widget(), "WindowWell")
        .iter()
        .filter_map(|well| well.test_id())
        .collect();
    for key in &wells {
        let place = places
            .iter()
            .find(|place| place.key == *key)
            .unwrap_or_else(|| {
                panic!("the well {key} stands for a place of the layout: {places:?}")
            });
        assert!(
            !known.contains(&place.shows.as_str()),
            "the place {key} shows {}, whose views the shell has, so its window draws them: it \
             stands in a well instead, blank",
            place.shows
        );
    }
    // The same holds for every place the shell draws as a window of its own, whatever it holds: a
    // place with no resource is another shell's window, and only that stands in a well.
    for place in &places {
        if !place.record.is_empty() || known.contains(&place.shows.as_str()) {
            assert!(
                !wells.contains(&place.key.as_str()),
                "the place {} shows a resource and stands in a well",
                place.key
            );
        }
    }
}

/// One bar, one "…": the window's decorations in their floating pill, the app's own controls in
/// its header row, and one button between them for what does not fit. `staged` is the layout of
/// places.
fn a_window_bar_keeps_one_overflow_menu(staged: &stage::Staged) {
    let root = staged.harness().root_widget();
    let frames = named(root, "WindowFrame");
    assert!(!frames.is_empty(), "the shell draws windows");
    for frame in frames {
        let window = frame.get_rect();
        let band = ui::kurbo::Rect::new(
            window.x0,
            window.y0,
            window.x1,
            window.y0 + 56.0_f64.min(window.height()),
        );
        // A button: an overflow button, or a menu button, drawn in the bar.
        let buttons: Vec<&dyn Widget> = ["OverflowButton", "MenuButton"]
            .iter()
            .flat_map(|name| named(frame, name))
            .filter(|button| {
                let rect = button.get_rect();
                rect.width() > 0.0 && rect.center().y >= band.y0 && rect.center().y <= band.y1
            })
            .collect();
        assert!(
            buttons.len() <= 1,
            "the window at {window:?} shows {} buttons in its one bar: {:?}",
            buttons.len(),
            buttons
                .iter()
                .map(|button| (button.name(), button.get_rect()))
                .collect::<Vec<_>>()
        );
    }
}

/// A window is a window: it fills its strip's whole room, and every window of the shell shares
/// that height, as the chrome's groups do.
fn windows_fill_their_strips(frames: &[&dyn Widget]) {
    // The shell's own windows: the overlays it stages beside them are frames too.
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
        "a window fills the shell's height: {height} at 1280x800"
    );
    for frame in &frames {
        let window = frame.get_rect();
        assert!(
            (window.height() - height).abs() < 1.0,
            "every window of the shell is as tall as the shell: {window:?} beside {height}"
        );
        // Its share of the row, never narrower than the chrome's own floor: a row of more windows
        // than the shell holds pans to them whole.
        assert!(
            window.width() >= 160.0,
            "a window keeps its share of the row: {window:?}"
        );
        bar_and_content_in(*frame, window);
    }
}

/// One bar per window, whichever state it is in — the tabs of a window of several places, or the
/// app's own header row — and its app's content under that, filling the rest.
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

/// A strip's places are its window's tabs, the place in front under them: a strip of several
/// places shows each as a tab, and a strip of one wears its one tab too, the app's glyph and name
/// saying what the window shows, so another place opens from the same bar.
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
                assert!(
                    tabs_of(*bar) >= 1,
                    "the window's place is a tab of its bar: {tabs:?}"
                );
                // The place in front is one app under the bar, filling the window; a place of a
                // kind this build has no app for (the terminal) draws no app's shell at all.
                let shells = named(*frame, "NavShell");
                assert!(
                    shells.len() <= 1,
                    "one app in front of the tabs, not {}",
                    shells.len()
                );
                for shell in shells {
                    let content = shell.get_rect();
                    assert!(
                        content.y0 >= tabs.y1 - 1.0 && window.y1 - content.y1 < 1.0,
                        "the place in front fills the window under its bar: {content:?} in {window:?}"
                    );
                }
            }
            _ => panic!("one tab bar to a window, not {}", strips.len()),
        }
    }
    // The strip of three notes shows each as a tab; the terminal's strip, its one.
    let mut counts: Vec<usize> = group
        .iter()
        .flat_map(|frame| tab_strips(*frame))
        .map(tabs_of)
        .collect();
    counts.sort_unstable();
    assert_eq!(
        counts,
        [1, 3],
        "every place of a strip is a tab of its window's bar, each strip's its own"
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
    for (part, rect) in root.test_rects() {
        if part == "count" || part == "icon" {
            out.push((format!("{} {part}", root.name()), rect));
        }
    }
    for child in root.children() {
        shell_controls(child, out);
    }
}

/// One size of the traditional desktop: `controls` are the shell's own controls at it, `width`
/// its output's, and `at` names it in every failure.
fn its_top_bar_is_one_row_whose_parts_never_press_into_each_other(
    controls: &[(String, Rect)],
    width: u32,
    at: &str,
) {
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

/// One size of the traditional desktop: its controls, its width and its name, as the top bar's
/// part above takes them.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "how far a count's centre stands from where it belongs, between two laid out points"
)]
fn a_count_stands_on_its_icons_corner_and_over_no_other_control(
    controls: &[(String, Rect)],
    width: u32,
    at: &str,
) {
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
    assert_eq!(
        counts.len(),
        2,
        "the bell's count and the dock's at {at}: {controls:?}"
    );
    for (count, icon) in counts {
        let own = pressed
            .iter()
            .find(|r| r.contains(icon.center()))
            .expect("the control the icon is on");
        // Centred on the icon's top trailing corner, as an app icon's count stands, and held
        // inside its own face where that corner would take it past the face's edge.
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

/// The one window `staged` shows stands between the bar and a dock that keeps its own room at the
/// foot, and fills that room: on a phone and on a laptop's screen alike, a window is the height
/// the shell has left it, never a share of it the dock's row took.
fn the_window_fills_the_room_between_the_bar_and_the_dock(staged: &stage::Staged, width: f64) {
    let root = staged.harness().root_widget();
    let frames = named(root, "WindowFrame");
    let [frame] = frames[..] else {
        panic!("the window in front alone, not {} windows", frames.len());
    };
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
        window.x0 >= 0.0 && window.x1 <= width && window.width() >= width - 2.0 * gap,
        "the window {window:?} spans the output"
    );
}

/// What assistive technology reads under `node` (names
/// and values), each with whether it stands in a
/// window.
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

/// What the traditional desktop `staged` at `size` reads out, laid out for a finger on a phone's
/// width (its controls a finger's target), as the layout check lays a phone out.
fn desktop_read_out(staged: &mut stage::Staged, size: (u32, u32)) -> Vec<(bool, String)> {
    let input = if size.0 < 600 {
        tilo_ui_base::interaction::InteractionEnvironment::TOUCH
    } else {
        tilo_ui_base::interaction::InteractionEnvironment::POINTER
    };
    staged
        .lay_out(size, tilo_ui_base::Theme::light(), input, 1.0)
        .expect("the desktop lays out");
    let mut said = Vec::new();
    read_out(&staged.harness().inspection().root, false, &mut said);
    said
}

#[test]
fn a_windows_header_holds_the_rest_of_its_actions_in_one_menu() {
    // A window draws its main actions as buttons and keeps the rest behind one menu: never every
    // action as a button of its own, and one button to what does not fit in one row.
    tilo_app_window::i18n::ensure();
    let packages = tasks_packages();
    for size in [(1280, 800), (390, 844)] {
        let mut staged = stage::Staged::over(
            "so.tilo.tasks",
            &packages,
            "a busy week",
            size,
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        let input = if size.0 < 600 {
            tilo_ui_base::interaction::InteractionEnvironment::TOUCH
        } else {
            tilo_ui_base::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, tilo_ui_base::Theme::light(), input, 1.0)
            .expect("the app lays out");
        back_to_the_list(&mut staged, size);
        let row = super::header_row_controls(&staged, size);
        let buttons = row.iter().filter(|control| !control.menu).count();
        assert!(
            buttons <= ui::window_toolbar::MAX_LEADING + ui::window_toolbar::MAX_ACTIONS,
            "a window draws no more actions as buttons than one row holds at {size:?}: {row:?}"
        );
        let menus: Vec<&super::RowControl> = row.iter().filter(|control| control.menu).collect();
        assert_eq!(
            menus.len(),
            1,
            "a window keeps the rest of its actions in one menu at {size:?}: {row:?}"
        );
        assert!(
            menus
                .iter()
                .all(|menu| menu.rect[2] > 0.0 && menu.rect[3] > 0.0),
            "the window's menu stands where the person reaches it at {size:?}: {menus:?}"
        );
        assert!(
            menus.iter().all(|menu| !menu.name.is_empty()),
            "the window's menu is named at {size:?}: {menus:?}"
        );
    }
}

#[test]
fn an_action_without_an_icon_is_reachable_from_the_row_that_offers_it() {
    // A window that is a tree of notes keeps a note's actions in the menu
    // of the row it stands for, where a person right-clicks it: the notes
    // app's icon-less Untag is there, at a desktop's width and in a hand's.
    tilo_app_window::i18n::ensure();
    let packages = app_packages("notes");
    let app = packages.compiled.last().expect("the app").meta.app.clone();
    for size in [(1280, 800), (390, 844)] {
        let mut staged = stage::Staged::over(
            &app,
            &packages,
            "a week of notes",
            size,
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        let input = if size.0 < 600 {
            tilo_ui_base::interaction::InteractionEnvironment::TOUCH
        } else {
            tilo_ui_base::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, tilo_ui_base::Theme::light(), input, 1.0)
            .expect("the notes lay out");
        let tree = staged.composed().expect("the window composes");
        // A tree's rows are built as the window shows them, so a note's menu stands in the row the
        // window built for it rather than in the tree the app sent.
        let mut labels = menu_labels(&tree);
        for (_, row) in &staged.rows() {
            labels.extend(menu_labels(row));
        }
        assert!(
            labels.iter().any(|label| label == "Untag…"),
            "a note's row offers its actions, its icon-less ones too, at {size:?}: {labels:?}"
        );
    }
}

/// A notebook is a tree in its sidebar and the note it opens beside it: a note stands indented
/// under the one it is of, to any depth, and the "+" that starts one stands in the sidebar's header
/// — once, where what it makes will stand.
fn rows_with_depth(
    node: &ui::headless::InspectionNode,
    out: &mut Vec<(String, tilo_ui_base::widget::RowLayout, [f64; 4])>,
) {
    if let Some(tilo_ui_base::widget::LayoutInfo::Row(row)) = &node.layout {
        let name = node
            .semantics
            .as_ref()
            .and_then(|said| said.name.clone())
            .unwrap_or_default();
        out.push((name, *row, node.rect));
    }
    for child in &node.children {
        rows_with_depth(child, out);
    }
}

#[test]
fn a_notebook_is_a_tree_beside_the_note_it_opens() {
    tilo_app_window::i18n::ensure();
    let packages = app_packages("notes");
    let app = packages.compiled.last().expect("the app").meta.app.clone();
    let size = (1280, 800);
    let mut staged = stage::Staged::over(
        &app,
        &packages,
        "a week of notes",
        size,
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    staged
        .lay_out(
            size,
            tilo_ui_base::Theme::light(),
            tilo_ui_base::interaction::InteractionEnvironment::POINTER,
            1.0,
        )
        .expect("the window lays out");

    let mut laid = Vec::new();
    rows_with_depth(&staged.harness().inspection().root, &mut laid);
    let of = |title: &str| {
        laid.iter().find(|(name, _, _)| name == title).map_or_else(
            || {
                let names: Vec<&str> = laid.iter().map(|(name, _, _)| name.as_str()).collect();
                panic!("the tree draws {title:?}: {names:?}")
            },
            |(_, row, rect)| (*row, rect[3]),
        )
    };
    let (plan, plan_height) = of("Q3 plan");
    let (standups, standups_height) = of("Standups");
    let (monday, _) = of("Monday");
    assert_eq!(
        (plan.depth, standups.depth, monday.depth),
        (0, 1, 2),
        "a note stands under the one it is of: {laid:?}"
    );
    assert!(
        plan.content_left < standups.content_left && standups.content_left < monday.content_left,
        "each depth indents the row: {} {} {}",
        plan.content_left,
        standups.content_left,
        monday.content_left
    );
    // The sidebar's tree is drawn: the notes' rows have a row's height. A tree given no room
    // (its stack hugging the sidebar instead of filling it) reports rows and paints none.
    assert!(
        plan_height > 0.0 && standups_height > 0.0,
        "the tree's rows are drawn in the sidebar: Q3 plan {plan_height}, Standups {standups_height}"
    );

    // One control starts a note: the window's bar offers it as the window's own action, and the
    // sidebar's header keeps the file commands, never a second button for what the bar has.
    let tree = staged.composed().expect("the window composes");
    let shells = tree.of_kind("ui.NavShell");
    let shell = shells.first().expect("the window is a shell");
    let part = |name: &str| {
        shell
            .children
            .iter()
            .find(|child| {
                child.kind == "part"
                    && child.props.get("name")
                        == Some(&tilo_render_protocol::patch::Value::text(name))
            })
            .unwrap_or_else(|| panic!("the shell has its {name} part: {shell:?}"))
    };
    let buttons = shell.of_kind("ui.Button");
    let starts: Vec<&tilo_app_window::UiNode> = buttons
        .iter()
        .filter(|button| {
            button.props.get("label") == Some(&tilo_render_protocol::patch::Value::text("New note"))
        })
        .copied()
        .collect();
    assert_eq!(starts.len(), 1, "one control starts a note: {starts:?}");
    let in_bar = (part("actions")).of_kind("ui.Button");
    assert!(
        in_bar
            .iter()
            .any(|b| b.props.get("label")
                == Some(&tilo_render_protocol::patch::Value::text("New note"))),
        "the window's bar starts a note"
    );
    let in_sidebar = (part("sidebar_actions")).of_kind("ui.Button");
    assert!(
        in_sidebar.is_empty(),
        "the sidebar's header offers no second button of the window's: {in_sidebar:?}"
    );
    let menus = (part("sidebar_actions")).of_kind("ui.MenuButton");
    assert!(
        menus.iter().any(|menu| menu.props.get("label")
            == Some(&tilo_render_protocol::patch::Value::text("File"))),
        "the sidebar's header keeps the notebook's file commands: {menus:?}"
    );
}

/// A notebook whose note says its sections keeps the tree in its sidebar: the outline of the note
/// open stands under the notes, and the notes keep the room they list in — at a desktop's width
/// and in a phone's, where the sidebar is the window's first screen.
#[test]
fn a_notebooks_tree_keeps_its_room_under_the_notes_outline() {
    tilo_app_window::i18n::ensure();
    let packages = app_packages("notes");
    let app = packages.compiled.last().expect("the app").meta.app.clone();
    for size in [(1280, 800), (390, 844)] {
        let mut staged = stage::Staged::over(
            &app,
            &packages,
            "the notebook and a note's outline",
            size,
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        let input = if size.0 < 600 {
            tilo_ui_base::interaction::InteractionEnvironment::TOUCH
        } else {
            tilo_ui_base::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, tilo_ui_base::Theme::light(), input, 1.0)
            .expect("the notes lay out");
        if size.0 < 600 {
            // A phone opens on the note the notebook shows; Back goes to the notes' list.
            let mut all = Vec::new();
            named_with_rect(&staged.harness().inspection().root, &mut all);
            let back = all
                .iter()
                .find(|(said, rect)| said == "Back" && rect[2] > 0.0)
                .map_or_else(
                    || panic!("the window offers the way back at {size:?}: {all:?}"),
                    |(_, rect)| *rect,
                );
            staged
                .press_at(back[0] + back[2] / 2.0, back[1] + back[3] / 2.0)
                .expect("the window goes back to its notes");
            staged
                .lay_out(size, tilo_ui_base::Theme::light(), input, 1.0)
                .expect("the notes lay out again");
        }
        let mut laid = Vec::new();
        rows_with_depth(&staged.harness().inspection().root, &mut laid);
        let row = |name: &str| {
            laid.iter().find(|(said, _, _)| said == name).map_or_else(
                || {
                    let names: Vec<&str> = laid.iter().map(|(said, _, _)| said.as_str()).collect();
                    panic!("the tree draws {name:?} at {size:?}: {names:?}")
                },
                |(_, _, rect)| *rect,
            )
        };
        let note = row("Autumn plan");
        assert!(
            note[2] > 0.0 && note[3] > 0.0,
            "the sidebar draws the notes' tree at {size:?}, where the note's outline stands too: \
             {note:?}"
        );
        // The outline is the note's headings, drawn under the notes, in the sidebar.
        let mut drawn = Vec::new();
        named_with_rect(&staged.harness().inspection().root, &mut drawn);
        let heading = drawn
            .iter()
            .find(|(said, rect)| said == "Ship the editor" && rect[2] > 0.0)
            .map_or_else(
                || {
                    let names: Vec<&str> = drawn.iter().map(|(said, _)| said.as_str()).collect();
                    panic!("the window draws the note's heading at {size:?}: {names:?}")
                },
                |(_, rect)| *rect,
            );
        assert!(
            heading[1] >= note[1] + note[3],
            "the outline stands under the tree at {size:?}: note {note:?}, heading {heading:?}"
        );
    }
}

/// Every node of `node` and its children that says a name, with where it is drawn: a widget the
/// window draws as a row or as anything else.
fn named_with_rect(node: &ui::headless::InspectionNode, out: &mut Vec<(String, [f64; 4])>) {
    if let Some(name) = node
        .semantics
        .as_ref()
        .and_then(|said| said.name.clone())
        .filter(|name| !name.is_empty())
    {
        out.push((name, node.rect));
    }
    for child in &node.children {
        named_with_rect(child, out);
    }
}

/// `shell` is the traditional desktop staged at 1280×800 in the light theme, which this lays out.
fn its_people_are_called_by_their_names_never_their_ids(shell: &mut stage::Staged) {
    let said = desktop_read_out(shell, (1280, 800));
    let ids: Vec<&String> = said
        .iter()
        .map(|(_, name)| name)
        .filter(|name| name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit()))
        .collect();
    assert!(ids.is_empty(), "people read as ids: {ids:?}");
    assert!(
        said.iter().any(|(_, name)| name.contains("Leo")),
        "the channel names who spoke: {said:?}"
    );
}

#[test]
fn a_scenario_on_two_layers_is_named_apart() {
    // The shots of the window's own layer and of the shell's settings layer must not land in one
    // file, or the shell's shot is the settings page.
    assert_eq!(
        super::on_layer("the traditional desktop", None),
        "the traditional desktop"
    );
    assert_ne!(
        super::on_layer("the traditional desktop", Some("Shell")),
        super::on_layer("the traditional desktop", None)
    );
}

/// Lay `body` out on a thread with a window's stack: a window's tree is
/// as deep as the documents it shows, and a rich-text editor inside a
/// row held open needs more room than a test thread's two
/// megabytes.
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
        "so.tilo.tasks",
        packages,
        "while you were away",
        size,
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    staged
        .lay_out(size, tilo_ui_base::Theme::light(), input_at(size), 1.0)
        .expect("the app lays out");
    staged
}

/// A window too narrow for its list and the item it opens beside it shows the item in front,
/// with the shell's Back to the list behind, as a press on the item's row does. The window's
/// header, its actions and its mark are the list's, so a check of them takes Back first. A window
/// that lists nothing beside what it opens keeps its actions on the screen in front, and is left
/// as it is.
fn back_to_the_list(staged: &mut stage::Staged, size: (u32, u32)) {
    let lists_beside = staged
        .composed()
        .expect("the window composed its view")
        .of_kind("part")
        .iter()
        .any(|part| {
            part.props.get("name") == Some(&tilo_render_protocol::patch::Value::text("overview"))
        });
    if !lists_beside || !super::back_chip(staged, size) {
        return;
    }
    staged
        .press_shell(tilo_ui_nav::WINDOW_BACK)
        .expect("the shell's back presses");
    staged.freshen().expect("the list comes back to rest");
    assert!(
        !super::back_chip(staged, size),
        "the shell's back shows the window's list at {size:?}"
    );
}

/// What a person holds a window with at `size`: a pointer at a desktop's width, a finger on one.
fn input_at(size: (u32, u32)) -> tilo_ui_base::interaction::InteractionEnvironment {
    if size.0 < 600 {
        tilo_ui_base::interaction::InteractionEnvironment::TOUCH
    } else {
        tilo_ui_base::interaction::InteractionEnvironment::POINTER
    }
}

/// The tasks app's "while you were away", from one build of the app and one window at 1280×800:
/// a capture of it shows what changed, and the catch-up its mark opens hangs inside the window.
/// The first part only reads the window; the second presses it, after, and stages the narrow
/// window it reads beside it.
#[test]
fn a_capture_shows_what_changed_and_its_catch_up_hangs_inside_the_window() {
    on_a_windows_stack(|| {
        tilo_app_window::i18n::ensure();
        tilo_ui_base::clock::freeze_for_capture();
        let packages = tasks_packages();
        let mut away = away_at(&packages, (1280, 800));
        a_capture_writes_the_scenarios_steps_down_so_what_changed_shows(&mut away);
        a_catch_up_hangs_inside_the_window_it_belongs_to(&packages, away);
    });
}

/// The marks a row and a window carry are read from the record's steps, which are the store's
/// versions and the person's seen marks: a scenario staged for a capture is written down as a
/// session writes it, or the capture shows nothing of what it staged. `away` is the window at
/// 1280×800, which this reads and leaves as it stood.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "frame and mark sizes are small counts, far inside usize and f64"
)]
fn a_capture_writes_the_scenarios_steps_down_so_what_changed_shows(away: &mut stage::Staged) {
    // The two tasks someone else changed while she was away: Bo took the mail and her agent
    // moved the survey to today, each row setting down who changed it and what they did.
    let changed = staged_changes(away);
    assert_eq!(
        changed.len(),
        2,
        "the two rows of the tasks Bo and her agent changed are marked: {changed:?}"
    );
    for (_, identity, said) in &changed {
        a_change_names_its_maker(identity, said);
    }
    assert!(
        changed
            .iter()
            .any(|(_, _, said)| said.contains("assigned") && said.contains("you")),
        "the mail Bo gave her says it was given to her: {changed:?}"
    );
    // Each task stays a row of the list beside the page she opened, and each row says what
    // changed under its title, where a person reads it: Bo's mail, and the survey her agent moved,
    // which is the one open.
    let rows = row_changes(away);
    assert_eq!(
        rows.len(),
        2,
        "the rows of the two tasks Bo and her agent changed say so under their titles: {rows:?}"
    );
    for (identity, name, said) in &rows {
        a_change_names_its_maker(identity, said);
        assert!(!name.is_empty(), "a marked row names the changer: {name}");
    }
    assert!(
        rows.iter()
            .any(|(_, _, said)| said.contains("assigned") && said.contains("you")),
        "the row of the mail Bo gave her says it was given to her: {rows:?}"
    );
    assert!(
        rows.iter().any(|(_, _, said)| said.contains("Today")),
        "the row of the survey she opened says her agent moved it: {rows:?}"
    );
    // The row of the task she opened carries the face of who changed it, where a press opens what
    // they did; the page beside it is the window's, whose header mark counts it below.
    let opened = changed
        .iter()
        .find(|(_, _, said)| said.contains("Today"))
        .map(|(key, _, _)| key.clone())
        .expect("her agent moved the survey to today");
    let face = list_rows(away)
        .into_iter()
        .find(|row| row.test_id().is_some_and(|id| id == opened))
        .and_then(ui::widget::ListRow::change_face)
        .unwrap_or_else(|| {
            panic!("the row she opened carries the face of who changed it: {opened}")
        });
    assert!(
        face.width() > 8.0 && face.height() > 8.0,
        "the face stands on the page it marks, at a size a person reads: {face:?}"
    );
    // Three: Bo's assign, and the agent's two, one of them the task that is in Done now.
    the_windows_header_mark(away, 3);
    // A node in the tree is not a pixel: the mark stands in the window's own header row, over
    // the rows it counts, and the frame has its ink there.
    let mark = floating_rect(away, "ChangeMark").expect("the window's mark is laid out");
    // The header of the list beside the page is narrow, so its controls stand compact: the mark
    // is the faces of who changed things, a target a finger or a pointer presses, its sentence
    // read out and on hover.
    assert!(
        mark.width() >= 32.0 && mark.height() >= 32.0,
        "the mark has room to stand in the header: {mark:?}"
    );
    // The rows the window lays out stand under the mark that counts them, inside it.
    let listed: Vec<Rect> = list_rows(away)
        .into_iter()
        .map(tilo_ui_base::widget::Widget::get_rect)
        .collect();
    assert!(!listed.is_empty(), "the window lists the tasks it marks");

    assert!(
        listed.iter().all(|row| mark.y1 <= row.y0),
        "the mark stands over the rows it counts, not among them: mark {mark:?}, rows {listed:?}"
    );
    assert!(
        mark.x0 >= 0.0 && mark.x1 <= 1280.0,
        "the mark stands inside the window it counts: {mark:?}"
    );
    assert!(
        mark.y1 <= face.y0 && face.x0 >= 0.0 && face.x1 <= 1280.0,
        "the face of the change stands under the mark, on the page in the window: \
         face {face:?}, mark {mark:?}"
    );
    let ink = |rect: Rect| painted(away, rect, (1280, 800));
    assert!(
        ink(mark) > 500,
        "the mark is painted in the header, not only built: {} px in {mark:?}",
        ink(mark)
    );
    assert!(
        ink(face) > 100,
        "the face on the page the row became is painted, not only built: {} px in {face:?}",
        ink(face)
    );
}

/// A mark's catch-up is a panel under the mark where the window is wide, a sheet at its foot where
/// it is narrow, and neither reaches past the window it belongs to. `away` is the window at
/// 1280×800 as it was staged and laid out; the narrow one is staged here, from `packages`.
fn a_catch_up_hangs_inside_the_window_it_belongs_to(
    packages: &Rc<stage::Packages>,
    mut away: stage::Staged,
) {
    let window = window_mark(&away.composed().expect("the window composed its view"))
        .expect("the window's header says what changed in it");
    away.press(&window.key, false)
        .expect("the window's mark opens its catch-up");
    away.lay_out(
        (1280, 800),
        tilo_ui_base::Theme::light(),
        input_at((1280, 800)),
        1.0,
    )
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

    // A window held in the hand is staged at its own size, as a capture stages it.
    let mut narrow = away_at(packages, (390, 844));
    back_to_the_list(&mut narrow, (390, 844));
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
}

#[test]
fn a_task_that_changed_since_you_looked_says_so_on_its_page() {
    tilo_app_window::i18n::ensure();
    tilo_ui_base::clock::freeze_for_capture();
    let packages = tasks_packages();
    let mut changed = stage::Staged::over(
        "so.tilo.tasks",
        &packages,
        "a task that changed since you looked",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    changed
        .lay_out(
            (1280, 800),
            tilo_ui_base::Theme::light(),
            input_at((1280, 800)),
            1.0,
        )
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
            Some(tilo_render_protocol::patch::Value::Actor(_))
        ),
        "the page names who changed it: {window:?}"
    );
    // Its catch-up hangs inside its window too, and nothing else about the window is wrong.
    changed
        .press(&window.key, false)
        .expect("the page's mark opens its catch-up");
    changed
        .lay_out(
            (1280, 800),
            tilo_ui_base::Theme::light(),
            input_at((1280, 800)),
            1.0,
        )
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

/// What a channel says of the messages the person has not seen, from one build of chat and one
/// window of "a team's channels" at 1280×800: its header counts them, and no message carries a
/// mark of its own. Both parts read the tree the window composed once it was laid out; the first
/// reads nothing else, and the second only reads the window.
#[test]
fn a_channel_counts_what_is_unseen_in_its_header_and_marks_no_message() {
    tilo_app_window::i18n::ensure();
    tilo_ui_base::clock::freeze_for_capture();
    let packages = app_packages("chat");
    let mut channels = stage::Staged::over(
        "so.tilo.chat",
        &packages,
        "a team's channels",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    channels
        .lay_out(
            (1280, 800),
            tilo_ui_base::Theme::light(),
            tilo_ui_base::interaction::InteractionEnvironment::POINTER,
            1.0,
        )
        .expect("the app lays out");
    let tree = channels.composed().expect("the window composes");
    a_channels_header_says_what_the_person_has_not_seen_of_it_by_whom(&tree);
    a_channels_messages_the_person_has_not_seen_take_no_mark_of_their_own(&mut channels, &tree);
}

/// Marks are every resource's, for every type: a channel declares nothing about them. `tree` is
/// what the window `channels` composed.
fn a_channels_messages_the_person_has_not_seen_take_no_mark_of_their_own(
    channels: &mut stage::Staged,
    tree: &tilo_app_window::UiNode,
) {
    // The transcript is told where the part the person has not read starts.
    let lists = tree.of_kind("ui.VirtualList");
    let unseen = lists
        .iter()
        .find(|list| list.props.contains_key("unseen_from"));
    assert!(
        unseen.is_some(),
        "the conversation says where its unseen part starts: {lists:?}"
    );
    // The toolkit draws the conversation's one band there, over the row where that part begins:
    // where that row also opens a day, the day and the place reading starts again are one band
    // saying both, in the accent, never a day's band stacked over the unread one.
    let dividers = dividers_of(channels);
    let merged = ui::widget::conversation::band::day_with_unread("");
    let unseen: Vec<&(String, Rect)> = dividers
        .iter()
        .filter(|(words, _)| words.ends_with(&merged))
        .collect();
    assert_eq!(
        unseen.len(),
        1,
        "one band, over the row where the unseen part starts, saying the day and that reading \
         starts there ({merged:?}): {dividers:?}"
    );
    assert!(
        !unseen[0].0.trim_end_matches(&merged).is_empty(),
        "the merged band names the day it opens: {dividers:?}"
    );
    assert!(
        dividers.len() > 1,
        "the days around it are the same component: {dividers:?}"
    );
    // The channel opens on its newest messages, so a band above them is mounted, scrolled out of
    // the list's view; each band the list shows stands in its column, under the header.
    let shown = shown_dividers(channels);
    assert!(
        !shown.is_empty(),
        "the conversation shows a band of its days: {dividers:?}"
    );
    for (words, rect) in &shown {
        assert!(
            rect.x0 > 300.0 && rect.y1 > 100.0,
            "the divider {words:?} stands in the conversation's column, under its header: {rect:?}"
        );
    }
    for (words, rect) in &dividers {
        assert_eq!(
            (rect.x0, rect.x1),
            (dividers[0].1.x0, dividers[0].1.x1),
            "the dividers rule the one column: {words:?} is {rect:?}, the first {dividers:?}"
        );
    }
    // Nothing marks a message of its own: a face over each line under the divider would read as who
    // read it, and the divider already says those rows are new.
    let marks = rects(channels, "ChangeMark");
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
    // And every band of the conversation stands in the frame as a band: a rule on each side of its
    // words, painted in ink the ground it lands on shows.
    let wrong = wrong_in(channels, (1280, 800));
    assert!(
        wrong.is_empty(),
        "the conversation's bands read as bands: {wrong:?}"
    );
}

/// A catch-up says what happened while the person was away, in the terms of what the window
/// shows: a channel of items Mia, Leo and Ada put in says how many are new, never that three
/// people changed one message. `tree` is what the window of "a team's channels" composed.
fn a_channels_header_says_what_the_person_has_not_seen_of_it_by_whom(
    tree: &tilo_app_window::UiNode,
) {
    let mark = window_mark(tree).expect("the channel's header says what is new");
    assert_eq!(
        mark.props.get("count"),
        Some(&tilo_render_protocol::patch::Value::Int(6)),
        "six messages wait in the transcript's column, not one change and not the thread the \
         transcript does not list: {mark:?}"
    );
    assert_eq!(
        mark.props.get("added"),
        Some(&tilo_render_protocol::patch::Value::Bool(true)),
        "every one of them is new: {mark:?}"
    );
    // And the channel's row in the sidebar says the same number: one count of what is new in a
    // conversation, never two numbers a person has to tell apart.
    let place = tree
        .of_kind("ui.NavItem")
        .into_iter()
        .find(|place| {
            matches!(
                place.props.get("item"),
                Some(tilo_render_protocol::patch::Value::Ref(item))
                    if matches!(mark.props.get("of"),
                        Some(tilo_render_protocol::patch::Value::Ref(of)) if item.id == of.id)
            )
        })
        .unwrap_or_else(|| panic!("the sidebar holds the channel's row: {tree:?}"));
    assert_eq!(
        place.props.get("live"),
        mark.props.get("count"),
        "the row counts what the header counts: {place:?} against {mark:?}"
    );
}

#[test]
fn a_scenario_that_fails_is_said_rather_than_shown_empty() {
    tilo_app_window::i18n::ensure();
    let (app, packages) = packages();
    let failed = stage::Staged::over(
        &app,
        &packages,
        "no such scenario",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    );
    let Err(why) = failed else {
        panic!("a scenario that does not run is an error");
    };
    assert!(why.contains("no such scenario"), "{why}");
}

/// The one main action an app declares for its window stands in its column's header row as the
/// primary button, at a desktop's width and in a hand's: a person sees at once what the window is for,
/// and the action narrows to its icon before it ever leaves the row.
#[test]
fn the_windows_main_action_stands_in_its_row_as_the_primary_button() {
    tilo_app_window::i18n::ensure();
    // Each app's directory under `apps/tl` and the id it answers to: Notes took the id the
    // native app had, so a directory's name does not say its app's id.
    for (dir, app, scenario, action) in [
        ("tasks", "so.tilo.tasks", "a busy week", "New task"),
        ("notes", "so.tilo.notes", "a first note", "New note"),
    ] {
        let packages = app_packages(dir);
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
            tilo_ui_base::Theme::dark()
        } else {
            tilo_ui_base::Theme::light()
        };
        let mut staged =
            stage::Staged::over(app, packages, scenario, size, theme.clone(), None, &[])
                .expect("the scenario stages");
        let input = if size.0 < 600 {
            tilo_ui_base::interaction::InteractionEnvironment::TOUCH
        } else {
            tilo_ui_base::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, theme, input, 1.0)
            .expect("the app lays out");
        back_to_the_list(&mut staged, size);
        let row = super::header_row_controls(&staged, size);
        let primary: Vec<&super::RowControl> =
            row.iter().filter(|control| control.primary).collect();
        assert_eq!(
            primary.len(),
            1,
            "one window draws one main action at {size:?}: {row:?}"
        );
        let control = primary[0];
        assert_eq!(
            control.name, action,
            "the app's own main action: {control:?}"
        );
        assert!(
            !control.menu,
            "what the window is for never hides behind the row's button: {control:?}"
        );
        assert!(
            control.rect[2] > 0.0 && control.rect[3] > 0.0,
            "it stands where the person reaches it at {size:?}: {control:?}"
        );
        if size.0 >= 1280 {
            assert!(
                control.labelled,
                "a desktop's row says the main action's words at {size:?}: {control:?}"
            );
        }
        if size.0 <= 320 {
            // The narrowest row says the words where it has room for them and stands as its icon
            // alone where it has not: either way it is the primary button still.
            assert!(
                control.labelled || control.rect[2] <= control.rect[3] * 1.5,
                "the narrowest row keeps the main action to its icon alone, still on the \
                 accent at {size:?}: {control:?}"
            );
        }
    }
}

/// What one layout shows of `staged`'s window bar, and what is wrong with it: whether the window
/// draws the band above its frame, whether the app's header row is the bar itself, and each problem
/// added to `problems`, named by `what` and the layout.
fn the_window_bar_at(
    staged: &mut stage::Staged,
    layout: &super::Layout,
    what: &str,
    problems: &mut Vec<String>,
) -> (bool, bool) {
    let size = (layout.size.width, layout.size.height);
    let (bar, _) = staged.harness().frame_extent(size);
    let row = staged.harness().window_header_row_height();
    if bar > 0.0 && row > 0.0 {
        problems.push(format!(
            "{what} at {layout}: a bar {bar:.0}px tall and, under it, a header row of the app's \
             {row:.0}px tall",
        ));
    }
    let held = staged.harness().row_holds_bar();
    if held {
        let furniture = staged.harness_mut().window_bar_layout();
        let window = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
        let close = furniture
            .controls
            .iter()
            .find(|control| control.part == "close" && control.rect[2] > 0.0);
        let inside = close.is_some_and(|close| {
            let [x0, y0, x1, y1] = close.rect;
            window.contains(Rect::new(x0, y0, x1, y1).center())
        });
        if !inside {
            problems.push(format!(
                "{what} at {layout}: the row holds the window's bar, and its controls stand \
                 outside the window: {:?}",
                furniture.controls,
            ));
        }
    }
    for problem in super::audit::inspect(
        &staged.harness().inspection().root,
        size,
        None,
        layout.display_scale,
    ) {
        if problem.kind == super::Kind::TitleClippedWithRoom {
            problems.push(format!(
                "{what} at {layout}: {}: {}",
                problem.widget, problem.detail
            ));
        }
    }
    (bar > 0.0, held)
}

/// The Tasks app at the sizes a person holds a window in the hand shows one bar each time: the
/// week's task page at phone width, the list behind it and the page a row opens in front of it,
/// and a project in a tablet's window.
#[test]
fn the_tasks_window_shows_one_bar_and_keeps_its_title_at_every_hand_held_size() {
    tilo_app_window::i18n::ensure();
    let packages = tasks_packages();
    let app = packages.compiled.last().expect("the app").meta.app.clone();
    let all = super::layouts();
    let mut problems: Vec<String> = Vec::new();
    let mut measured = 0_usize;
    let (mut band, mut held) = (0_usize, 0_usize);
    for (scenario, size) in [
        ("a busy week", (320_u32, 568_u32)),
        ("a project board", (390_u32, 844_u32)),
    ] {
        // The window as the person holds it: one display's pixels, at both text scales.
        let of_size: Vec<super::Layout> = all
            .iter()
            .copied()
            .filter(|layout| (layout.size.width, layout.size.height) == size)
            .filter(|layout| (layout.display_scale - 1.0).abs() < f64::EPSILON)
            .collect();
        let first = of_size[0];
        let mut staged =
            stage::Staged::over(&app, &packages, scenario, size, first.theme(), None, &[])
                .expect("the scenario stages");
        let mut lay_out = |staged: &mut stage::Staged, layout: &super::Layout, what: &str| {
            staged
                .lay_out(
                    (layout.size.width, layout.size.height),
                    layout.theme(),
                    layout.input(),
                    layout.display_scale,
                )
                .expect("lays out");
            let (drew, holds) = the_window_bar_at(staged, layout, what, &mut problems);
            measured += 1;
            band += usize::from(drew);
            held += usize::from(holds);
        };
        for layout in &of_size {
            lay_out(&mut staged, layout, scenario);
        }
        // A window opened on a task shows its page in front, the list behind the shell's Back:
        // the list is a screen of the window too.
        if super::back_chip(&staged, size) {
            back_to_the_list(&mut staged, size);
            let listed = format!("{scenario}, back on its list");
            for layout in &of_size {
                lay_out(&mut staged, layout, &listed);
            }
        }
        // What the window opens in front of its list, as a finger opens it.
        if let Some(tapped) = super::tappable_row(&staged, size) {
            staged.tap_at(tapped).expect("the row opens");
            let opened = format!("{scenario}, then the row opened");
            for layout in &of_size {
                lay_out(&mut staged, layout, &opened);
            }
        }
    }
    assert!(measured >= 8, "the windows were laid out: {measured}");
    assert!(
        problems.is_empty(),
        "a window shows one bar, and its title where the row has room:\n  {}",
        problems.join("\n  ")
    );
    assert!(
        band == 0 && held == measured,
        "the app's header row is the window's bar at every size: {band} with the window's own \
         bar, {held} of {measured} with the app's row as the bar"
    );
}

/// What a row of a list says its item is: the title the row's own tree carries.
fn row_title(node: &tilo_app_window::UiNode) -> Option<String> {
    if let Some(tilo_render_protocol::patch::Value::Text(title)) = node.props.get("title") {
        return Some(title.clone());
    }
    node.children.iter().find_map(row_title)
}

/// A page the check stages over a list opens a row the list itself shows first, and the same row in
/// every run: the list's own order, never the order a hash of the row keys happens to give.
#[test]
fn a_list_opens_the_page_of_its_first_row_in_every_run() {
    tilo_app_window::i18n::ensure();
    let packages = tasks_packages();
    let task = packages
        .compiled
        .iter()
        .find(|package| package.meta.app == "so.tilo.tasks")
        .and_then(|package| package.meta.types.iter().find(|ty| ty.name == "Task"))
        .map(|ty| ty.id.clone())
        .expect("the tasks app declares its task");
    let first = super::layouts().first().copied().expect("a layout");
    let size = (first.size.width, first.size.height);
    let mut opened = Vec::new();
    for run in 0..4 {
        let mut staged = stage::Staged::over(
            "so.tilo.tasks",
            &packages,
            "a busy week",
            size,
            first.theme(),
            None,
            &[],
        )
        .expect("the scenario stages");
        // The rows as the window lists them, and the page the run opens off them: the row it opens is
        // the first one, by the title the row shows rather than by the id a staging gives its records.
        let rows = staged.rows();
        let named = staged.resources();
        let picked = super::listed_of(&mut staged, &task).expect("a task to open a page of");
        let at = rows
            .iter()
            .position(|(key, _)| key.ends_with(&picked.id))
            .unwrap_or_else(|| panic!("the page opens a row the list shows, run {run}"));
        assert_eq!(
            at, 0,
            "the page opens the row the list shows first, run {run}"
        );
        let listed = super::row_items(&rows, &named);
        assert_eq!(
            listed.first(),
            Some(&picked),
            "the row is the first of its type the list shows, run {run}"
        );
        opened.push(row_title(&rows[at].1).expect("the row says what its task is called"));
    }
    assert!(
        opened.windows(2).all(|pair| pair[0] == pair[1]),
        "every run opens the same task: {opened:?}"
    );
}

#[test]
fn a_narrow_channels_mark_opens_its_catch_up() {
    // The channel opens scrolled to its newest message, so rows stand above the list's viewport,
    // under the window's header: the mark the header holds is where the person's finger lands,
    // and it opens the catch-up as a sheet across the window's foot.
    on_a_windows_stack(|| {
        tilo_app_window::i18n::ensure();
        tilo_ui_base::clock::freeze_for_capture();
        let packages = app_packages("chat");
        let mut channels = stage::Staged::over(
            "so.tilo.chat",
            &packages,
            "a team's channels",
            (320, 568),
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        channels
            .lay_out_moving(
                (320, 568),
                tilo_ui_base::Theme::light(),
                input_at((320, 568)),
            )
            .expect("the app lays out");
        // The header names the channel it shows: the row gives the title the room the word
        // needs, and the mark beside it gives up its names for one face instead of cutting it
        // short.
        let title = shell_at(&channels)
            .and_then(|shell| {
                shell
                    .rows
                    .rows
                    .into_iter()
                    .flatten()
                    .find(|row| row.column == "content")
            })
            .expect("the window's shell says how it held its header");
        assert!(
            title.title_opens && title.title_needs <= title.title_has + 0.5,
            "the header's title reads whole: {title:?}"
        );
        let mark = shown_floating_rect(&channels, "ChangeMark").expect("the header shows its mark");
        assert!(
            mark.y0 >= 0.0 && mark.y1 <= 56.0,
            "the mark stands in the window's header: {mark:?}"
        );
        assert!(
            mark.width() <= mark.height() + 1.0,
            "the mark stands as one face beside the title, not as a run of names: {mark:?}"
        );
        channels
            .press_at(mark.center().x, mark.center().y)
            .expect("the mark presses");
        channels
            .freshen()
            .expect("the app lays out with its catch-up open");
        let sheet =
            shown_floating_rect(&channels, "Popover").expect("the narrow mark opens its catch-up");
        // A surface of its own, read out as what it holds rather than as the words of the
        // control it hangs from.
        assert_eq!(
            shown_floating_name(&channels, "Popover").as_deref(),
            Some("Changes since you looked"),
            "the catch-up names itself"
        );
        let (_, frame) = channels.harness().frame_extent((320, 568));
        assert!(
            sheet.x0 >= 0.0
                && sheet.x1 <= 320.0
                && sheet.width() >= 300.0
                && sheet.y1 <= frame
                && sheet.y1 >= frame - 24.0,
            "the catch-up stands as a sheet across the window's foot: {sheet:?} of {frame}"
        );
        // What it says: history's words for a post, never the action's own label, and how many
        // changes the step folds beside them, since the channel's posts are not all it holds.
        let composed = channels.composed().expect("the page composes");
        let rows = catch_up_rows(&composed);
        assert!(
            rows.iter().any(|row| row.contains("wrote a message")),
            "the row says what was done in history's words: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("Send")),
            "never the action's own label: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("7 changes")),
            "the step of posts and a thread says how many changes it folds: {rows:?}"
        );
    });
}

/// A catch-up row says what a person did in the app's own words, and how many messages a step of
/// posts holds, once: the count stands in the sentence, not as a count beside it.
#[test]
fn a_catch_up_row_counts_the_messages_a_step_holds_in_its_sentence() {
    tilo_app_window::i18n::ensure();
    tilo_ui_base::clock::freeze_for_capture();
    let packages = app_packages("chat");
    // A thread two people wrote in: two posts, by two people, and nothing else since.
    let mut thread = stage::Staged::over(
        "so.tilo.chat",
        &packages,
        "a thread",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    thread
        .lay_out(
            (1280, 800),
            tilo_ui_base::Theme::light(),
            input_at((1280, 800)),
            1.0,
        )
        .expect("the app lays out");
    let mark = shown_floating_rect(&thread, "ChangeMark").expect("the header shows its mark");
    thread
        .press_at(mark.center().x, mark.center().y)
        .expect("the mark presses");
    thread
        .freshen()
        .expect("the app lays out with its catch-up open");
    let composed = thread.composed().expect("the page composes");
    let rows = catch_up_rows(&composed);
    assert_eq!(rows.len(), 1, "the thread stands as one row: {rows:?}");
    assert!(
        rows[0].contains("wrote 2 messages"),
        "the row counts its posts in its own words: {rows:?}"
    );
    assert!(
        !rows[0].contains("changes"),
        "and says it once, never as a count beside the sentence: {rows:?}"
    );
}

#[test]
fn a_task_two_people_changed_names_them_both_in_its_catch_up() {
    // One row for the task however many changed it: everyone who did is named on the line, with a
    // face each, and what it says is the newest change's words.
    tilo_app_window::i18n::ensure();
    tilo_ui_base::clock::freeze_for_capture();
    let packages = tasks_packages();
    let mut page = stage::Staged::over(
        "so.tilo.tasks",
        &packages,
        "a task two people changed",
        (1280, 800),
        tilo_ui_base::Theme::light(),
        None,
        &[],
    )
    .expect("the scenario stages");
    page.lay_out(
        (1280, 800),
        tilo_ui_base::Theme::light(),
        input_at((1280, 800)),
        1.0,
    )
    .expect("the app lays out");
    let window = {
        let composed = page.composed().expect("the page composes");
        window_mark(&composed).expect("the page says what changed since the person looked")
    };
    assert_eq!(
        window.props.get("count"),
        Some(&tilo_render_protocol::patch::Value::Int(1)),
        "the task changed, once: {window:?}"
    );
    let content = window
        .children
        .iter()
        .find(|child| {
            child.kind == "part"
                && child.props.get("name")
                    == Some(&tilo_render_protocol::patch::Value::text("content"))
        })
        .and_then(|part| part.children.first())
        .unwrap_or_else(|| panic!("the mark opens its catch-up: {window:?}"));
    let rows = content.of_kind("row");
    let steps: Vec<&tilo_app_window::UiNode> = rows
        .into_iter()
        .filter(|row| row.key.ends_with("~step"))
        .collect();
    assert_eq!(
        steps.len(),
        1,
        "the task stands as one row, not one per change: {content:?}"
    );
    let step = steps[0];
    let faces = step
        .of_kind("ui.Avatar")
        .into_iter()
        .filter(|face| {
            matches!(
                face.props.get("who"),
                Some(tilo_render_protocol::patch::Value::Actor(_))
            )
        })
        .count();
    assert_eq!(
        faces, 2,
        "a face for each of the two who changed it: {step:?}"
    );
    let words: Vec<String> = step
        .of_kind("ui.Text")
        .into_iter()
        .filter_map(|text| match text.props.get("value") {
            Some(tilo_render_protocol::patch::Value::Text(value)) => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        words.first().map(String::as_str),
        Some("Bo and Scout"),
        "the line names both who changed the task, in the order they changed it: {words:?}"
    );
    assert!(
        // Checking it off moves it to the Done column: its status is what says it is done.
        words
            .get(1)
            .is_some_and(|said| said == "set status to Done"),
        "and goes on with the newest change's words: {words:?}"
    );
}

/// The gallery's controls show what the fields they are bound to hold: a room the type starts at,
/// and the case an urgency starts as, in the words the enum gives it.
#[test]
fn the_gallerys_controls_show_what_their_fields_hold() {
    on_a_windows_stack(|| {
        tilo_app_window::i18n::ensure();
        tilo_ui_base::clock::freeze_for_capture();
        let packages = app_packages("gallery");
        let ty = packages
            .compiled
            .iter()
            .find(|c| c.meta.app == "so.tilo.lang.gallery")
            .and_then(|c| c.meta.types.iter().find(|t| t.name == "Gallery"))
            .map(|t| t.id.clone())
            .expect("the gallery declares its root");
        let size = (1280, 800);
        let mut staged = stage::Staged::over(
            "so.tilo.lang.gallery",
            &packages,
            "controls",
            size,
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged
            .lay_out(size, tilo_ui_base::Theme::light(), input_at(size), 1.0)
            .expect("the app lays out");
        let controls = super::listed_of(&mut staged, &ty).expect("the gallery is listed");
        staged.open(controls.clone());
        staged
            .lay_out(size, tilo_ui_base::Theme::light(), input_at(size), 1.0)
            .expect("the gallery lays out");
        // The window opens on its basics: the controls' page is a view of the same resource.
        let page = packages
            .compiled
            .iter()
            .find(|c| c.meta.app == "so.tilo.lang.gallery")
            .and_then(|c| c.meta.views.iter().find(|v| v.name == "controls"))
            .map(|v| v.id.clone())
            .expect("the gallery declares the controls' page");
        staged.open_with(&controls, &page);
        staged
            .lay_out(size, tilo_ui_base::Theme::light(), input_at(size), 1.0)
            .expect("the controls lay out");
        let nodes: Vec<(String, String)> = staged
            .harness()
            .a11y_nodes()
            .into_iter()
            .map(|(_, node)| {
                (
                    node.label().unwrap_or_default().to_owned(),
                    node.value().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        assert!(
            nodes.iter().any(|(label, _)| label == "Urgency, No hurry"),
            "the urgency shows its case: {nodes:?}"
        );
        assert!(
            nodes.iter().any(|(_, value)| value == "Studio"),
            "the room shows the one the type starts at: {nodes:?}"
        );
    });
}

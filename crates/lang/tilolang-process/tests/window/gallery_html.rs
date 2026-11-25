//! The gallery as the server page shows it. Each place is picked as a person picks it there: the
//! action its sidebar row's element names, on the widget that element stands for. Its
//! composition is rendered as the HTML the page is sent (`Page::render`), and what changed since
//! the page last answered is sent as patches onto the elements it holds (`Page::patch`).

use base64::Engine as _;
use tilo_app_test_host::TestApp;
use tilolang_process::window::app::TlApp;
use ui::a11y::Role;
use ui::composition::page::{Page, PageAction, Patch};
use ui::composition::{Composed, Composition};
use ui::headless::Driver;

const PLACES: [&str; 15] = [
    "Basics",
    "Controls",
    "Feedback",
    "States",
    "Backlog",
    "Release board",
    "Data",
    "Media",
    "Places",
    "Shell parts",
    "Charts and views",
    "Trends and series",
    "Shares and shapes",
    "Flows and trees",
    "Type and scripts",
];

/// Every place of the sidebar, picked on the page: the page offers its row as an element it acts
/// on, the place shows, the patches the page is sent turn what it showed into what a whole render
/// of the place gives, and the page lays itself out.
#[test]
fn every_place_is_picked_and_patched_on_the_page() {
    crate::support::installed("gallery");
    let mut app = TestApp::<TlApp>::drive_at("custom", (1280, 800)).expect("the scenario stages");
    let harness = app.harness();
    let mut composition = until(harness, "the sidebar lists its places", |composition| {
        PLACES.iter().all(|place| row(composition, place).is_some())
    });
    let mut page = Page::new();
    let mut shown = page.render(&composition);
    for place in PLACES {
        let picked =
            row(&composition, place).unwrap_or_else(|| panic!("the sidebar lists {place}"));
        let id = (0..)
            .map_while(|id| page.path(id).map(|path| (id, path)))
            .find_map(|(id, path)| (path == picked.path).then_some(id))
            .unwrap_or_else(|| panic!("the page numbers {place}'s row ({})", picked.path));
        let tag = opening_tag(&shown, id).unwrap_or_else(|| panic!("the page shows {place}'s row"));
        assert!(
            tag.contains(" data-a=\"activate\""),
            "the page acts on {place}'s row: {tag}"
        );
        // What the page sends back for a press on it: its action, on the widget its element
        // stands for, at the target it carries.
        let target = attribute(tag, "data-t");
        page.act(harness, id, target.as_deref(), PageAction::Activate)
            .unwrap_or_else(|refused| panic!("a press on {place}'s row is taken: {refused}"));
        composition = until(
            harness,
            &format!("{place} shows once picked"),
            |composition| shows(composition, place),
        );
        let sent = page.patch(&composition);
        assert!(
            place == PLACES[0] || !sent.is_empty(),
            "{place}: picking it changes the page"
        );
        let patched = sent
            .iter()
            .fold(shown, |html, patch| apply(&html, patch, place));
        shown = page.render(&composition);
        assert_eq!(
            patched, shown,
            "{place}: the patches turn what the page showed into the place"
        );
        assert!(
            !shown.contains("position:absolute") && !shown.contains(";left:"),
            "{place}: the page lays itself out: {shown}"
        );
        if place == "Data" {
            let tall = tall_enough_for_the_tree(harness);
            let tall_shown = Page::new().render(&tall);
            assert!(
                !tall_shown.contains("position:absolute") && !tall_shown.contains(";left:"),
                "Data, its tree in view: the page lays itself out: {tall_shown}"
            );
            rows_under_a_row_stand_below_it(&tall);
            harness
                .resize((1280, 800))
                .expect("the window takes its size back");
            composition = until(
                harness,
                "Data shows again at the window's size",
                |composition| composition.viewport == [1280, 800] && shows(composition, place),
            );
            shown = page.render(&composition);
        }
        if place == "Charts and views" {
            charts_show_their_marks(&composition, &shown);
        }
    }
}

/// The view in a window tall enough to show the Data page's tree, which stands below the fold of
/// the window the test drives, with its rows shown.
fn tall_enough_for_the_tree(harness: &mut Driver) -> Composition {
    harness
        .resize((1280, 2400))
        .expect("the window takes its new size");
    until(
        harness,
        "the Data page's tree shows its rows",
        |composition| {
            nodes(composition).into_iter().any(|node| {
                node.widget == "TreeView"
                    && node
                        .children
                        .iter()
                        .any(|row| row.rect[2] > 0.0 && row.rect[3] > 0.0)
            })
        },
    )
}

/// A tree's rows stand one under another on the page, each below the one before it, never beside
/// it: the page lays them out in the tree's rows, it does not place them.
fn rows_under_a_row_stand_below_it(composition: &Composition) {
    let tree = nodes(composition)
        .into_iter()
        .find(|node| node.widget == "TreeView")
        .expect("the Data page shows a tree");
    assert!(
        tree.flows,
        "{}: the page lays out the tree's rows",
        tree.path
    );
    let rows: Vec<&Composed> = tree
        .children
        .iter()
        .filter(|row| row.rect[2] > 0.0 && row.rect[3] > 0.0)
        .collect();
    assert!(
        rows.len() > 1,
        "the Data page's tree shows its rows: {rows:?}"
    );
    for pair in rows.windows(2) {
        let [above, below] = [pair[0].rect, pair[1].rect];
        assert!(
            below[1] >= above[1] + above[3] - 0.5,
            "{}: a row stands below the one before it, never beside it: {above:?} then {below:?}",
            pair[1].path
        );
    }
    for row in &rows {
        assert!(
            row.layout
                .as_deref()
                .is_some_and(|css| css.contains("grid-row:")),
            "{}: the page lays the row out in the tree's rows: {:?}",
            row.path,
            row.layout
        );
    }
}

/// Every part the page shows as a picture (a chart's marks, a sparkline's line) is sent drawn: the
/// page shows a picture where it stands, and the picture shows something.
fn charts_show_their_marks(composition: &Composition, shown: &str) {
    let painted: Vec<(&str, &str)> = nodes(composition)
        .into_iter()
        .filter_map(|node| {
            let picture = node.picture.as_ref()?;
            Some((
                node.path.as_str(),
                picture
                    .data
                    .as_deref()
                    .unwrap_or_else(|| panic!("{} is drawn for the page", node.path)),
            ))
        })
        .collect();
    assert!(
        nodes(composition).into_iter().any(|node| {
            node.picture.is_some() && matches!(node.widget.as_str(), "Chart" | "Sparkline")
        }),
        "the page shows a chart's marks: {painted:?}"
    );
    for (path, picture) in painted {
        assert!(
            shown.contains("<img class=\"tilo-paint\"") && shown.contains(picture),
            "the page shows {path}'s picture"
        );
        let png = picture
            .strip_prefix("data:image/png;base64,")
            .and_then(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .ok()
            })
            .unwrap_or_else(|| panic!("{path}'s picture is a PNG"));
        let drawn = ui::png::decode_rgba8(&png).expect("the picture decodes");
        assert!(
            drawn
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel.get(3) > Some(&0)),
            "{path}'s picture shows something"
        );
    }
}

/// The view as the page reads it, until `reads` holds of it: what reached it laid out, as a page's
/// read lays it out. The gallery reads its indexes on workers that wake the view when a read
/// lands, so the test waits for what it reads, and fails saying so if it never comes.
pub(crate) fn until(
    harness: &mut Driver,
    waited: &str,
    reads: impl Fn(&Composition) -> bool,
) -> Composition {
    let began = std::time::Instant::now();
    loop {
        for _ in 0..8 {
            if !harness.poll_frame().unwrap_or(false) {
                break;
            }
        }
        let mut composition = harness.composition();
        if reads(&composition) {
            harness.paint_pictures(&mut composition);
            return composition;
        }
        assert!(began.elapsed().as_secs() < 60, "{waited}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Every part of `composition`, outermost first.
pub(crate) fn nodes(composition: &Composition) -> Vec<&Composed> {
    let mut all = Vec::new();
    let mut pending: Vec<&Composed> = std::iter::once(&composition.root)
        .chain(&composition.layers)
        .collect();
    while let Some(node) = pending.pop() {
        all.push(node);
        pending.extend(&node.children);
    }
    all
}

/// The sidebar's row for `place`.
pub(crate) fn row<'a>(composition: &'a Composition, place: &str) -> Option<&'a Composed> {
    nodes(composition).into_iter().find(|node| {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.role == Role::TreeItem && access.answers_to(place))
    })
}

/// Whether the page beside the sidebar shows `place`: a part other than its row is named for it.
pub(crate) fn shows(composition: &Composition, place: &str) -> bool {
    nodes(composition).into_iter().any(|node| {
        node.semantics.as_ref().is_some_and(|access| {
            access.role != Role::TreeItem && access.name.as_deref() == Some(place)
        })
    })
}

/// Where the opening tag of the element numbered `id` in `html` starts and ends.
fn opening(html: &str, id: u32) -> Option<(usize, usize)> {
    let at = html.find(&format!(" data-p=\"{id}\""))?;
    let start = html.get(..at)?.rfind('<')?;
    let end = html
        .get(start..)?
        .find('>')?
        .checked_add(start)?
        .checked_add(1)?;
    Some((start, end))
}

/// The opening tag of the element numbered `id` in `html`.
fn opening_tag(html: &str, id: u32) -> Option<&str> {
    let (start, end) = opening(html, id)?;
    html.get(start..end)
}

/// The value of `name` in an opening tag, as the browser reads it.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let (_, rest) = tag.split_once(&format!(" {name}=\""))?;
    let (value, _) = rest.split_once('"')?;
    Some(
        value
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// `html` with `patch` applied as the page applies it: the element it numbers replaced whole or
/// taken out, or the new one put in as its parent's child element at its place.
fn apply(html: &str, patch: &Patch, place: &str) -> String {
    let held = |id: u32| {
        opening(html, id).unwrap_or_else(|| {
            panic!("{place}: a patch names element {id}, which the page does not hold")
        })
    };
    // The element starting at `start`, cut out of the page with `with` in its place.
    let cut = |start: usize, with: &str| {
        let (before, from) = html.split_at_checked(start).expect("a tag starts there");
        let (_, after) = from
            .split_at_checked(element_len(from))
            .expect("the element ends inside the page");
        format!("{before}{with}{after}")
    };
    match patch {
        Patch::Replace { id, html: fresh } => cut(held(*id).0, fresh),
        Patch::Remove { id } => cut(held(*id).0, ""),
        Patch::Insert {
            parent,
            at,
            html: fresh,
        } => {
            // Past the parent's opening tag and its own words, then past `at` child elements.
            let skip_words = |from: usize| {
                from.saturating_add(
                    html.get(from..)
                        .and_then(|rest| rest.find('<'))
                        .expect("the parent closes inside the page"),
                )
            };
            let mut point = skip_words(held(*parent).1);
            for _ in 0..*at {
                let child = html.get(point..).expect("a child starts there");
                point = skip_words(point.saturating_add(element_len(child)));
            }
            let (before, after) = html.split_at_checked(point).expect("a tag starts there");
            format!("{before}{fresh}{after}")
        }
    }
}

/// How long the element `html` starts with is, up to and with its closing tag.
fn element_len(html: &str) -> usize {
    const VOID: [&str; 5] = ["input", "br", "hr", "img", "wbr"];
    let mut depth = 0_usize;
    let mut len = 0_usize;
    for piece in html.split_inclusive('>') {
        len = len.saturating_add(piece.len());
        let Some((_, tag)) = piece.split_once('<') else {
            continue;
        };
        let tag = tag.trim_end_matches('>');
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default();
        if tag.starts_with('/') {
            depth = depth.saturating_sub(1);
        } else if !tag.ends_with('/') && !VOID.contains(&name) {
            depth = depth.saturating_add(1);
        }
        if depth == 0 {
            return len;
        }
    }
    panic!("the element closes")
}

//! The gallery as the server page shows it. Each place is picked as a person picks it there: the
//! action its sidebar row's element names, on the widget that element stands for. Its
//! composition is rendered as the HTML the page is sent (`Page::render`), and what changed since
//! the page last answered is sent as patches onto the elements it holds (`Page::patch`).

use base64::Engine as _;
use tilolang_app::app::TlApp;
use ui::a11y::{Action, Role};
use ui::composition::page::{Page, Patch};
use ui::composition::{Composed, Composition};
use ui::framework::testing::TestApp;
use ui::headless::FrameHarness;

const PLACES: [&str; 11] = [
    "Basics",
    "Controls",
    "Feedback",
    "Backlog",
    "Release board",
    "Data",
    "Media",
    "Places",
    "Shell parts",
    "Charts and views",
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
        let widget = picked
            .path
            .split_once('#')
            .map_or(picked.path.as_str(), |(widget, _)| widget);
        harness
            .perform_semantic_named(widget, target.as_deref(), &Action::Activate)
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
            rows_under_a_row_stand_below_it(&composition);
        }
        if place == "Charts and views" {
            charts_show_their_marks(&composition, &shown);
        }
    }
}

/// A tree's row that holds others is a column on the page as tall as both: its own line first,
/// the rows under it below that line, never beside it.
fn rows_under_a_row_stand_below_it(composition: &Composition) {
    let mut holding = 0_usize;
    for node in nodes(composition) {
        let Some(notes) = node
            .children
            .iter()
            .find(|child| child.widget == "TreeNotes")
        else {
            continue;
        };
        holding = holding.saturating_add(1);
        assert!(
            node.flows
                && node
                    .layout
                    .as_deref()
                    .is_some_and(|css| css.contains("flex-direction:column")),
            "{}: a row holding others lays them out down the page: {:?}",
            node.path,
            node.layout
        );
        assert_eq!(
            node.children.last().map(|child| child.path.as_str()),
            Some(notes.path.as_str()),
            "{}: the rows under it come after its own line",
            node.path
        );
    }
    assert!(holding > 0, "the Data page's tree holds rows under rows");
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
fn until(
    harness: &mut FrameHarness,
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
fn nodes(composition: &Composition) -> Vec<&Composed> {
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
fn row<'a>(composition: &'a Composition, place: &str) -> Option<&'a Composed> {
    nodes(composition).into_iter().find(|node| {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.role == Role::TreeItem && access.answers_to(place))
    })
}

/// Whether the page beside the sidebar shows `place`: a part other than its row is named for it.
fn shows(composition: &Composition, place: &str) -> bool {
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

/// `html` with `patch` applied as the page applies it: the element it numbers replaced whole.
fn apply(html: &str, patch: &Patch, place: &str) -> String {
    let (start, _) = opening(html, patch.id).unwrap_or_else(|| {
        panic!(
            "{place}: a patch replaces element {}, which the page does not hold",
            patch.id
        )
    });
    let (before, from) = html.split_at_checked(start).expect("a tag starts there");
    let (_, after) = from
        .split_at_checked(element_len(from))
        .expect("the element ends inside the page");
    format!("{before}{}{after}", patch.html)
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

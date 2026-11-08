//! Acceptance step 9: switching the theme changes how the tilolang shell looks,
//! not its layout.
//!
//! The headless session's whole screen is the tilolang shell, with the seeded
//! notes and mail apps published and a note and the inbox placed on the canvas.
//! The theme changes the way Settings and Quick Settings change it: the person's
//! appearance section's own published `field-edit` action on the settings
//! service the session hosts — never a theme poked straight into a host.
//!
//! Across Default (Dark), Glass, Pixelated, Liquid Glass and Liquid Glass light
//! the test holds three things:
//!
//! 1. **The layout does not change.** Every node the shell lays out for assistive
//!    technology — its kind, its name, its order, and its rectangle — is the same
//!    in every theme, with one exception a theme role legitimately makes:
//!    `type.weights` sets the weight body text, labels and headings are drawn in,
//!    and a heavier weight measures a wider text run. Pixelated and Block set
//!    `heading: bold` against the stock `semibold`, so a button's label or a
//!    heading measures a pixel or two wider, and what follows it in a packed row
//!    starts that much further along. That is all: a run's own width moves by a
//!    pixel or two, the vertical and every box a view lays out (a place, a strip,
//!    a panel) stay exactly where Default puts them, and a theme whose `type` role
//!    is the stock one lays out Default's rectangles exactly. Nothing else in any
//!    theme moves: not a gap, not a row, not a size a view asked for.
//! 2. **The look does change.** The frames the compositor draws differ between
//!    every pair of themes.
//! 3. **Nothing is drawn while idle.** Once a switch settles, the shell's views
//!    run no more and the toolkit draws no frame.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{OutputConfig, Screenshot};
use tilo_session::{Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";
/// The person's appearance section, as the settings service publishes it: the resource whose
/// `field-edit` action the theme is chosen through, and whose stored fields say what it holds.
const APPEARANCE: &str = "settings:settings.appearance";

/// A node the shell lays out for assistive technology: what it is, what it says,
/// and where it stands, logical `(x0, y0, x1, y1)`.
#[derive(Clone, Debug, PartialEq)]
struct Node {
    role: String,
    label: Option<String>,
    bounds: Option<[f64; 4]>,
}

impl Node {
    /// The node, as the toolkit's accessibility tree is walked for it: every window's nodes in the
    /// order the exporter lists them.
    fn floor(&self) -> [f64; 4] {
        self.bounds.unwrap_or([0.0; 4])
    }
}

/// The theme a switch asks for, and how the appearance section names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Theme {
    /// The name the test prints it by.
    name: &'static str,
    /// The preset id `appearance.dark_theme` / `appearance.light_theme` holds.
    id: &'static str,
    /// The mode its palette is on (`appearance.mode`).
    light: bool,
}

impl Theme {
    const ALL: [Self; 5] = [
        Self {
            name: "Default",
            id: "dark",
            light: false,
        },
        Self {
            name: "Glass",
            id: "glass",
            light: false,
        },
        Self {
            name: "Pixelated",
            id: "pixelated",
            light: false,
        },
        Self {
            name: "Liquid Glass",
            id: "liquid-glass",
            light: false,
        },
        Self {
            name: "Liquid Glass light",
            id: "liquid-glass-light",
            light: true,
        },
    ];

    /// Whether this theme's `type` role draws text at another weight than the stock one (the design
    /// language's 400 / 500 / 600), so its text runs measure a pixel wider or narrower.
    fn moves_type_metrics(self) -> bool {
        ui::theme::Preset::find(self.id)
            .expect("a shipped preset")
            .document()
            .type_
            .weights
            != ui::theme::ThemeWeights::STOCK
    }
}

/// The session the test runs in: the tilolang shell as the whole screen, the seeded notes, tasks,
/// chat and mail apps published, and the person's settings service beside them, which publishes the
/// appearance section the theme is chosen through.
fn session() -> Session {
    let apps = common::seeded_apps();
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![
                Box::new(apps),
                Box::new(tilo_settings_service::SettingsService::new()),
            ],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Run `frames` frames of the session.
fn step(session: &mut Session, frames: usize) {
    for _ in 0..frames {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Run frames until `done` holds; `false` when it never did.
fn reached(session: &mut Session, mut done: impl FnMut(&Session) -> bool) -> bool {
    common::reached(session, |session| done(session))
}

/// Set the field `field` of the appearance section to `value` through the section's own published
/// `field-edit` : what Settings' Appearance page and Quick Settings' Dark mode tile run, and what
/// an agent or a script runs.
fn set_field(session: &mut Session, field: &str, value: &Value) {
    let arguments: Map<String, Value> = json!({
        "edit": {"path": [{"field": field}], "op": "set", "value": value}
    })
    .as_object()
    .expect("an object")
    .clone();
    session
        .invoke(APPEARANCE, "field-edit", &arguments)
        .unwrap_or_else(|error| panic!("set {field}: {error:?}"));
}

/// How the session publishes the appearance section's field `field` .
fn published(session: &Session, field: &str) -> Value {
    session
        .read(APPEARANCE)
        .unwrap_or_else(|| panic!("the session publishes the appearance section"))
        .get(field)
        .cloned()
        .unwrap_or(Value::Null)
}

/// Choose `theme` the way a person does: the theme of its mode, then that mode.
fn choose_theme(session: &mut Session, theme: Theme) {
    let key = if theme.light {
        "light_theme"
    } else {
        "dark_theme"
    };
    set_field(session, key, &json!(theme.id));
    set_field(
        session,
        "mode",
        &json!(if theme.light { "light" } else { "dark" }),
    );
    // The section is what the shell reads its theme from; the shell has drawn the choice when the
    // session publishes it.
    assert!(
        reached(session, |session| {
            published(session, key) == json!(theme.id)
                && published(session, "mode") == json!(if theme.light { "light" } else { "dark" })
        }),
        "the appearance section holds {}: {:?}",
        theme.name,
        session.read(APPEARANCE)
    );
}

/// The resource of the type `ty` whose label is `label` , from `resources` .
fn find(resources: &[tilo_actions::ResourceSpec], ty: &str, label: &str) -> Option<String> {
    resources.iter().find_map(|resource| {
        if resource.kind == ty && resource.label == label {
            Some(resource.id.as_str().to_owned())
        } else {
            find(&resource.children, ty, label)
        }
    })
}

/// Place `resource` on the canvas of the workspace in front, at `(x, y)` and `width` by `height` ,
/// through the arrangement's published `place` : what an agent does when it puts something on the
/// canvas for the person.
fn place(session: &mut Session, resource: &str, x: i32, y: i32, width: i32, height: i32) {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": x,
        "y": y,
        "width": width,
        "height": height,
    })
    .as_object()
    .expect("an object")
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
}

/// The whole laid-out tree, in place: every window's node, as assistive technology reads it from
/// the session's own export.
fn laid_out(session: &Session) -> Vec<Node> {
    let mut out = Vec::new();
    for (_, _, tree) in session.a11y_windows() {
        for (_, node) in tree.nodes {
            out.push(Node {
                role: format!("{:?}", node.role()),
                label: node.label().map(str::to_owned),
                bounds: node
                    .bounds()
                    .map(|rect| [rect.x0, rect.y0, rect.x1, rect.y1]),
            });
        }
    }
    out
}

/// The frame the compositor draws for the whole output.
fn frame(session: &mut Session) -> Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the session draws its display")
}

/// How many of `a` 's and `b` 's pixels differ.
fn differing_pixels(a: &Screenshot, b: &Screenshot) -> usize {
    assert_eq!(
        (a.width, a.height),
        (b.width, b.height),
        "both frames are of one display"
    );
    a.rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .filter(|(left, right)| left != right)
        .count()
}

/// The names the tree says, in order: what the shell shows, without a theme's hand in it.
fn names(nodes: &[Node]) -> Vec<(String, &str)> {
    nodes
        .iter()
        .map(|node| (node.role.clone(), node.label.as_deref().unwrap_or("")))
        .collect()
}

/// The shell at rest with a note and the inbox on the canvas: the seeded apps published, both
/// places drawing their app, and the shell's views settled.
fn seated_session() -> Session {
    let mut session = session();
    step(&mut session, 200);

    // A note and the inbox on the canvas, at two places of their own, so the shell's own views and
    // two apps' views all stand on what is compared.
    let resources = session.resources();
    let note = find(&resources, "tl.so.tilo.lang.notes.Note", "Groceries").expect("a seeded note");
    let inbox =
        find(&resources, "tl.so.tilo.lang.mail.Mailbox", "Inbox").expect("the seeded inbox");
    place(&mut session, &note, 80, 60, 480, 360);
    place(&mut session, &inbox, 700, 60, 480, 360);
    assert!(
        reached(&mut session, |session| session.shell_apps().len() == 2),
        "both places show their app: {:?}",
        session.shell_apps()
    );
    assert!(
        reached(&mut session, |session| {
            session.shell_says("Groceries") && session.shell_says("Inbox")
        }),
        "both places stand on the canvas: {:?}",
        session.accessible_labels()
    );
    session
}

/// Choose `theme` , wait for the switch to settle, and read what the shell lays out and draws; then
/// prove the shell runs no view and draws no frame while it stays at rest.
fn settle_into(session: &mut Session, theme: Theme) -> (Vec<Node>, Screenshot) {
    choose_theme(session, theme);
    // The switch is a change of the theme every part is drawn in; once it has settled the shell is
    // at rest.
    step(session, 300);

    let tree = laid_out(session);
    assert!(
        tree.len() > 40,
        "{}: the shell lays out its whole screen: {} nodes",
        theme.name,
        tree.len()
    );
    let shot = frame(session);

    // Nothing is drawn while idle: two windows of frames each draw no frame and run no view.
    for window in 0..2 {
        let before = session.shell_work().expect("the shell runs");
        step(session, 200);
        let after = session.shell_work().expect("the shell runs");
        assert_eq!(
            (after.turns, after.drawn),
            (before.turns, before.drawn),
            "{}: idle window {window} runs the shell's views and draws no frame",
            theme.name
        );
    }
    (tree, shot)
}

/// The layout does not change: `theme` 's tree is `default` 's node for node — the same kinds in
/// the same order, the same names, the same heights, and the same boxes — except where a theme's
/// own `type` role moves a text run's width. A run set in a heavier weight measures wider; in a
/// packed row, what stands after it starts a pixel further along and keeps the row's own gaps.
/// Nothing else moves: a box a view lays out (a place, a strip, a panel, a heading's block) stands
/// exactly where the default theme stands it.
fn assert_same_layout(default: &[Node], theme: Theme, tree: &[Node]) {
    assert_eq!(
        names(tree),
        names(default),
        "{}: the shell lays out the same nodes, kinds and names as Default",
        theme.name
    );
    assert_eq!(
        tree.len(),
        default.len(),
        "{}: as many nodes as Default",
        theme.name
    );
    let mut moved = 0;
    for (index, (default, themed)) in default.iter().zip(tree).enumerate() {
        let [dx0, dy0, dx1, dy1] = default.floor();
        let [x0, y0, x1, y1] = themed.floor();
        // The vertical never moves: a text weight changes how wide a run measures, not its leading,
        // its row or its line.
        assert_eq!(
            (y0, y1),
            (dy0, dy1),
            "{}: node {index} ({:?}) keeps Default's line and height",
            theme.name,
            default.label
        );
        if (x0, x1) == (dx0, dx1) {
            continue;
        }
        // This node moved across. Only a theme whose `type` role moves text weights may move
        // anything at all.
        assert!(
            theme.moves_type_metrics(),
            "{}: node {index} ({:?}) stands at ({x0}, {x1}) where Default stands it at \
             ({dx0}, {dx1}); only a theme whose type role moves text weights may move a run, \
             and this theme's type role is the stock one",
            theme.name,
            default.label
        );
        moved += 1;
        // A run set in another weight measures a pixel or two wider or narrower at this text size:
        // never a step a view asked for.
        let own_delta = ((x1 - x0) - (dx1 - dx0)).abs();
        assert!(
            own_delta <= 3.0,
            "{}: node {index} ({:?}) measures {} where Default measures {}: a text weight \
             moves a run by a pixel or two, not by {own_delta}",
            theme.name,
            default.label,
            x1 - x0,
            dx1 - dx0
        );
        // A run pushed out by one before it in a packed row starts further along by what that one
        // took — a pixel or two a run, never a gap or a margin the view lays out.
        assert!(
            (-0.01..=6.0).contains(&(x0 - dx0)),
            "{}: node {index} ({:?}) starts {} px from Default's {dx0}",
            theme.name,
            default.label,
            x0 - dx0
        );
    }
    // Nothing but a theme's type role moves anything: a theme whose type role is the stock one lays
    // out Default's rectangles exactly.
    if theme.moves_type_metrics() {
        println!(
            "{}: {} nodes, {moved} moved across by its type role's weights, all heights and \
             boxes held",
            theme.name,
            tree.len()
        );
    } else {
        assert!(
            tree.iter()
                .zip(default)
                .all(|(themed, default)| themed.bounds == default.bounds),
            "{}: the shell lays out Default's rectangles exactly",
            theme.name
        );
        println!(
            "{}: {} nodes, the same rectangles as Default",
            theme.name,
            tree.len()
        );
    }
}

/// The look does change: every pair of themes draws a different frame.
fn assert_different_frames(frames: &[(Theme, Screenshot)]) {
    for (index, (theme, shot)) in frames.iter().enumerate() {
        for (other, theirs) in &frames[index + 1..] {
            let differing = differing_pixels(shot, theirs);
            assert!(
                differing > 1000,
                "{} and {} draw different frames; {differing} pixels differ",
                theme.name,
                other.name
            );
            println!(
                "{} vs {}: {differing} of {} pixels differ",
                theme.name,
                other.name,
                shot.rgba.len() / 4
            );
        }
    }
}

/// Acceptance step 9: switching the theme changes how the tilolang shell looks,
/// not its layout.
#[test]
fn switching_the_theme_changes_how_the_shell_looks_not_its_layout() {
    // This test's own home, beside the run's app store the session's apps come from.
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    // The bar draws the time, and a minute turning over is a change, not idle work: the test holds
    // the clock at the minute it runs in, as a capture does, so what it watches at rest is the
    // shell and nothing else.
    let at = ui::time::wall()
        .checked_sub(Duration::from_secs(
            u64::try_from(ui::clock::local_now().timestamp().rem_euclid(60))
                .expect("a second of the minute"),
        ))
        .expect("the minute the test runs in");
    ui::time::testing::with_wall(at, || {
        let mut session = seated_session();
        let read: Vec<(Theme, Vec<Node>, Screenshot)> = Theme::ALL
            .into_iter()
            .map(|theme| {
                let (tree, shot) = settle_into(&mut session, theme);
                (theme, tree, shot)
            })
            .collect();
        let (_, default, _) = &read[0];
        for (theme, tree, _) in &read[1..] {
            assert_same_layout(default, *theme, tree);
        }
        let frames: Vec<(Theme, Screenshot)> = read
            .into_iter()
            .map(|(theme, _, shot)| (theme, shot))
            .collect();
        assert_different_frames(&frames);
    });
}

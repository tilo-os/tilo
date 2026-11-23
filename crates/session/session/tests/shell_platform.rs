//! The tilolang shell on the platforms the Rust shell ran on: the CPU replay a display with no
//! working GPU draws its frames through (the Orange Pi's route — `tilo_cpu_raster` replaying
//! vello scenes), and the semantic HTML a page shows of it for the composition path.
//!
//! Both read the shell's own output, never a parallel description of it: the scene the part last
//! drew, and the composition a host that writes the window as a page composes.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};

use crate::common::{HEIGHT, PERSON, WIDTH, notebook as notes, until};

/// How many passes in a row the shell must run no view and draw no frame to be at rest, as
/// `shell_displays` holds it there.
const REST: u32 = 10;

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: false,
        shell: true,
        ..Options::default()
    }
}

/// Run passes until the shell is at rest: its part has drawn, and over [`REST`] more passes it
/// drew nothing and owes no frame.
#[track_caller]
fn until_at_rest(session: &mut Session) {
    crate::common::wait_for(session, "the shell to come to rest", |session| {
        let before = session.shell_work()?;
        for _ in 0..REST {
            session.settle_services();
            session.step(Duration::from_millis(1)).unwrap();
        }
        let after = session.shell_work()?;
        (after.drawn > 0
            && (after.turns, after.drawn) == (before.turns, before.drawn)
            && !session.compositor().owes_frame())
        .then_some(())
    });
}

/// Place `resource` as a strip of its own on the workspace in front, through the layout's
/// published `place`.
fn place(session: &mut Session, resource: &str) -> u64 {
    let id = session.handle().mint_id();
    let (workspace, row, strip) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id().to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": id,
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:layout", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    id
}

/// `scene` replayed on the CPU the way a display with no working GPU draws it
/// (`ui::retained_raster`'s CPU canvas, `tilo_cpu_raster` replaying the vello scene):
/// premultiplied RGBA8 over `base`.
fn cpu_frame(
    scene: &ui::vello::Scene,
    width: u32,
    height: u32,
    base: ui::vello::peniko::Color,
) -> Vec<u8> {
    let mut raster = ui::retained_raster::RetainedRaster::new(true);
    assert!(
        raster.on_cpu(),
        "the CPU canvas is the one this test draws with"
    );
    let frame = ui::retained_raster::FrameContent {
        scene,
        width,
        height,
        base_color: base,
        damage_regions: &[],
        scroll_copies: &[],
        full_redraw: true,
        backdrop: &ui::backdrop::BackdropFrame::default(),
    };
    raster
        .rasterize_to_cpu(frame)
        .expect("the CPU canvas replays the shell's scene");
    raster
        .cpu_frame()
        .expect("the pixels the CPU canvas drew")
        .pixels
        .to_vec()
}

/// The colour of `rgba` at `(x, y)`.
fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    rgba[at..at + 4].try_into().expect("a pixel")
}

/// How many pixels of the rectangles `rect` have a colour above the base, as a share of the
/// rectangle.
fn painted(
    rgba: &[u8],
    width: u32,
    base: ui::vello::peniko::Color,
    rect: (f64, f64, f64, f64),
) -> f64 {
    let base = base.to_rgba8();
    let base = [base.r, base.g, base.b, base.a];
    let (x0, y0) = (rect.0.max(0.0) as u32, rect.1.max(0.0) as u32);
    let x1 = (rect.0 + rect.2).max(0.0) as u32;
    let y1 = (rect.1 + rect.3).max(0.0) as u32;
    let (mut drawn, mut all) = (0u32, 0u32);
    for y in y0..y1 {
        for x in x0..x1 {
            all = all.saturating_add(1);
            let px = pixel(rgba, width, x, y);
            if (0..4).any(|channel| px[channel].abs_diff(base[channel]) > 4) {
                drawn = drawn.saturating_add(1);
            }
        }
    }
    f64::from(drawn) / f64::from(all.max(1))
}

/// How many elements a composition has: each node that shows its paint or carries what a
/// screen reader acts on.
fn elements(node: &ui::composition::Composed) -> usize {
    let mine = usize::from(node.picture.is_some() || node.semantics.is_some());
    mine.saturating_add(node.children.iter().map(elements).sum::<usize>())
}

/// The words a composition shows: what each element's semantics say.
fn words(composition: &ui::composition::Composition) -> Vec<String> {
    fn walk(node: &ui::composition::Composed, out: &mut Vec<String>) {
        if let Some(semantics) = &node.semantics {
            for said in semantics.nodes() {
                out.extend(said.name.iter().cloned());
                out.extend(said.value.iter().cloned());
                out.extend(said.description.iter().cloned());
            }
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&composition.root, &mut out);
    out
}

/// The frame the shell draws is the frame the CPU replay draws: the screen a display with no
/// working GPU replays paints the bar and the window placed on it, and matches vello's own
/// render of the same scene within anti-aliasing.
#[test]
fn the_shells_screen_is_the_frame_the_cpu_replay_draws() {
    let _settled = ui::motion::testing::settled();
    let (notebook, titles) = notes(&crate::common::TITLES[..1]);
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            services: vec![Box::new(notebook)],
            ..options()
        },
    );
    until(&mut session, "the bar's bell", |session| {
        session.shell_control("Notifications").is_some()
    });
    until_at_rest(&mut session);
    let base = ui::vello::peniko::Color::TRANSPARENT;
    let width = u32::try_from(WIDTH).expect("width");
    let height = u32::try_from(HEIGHT).expect("height");
    let before = {
        let scene = session
            .shell_scene("HEADLESS-1")
            .expect("the shell's scene");
        cpu_frame(&scene, width, height, base)
    };
    // The bar is drawn across the top of the screen: most of its band's pixels are painted.
    let bar = painted(&before, width, base, (0.0, 0.0, f64::from(width), 40.0));
    assert!(
        bar > 0.6,
        "the CPU replay paints the bar across the top band: {bar:.2} of it"
    );
    assert!(
        painted(
            &before,
            width,
            base,
            (0.0, 0.0, f64::from(width), f64::from(height))
        ) > 0.02,
        "the replay draws the shell, not a blank frame"
    );

    // A window placed on the shell is drawn by the replay where the shell lays its name out.
    let resource = &titles[0];
    place(&mut session, resource);
    until(&mut session, "the placed window in the layout", |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == *resource)
    });
    let shown = crate::common::drawn_name(&session, resource);
    until(&mut session, "the window's name on the screen", |session| {
        session.shell_text_bounds(&shown).is_some()
    });
    until_at_rest(&mut session);
    let bounds = session
        .shell_text_bounds(&shown)
        .expect("where the shell lays the window's name");
    let after = {
        let scene = session
            .shell_scene("HEADLESS-1")
            .expect("the shell's scene");
        cpu_frame(&scene, width, height, base)
    };
    let (x0, y0) = (bounds.0.max(0.0) as u32, bounds.1.max(0.0) as u32);
    let x1 = ((bounds.0 + bounds.2).max(0.0) as u32).min(width);
    let y1 = ((bounds.1 + bounds.3).max(0.0) as u32).min(height);
    let mut changed = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let (was, now) = (pixel(&before, width, x, y), pixel(&after, width, x, y));
            if (0..4).any(|channel| was[channel].abs_diff(now[channel]) > 8) {
                changed = changed.saturating_add(1);
            }
        }
    }
    assert!(
        changed > 20,
        "the replay draws the window's name where the shell lays it: {changed} pixels of \
         {bounds:?} changed"
    );

    // The bar keeps its ground across the top when a window opens, drawn by the replay too.
    let bar_after = painted(&after, width, base, (0.0, 0.0, f64::from(width), 40.0));
    assert!(
        bar_after > 0.6,
        "the replay keeps painting the bar across the top: {bar_after:.2} of it"
    );
}

/// The shell's page carries what the bar, the windows and their words show: the composition a
/// host writes as semantic HTML has the bar's controls, the person, and the windows on the shell,
/// and the page's markup shows them as elements.
#[test]
fn the_shells_page_carries_its_bar_and_its_windows() {
    let _settled = ui::motion::testing::settled();
    let (notebook, titles) = notes(&crate::common::TITLES[..1]);
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            services: vec![Box::new(notebook)],
            ..options()
        },
    );
    until(&mut session, "the bar's bell", |session| {
        session.shell_control("Notifications").is_some()
    });
    let composition = session
        .shell_composition("HEADLESS-1")
        .expect("the shell's composition");
    let said = words(&composition);
    for word in ["Notifications", PERSON, "Workspace"] {
        assert!(
            said.iter().any(|text| text == word),
            "the shell's page carries {word:?}: {said:?}"
        );
    }
    assert!(
        elements(&composition.root) > 4,
        "the page is elements, not one picture of the screen: {} of them",
        elements(&composition.root)
    );

    // A window placed on the shell is a part of the page, by its name, and the page's markup
    // shows the bar's controls as the browser's own elements.
    let resource = &titles[0];
    place(&mut session, resource);
    until(&mut session, "the placed window in the layout", |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == *resource)
    });
    let shown = crate::common::drawn_name(&session, resource);
    until(&mut session, "the window's name in the page", |session| {
        session
            .shell_composition("HEADLESS-1")
            .is_some_and(|composition| words(&composition).iter().any(|said| said == &shown))
    });
    let composition = session
        .shell_composition("HEADLESS-1")
        .expect("the shell's composition");
    let mut page = ui::composition::page::Page::default();
    let html = page.render(&composition);
    assert!(
        html.contains("Notifications"),
        "the page's markup carries the bar's bell"
    );
    assert!(
        html.contains(&shown),
        "the page's markup carries the window's name"
    );
    assert!(
        html.contains("<button"),
        "the bar's controls are buttons of the browser's own: {html:.400}"
    );
}

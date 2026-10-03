#![allow(
    clippy::exit,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Offscreen shots for the HIG §1 `NavShell` primitive.
//!
//! Renders the four canonical states — no window opened — so the layout can be
//! self-inspected against HIG §1:
//!   1. `nav_shell-wide.png`          — `[ sidebar | overview | detail ]` three
//!      abreast, docked sidebar + collapse chevron.
//!   2. `nav_shell-narrow-overview.png` — full-screen overview + hamburger.
//!   3. `nav_shell-narrow-drawer.png`   — sidebar overlay drawer over the overview,
//!      scrim behind, collapse chevron.
//!   4. `nav_shell-narrow-detail.png`   — detail slid over the overview + Back button.
//!
//! Run: `cargo run -p ui-examples --example ui-shots -- nav_shell_shots`
//! Output dir: `$NAV_SHELL_SHOTS_OUT` (default `/tmp/nav_shell_shots`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use tilo_ui::TextRole;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Column, Container, NavPane, NavShell, Row, Spacer, Text, Widget};

/// A titled list panel filled with `fill`, a header band, and rows. The header
/// title is indented by `header_indent` px so it clears the shell's top-left
/// affordance chip (which floats over whichever panel sits at the left edge).
fn panel(
    theme: &Theme,
    fill: Color,
    header_fill: Color,
    title: &str,
    header_indent: f32,
    rows: &[(&str, &str)],
) -> Box<dyn Widget> {
    let mut col = Column::new().with_no_padding();

    // Header band — a rigid leading spacer indents the title past the chip.
    let header = Container::new()
        .with_fill(header_fill)
        .with_height(56.0)
        .with_child(Box::new(
            Row::new()
                .push(Box::new(
                    Container::new().with_width(header_indent).with_height(1.0),
                ))
                .push(Box::new(
                    Text::new(title)
                        .with_role(TextRole::Title)
                        .with_color(theme.text_primary),
                )),
        ));
    col = col.push(Box::new(header));

    // Rows.
    for (primary, secondary) in rows {
        let mut row_col = Column::new().with_intrinsic_height();
        row_col = row_col.push(Box::new(
            Text::new(primary)
                .with_role(TextRole::Body)
                .with_color(theme.text_primary),
        ));
        if !secondary.is_empty() {
            row_col = row_col.push(Box::new(
                Text::new(secondary)
                    .with_role(TextRole::Caption)
                    .with_color(theme.text_dim),
            ));
        }
        let row = Container::new()
            .with_height(52.0)
            .with_child(Box::new(row_col));
        col = col.push(Box::new(row));
    }
    col = col.push(Box::new(Spacer::new()));

    Box::new(
        Container::new()
            .with_fill(fill)
            .filling()
            .with_child(Box::new(col)),
    )
}

fn make_shell(theme: &Theme) -> NavShell {
    let sidebar = panel(
        theme,
        theme.bg_app,
        theme.tokens.colors.surfaces.base,
        "Calendars",
        52.0,
        &[
            ("Personal", "12 events"),
            ("Work", "8 events"),
            ("Family", "3 events"),
            ("Holidays", "subscribed"),
        ],
    );
    let overview = panel(
        theme,
        theme.bg_primary,
        theme.bg_active,
        "September 2026",
        52.0,
        &[
            ("Team sync", "Mon 10:00"),
            ("Design review", "Tue 14:00"),
            ("1:1 with Sam", "Wed 09:30"),
            ("Release cut", "Thu 16:00"),
            ("Retro", "Fri 11:00"),
        ],
    );
    let detail = panel(
        theme,
        theme.bg_app,
        theme.tokens.colors.intent.inverted,
        "Design review",
        116.0,
        &[
            ("Tuesday, 8 September", "14:00 – 15:00"),
            ("Room", "Aurora / video"),
            ("Attendees", "Sam, Kai, Robin"),
            ("Notes", "Bring the NavShell shots"),
        ],
    );

    NavShell::new(overview, detail)
        .with_sidebar(sidebar)
        .with_breakpoint(700.0)
        .with_sidebar_width(240.0)
        .with_overview_width(320.0)
}

/// Widens a shot's pixel dimension (window w/h, always a small on-screen
/// size in practice) to `f32` for taffy's layout math.
#[allow(
    clippy::cast_precision_loss,
    reason = "shot dimensions are small fixed constants, far below f32's exact-integer range"
)]
fn to_f32(v: u32) -> f32 {
    v as f32
}

/// Lay `shell` out at `w×h`, apply `set` (drawer/pane state), then draw a
/// deterministic frame to `out`.
///
/// The `set`-then-`apply_layout` order matters: `apply_layout` is what
/// re-places the child widgets at the resolved rects, so any state that moves
/// the panes (open drawer, pushed detail) must be applied *before* the final
/// layout pass. `apply_layout` at an unchanged size is a no-op for the flap
/// reconcile (same breakpoint side), so it preserves the snapped state.
fn shoot(shell: &mut NavShell, w: u32, h: u32, out: &Path, set: impl Fn(&mut NavShell)) {
    let theme = shot_theme();
    let mut tr = TextRenderer::new();

    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = shell.build_layout(&mut taffy, &theme, &mut node_map);

    // First layout establishes the breakpoint side + default state; the second
    // re-places the children after `set` picks the drawer / pane state.
    for _ in 0..2 {
        let _ = taffy.compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(to_f32(w)),
                height: AvailableSpace::Definite(to_f32(h)),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        );
        shell.apply_layout(&taffy, root, Point::ZERO);
        set(shell);
    }
    // Settle every flap to its resolved endpoint so the single frame is
    // deterministic (a fresh sidebar's reveal defaults high; snap reconciles
    // it to the resolved open/closed state).
    shell.snap();

    let mut state = AutoCommit::new();
    shell.sync(&mut state, &automerge::ObjId::Root);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    shell.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let mut r = OffscreenRenderer::new(w, h);
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
}

pub fn run(_args: &[String]) {
    let dir =
        std::env::var("NAV_SHELL_SHOTS_OUT").unwrap_or_else(|_| "/tmp/nav_shell_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);
    let theme = shot_theme();

    // 1. WIDE: three abreast, docked sidebar (open), collapse chevron.
    let mut s = make_shell(&theme);
    shoot(&mut s, 1240, 780, &dir.join("nav_shell-wide.png"), |_| {});

    // 2. NARROW overview: full-screen overview, hamburger to summon the drawer.
    let mut s = make_shell(&theme);
    shoot(
        &mut s,
        440,
        780,
        &dir.join("nav_shell-narrow-overview.png"),
        |_| {},
    );

    // 3. NARROW drawer: overlay sidebar open over the overview, scrim + chevron.
    let mut s = make_shell(&theme);
    shoot(
        &mut s,
        440,
        780,
        &dir.join("nav_shell-narrow-drawer.png"),
        |sh| {
            sh.open_sidebar();
            sh.snap_sidebar(true);
        },
    );

    // 4. NARROW detail: detail slid over the overview, Back button top-left.
    let mut s = make_shell(&theme);
    shoot(
        &mut s,
        440,
        780,
        &dir.join("nav_shell-narrow-detail.png"),
        |sh| {
            sh.snap_pane(NavPane::Detail);
        },
    );
}

/// `TILO_SHOTS_THEME` picks the palette (`dark` by default) and
/// `TILO_SHOTS_TEXT_SCALE` the text scale — the switches every app's `--shots`
/// honours through `tilo_ui::launch::shot_theme`, whose `LaunchArgs` an example
/// cannot construct. An unknown theme name is an error, never a silent dark
/// shot: a capture that cannot observe its subject must fail.
fn shot_theme() -> Theme {
    tilo_ui::launch::shot_theme_from_env().unwrap_or_else(|error| {
        eprintln!("ui-shots: {error}");
        std::process::exit(2)
    })
}

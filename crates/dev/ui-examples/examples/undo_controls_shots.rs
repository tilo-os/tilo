//! Offscreen renders of the [`UndoButton`] pair in every availability state,
//! in **both** light and dark, for self-inspection without opening a window.
//!
//! Drives the real pipeline (`sync` → `build_layout` → `compute_layout` →
//! `apply_layout` → `draw`) over a real [`State`] whose history was built by real
//! committed actions, and publishes the status through the same
//! `State::undo_status` → `UndoControl::set_undo_status` route the host uses — so
//! what is painted here is what a user sees.
//!
//! Run: `cargo run -p ui-examples --example ui-shots -- undo_controls_shots`
//! Out: `$UNDO_SHOTS_OUT` (default `/tmp/undo_controls_shots`) —
//!       undo-{dark,light}-{empty,undoable,redoable,hovered}.png
//!
//!   * `empty`    — a fresh document: both controls dimmed and inert.
//!   * `undoable` — one action performed: Undo live, Redo still dimmed.
//!   * `redoable` — that action undone: Undo dimmed, Redo live.
//!   * `hovered`  — pointer on the icon Undo, keyboard focus on the labelled
//!     one: the two interactive states, both on controls that can actually
//!     act (a control the host would never focus must never look focused).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{ObjType, ROOT, transaction::Transactable};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::State;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::permissions::UserId;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Row, UndoButton, Widget};

const WIDTH: u32 = 300;
const HEIGHT: u32 = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Empty,
    Undoable,
    Redoable,
    Hovered,
}

fn author() -> UserId {
    UserId("shots".into())
}

/// One real edit, committed as one action — so the history under test is a
/// history, not a hand-built status struct.
fn type_action(state: &mut State, text: &str) -> Result<(), String> {
    let obj = state
        .doc
        .put_object(&ROOT, "body", ObjType::Text)
        .map_err(|error| format!("cannot create body: {error}"))?;
    state
        .doc
        .splice_text(&obj, 0, 0, text)
        .map_err(|error| format!("cannot seed body: {error}"))?;
    if !state.commit_action_authored(None, None, Some(author())) {
        return Err("seed edit did not create an undo entry".to_owned());
    }
    Ok(())
}

fn cursor_at(p: Point) -> winit::event::WindowEvent {
    winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn shoot(theme: &Theme, out: &Path, case: Case) -> Result<(), String> {
    let mut state = State::new();
    match case {
        Case::Empty => {}
        Case::Undoable | Case::Hovered => type_action(&mut state, "hello")?,
        Case::Redoable => {
            type_action(&mut state, "hello")?;
            if state.undo_as(Some(&author()), None, None, None).is_none() {
                return Err("seed edit could not be undone".to_owned());
            }
        }
    }

    // Icon-only pair on the left, a labelled Undo on the right so both looks
    // are visible in one frame.
    let mut row = Row::new()
        .push(Box::new(UndoButton::undo()))
        .push(Box::new(UndoButton::redo()))
        .push(Box::new(UndoButton::undo().with_label("Undo")));

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    row.sync(&state.doc, &state.root);
    let node = row.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(300.0),
                height: AvailableSpace::Definite(64.0),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .map_err(|error| format!("layout failed: {error}"))?;
    row.apply_layout(&taffy, node, Point::new(16.0, 16.0));

    // The host's publish: derive the status from the live history and hand it
    // to every laid-out widget by sweeping `node_map`.
    let status = state.undo_status(Some(&author()));
    for &w_ptr in node_map.values() {
        unsafe { tilo_ui::undo::set_undo_status(&mut (*w_ptr), &mut || status.clone()) };
    }
    println!(
        "  {:>8}  can_undo={} can_redo={} undo={:?}",
        out.display(),
        status.can_undo,
        status.can_redo,
        status.undo_summary
    );

    if case == Case::Hovered {
        // The controls left to right. Selected by widget name, not by rect
        // size: the enclosing `Row` registers itself in `node_map` too, and
        // picking it would drop the pointer in the middle of the strip and
        // hover whichever control happened to sit there.
        let mut targets: Vec<vello::kurbo::Rect> = node_map
            .values()
            .filter(|&&p| unsafe { (*p).name() } == "UndoButton")
            .map(|&p| unsafe { (*p).get_rect() })
            .collect();
        targets.sort_by(|a, b| a.x0.total_cmp(&b.x0));

        // Pointer on the icon-only Undo — the real hover path.
        if let Some(first) = targets.first() {
            let mut handled = false;
            let mut ft = None;
            row.handle_event(
                &cursor_at(first.center()),
                &mut tilo_ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: tilo_ui::widget::Edits::lend(&mut state.doc),
                    path: &state.root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
        }
        // Keyboard focus on the labelled Undo, which is equally available.
        if let Some(last) = targets.last() {
            for &w_ptr in node_map.values() {
                let w = unsafe { &mut *w_ptr };
                if w.get_rect() == *last {
                    w.set_focus(true);
                }
            }
        }
    }

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    row.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
    Ok(())
}

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("UNDO_SHOTS_OUT").unwrap_or_else(|_| "/tmp/undo_controls_shots".to_string());
    if let Err(error) = std::fs::create_dir_all(&dir) {
        crate::registry::fatal(format_args!("cannot create {dir}: {error}"));
    }
    let dir = PathBuf::from(dir);

    for (theme, tag) in [(Theme::dark(), "dark"), (Theme::light(), "light")] {
        for (case, name) in [
            (Case::Empty, "empty"),
            (Case::Undoable, "undoable"),
            (Case::Redoable, "redoable"),
            (Case::Hovered, "hovered"),
        ] {
            let out = dir.join(format!("undo-{tag}-{name}.png"));
            if let Err(error) = shoot(&theme, &out, case) {
                crate::registry::fatal(format_args!("{}: {error}", out.display()));
            }
        }
    }
}

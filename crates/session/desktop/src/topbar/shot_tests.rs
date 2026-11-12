//! The bar's right end as a person sees it: on a page (the bell alone) and on a machine (its
//! readings too), light and dark, alone, with others and an agent, and phone-wide. Each case is
//! written as a PNG when `TILO_SURFACE_SHOTS` names a directory, and always held to what the bar
//! promises: this person appears once, nothing reserves room it leaves empty, and the bar's
//! glyph controls share one size.
use super::fixture::FixtureChrome;
use super::*;
use crate::chrome::AvatarAsk;
use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceSlot};
use ui::widget::DrawChild;

/// This person, as their own avatar names them.
const OWN: &str = "me-root";

struct Case {
    name: &'static str,
    width: f32,
    dark: bool,
    native: bool,
    others: &'static [(&'static str, &'static str, bool)],
    /// The ask about keeping who they are, open on their avatar, as the page puts it.
    ask: bool,
}

/// What the page asks the person, in the person's own words.
const ASK_QUESTION: &str = "Keep your name and work?";

/// The ask the page puts on the person's avatar: its words, its body and its answer.
fn ask() -> AvatarAsk {
    AvatarAsk {
        question: ASK_QUESTION.into(),
        detail: "A passkey on this device brings you back as yourself, on any browser. Any time \
                 later: Ctrl+K, “Keep your name and work”."
            .into(),
        answer: "Keep with passkey".into(),
        open: true,
    }
}

fn tree(others: &[(&str, &str, bool)]) -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Studio");
    // The host lists this person among who is here too, as a roster of everyone does.
    workspace.collaborators = std::iter::once(Collaborator::new(OWN, "Jo Doe"))
        .chain(others.iter().map(|(id, name, agent)| {
            let row = Collaborator::new(*id, *name);
            if *agent { row.with_role("agent") } else { row }
        }))
        .collect();
    DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    }
}

fn draw(case: &Case) -> (TopBarChrome, usize) {
    let theme = if case.dark {
        Theme::dark()
    } else {
        Theme::light()
    };
    let tree = tree(case.others);
    let chrome = FixtureChrome::new(tree.clone());
    {
        let mut state = chrome.borrow_mut();
        state.can_invite = true;
        state.own = Some(crate::chrome::OwnChip {
            name: "Jo Doe".into(),
            rgb: (247, 133, 125),
            identity: OWN.into(),
        });
        state.notifications_only = !case.native;
        if case.ask {
            state.ask = Some(ask());
        }
        if case.native {
            state.status = crate::chrome::SystemStatus {
                battery_percent: Some(76),
                link: Some(crate::chrome::LinkStatus {
                    kind: crate::chrome::LinkKind::Wifi,
                    name: Some("Studio".into()),
                    signal: 3,
                }),
                volume: Some(crate::chrome::VolumeState {
                    level: 0.6,
                    muted: false,
                }),
                ..crate::chrome::SystemStatus::default()
            };
        }
    }
    let mut bar = TopBarChrome::from_tree(&tree)
        .with_chrome(chrome)
        .with_tilo_mark();
    bar.sync_from_tree(&tree);
    let mut text = TextRenderer::new();
    super::tests::lay_out_and_draw_with_text(&mut bar, case.width, &theme, &mut text);
    let mut scene = Scene::new();
    bar.draw_child(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::default(),
    );
    // The glyphs the ask's words set, counted where the question starts: a question cut with an
    // ellipsis sets fewer glyphs than the whole one, and no pill at all leaves none.
    let question_glyphs = if case.ask && !bar.avatar_ask_rect().is_zero_area() {
        let pill = bar.avatar_ask_rect();
        let pad = f64::from(theme.tokens.spacing.s2);
        let caption = theme.tokens.typography.size(TextRole::Caption);
        glyphs_from(&scene, pill, pill.x0 + pad, caption)
    } else {
        0
    };
    if let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&directory).expect("the directory for shots");
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bar's width and height are small whole pixels"
        )]
        let (w, h) = (case.width as u32, bar.rect.y1.ceil() as u32);
        ui::offscreen::OffscreenRenderer::new(w, h).render_png(
            &scene,
            theme.tokens.colors.surfaces.base,
            directory.join(format!("{}.png", case.name)),
        );
    }
    (bar, question_glyphs)
}

/// The glyphs painted for the words that start at `x` within `pill`, in `size`: the words a person
/// reading them there sees, whole or cut short, and none where nothing stands there.
pub(super) fn glyphs_from(scene: &Scene, pill: Rect, x: f64, size: f32) -> usize {
    scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .filter(|run| {
            let [tx, ty] = run.transform.translation;
            (run.font_size - size).abs() < 0.01
                && (f64::from(tx) - x).abs() < 0.5
                && (f64::from(ty) - pill.center().y).abs() <= pill.height() / 2.0 + 1.0
        })
        .map(|run| run.glyphs.len())
        .sum()
}

/// The glyphs `question` sets when it paints whole, at any room it likes.
pub(super) fn whole_question_glyphs(theme: &Theme, question: &str) -> usize {
    let mut text = TextRenderer::new();
    let mut scene = Scene::new();
    text.draw_style(
        &mut scene,
        question,
        theme.tokens.colors.text_dim,
        Affine::IDENTITY,
        None,
        &theme.text(TextRole::Caption),
    );
    scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| run.glyphs.len())
        .sum()
}

const CASES: &[Case] = &[
    Case {
        name: "web-alone-dark",
        width: 1280.0,
        dark: true,
        native: false,
        others: &[],
        ask: false,
    },
    Case {
        name: "web-alone-light",
        width: 1280.0,
        dark: false,
        native: false,
        others: &[],
        ask: false,
    },
    Case {
        name: "web-others-agent-dark",
        width: 1280.0,
        dark: true,
        native: false,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
        ask: false,
    },
    Case {
        name: "web-crowd-light",
        width: 1280.0,
        dark: false,
        native: false,
        others: &[
            ("ada", "Ada Lovelace", false),
            ("bo", "Bo Brave", false),
            ("cy", "Cy Young", false),
            ("cc", "Claude Code", true),
            ("di", "Di Lane", false),
        ],
        ask: false,
    },
    Case {
        name: "web-phone-alone-dark",
        width: 390.0,
        dark: true,
        native: false,
        others: &[],
        ask: false,
    },
    Case {
        name: "web-phone-others-dark",
        width: 390.0,
        dark: true,
        native: false,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
        ask: false,
    },
    Case {
        name: "native-alone-dark",
        width: 1280.0,
        dark: true,
        native: true,
        others: &[],
        ask: false,
    },
    Case {
        name: "native-others-light",
        width: 1280.0,
        dark: false,
        native: true,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
        ask: false,
    },
    Case {
        name: "web-ask-dark",
        width: 1280.0,
        dark: true,
        native: false,
        others: &[],
        ask: true,
    },
    Case {
        name: "web-ask-light",
        width: 1280.0,
        dark: false,
        native: false,
        others: &[],
        ask: true,
    },
    Case {
        name: "web-others-ask-light",
        width: 1280.0,
        dark: false,
        native: false,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
        ask: true,
    },
    Case {
        name: "web-phone-ask-dark",
        width: 390.0,
        dark: true,
        native: false,
        others: &[],
        ask: true,
    },
    Case {
        name: "web-phone-ask-light",
        width: 390.0,
        dark: false,
        native: false,
        others: &[],
        ask: true,
    },
];

#[test]
fn the_right_end_shows_each_person_once_flush_and_in_one_icon_size() {
    let theme = Theme::dark();
    // Every case is drawn (and written) before any is judged, so a run that fails still leaves
    // every shot to look at.
    let whole = whole_question_glyphs(&theme, ASK_QUESTION);
    let bars: Vec<(TopBarChrome, usize)> = CASES.iter().map(draw).collect();
    for (case, (bar, question_glyphs)) in CASES.iter().zip(bars) {
        let name = case.name;
        // The ask's question reads whole wherever the ask stands on the bar, never as an elided
        // "Keep your name an…": a bar too narrow for it leaves the ask as the face's dot.
        if case.ask {
            if bar.avatar_ask_rect().is_zero_area() {
                assert!(
                    !bar.own_avatar_rect().is_zero_area(),
                    "{name}: with no room for the question, the ask waits on the face"
                );
            } else {
                assert_eq!(
                    question_glyphs, whole,
                    "{name}: the question stands whole in its pill, not cut short"
                );
            }
        }
        // This person appears once: their own avatar, never also among the faces.
        assert!(bar.face_rect(OWN).is_none(), "{name}: not among the others");
        assert!(
            !bar.own_avatar_rect().is_zero_area(),
            "{name}: their avatar stands"
        );
        assert!(
            !bar.collaborator_ids.iter().any(|id| id == OWN),
            "{name}: counted once"
        );
        // The faces open who is here: no second people button beside them.
        if !bar.face_rects.is_empty() {
            assert!(
                bar.people_button_rect().is_zero_area(),
                "{name}: no people button beside the faces"
            );
            for face in &bar.face_rects {
                assert!(
                    (face.width() - bar.own_avatar_rect().width()).abs() < 0.01,
                    "{name}: one size for every face"
                );
            }
        }
        // No reserved seat left empty: the status cluster is its slots and its pad, nothing
        // more, and it ends at the bar's edge pad.
        let pill = bar.tray_pill_rect();
        let pad = f64::from(theme.tokens.spacing.rhythm.tight);
        let slots = bar.tray_slots().len();
        let mut room =
            2.0 * pad + f64::from(u32::try_from(slots).unwrap_or(u32::MAX)) * BAR_CONTROL_PX;
        if bar.status.battery_percent().is_some() {
            room += 40.0;
        }
        assert!(
            pill.width() <= room,
            "{name}: the status cluster {pill:?} reserves no empty room ({slots} slots)"
        );
        assert!(
            (bar.rect.x1 - pill.x1 - f64::from(theme.tokens.spacing.s3)).abs() < 0.5,
            "{name}: the right group sits flush at the bar's edge pad"
        );
        // The avatar stands the bar's one control gap before the notifications.
        let gap = pill.x0 - bar.own_avatar_rect().x1;
        assert!(
            gap <= f64::from(theme.tokens.spacing.s2) + ui::presence::Face::REACH + 0.5,
            "{name}: the avatar stands one gap before the notifications, not {gap}"
        );
    }
}

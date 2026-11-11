//! The bar's right end as a person sees it: on a page (the bell alone) and on a machine (its
//! readings too), light and dark, alone, with others and an agent, and phone-wide. Each case is
//! written as a PNG when `TILO_SURFACE_SHOTS` names a directory, and always held to what the bar
//! promises: this person appears once, nothing reserves room it leaves empty, and the bar's
//! glyph controls share one size.
use super::fixture::FixtureChrome;
use super::*;
use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceSlot};

/// This person, as their own avatar names them.
const OWN: &str = "me-root";

struct Case {
    name: &'static str,
    width: f32,
    dark: bool,
    native: bool,
    others: &'static [(&'static str, &'static str, bool)],
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

fn draw(case: &Case) -> TopBarChrome {
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
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::default(),
    );
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
    bar
}

const CASES: &[Case] = &[
    Case {
        name: "web-alone-dark",
        width: 1280.0,
        dark: true,
        native: false,
        others: &[],
    },
    Case {
        name: "web-alone-light",
        width: 1280.0,
        dark: false,
        native: false,
        others: &[],
    },
    Case {
        name: "web-others-agent-dark",
        width: 1280.0,
        dark: true,
        native: false,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
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
    },
    Case {
        name: "web-phone-alone-dark",
        width: 390.0,
        dark: true,
        native: false,
        others: &[],
    },
    Case {
        name: "web-phone-others-dark",
        width: 390.0,
        dark: true,
        native: false,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
    },
    Case {
        name: "native-alone-dark",
        width: 1280.0,
        dark: true,
        native: true,
        others: &[],
    },
    Case {
        name: "native-others-light",
        width: 1280.0,
        dark: false,
        native: true,
        others: &[("ada", "Ada Lovelace", false), ("cc", "Claude Code", true)],
    },
];

#[test]
fn the_right_end_shows_each_person_once_flush_and_in_one_icon_size() {
    let theme = Theme::dark();
    // Every case is drawn (and written) before any is judged, so a run that fails still leaves
    // every shot to look at.
    let bars: Vec<TopBarChrome> = CASES.iter().map(draw).collect();
    for (case, bar) in CASES.iter().zip(bars) {
        let name = case.name;
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

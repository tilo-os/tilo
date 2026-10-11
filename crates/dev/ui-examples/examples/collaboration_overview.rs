//! Interactive and headless visual fixture for the universal collaboration rail.

use collab_types::{ArtifactKind, OpClass};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use tilo_actions::ActionContext;
use tilo_ui::num::f32_of_u32;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::permissions::UserId;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::{
    ActivityDigest, App, CollaborationExperience, CollaborationOverview, HistoryAuthor,
    HistoryEntry, HistoryEntryId, LiveStatus, PersonPresence, PresenceCtx, SemanticActivity, State,
    StatusOrigin, Theme, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

fn entry(id: u8, author: &str, at: u64, summary: &str) -> HistoryEntry {
    let mut bytes = [0; 16];
    bytes[0] = id;
    HistoryEntry {
        id: HistoryEntryId(bytes),
        author: HistoryAuthor::new(author, f32::from(id) * 61.0, format!("person-{id}")),
        op_class: OpClass::Splice,
        artifact_kind: ArtifactKind::File,
        timestamp: SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(at))
            .unwrap_or(SystemTime::UNIX_EPOCH),
        summary: summary.into(),
        text_delta: Some((12, 2)),
        diff_payload: None,
        redaction: None,
        context: Some(
            ActionContext::for_resource(
                "Edit",
                "finance:q2",
                "finance.report",
                "Q2 finance report",
            )
            .with_app("org.tilo.Sheets", "Sheets")
            .with_group("q2-close", "Prepare Q2 close", "project"),
        ),
    }
}

fn experience() -> CollaborationExperience {
    let now = tilo_ui::time::wall();
    let mut x = CollaborationExperience::new();
    x.upsert_person(PersonPresence {
        id: UserId("me".into()),
        name: "ada".into(),
        is_self: true,
        is_agent: false,
        current: None,
        status: Some(LiveStatus {
            text: "Getting the Q2 close ready for review".into(),
            origin: StatusOrigin::Manual,
            updated_at: now,
        }),
    });
    x.upsert_person(PersonPresence {
        id: UserId("bob".into()),
        name: "bob".into(),
        is_self: false,
        is_agent: false,
        current: Some(SemanticActivity {
            action: "Editing revenue assumptions".into(),
            app: "Sheets".into(),
            resource: Some("Q2 finance report".into()),
            occurred_at: now,
        }),
        status: Some(LiveStatus {
            text: "Reconciling forecast against actuals".into(),
            origin: StatusOrigin::Inferred {
                generator: "local".into(),
            },
            updated_at: now,
        }),
    });
    x.upsert_person(PersonPresence {
        id: UserId("maren".into()),
        name: "Maren Okafor".into(),
        is_self: false,
        is_agent: false,
        current: Some(SemanticActivity {
            action: "Reviewing the executive summary".into(),
            app: "Writer".into(),
            resource: Some("Board update".into()),
            occurred_at: now,
        }),
        status: None,
    });
    x.upsert_person(PersonPresence {
        id: UserId("agent".into()),
        name: "Research agent".into(),
        is_self: false,
        is_agent: true,
        current: Some(SemanticActivity {
            action: "Checking cited market figures".into(),
            app: "Browser".into(),
            resource: None,
            occurred_at: now,
        }),
        status: Some(LiveStatus {
            text: "Verifying sources for the board narrative".into(),
            origin: StatusOrigin::Inferred {
                generator: "local".into(),
            },
            updated_at: now,
        }),
    });
    x
}

fn widget() -> Box<dyn Widget> {
    let entries = vec![
        entry(1, "bob", 100, "Updated revenue assumptions"),
        entry(2, "ada", 110, "Reconciled operating costs"),
        entry(3, "Maren Okafor", 120, "Tightened the executive summary"),
    ];
    Box::new(
        CollaborationOverview::new(experience()).with_digest(ActivityDigest::from_entries(
            &entries,
            SystemTime::UNIX_EPOCH,
            3,
        )),
    )
}

pub fn run(args: &[String]) {
    let shot = args
        .iter()
        .position(|a| a == "--shots")
        .and_then(|i| args.get(i.saturating_add(1)));
    if let Some(path) = shot {
        render(widget(), Path::new(path));
    } else {
        App::new(State::new(), widget()).run();
    }
}

fn render(mut widget: Box<dyn Widget>, out: &Path) {
    let (w, h) = (420, 760);
    let state = State::new();
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    widget.sync(&state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut map);
    let Ok(root) = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(f32_of_u32(w)),
                height: Dimension::length(f32_of_u32(h)),
            },
            ..Default::default()
        },
        &[child],
    ) else {
        eprintln!("could not build collaboration fixture layout");
        return;
    };
    let measure = map.clone();
    if taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(f32_of_u32(w)),
                height: AvailableSpace::Definite(f32_of_u32(h)),
            },
            |known, avail, id, _, _| {
                if let Some(&ptr) = measure.get(&id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .is_err()
    {
        eprintln!("could not measure collaboration fixture layout");
        return;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    widget.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OffscreenRenderer::new(w, h).render_png(&scene, Color::from_rgb8(0x14, 0x16, 0x1c), out);
}

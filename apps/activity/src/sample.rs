//! What the screenshot scenarios show: a small workspace's afternoon, as
//! the session would serve it. A capture reaches no session; these are the
//! answers its reads get instead. Never written into a real workspace.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;

use crate::{Filter, Nav, Place};

pub(crate) const SCENARIOS: &[&str] = &["whats-new", "history", "detail", "replay", "caught-up"];

const MINUTE_MS: i64 = 60_000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// When it was `minutes` ago, in milliseconds since the epoch.
fn minutes_ago_ms(minutes: i64) -> i64 {
    now_ms().saturating_sub(minutes.saturating_mul(MINUTE_MS))
}

fn person(name: &str, rgb: [u8; 3]) -> wire::Author {
    wire::Author {
        name: name.to_owned(),
        rgb,
        identity: format!("person:{}", name.to_lowercase()),
        relation: wire::Relation::Person,
        on_behalf_of: None,
    }
}

fn agent(name: &str, person: &str) -> wire::Author {
    wire::Author {
        name: name.to_owned(),
        rgb: [120, 110, 220],
        identity: format!("agent:{}", name.to_lowercase()),
        relation: wire::Relation::Software,
        on_behalf_of: Some(person.to_owned()),
    }
}

/// The id of the `n`th change, as the history names it.
fn id(n: u8) -> String {
    wire::entry_hex(&[n; 16])
}

fn entry(
    n: u8,
    author: wire::Author,
    minutes_ago: i64,
    summary: &str,
    resource: &str,
) -> wire::Entry {
    wire::Entry {
        id: id(n),
        author,
        kind: wire::Kind::Text,
        at_ms: minutes_ago_ms(minutes_ago),
        summary: summary.to_owned(),
        workspace: Some("Launch".to_owned()),
        app: Some("Notes".to_owned()),
        resource: Some(resource.to_owned()),
        group: (n <= 3).then(|| "press".to_owned()),
        redacted: false,
    }
}

fn feed() -> wire::Feed {
    let ada = person("Ada", [214, 96, 77]);
    let grace = person("Grace", [70, 150, 110]);
    let entries = vec![
        entry(
            1,
            ada.clone(),
            4,
            "Rewrote the press release intro",
            "Press release",
        ),
        entry(
            2,
            agent("Claude", "Ada"),
            9,
            "Tightened the quotes",
            "Press release",
        ),
        entry(3, ada.clone(), 15, "Added the launch date", "Press release"),
        entry(
            4,
            grace.clone(),
            70,
            "Moved the venue to the river hall",
            "Plan",
        ),
        entry(5, grace, 95, "Checked off the catering", "Checklist"),
        entry(6, ada, 26 * 60, "Created the launch plan", "Plan"),
    ];
    wire::Feed {
        workspace: "Launch".to_owned(),
        scope: wire::Scope::ThisWorkspace,
        total: entries.len(),
        entries,
        groups: vec![wire::Group {
            id: "press".to_owned(),
            label: "Press release pass".to_owned(),
            start: id(3),
            end: Some(id(1)),
            named_by: Some("Ada".to_owned()),
        }],
        people: vec!["Ada".to_owned(), "Claude".to_owned(), "Grace".to_owned()],
        lenses: vec![
            wire::Lens {
                key: "app:so.tilo.notes".to_owned(),
                label: "Notes".to_owned(),
            },
            wire::Lens {
                key: "agent-tasks".to_owned(),
                label: "Agent tasks".to_owned(),
            },
        ],
        pending: false,
        mutations: true,
    }
}

fn summary(caught_up: bool) -> wire::Summary {
    let ada = person("Ada", [214, 96, 77]);
    let grace = person("Grace", [70, 150, 110]);
    let briefs = if caught_up {
        Vec::new()
    } else {
        vec![
            wire::Brief {
                title: "Ada and Claude reworked the press release".to_owned(),
                detail: "A new intro, tighter quotes and the launch date".to_owned(),
                authors: vec![ada.clone(), agent("Claude", "Ada")],
                apps: vec!["Notes".to_owned()],
                resources: vec!["Press release".to_owned()],
                changes: 3,
                actions: Vec::new(),
                first_ms: minutes_ago_ms(15),
                last_ms: minutes_ago_ms(4),
                group: Some("press".to_owned()),
                work_session: None,
                entries: vec![id(3), id(2), id(1)],
            },
            wire::Brief {
                title: "Grace moved the venue to the river hall".to_owned(),
                detail: "And checked off the catering".to_owned(),
                authors: vec![grace],
                apps: vec!["Notes".to_owned()],
                resources: vec!["Plan".to_owned(), "Checklist".to_owned()],
                changes: 2,
                actions: Vec::new(),
                first_ms: minutes_ago_ms(95),
                last_ms: minutes_ago_ms(70),
                group: None,
                work_session: None,
                entries: vec![id(5), id(4)],
            },
        ]
    };
    wire::Summary {
        workspace: "Launch".to_owned(),
        briefs,
        narrative: (!caught_up).then(|| {
            "The press release is nearly done; the venue changed, so the invitations need the new address."
                .to_owned()
        }),
        resume: Some(wire::Resume {
            target: wire::Target {
                app_id: Some("so.tilo.notes".to_owned()),
                app_label: Some("Notes".to_owned()),
                id: "press-release".to_owned(),
                kind: "notes.page".to_owned(),
                label: "Press release".to_owned(),
            },
            author: ada,
            app: Some("Notes".to_owned()),
            at_ms: minutes_ago_ms(1560),
        }),
        away: !caught_up,
        sources_available: true,
        sources: Vec::new(),
        pending: false,
    }
}

/// The ledger's outcomes beside the workspace's changes: what this device
/// did that needed nobody.
fn seed_ledger() {
    let owner = |app: &str, name: &str| {
        let icon = match name {
            "Print centre" => ui::icons::Icon::Printer,
            "Store" => ui::icons::Icon::Download,
            _ => ui::icons::Icon::Folder,
        };
        ui::attention::Owner::new(app, name).with_icon(icon)
    };
    for (app, name, what) in [
        (
            "so.tilo.print-centre",
            "Print centre",
            "Printed 3 pages of Travel itinerary",
        ),
        ("so.tilo.store", "Store", "Installed LibreOffice 7.6"),
        ("so.tilo.files", "Files", "Moved 3 files to Trash"),
    ] {
        ui::attention::publish(ui::attention::Notice::did(owner(app, name), what));
    }
}

fn detail() -> wire::Detail {
    wire::Detail {
        title: "Press release pass".to_owned(),
        provenance: "3 changes by Ada and Claude, 15 minutes ago to 4 minutes ago".to_owned(),
        authors: vec!["Ada".to_owned(), "Claude".to_owned()],
        resources: vec![wire::Target {
            app_id: Some("so.tilo.notes".to_owned()),
            app_label: Some("Notes".to_owned()),
            id: "press-release".to_owned(),
            kind: "notes.page".to_owned(),
            label: "Press release".to_owned(),
        }],
        group: Some("press".to_owned()),
        group_label: Some("Press release pass".to_owned()),
        offers: [
            (wire::DetailAction::Open, "Open"),
            (wire::DetailAction::Compare, "Compare"),
            (wire::DetailAction::Undo, "Undo"),
            (wire::DetailAction::Restore, "Restore"),
        ]
        .into_iter()
        .map(|(action, label)| wire::Offer {
            action,
            label: label.to_owned(),
        })
        .collect(),
        run: None,
        comparison: Some(wire::Comparison::Changes {
            lines: vec![
                "Edited text".to_owned(),
                "Updated launch date".to_owned(),
                "Added 2 paragraphs".to_owned(),
            ],
        }),
        changes: vec![
            wire::FieldChange {
                field: "Launch date".to_owned(),
                before: "14 May".to_owned(),
                after: "21 May".to_owned(),
            },
            wire::FieldChange {
                field: "Title".to_owned(),
                before: "Press release".to_owned(),
                after: "Press release, final".to_owned(),
            },
        ],
        gone: false,
    }
}

fn replay() -> wire::Replay {
    let frame = |n: u8, minutes_ago: i64, summary: &str, tabs: &[&str]| wire::Frame {
        entry: id(n),
        author: person("Ada", [214, 96, 77]),
        summary: summary.to_owned(),
        at_ms: minutes_ago_ms(minutes_ago),
        workspaces: vec![wire::ReplayWorkspace {
            name: "Launch".to_owned(),
            groups: vec![
                wire::ReplayGroup {
                    tabs: tabs.iter().map(|tab| (*tab).to_owned()).collect(),
                    active: 0,
                    width_permille: 600,
                },
                wire::ReplayGroup {
                    tabs: vec!["Plan".to_owned()],
                    active: 0,
                    width_permille: 400,
                },
            ],
        }],
        pointers: vec![wire::ReplayPointer {
            name: "Grace".to_owned(),
            rgb: [70, 150, 110],
        }],
    };
    wire::Replay {
        frames: vec![
            frame(3, 15, "Opened the press release", &["Press release"]),
            frame(
                2,
                9,
                "Opened the checklist beside it",
                &["Press release", "Checklist"],
            ),
            frame(
                1,
                4,
                "Rearranged the strip",
                &["Checklist", "Press release"],
            ),
        ],
        unavailable: 0,
        note: None,
        pending: false,
    }
}

/// The answers a capture's reads get.
pub(crate) fn fixture(fixture: &mut Fixture<Filter>) {
    seed_ledger();
    let caught_up = fixture.scenario() == Some("caught-up");
    fixture.answer(&wire::target(wire::SUMMARY), &summary(caught_up));
    fixture.answer(&wire::target(wire::FEED), &feed());
    fixture.answer(&wire::target(wire::DETAIL), &detail());
    fixture.answer(&wire::target(wire::REPLAY), &replay());
    fixture.answer(&wire::target(wire::FOCUS), &None::<wire::Focus>);
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav) {
    match scenario {
        Some("history") => nav.show(Place::History),
        Some("detail") => {
            nav.open_in_history(wire::Range {
                from: id(3),
                to: id(1),
            });
            nav.compare.set(true);
        }
        Some("replay") => {
            nav.show(Place::Replay);
            nav.replay_at.set(1.0);
        }
        _ => nav.show(Place::WhatsNew),
    }
}

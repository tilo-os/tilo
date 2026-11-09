//! What the screenshot scenarios show: a person whose two agents worked in
//! their workspaces this afternoon, as the history and the agent door would
//! serve it. A capture reaches no session; these are the answers its reads
//! get instead. Never written into a real workspace.

use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;

use crate::{Console, Nav, Place};

pub(crate) const SCENARIOS: &[&str] =
    &["agents", "run", "command", "workspaces", "connect", "usage"];

const MINUTE_MS: i64 = 60_000;

fn minutes_ago_ms(minutes: i64) -> i64 {
    let now = ui::time::wall()
        .duration_since(ui::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    now.saturating_sub(minutes.saturating_mul(MINUTE_MS))
}

const CLAUDE: &str = "agent:claude-code";
const CODEX: &str = "agent:codex";

fn agent(name: &str, identity: &str, rgb: [u8; 3]) -> wire::Author {
    wire::Author {
        name: name.to_owned(),
        rgb,
        identity: identity.to_owned(),
        relation: wire::Relation::Software,
        on_behalf_of: Some("Ada".to_owned()),
    }
}

fn claude() -> wire::Author {
    agent("Claude Code", CLAUDE, [214, 130, 77])
}

fn codex() -> wire::Author {
    agent("Codex", CODEX, [90, 140, 220])
}

/// The id of the `n`th change, as the history names it.
fn id(n: u8) -> String {
    wire::entry_hex(&[n; 16])
}

fn target(app: &str, app_label: &str, id: &str, label: &str) -> wire::Target {
    wire::Target {
        app_id: Some(app.to_owned()),
        app_label: Some(app_label.to_owned()),
        id: id.to_owned(),
        kind: String::new(),
        label: label.to_owned(),
    }
}

fn action(n: u8, minutes_ago: i64, summary: &str, view: wire::ActionView) -> wire::RunAction {
    let target = view.target().clone();
    // A program that ran and a page that was opened changed no record: there
    // is nothing of theirs to take back.
    let undoable = matches!(
        view,
        wire::ActionView::File { .. } | wire::ActionView::Record { .. }
    );
    wire::RunAction {
        range: wire::Range::one(&id(n)),
        at_ms: minutes_ago_ms(minutes_ago),
        summary: summary.to_owned(),
        app_id: target.app_id.unwrap_or_default(),
        record: target.id,
        view,
        undo: undoable.then(|| wire::Offer {
            action: wire::DetailAction::UndoRecord,
            label: "Undo".to_owned(),
        }),
    }
}

fn line(side: wire::DiffSide, text: &str) -> wire::DiffLine {
    wire::DiffLine {
        side,
        text: text.to_owned(),
    }
}

/// Claude Code's run, working now: a test it ran, a file it changed, the
/// test again, and the page it opened.
fn fix_run() -> wire::AgentRun {
    use wire::DiffSide::{Added, Both, Removed};
    wire::AgentRun {
        id: "run:fix-signup".to_owned(),
        agent: claude(),
        workspace: Some("Storefront".to_owned()),
        instruction: "Fix the failing signup test and open a draft PR".to_owned(),
        decision: Some(
            "The test failed because the email check ran before trimming; trimming first fixes it without loosening the check."
                .to_owned(),
        ),
        started_ms: minutes_ago_ms(14),
        last_ms: minutes_ago_ms(1),
        range: wire::Range {
            from: id(1),
            to: id(4),
        },
        actions: vec![
            action(
                1,
                14,
                "Ran the signup tests",
                wire::ActionView::Command {
                    target: target("so.tilo.terminal", "Terminal", "term:1", "storefront"),
                    command: "cargo test -p signup".to_owned(),
                    output: vec![
                        "running 12 tests".to_owned(),
                        "test email::rejects_padded_address ... FAILED".to_owned(),
                        "test result: FAILED. 11 passed; 1 failed".to_owned(),
                    ],
                    exit: Some(101),
                },
            ),
            action(
                2,
                9,
                "Trimmed the address before checking it",
                wire::ActionView::File {
                    target: target("so.tilo.code-editor", "Code", "file:src/email.rs", "email.rs"),
                    path: "src/email.rs".to_owned(),
                    lines: vec![
                        line(Both, "pub fn valid(address: &str) -> bool {"),
                        line(Removed, "    let at = address.find('@');"),
                        line(Added, "    let address = address.trim();"),
                        line(Added, "    let at = address.find('@');"),
                        line(Both, "    at.is_some_and(|at| at > 0)"),
                    ],
                },
            ),
            action(
                3,
                5,
                "Ran the signup tests again",
                wire::ActionView::Command {
                    target: target("so.tilo.terminal", "Terminal", "term:1", "storefront"),
                    command: "cargo test -p signup".to_owned(),
                    output: vec![
                        "running 12 tests".to_owned(),
                        "test result: ok. 12 passed; 0 failed".to_owned(),
                    ],
                    exit: Some(0),
                },
            ),
            action(
                4,
                1,
                "Opened the pull request page",
                wire::ActionView::Page {
                    target: target(
                        "so.tilo.browser",
                        "Browser",
                        "page:1",
                        "Trim addresses before checking them",
                    ),
                    url: "https://git.example.com/storefront/pulls/412".to_owned(),
                },
            ),
        ],
        undo_blocked: None,
    }
}

fn runs() -> wire::Runs {
    let notes = wire::AgentRun {
        id: "run:release-notes".to_owned(),
        agent: codex(),
        workspace: Some("Launch".to_owned()),
        instruction: "Draft the release notes from this week's merged changes".to_owned(),
        decision: None,
        started_ms: minutes_ago_ms(95),
        last_ms: minutes_ago_ms(80),
        range: wire::Range {
            from: id(5),
            to: id(6),
        },
        actions: vec![
            action(
                5,
                95,
                "Created the page Release notes",
                wire::ActionView::Record {
                    target: target("so.tilo.notes", "Notes", "release-notes", "Release notes"),
                },
            ),
            action(
                6,
                80,
                "Wrote 6 paragraphs",
                wire::ActionView::Record {
                    target: target("so.tilo.notes", "Notes", "release-notes", "Release notes"),
                },
            ),
        ],
        undo_blocked: None,
    };
    let budget = wire::AgentRun {
        id: "run:budget".to_owned(),
        agent: claude(),
        workspace: Some("Launch".to_owned()),
        instruction: "Total the venue quotes in the budget sheet".to_owned(),
        decision: None,
        started_ms: minutes_ago_ms(26 * 60),
        last_ms: minutes_ago_ms(26 * 60 - 3),
        range: wire::Range {
            from: id(7),
            to: id(7),
        },
        actions: vec![action(
            7,
            26 * 60 - 3,
            "Added a total row",
            wire::ActionView::Record {
                target: target("so.tilo.sheets", "Sheets", "budget", "Budget"),
            },
        )],
        undo_blocked: Some("Open Budget to undo this run".to_owned()),
    };
    wire::Runs {
        total: 3,
        runs: vec![fix_run(), notes, budget],
        pending: false,
        mutations: true,
    }
}

fn the_door() -> door::Door {
    door::Door {
        endpoint: "https://mcp.tilo.sh".to_owned(),
        grants: vec![
            door::Grant {
                id: "grant:claude".to_owned(),
                agent: "Claude Code".to_owned(),
                identity: CLAUDE.to_owned(),
                from: Some("ada@studio".to_owned()),
                rgb: [214, 130, 77],
                reach: door::Reach {
                    workspaces: Vec::new(),
                    create: true,
                },
                granted_ms: minutes_ago_ms(9 * 24 * 60),
                last_ms: Some(minutes_ago_ms(1)),
                connected: 1,
            },
            door::Grant {
                id: "grant:codex".to_owned(),
                agent: "Codex".to_owned(),
                identity: CODEX.to_owned(),
                from: Some("ada@laptop".to_owned()),
                rgb: [90, 140, 220],
                reach: door::Reach {
                    workspaces: vec!["Launch".to_owned()],
                    create: false,
                },
                granted_ms: minutes_ago_ms(3 * 24 * 60),
                last_ms: Some(minutes_ago_ms(80)),
                connected: 0,
            },
        ],
        workspaces: vec![
            door::Workspace {
                id: "ws-storefront".to_owned(),
                name: "Storefront".to_owned(),
                made_by: door::Maker::Agent {
                    grant: "grant:claude".to_owned(),
                    agent: "Claude Code".to_owned(),
                },
                made_ms: minutes_ago_ms(2 * 24 * 60),
                open: "tilo://open#/room/ws-storefront".to_owned(),
            },
            door::Workspace {
                id: "ws-launch".to_owned(),
                name: "Launch".to_owned(),
                made_by: door::Maker::Person,
                made_ms: minutes_ago_ms(12 * 24 * 60),
                open: "tilo://open#/room/ws-launch".to_owned(),
            },
        ],
    }
}

fn detail() -> wire::Detail {
    wire::Detail {
        title: "Wrote 6 paragraphs".to_owned(),
        changes: vec![wire::FieldChange {
            field: "Highlights".to_owned(),
            before: String::new(),
            after: "Signup accepts addresses with stray spaces".to_owned(),
        }],
        ..wire::Detail::default()
    }
}

/// The answers a capture's reads get.
pub(crate) fn fixture(fixture: &mut Fixture<Console>) {
    fixture.answer(&wire::target(wire::RUNS), &runs());
    fixture.answer(&wire::target(wire::DETAIL), &detail());
    fixture.answer(&door::target(door::DOOR), &the_door());
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav) {
    match scenario {
        Some("run") => {
            nav.show(Place::Agents);
            nav.open_run("run:fix-signup");
            nav.action.set(Some(1));
        }
        Some("command") => {
            nav.show(Place::Agents);
            nav.open_run("run:fix-signup");
            nav.action.set(Some(0));
        }
        Some("workspaces") => nav.show(Place::Workspaces),
        Some("connect") => nav.show(Place::Connect),
        Some("usage") => nav.show(Place::Usage),
        _ => nav.show(Place::Agents),
    }
}

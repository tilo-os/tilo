//! Pictures of the desktop's search over what a workspace's records hold.
//!
//! A message, a conversation and an agent of Chat, found from the records
//! themselves: the index these land in is the one an app's closed window
//! leaves behind, and no Chat window runs here. Captured through the
//! production frame path of a headless session, as a person sees it.
//!
//! ```text
//! cargo run -p tilo-session --example record_search_shots -- ~/.cache/record-search-shots
//! ```

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a capture binary reports the files it wrote"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_desktop::desk_verbs::{DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::providers::AppEntry;
use tilo_session::{AppLists, Lists, Options, SearchOptions, Session};
use ui::search::index_publication::{IndexDelta, IndexEntry};

/// The app whose records are read.
const CHAT: &str = "so.tilo.chat";

/// One entry an app published of a record: what a hit opens, what it says
/// and where the panel lists it.
fn entry(key: &str, title: &str, subtitle: Option<&str>) -> IndexEntry {
    IndexEntry {
        key: key.to_owned(),
        resource: key.to_owned(),
        action: None,
        title: title.to_owned(),
        subtitle: subtitle.map(str::to_owned),
        group: None,
        icon: None,
        boost: 0,
        fields: Vec::new(),
        secret: false,
        references: Vec::new(),
    }
}

/// `entries` published as what `record` of `workspace` holds, as the app's
/// own record reader publishes them.
fn published(content: &AppContentIndex, workspace: &str, record: &str, entries: Vec<IndexEntry>) {
    let delta = IndexDelta {
        reset: true,
        upsert: entries,
        ..IndexDelta::default()
    };
    content
        .publish(workspace, CHAT, record, &delta.encode().expect("a delta"))
        .expect("the index takes the publication");
}

/// The chat this workspace's records hold, as a person's Chat would have
/// written it: a conversation named for its work, the message that asks an
/// agent into it, and the agent itself.
fn hold(content: &AppContentIndex, workspace: &str) {
    published(
        content,
        workspace,
        "chat.workspace",
        vec![entry(
            "chat.workspace/channels/c_crit",
            "Ada's design crit",
            Some("Share work, get sharp feedback"),
        )],
    );
    published(
        content,
        workspace,
        "messages:1",
        vec![entry(
            "chat.workspace/messages/m_wheel",
            "Ada, can you look at the wheel again?",
            None,
        )],
    );
    published(
        content,
        workspace,
        "agents",
        vec![entry("chat.agent/agents/a_ada", "Ada", Some("Agent"))],
    );
}

fn main() -> std::process::ExitCode {
    // A session's workers are this same binary in their own modes, so it
    // serves them first: a worker that fell through to the capture would be
    // a session starting inside a session's worker.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = tilo_render_service::dispatch(&args) {
        return code;
    }

    let dir: PathBuf = args
        .first()
        .expect("a directory to write the pictures in")
        .into();
    std::fs::create_dir_all(&dir).expect("the pictures' directory");

    let _settled = ui::motion::testing::settled();
    let content = Arc::new(AppContentIndex::in_memory());
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            search: SearchOptions {
                lists: Lists::fixed(AppLists {
                    applications: vec![AppEntry {
                        id: CHAT.to_owned(),
                        title: "Chat".to_owned(),
                        category: None,
                        description: None,
                    }],
                    ..AppLists::default()
                }),
                content: Some(Arc::clone(&content)),
                ..SearchOptions::default()
            },
            person: "Ada".to_owned(),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("step");
    let workspace = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("a workspace");
    hold(&content, &workspace);

    // The person types `ada` into search: the conversation, the message and
    // the agent it names come up, each from its own record.
    let query = serde_json::Map::from_iter([(
        "query".to_owned(),
        serde_json::Value::String("ada".to_owned()),
    )]);
    session
        .invoke(DESK_RESOURCE, "search", &query)
        .expect("search opens");
    // The panel's rows are the desk's own resources: every hit the person
    // sees is one, so the picture is of a search that found them.
    let mut titles = Vec::new();
    for _ in 0..200 {
        session.step(Duration::from_millis(16)).expect("step");
        titles = rows(&session);
        let listed = [
            "Ada's design crit",
            "Ada, can you look at the wheel again?",
            "Ada",
        ];
        if listed
            .iter()
            .all(|title| titles.iter().any(|row| row.starts_with(title)))
        {
            break;
        }
    }
    for wanted in [
        "Ada's design crit",
        "Ada, can you look at the wheel again?",
        "Ada",
    ] {
        assert!(
            titles.iter().any(|row| row.starts_with(wanted)),
            "the panel lists {wanted:?}: {titles:?}"
        );
    }

    // The frame the person sees is the one after the rows reached the desk.
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).expect("step");
    }

    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a screenshot of the display");
    let name = dir.join("search-ada.png");
    image::save_buffer(
        &name,
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("the capture written");
    println!("{}", name.display());
    for title in &titles {
        println!("  {title}");
    }
    std::process::ExitCode::SUCCESS
}

/// The search resource the desk publishes, wherever it sits in its tree.
fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// The labels of the open search's rows, as the desk publishes them.
fn rows(session: &Session) -> Vec<String> {
    find(&session.resources(), SEARCH_RESOURCE)
        .map(|search| {
            search
                .children
                .into_iter()
                .filter(|row| row.kind == SEARCH_ROW_KIND)
                .map(|row| row.label)
                .collect()
        })
        .unwrap_or_default()
}
